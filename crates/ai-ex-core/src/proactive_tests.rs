use super::*;

struct CapturedEvents(Arc<Mutex<Vec<SystemEvent>>>);
#[async_trait]
impl EventSink for CapturedEvents {
    async fn publish(&mut self, event: SystemEvent) {
        self.0.lock().await.push(event);
    }
}

struct Requests(Arc<Mutex<Vec<ModelRequest>>>);
#[async_trait]
impl LanguageModelPort for Requests {
    async fn stream(
        &mut self,
        request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let mut requests = self.0.lock().await;
        requests.push(request);
        let (sender, receiver) = mpsc::channel(1);
        sender
            .send(Ok(format!("response-{}.", requests.len())))
            .await
            .unwrap();
        Ok(receiver)
    }
    async fn cancel(&mut self, _: TurnId) -> Result<(), AppError> {
        Ok(())
    }
}

#[tokio::test]
async fn system_intent_generates_without_inventing_user_history_or_memory() {
    let state = Arc::new(TestState::default());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));
    let handle = spawn_runtime(
        Runtime::new(
            Requests(requests.clone()),
            TestSpeech(state.clone()),
            TestAvatar(state.clone()),
            TestMemory(state.clone()),
            CapturedEvents(events.clone()),
        ),
        4,
    )
    .unwrap();
    handle.submit("first user input").await.unwrap();
    let TurnOutcome::Completed(proactive) = handle.proactive().await.unwrap() else {
        panic!("completed system intent")
    };
    // Completed proactive audio can still be cancelled while generation is idle.
    state.speech_interrupted.store(false, Ordering::Release);
    handle.cancel_proactive().await.unwrap();
    assert!(state.speech_interrupted.load(Ordering::Acquire));
    handle.submit("second user input").await.unwrap();
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].messages.last().unwrap().role, Role::System);
    assert_eq!(
        requests[1]
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
    assert!(
        !requests[2]
            .messages
            .iter()
            .any(|message| message.content == "response-2.")
    );
    assert_eq!(state.remembered.load(Ordering::Acquire), 2);
    let events = events.lock().await;
    assert!(events.iter().any(
        |event| matches!(event, SystemEvent::ProactiveStarted { turn_id } if *turn_id == proactive)
    ));
    assert!(!events.iter().any(
        |event| matches!(event, SystemEvent::TurnStarted { turn_id, .. } if *turn_id == proactive)
    ));
    drop(events);
    drop(requests);
    handle.shutdown().await.unwrap();
}

fn blocking_handle(state: &Arc<TestState>) -> crate::RuntimeHandle {
    spawn_runtime(
        Runtime::new(
            TestModel {
                state: state.clone(),
                sender: None,
            },
            TestSpeech(state.clone()),
            TestAvatar(state.clone()),
            TestMemory(state.clone()),
            TestEvents,
        ),
        4,
    )
    .unwrap()
}

async fn started(state: &TestState) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !state.model_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn busy_user_turn_rejects_proactive_and_ignores_system_only_cancel() {
    let state = Arc::new(TestState::default());
    let handle = blocking_handle(&state);
    let user = handle.enqueue("actual user").await.unwrap();
    started(&state).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), handle.proactive())
            .await
            .unwrap()
            .is_err()
    );
    handle.cancel_proactive().await.unwrap();
    assert!(!state.model_cancelled.load(Ordering::Acquire));
    handle.interrupt("test complete").await.unwrap();
    assert!(matches!(
        user.wait().await.unwrap(),
        TurnOutcome::Interrupted(_)
    ));
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn actual_user_admission_preempts_proactive_without_remembering_system_intent() {
    let state = Arc::new(TestState::default());
    let handle = blocking_handle(&state);
    let output = handle.clone();
    let proactive = tokio::spawn(async move { output.proactive().await });
    started(&state).await;
    let user = handle.enqueue("actual user").await.unwrap();
    assert!(matches!(
        proactive.await.unwrap().unwrap(),
        TurnOutcome::Interrupted(_)
    ));
    assert!(matches!(
        user.wait().await.unwrap(),
        TurnOutcome::Completed(_)
    ));
    assert_eq!(state.remembered.load(Ordering::Acquire), 1);
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropped_proactive_future_cancels_its_generation_and_allows_user_to_continue() {
    let state = Arc::new(TestState::default());
    let handle = blocking_handle(&state);
    let output = handle.clone();
    let proactive = tokio::spawn(async move { output.proactive().await });
    started(&state).await;
    proactive.abort();
    assert!(proactive.await.unwrap_err().is_cancelled());
    let user = handle.enqueue("actual user").await.unwrap();
    assert!(matches!(
        user.wait().await.unwrap(),
        TurnOutcome::Completed(_)
    ));
    assert!(state.model_cancelled.load(Ordering::Acquire));
    assert_eq!(state.remembered.load(Ordering::Acquire), 1);
    handle.shutdown().await.unwrap();
}
