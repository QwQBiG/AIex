use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use ai_ex_domain::{AppError, Emotion, Message, Role, SystemEvent, TurnId};
use async_trait::async_trait;
use tokio::sync::{Mutex, Notify, mpsc};

use crate::{
    AvatarPort, ConversationPolicy, EventSink, LanguageModelPort, MemoryPort, ModelRequest,
    Runtime, RuntimeControl, SpeechPort, TurnOutcome, spawn_runtime,
};

#[path = "persona_tests.rs"]
mod persona_tests;

#[path = "runtime_memory_tests.rs"]
mod memory_tests;

#[path = "runtime_cancellation_tests.rs"]
mod cancellation_tests;

#[path = "proactive_tests.rs"]
mod proactive_tests;

#[derive(Default)]
struct TestState {
    model_cancelled: AtomicBool,
    model_started: AtomicBool,
    stream_count: AtomicUsize,
    speech_interrupted: AtomicBool,
    avatar_neutral: AtomicBool,
    remembered: AtomicUsize,
    emotion: Mutex<Option<Emotion>>,
}

struct TestModel {
    state: Arc<TestState>,
    sender: Option<mpsc::Sender<Result<String, AppError>>>,
}

#[async_trait]
impl LanguageModelPort for TestModel {
    async fn stream(
        &mut self,
        _request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let (sender, receiver) = mpsc::channel(4);
        let count = self.state.stream_count.fetch_add(1, Ordering::AcqRel);
        if count == 0 {
            self.sender = Some(sender);
        } else {
            sender
                .send(Ok("completed.".to_owned()))
                .await
                .expect("chunk sent");
        }
        self.state.model_started.store(true, Ordering::Release);
        Ok(receiver)
    }

    async fn cancel(&mut self, _turn_id: TurnId) -> Result<(), AppError> {
        self.sender = None;
        self.state.model_cancelled.store(true, Ordering::Release);
        Ok(())
    }
}

struct TestSpeech(Arc<TestState>);

#[async_trait]
impl SpeechPort for TestSpeech {
    async fn enqueue(&mut self, _turn_id: TurnId, _sentence: String) -> Result<(), AppError> {
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<(), AppError> {
        self.0.speech_interrupted.store(true, Ordering::Release);
        Ok(())
    }
}

struct TestAvatar(Arc<TestState>);

#[async_trait]
impl AvatarPort for TestAvatar {
    async fn set_speaking(&mut self, _speaking: bool) -> Result<(), AppError> {
        Ok(())
    }

    async fn set_neutral(&mut self) -> Result<(), AppError> {
        self.0.avatar_neutral.store(true, Ordering::Release);
        Ok(())
    }

    async fn set_emotion(&mut self, emotion: Emotion) -> Result<(), AppError> {
        *self.0.emotion.lock().await = Some(emotion);
        Ok(())
    }
}

struct TestMemory(Arc<TestState>);

#[async_trait]
impl MemoryPort for TestMemory {
    async fn recall(&self, _query: &str, _limit: usize) -> Result<Vec<Message>, AppError> {
        Ok(Vec::new())
    }

    async fn remember(
        &mut self,
        _turn_id: TurnId,
        _user_text: String,
        _assistant_text: String,
    ) -> Result<(), AppError> {
        self.0.remembered.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

struct CaptureModel(Arc<Mutex<Option<ModelRequest>>>);

#[async_trait]
impl LanguageModelPort for CaptureModel {
    async fn stream(
        &mut self,
        request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        *self.0.lock().await = Some(request);
        let (sender, receiver) = mpsc::channel(1);
        sender
            .send(Ok("done.".to_owned()))
            .await
            .expect("chunk sent");
        Ok(receiver)
    }

    async fn cancel(&mut self, _turn_id: TurnId) -> Result<(), AppError> {
        Ok(())
    }
}

struct TaggedModel;

#[async_trait]
impl LanguageModelPort for TaggedModel {
    async fn stream(
        &mut self,
        _request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let (sender, receiver) = mpsc::channel(2);
        sender
            .send(Ok("[hap".to_owned()))
            .await
            .expect("chunk sent");
        sender
            .send(Ok("py] hello.".to_owned()))
            .await
            .expect("chunk sent");
        Ok(receiver)
    }

    async fn cancel(&mut self, _turn_id: TurnId) -> Result<(), AppError> {
        Ok(())
    }
}

struct CaptureMemory(Arc<AtomicUsize>);

#[async_trait]
impl MemoryPort for CaptureMemory {
    async fn recall(&self, _query: &str, limit: usize) -> Result<Vec<Message>, AppError> {
        self.0.store(limit, Ordering::Release);
        Ok(vec![Message::new(Role::System, "remembered fact")])
    }

    async fn remember(
        &mut self,
        _turn_id: TurnId,
        _user_text: String,
        _assistant_text: String,
    ) -> Result<(), AppError> {
        Ok(())
    }
}

struct RememberedText(Arc<Mutex<Option<String>>>);

#[async_trait]
impl MemoryPort for RememberedText {
    async fn recall(&self, _query: &str, _limit: usize) -> Result<Vec<Message>, AppError> {
        Ok(Vec::new())
    }

    async fn remember(
        &mut self,
        _turn_id: TurnId,
        _user_text: String,
        assistant_text: String,
    ) -> Result<(), AppError> {
        *self.0.lock().await = Some(assistant_text);
        Ok(())
    }
}

struct TestEvents;

#[tokio::test]
async fn actor_interrupts_remaining_audio_after_generation_has_finished() {
    let state = Arc::new(TestState::default());
    let runtime = Runtime::new(
        CaptureModel(Arc::new(Mutex::new(None))),
        TestSpeech(Arc::clone(&state)),
        TestAvatar(Arc::clone(&state)),
        TestMemory(Arc::clone(&state)),
        TestEvents,
    );
    let handle = spawn_runtime(runtime, 4).unwrap();
    handle.submit("hello".to_owned()).await.unwrap();
    // Starting an input has already cancelled any preceding audio.
    state.speech_interrupted.store(false, Ordering::Release);
    handle.interrupt("stop remaining speech").await.unwrap();
    assert!(state.speech_interrupted.load(Ordering::Acquire));
    assert!(state.avatar_neutral.load(Ordering::Acquire));
    handle.interrupt("repeat stop").await.unwrap();
    handle.shutdown().await.unwrap();
}

#[async_trait]
impl EventSink for TestEvents {
    async fn publish(&mut self, _event: SystemEvent) {}
}

struct BlockingEvents {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl EventSink for BlockingEvents {
    async fn publish(&mut self, event: SystemEvent) {
        if matches!(event, SystemEvent::TurnInterrupted { .. }) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[tokio::test]
async fn control_interrupt_cancels_every_active_output() {
    let state = Arc::new(TestState::default());
    let model = TestModel {
        state: Arc::clone(&state),
        sender: None,
    };
    let mut runtime = Runtime::new(
        model,
        TestSpeech(Arc::clone(&state)),
        TestAvatar(Arc::clone(&state)),
        TestMemory(Arc::clone(&state)),
        TestEvents,
    );
    let (control, mut receiver) = mpsc::channel(1);
    control
        .send(RuntimeControl::Interrupt {
            reason: "barge-in".to_owned(),
        })
        .await
        .expect("control queued");

    let outcome = runtime
        .run_turn_controlled("hello", &mut receiver)
        .await
        .expect("turn interrupted cleanly");
    assert!(matches!(outcome, TurnOutcome::Interrupted(_)));
    assert!(state.model_cancelled.load(Ordering::Acquire));
    assert!(state.speech_interrupted.load(Ordering::Acquire));
    assert!(state.avatar_neutral.load(Ordering::Acquire));
    assert_eq!(state.remembered.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn actor_accepts_interrupt_while_submit_is_waiting() {
    let state = Arc::new(TestState::default());
    let runtime = Runtime::new(
        TestModel {
            state: Arc::clone(&state),
            sender: None,
        },
        TestSpeech(Arc::clone(&state)),
        TestAvatar(Arc::clone(&state)),
        TestMemory(Arc::clone(&state)),
        TestEvents,
    );
    let handle = spawn_runtime(runtime, 4).expect("actor starts");
    let submit_handle = handle.clone();
    let turn = tokio::spawn(async move { submit_handle.submit("hello").await });
    while !state.model_started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    handle
        .interrupt("barge-in")
        .await
        .expect("interrupt accepted");
    let outcome = turn
        .await
        .expect("submit task joins")
        .expect("turn resolves");
    assert!(matches!(outcome, TurnOutcome::Interrupted(_)));
    assert!(state.model_cancelled.load(Ordering::Acquire));
    handle.shutdown().await.expect("actor shuts down");
}

#[tokio::test]
async fn actor_queues_a_new_turn_during_barge_in() {
    let state = Arc::new(TestState::default());
    let runtime = Runtime::new(
        TestModel {
            state: Arc::clone(&state),
            sender: None,
        },
        TestSpeech(Arc::clone(&state)),
        TestAvatar(Arc::clone(&state)),
        TestMemory(Arc::clone(&state)),
        TestEvents,
    );
    let handle = spawn_runtime(runtime, 4).expect("actor starts");
    let first_handle = handle.clone();
    let first = tokio::spawn(async move { first_handle.submit("first").await });
    while !state.model_started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    let second_handle = handle.clone();
    let second = tokio::spawn(async move { second_handle.submit("second").await });
    tokio::task::yield_now().await;
    handle
        .interrupt("barge-in")
        .await
        .expect("interrupt accepted");

    let first = first.await.expect("first joins").expect("first resolves");
    let second = second
        .await
        .expect("second joins")
        .expect("second resolves");
    assert!(matches!(first, TurnOutcome::Interrupted(_)));
    assert!(matches!(second, TurnOutcome::Completed(_)));
    assert_eq!(state.stream_count.load(Ordering::Acquire), 2);
    handle.shutdown().await.expect("actor shuts down");
}

#[tokio::test]
async fn actor_shutdown_waits_until_the_active_turn_has_stopped() {
    let state = Arc::new(TestState::default());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runtime = Runtime::new(
        TestModel {
            state: Arc::clone(&state),
            sender: None,
        },
        TestSpeech(Arc::clone(&state)),
        TestAvatar(Arc::clone(&state)),
        TestMemory(Arc::clone(&state)),
        BlockingEvents {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
    );
    let handle = spawn_runtime(runtime, 4).expect("actor starts");
    let submitter = handle.clone();
    let turn = tokio::spawn(async move { submitter.submit("hello").await });
    while !state.model_started.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    let shutdown_handle = handle.clone();
    let mut shutdown = tokio::spawn(async move { shutdown_handle.shutdown().await });
    entered.notified().await;

    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut shutdown)
            .await
            .is_err()
    );
    release.notify_one();
    shutdown
        .await
        .expect("shutdown joins")
        .expect("shutdown succeeds");
    let outcome = turn.await.expect("turn joins").expect("turn resolves");
    assert!(matches!(outcome, TurnOutcome::Shutdown(_)));
    assert!(state.speech_interrupted.load(Ordering::Acquire));
    assert!(state.avatar_neutral.load(Ordering::Acquire));
}

#[tokio::test]
async fn conversation_policy_controls_prompt_and_memory_budget() {
    let request = Arc::new(Mutex::new(None));
    let recall_limit = Arc::new(AtomicUsize::new(usize::MAX));
    let mut runtime = Runtime::with_policy(
        CaptureModel(Arc::clone(&request)),
        TestSpeech(Arc::new(TestState::default())),
        TestAvatar(Arc::new(TestState::default())),
        CaptureMemory(Arc::clone(&recall_limit)),
        TestEvents,
        ConversationPolicy {
            system_prompt: "You are AIex".to_owned(),
            history_turn_limit: 2,
            memory_recall_limit: 3,
        },
    )
    .expect("policy is valid");

    runtime.run_turn("hello").await.expect("turn completes");

    assert_eq!(recall_limit.load(Ordering::Acquire), 3);
    let request = request.lock().await.take().expect("request captured");
    assert_eq!(request.messages.len(), 3);
    assert_eq!(
        request.messages[0],
        Message::new(Role::System, "You are AIex")
    );
    assert_eq!(
        request.messages[1],
        Message::new(Role::System, "remembered fact")
    );
    assert_eq!(request.messages[2], Message::new(Role::User, "hello"));
}

#[tokio::test]
async fn runtime_accepts_persona_prompt_updates_between_turns() {
    let request = Arc::new(Mutex::new(None));
    let mut runtime = Runtime::new(
        CaptureModel(Arc::clone(&request)),
        TestSpeech(Arc::new(TestState::default())),
        TestAvatar(Arc::new(TestState::default())),
        TestMemory(Arc::new(TestState::default())),
        TestEvents,
    );
    runtime
        .set_system_prompt("new persona")
        .expect("persona update accepted");
    runtime.run_turn("hello").await.expect("turn completes");
    let request = request.lock().await.take().expect("request captured");
    assert_eq!(
        request.messages[0],
        Message::new(Role::System, "new persona")
    );
}
#[tokio::test]
async fn leading_emotion_tag_controls_avatar_but_not_conversation_text() {
    let state = Arc::new(TestState::default());
    let remembered = Arc::new(Mutex::new(None));
    let mut runtime = Runtime::new(
        TaggedModel,
        TestSpeech(Arc::clone(&state)),
        TestAvatar(Arc::clone(&state)),
        RememberedText(Arc::clone(&remembered)),
        TestEvents,
    );

    runtime.run_turn("hello").await.expect("turn completes");

    assert_eq!(*state.emotion.lock().await, Some(Emotion::Happy));
    assert_eq!(remembered.lock().await.as_deref(), Some("hello."));
}
