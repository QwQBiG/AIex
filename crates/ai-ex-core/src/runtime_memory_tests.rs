use super::*;
use ai_ex_domain::{ErrorKind, MemoryPage, MemoryRequest, MemoryResponse};

#[derive(Default)]
struct ManagedMemory {
    fail: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    entered: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
}

#[async_trait]
impl MemoryPort for ManagedMemory {
    async fn manage(&mut self, request: MemoryRequest) -> Result<MemoryResponse, AppError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if let Some(entered) = &self.entered {
            entered.notify_one();
        }
        if let Some(release) = &self.release {
            release.notified().await;
        }
        if self.fail.load(Ordering::Acquire) {
            return Err(AppError::unavailable("simulated durable write failed"));
        }
        Ok(if request.is_mutation() {
            MemoryResponse::Changed
        } else {
            MemoryResponse::Page(MemoryPage {
                profile_id: request.profile_id().to_owned(),
                enabled: true,
                total: 0,
                offset: 0,
                entries: Vec::new(),
                truncated_ids: Vec::new(),
                knowledge: Default::default(),
            })
        })
    }

    async fn recall(&self, _: &str, _: usize) -> Result<Vec<Message>, AppError> {
        Ok(Vec::new())
    }

    async fn remember(&mut self, _: TurnId, _: String, _: String) -> Result<(), AppError> {
        Ok(())
    }
}

fn note(profile: &str) -> MemoryRequest {
    MemoryRequest::Remember {
        profile_id: profile.to_owned(),
        text: "an explicit preference".to_owned(),
    }
}

fn list() -> MemoryRequest {
    MemoryRequest::List {
        profile_id: "default".to_owned(),
        query: String::new(),
        kind: None,
        offset: 0,
        limit: 20,
    }
}

#[tokio::test]
async fn only_successful_mutations_clear_history_and_profile_is_checked_before_storage() {
    let captured = Arc::new(Mutex::new(None));
    let state = Arc::new(TestState::default());
    let memory = ManagedMemory::default();
    let fail = memory.fail.clone();
    let calls = memory.calls.clone();
    let runtime = Runtime::new(
        CaptureModel(captured.clone()),
        TestSpeech(state.clone()),
        TestAvatar(state.clone()),
        memory,
        TestEvents,
    );
    let handle = spawn_runtime(runtime, 4).unwrap();
    handle.submit("old context value").await.unwrap();
    assert_eq!(
        handle.memory(note("other")).await.unwrap_err().kind,
        ErrorKind::InvalidTransition
    );
    assert_eq!(calls.load(Ordering::Acquire), 0);
    handle.memory(list()).await.unwrap();
    fail.store(true, Ordering::Release);
    assert!(handle.memory(note("default")).await.is_err());
    handle.submit("first followup").await.unwrap();
    assert!(
        captured
            .lock()
            .await
            .take()
            .unwrap()
            .messages
            .iter()
            .any(|item| item.content == "old context value")
    );
    fail.store(false, Ordering::Release);
    state.speech_interrupted.store(false, Ordering::Release);
    state.avatar_neutral.store(false, Ordering::Release);
    handle.memory(note("default")).await.unwrap();
    assert!(state.speech_interrupted.load(Ordering::Acquire));
    assert!(state.avatar_neutral.load(Ordering::Acquire));
    handle.submit("second followup").await.unwrap();
    assert!(
        !captured
            .lock()
            .await
            .take()
            .unwrap()
            .messages
            .iter()
            .any(|item| item.content == "old context value" || item.content == "first followup")
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn active_generation_rejects_memory_without_cancelling_reply_then_allows_it_after_interrupt()
{
    let state = Arc::new(TestState::default());
    let runtime = Runtime::new(
        TestModel {
            state: state.clone(),
            sender: None,
        },
        TestSpeech(state.clone()),
        TestAvatar(state.clone()),
        ManagedMemory::default(),
        TestEvents,
    );
    let handle = spawn_runtime(runtime, 4).unwrap();
    let pending = handle.enqueue("keep talking").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !state.model_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for request in [note("default"), list()] {
        assert_eq!(
            handle.memory(request).await.unwrap_err().kind,
            ErrorKind::InvalidTransition
        );
    }
    assert!(!state.model_cancelled.load(Ordering::Acquire));
    handle.interrupt("edit memory").await.unwrap();
    assert!(matches!(
        pending.wait().await.unwrap(),
        TurnOutcome::Interrupted(_)
    ));
    assert_eq!(
        handle.memory(note("default")).await.unwrap(),
        MemoryResponse::Changed
    );
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn slow_memory_change_preserves_write_while_interrupts_stop_output() {
    let state = Arc::new(TestState::default());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let captured = Arc::new(Mutex::new(None));
    let runtime = Runtime::new(
        CaptureModel(captured.clone()),
        TestSpeech(state.clone()),
        TestAvatar(state.clone()),
        ManagedMemory {
            entered: Some(entered.clone()),
            release: Some(release.clone()),
            ..Default::default()
        },
        TestEvents,
    );
    let handle = spawn_runtime(runtime, 4).unwrap();
    handle.submit("old context").await.unwrap();
    state.speech_interrupted.store(false, Ordering::Release);
    let editor = handle.clone();
    let edit = tokio::spawn(async move { editor.memory(note("default")).await });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        handle.enqueue("during edit").await.err().unwrap().kind,
        ErrorKind::InvalidTransition
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        for _ in 0..16 {
            handle.interrupt("stop output").await.unwrap();
        }
        while !state.speech_interrupted.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("interrupts must not wait for the durable write");
    assert!(!edit.is_finished());
    release.notify_one();
    assert_eq!(edit.await.unwrap().unwrap(), MemoryResponse::Changed);
    handle.submit("after edit").await.unwrap();
    assert!(
        !captured
            .lock()
            .await
            .take()
            .unwrap()
            .messages
            .iter()
            .any(|item| item.content == "old context")
    );
    handle.shutdown().await.unwrap();
}

struct FailingCancellation;

#[async_trait]
impl SpeechPort for FailingCancellation {
    async fn enqueue(&mut self, _: TurnId, _: String) -> Result<(), AppError> {
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<(), AppError> {
        Err(AppError::unavailable("simulated audio output failure"))
    }
}

#[tokio::test]
async fn output_failure_cannot_hide_a_committed_memory_edit() {
    let state = Arc::new(TestState::default());
    let mut runtime = Runtime::new(
        CaptureModel(Arc::new(Mutex::new(None))),
        FailingCancellation,
        TestAvatar(state.clone()),
        ManagedMemory::default(),
        TestEvents,
    );
    assert_eq!(
        runtime.memory(note("default")).await.unwrap(),
        MemoryResponse::Changed
    );
    assert!(state.avatar_neutral.load(Ordering::Acquire));
}

#[tokio::test]
async fn abandoned_memory_caller_and_shutdown_do_not_cancel_a_durable_write() {
    let state = Arc::new(TestState::default());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime = Runtime::new(
        CaptureModel(Arc::new(Mutex::new(None))),
        TestSpeech(state.clone()),
        TestAvatar(state.clone()),
        ManagedMemory {
            entered: Some(entered.clone()),
            release: Some(release.clone()),
            ..Default::default()
        },
        TestEvents,
    );
    let handle = spawn_runtime(runtime, 4).unwrap();
    let editor = handle.clone();
    let edit = tokio::spawn(async move { editor.memory(note("default")).await });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    edit.abort();
    assert!(edit.await.unwrap_err().is_cancelled());
    let closer = handle.clone();
    let mut close = tokio::spawn(async move { closer.shutdown().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut close)
            .await
            .is_err()
    );
    assert!(state.speech_interrupted.load(Ordering::Acquire));
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(handle.memory(note("default")).await.is_err());
}
