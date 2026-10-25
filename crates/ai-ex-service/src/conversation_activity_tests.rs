use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use ai_ex_audio::{SpeechQueue, SpeechReceiver};
use ai_ex_core::{AvatarPort, LanguageModelPort, ModelRequest, Runtime, SpeechPort, spawn_runtime};
use ai_ex_domain::TurnId;
use ai_ex_memory::MemoryStore;
use ai_ex_observability::EventHub;
use ai_ex_safety::{SafetyGate, SafetyPolicy};
use async_trait::async_trait;
use tokio::sync::mpsc;
use uuid::Uuid;

struct HeldModel {
    entered: mpsc::Sender<oneshot::Sender<()>>,
}

#[async_trait]
impl LanguageModelPort for HeldModel {
    async fn stream(
        &mut self,
        _request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let (release, held) = oneshot::channel();
        self.entered
            .send(release)
            .await
            .map_err(|_| AppError::unavailable("test observer closed"))?;
        held.await
            .map_err(|_| AppError::unavailable("test request cancelled"))?;
        let (sender, receiver) = mpsc::channel(1);
        sender.send(Ok("Synthetic answer.".into())).await.unwrap();
        Ok(receiver)
    }
    async fn cancel(&mut self, _turn_id: TurnId) -> Result<(), AppError> {
        Ok(())
    }
}

struct SilentSpeech;
#[async_trait]
impl SpeechPort for SilentSpeech {
    async fn enqueue(&mut self, _turn_id: TurnId, _sentence: String) -> Result<(), AppError> {
        Ok(())
    }
    async fn interrupt(&mut self) -> Result<(), AppError> {
        Ok(())
    }
}

struct SilentAvatar;
#[async_trait]
impl AvatarPort for SilentAvatar {
    async fn set_speaking(&mut self, _speaking: bool) -> Result<(), AppError> {
        Ok(())
    }
    async fn set_neutral(&mut self) -> Result<(), AppError> {
        Ok(())
    }
}

fn fixture() -> (
    ConversationActivity,
    TemplateHost,
    mpsc::Receiver<oneshot::Sender<()>>,
    SpeechReceiver,
    EventHub,
) {
    let (speech, receiver) = SpeechQueue::new(2).unwrap();
    let host = TemplateHost::new(
        Uuid::new_v4(),
        speech.activity(),
        Arc::new(SafetyGate::new(SafetyPolicy {
            automation_enabled: false,
            allowed_capabilities: BTreeSet::new(),
            allowed_targets: Vec::new(),
        })),
    )
    .unwrap();
    let (entered, observed) = mpsc::channel(8);
    let events = EventHub::new(64).unwrap();
    let runtime = Runtime::new(
        HeldModel { entered },
        SilentSpeech,
        SilentAvatar,
        MemoryStore::disabled(),
        events.clone(),
    );
    let handle = spawn_runtime(runtime, 1).unwrap();
    (
        ConversationActivity::new(handle, host.clone()),
        host,
        observed,
        receiver,
        events,
    )
}

async fn pending_count(host: &TemplateHost, expected: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if host.pending_chat_count().unwrap() == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual conversation owner releases its reservation");
}

async fn entered(observed: &mut mpsc::Receiver<oneshot::Sender<()>>) -> oneshot::Sender<()> {
    tokio::time::timeout(Duration::from_secs(3), observed.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn synchronous_start_registers_before_an_unpolled_wait_is_dropped() {
    let (runtime, host, mut observed, _receiver, events) = fixture();
    let pending = runtime.start_submit("detached request").unwrap();
    assert_eq!(
        host.pending_chat_count().unwrap(),
        1,
        "admission is synchronous before a monitor is scheduled"
    );
    drop(Box::pin(pending.wait()));
    assert_eq!(host.pending_chat_count().unwrap(), 1);
    let release = entered(&mut observed).await;
    assert_eq!(host.pending_chat_count().unwrap(), 1);
    release.send(()).unwrap();
    pending_count(&host, 0).await;
    assert_eq!(
        events.current().turns_completed,
        1,
        "caller loss does not cancel admitted generation"
    );
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropped_admitted_queue_wait_still_blocks_idle_until_its_real_turn_settles() {
    let (runtime, host, mut observed, _receiver, events) = fixture();
    let first = runtime.enqueue("first").await.unwrap();
    let first_release = entered(&mut observed).await;
    let second = runtime.enqueue("queued second").await.unwrap();
    assert_eq!(host.pending_chat_count().unwrap(), 2);
    drop(second);
    first_release.send(()).unwrap();
    assert!(matches!(
        first.wait().await.unwrap(),
        TurnOutcome::Completed(_)
    ));
    let second_release = entered(&mut observed).await;
    pending_count(&host, 1).await;
    runtime
        .interrupt("cancel queued generation after admission")
        .await
        .unwrap();
    pending_count(&host, 0).await;
    assert!(
        second_release.send(()).is_err(),
        "interruption cancels the fake provider future"
    );
    assert_eq!(events.current().turns_started, 2);
    assert_eq!(events.current().turns_interrupted, 1);
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_admission_releases_only_its_reservation_and_keeps_existing_work_busy() {
    let (runtime, host, mut observed, _receiver, _events) = fixture();
    let first = runtime.enqueue("active").await.unwrap();
    let first_release = entered(&mut observed).await;
    let second = runtime.enqueue("pending").await.unwrap();
    assert!(runtime.enqueue("overflow").await.is_err());
    pending_count(&host, 2).await;
    runtime.interrupt("release active").await.unwrap();
    assert!(matches!(
        first.wait().await.unwrap(),
        TurnOutcome::Interrupted(_)
    ));
    assert!(first_release.send(()).is_err());
    let second_release = entered(&mut observed).await;
    pending_count(&host, 1).await;
    second_release.send(()).unwrap();
    assert!(matches!(
        second.wait().await.unwrap(),
        TurnOutcome::Completed(_)
    ));
    pending_count(&host, 0).await;
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_drops_queued_response_but_never_leaks_an_activity_reservation() {
    let (runtime, host, mut observed, _receiver, _events) = fixture();
    assert!(runtime.start_submit("  ").is_err());
    assert_eq!(host.pending_chat_count().unwrap(), 0);
    let first = runtime.enqueue("active").await.unwrap();
    let release = entered(&mut observed).await;
    let second = runtime.enqueue("queued").await.unwrap();
    assert_eq!(host.pending_chat_count().unwrap(), 2);
    tokio::time::timeout(Duration::from_secs(3), runtime.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        first.wait().await.unwrap(),
        TurnOutcome::Shutdown(_)
    ));
    assert!(second.wait().await.is_err());
    pending_count(&host, 0).await;
    assert!(release.send(()).is_err());
    host.shutdown().await.unwrap();
    assert!(runtime.start_submit("closed host").is_err());
}
