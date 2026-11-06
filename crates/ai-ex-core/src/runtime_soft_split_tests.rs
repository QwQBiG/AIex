use super::*;

struct ChunkModel;

#[async_trait]
impl LanguageModelPort for ChunkModel {
    async fn stream(
        &mut self,
        _: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let (sender, receiver) = mpsc::channel(3);
        for text in ["甲乙", "丙丁，", "戊己。"] {
            sender.send(Ok(text.into())).await.unwrap();
        }
        Ok(receiver)
    }
    async fn cancel(&mut self, _: TurnId) -> Result<(), AppError> {
        Ok(())
    }
}

struct RecordingSpeech(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl SpeechPort for RecordingSpeech {
    async fn enqueue(&mut self, _: TurnId, text: String) -> Result<(), AppError> {
        self.0.lock().await.push(text);
        Ok(())
    }
    async fn interrupt(&mut self) -> Result<(), AppError> {
        Ok(())
    }
}

#[tokio::test]
async fn soft_split_changes_speech_queue_only_when_enabled() {
    let sentences = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new(TestState::default());
    let mut runtime = Runtime::new(
        ChunkModel,
        RecordingSpeech(sentences.clone()),
        TestAvatar(state.clone()),
        TestMemory(state.clone()),
        TestEvents,
    );
    assert!(runtime.set_soft_split(Some(3)).is_err());
    runtime.run_turn("first").await.unwrap();
    assert_eq!(*sentences.lock().await, ["甲乙丙丁，戊己。"]);
    sentences.lock().await.clear();
    runtime.set_soft_split(Some(4)).unwrap();
    runtime.run_turn("second").await.unwrap();
    assert_eq!(*sentences.lock().await, ["甲乙丙丁，", "戊己。"]);
    sentences.lock().await.clear();
    runtime.set_soft_split(None).unwrap();
    runtime.run_turn("third").await.unwrap();
    assert_eq!(*sentences.lock().await, ["甲乙丙丁，戊己。"]);
}

#[tokio::test]
async fn actor_split_state_is_current_versioned_and_rejects_active_changes() {
    let state = Arc::new(TestState::default());
    let mut runtime = Runtime::new(
        TestModel {
            state: state.clone(),
            sender: None,
        },
        TestSpeech(state.clone()),
        TestAvatar(state.clone()),
        TestMemory(state.clone()),
        TestEvents,
    );
    runtime.set_soft_split(Some(10)).unwrap();
    let handle = spawn_runtime(runtime, 4).unwrap();
    assert_eq!(handle.speech_split(None).await.unwrap(), (0, Some(10)));
    assert_eq!(
        handle.speech_split(Some((0, Some(4)))).await.unwrap(),
        (1, Some(4))
    );
    assert!(handle.speech_split(Some((0, None))).await.is_err());
    assert!(handle.speech_split(Some((1, Some(3)))).await.is_err());
    assert_eq!(handle.speech_split(None).await.unwrap(), (1, Some(4)));

    let running = handle.clone();
    let turn = tokio::spawn(async move { running.submit("active").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !state.model_started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(handle.speech_split(None).await.unwrap(), (1, Some(4)));
    assert!(handle.speech_split(Some((1, None))).await.is_err());
    handle.interrupt("stop").await.unwrap();
    assert!(matches!(
        turn.await.unwrap().unwrap(),
        TurnOutcome::Interrupted(_)
    ));
    assert_eq!(handle.speech_split(None).await.unwrap(), (1, Some(4)));
    assert_eq!(
        handle.speech_split(Some((1, None))).await.unwrap(),
        (2, None)
    );
    handle.shutdown().await.unwrap();
}
