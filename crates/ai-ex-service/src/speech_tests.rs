use super::*;
use ai_ex_core::SpeechPort;
use tokio::io::AsyncReadExt;

struct SentenceModel(usize);

#[async_trait]
impl LanguageModelPort for SentenceModel {
    async fn stream(
        &mut self,
        _request: ModelRequest,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<String, AppError>>, AppError> {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let text = if self.0 == 0 {
            "[happy] Hello. Hello."
        } else {
            "A new answer."
        };
        self.0 += 1;
        sender.send(Ok(text.to_owned())).await.unwrap();
        Ok(receiver)
    }

    async fn cancel(&mut self, _turn_id: TurnId) -> Result<(), AppError> {
        Ok(())
    }
}

#[tokio::test]
async fn runtime_stage_queue_and_playback_keep_sentence_identity_after_generation() {
    use ai_ex_domain::{Emotion, SpeechPlaybackSnapshot};
    let (speech, mut receiver) = SpeechQueue::new(8).unwrap();
    let mut router = StageRouter::new();
    router.push(speech);
    router.push(VtsClient::disabled());
    let stage = StageOutput::new(router);
    let events = EventHub::new(256).unwrap();
    let mut runtime = Runtime::new(
        SentenceModel(0),
        stage.speech(),
        stage.avatar(),
        MemoryStore::disabled(),
        events.clone(),
    );
    let turn = runtime.run_turn("hello").await.unwrap();
    let first = receiver.receive().await.unwrap();
    let second = receiver.receive().await.unwrap();
    assert_eq!(first.turn_id, turn);
    assert_eq!(first.text.trim(), "Hello.");
    assert_eq!(first.text.trim(), second.text.trim());
    assert_ne!(first.sentence_id, second.sentence_id);
    assert_eq!(first.emotion, Emotion::Happy);
    assert_eq!(
        events.current().current_emotion,
        None,
        "generation already finished"
    );
    let publisher = playback::PlaybackPublisher::new(events.clone());
    publisher.observe(first.playback_snapshot(0, 1000, 700));
    publisher.observe(first.playback_snapshot(400, 1000, 200));
    let payload = ControlPayload::Snapshot(events.current());
    let decoded: ControlPayload =
        serde_json::from_slice(&serde_json::to_vec(&payload).unwrap()).unwrap();
    let ControlPayload::Snapshot(snapshot) = decoded else {
        panic!("snapshot payload expected")
    };
    assert_eq!(snapshot.playback.text, first.text);
    assert_eq!(snapshot.playback.emotion, Some(Emotion::Happy));
    assert_eq!(snapshot.playback.position_ms, 400);
    assert_eq!(snapshot.playback.mouth_level, 200);
    runtime.interrupt("stop the remaining audio").await.unwrap();
    publisher.observe(first.playback_snapshot(440, 1000, 800));
    assert_eq!(events.current().playback, SpeechPlaybackSnapshot::default());
    runtime.run_turn("continue").await.unwrap();
    let fresh = receiver.receive().await.unwrap();
    assert_eq!(fresh.emotion, Emotion::Neutral);
    events.publish_now(SystemEvent::SpeechPlayback {
        playback: second.playback_snapshot(0, 1000, 700),
    });
    assert!(
        !events.current().playback.active,
        "late starts from the old turn cannot revive it"
    );
    let publisher = playback::PlaybackPublisher::new(events.clone());
    publisher.observe(fresh.playback_snapshot(0, 1000, 300));
    assert_eq!(events.current().playback.text, "A new answer.");
    publisher.observe(SpeechPlaybackSnapshot::default());
    assert_eq!(events.current().playback, SpeechPlaybackSnapshot::default());
}

#[tokio::test]
async fn long_sentence_progress_fits_default_control_message_budget() {
    let (mut speech, mut receiver) = SpeechQueue::new(1).unwrap();
    speech
        .enqueue(TurnId::new(), "字".repeat(4096))
        .await
        .unwrap();
    let job = receiver.receive().await.unwrap();
    let events = EventHub::new(256).unwrap();
    let publisher = playback::PlaybackPublisher::new(events.clone());
    for position in 0..256 {
        publisher.observe(job.playback_snapshot(position * 40, 12000, 500));
    }
    let payload = ControlPayload::Events(events.events_since(0, 256));
    assert!(serde_json::to_vec(&payload).unwrap().len() < 65_000);
    assert_eq!(events.current().playback.text.chars().count(), 4096);
    assert_eq!(events.current().playback.position_ms, 10200);
}

#[tokio::test]
async fn interrupted_synthesis_does_not_delay_speech_worker_shutdown() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tts = GptSovitsClient::new(GptSovitsSettings {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        timeout: Duration::from_secs(30),
        text_lang: "zh".to_owned(),
        ref_audio_path: "test-reference.wav".to_owned(),
        prompt_text: String::new(),
        prompt_lang: "zh".to_owned(),
    })
    .unwrap();
    let (started, received) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(stream.read(&mut [0; 1024]).await.unwrap() > 0);
        started.send(()).unwrap();
        held.await.unwrap();
        drop(stream);
    });
    let (mut speech, receiver) = SpeechQueue::new(2).unwrap();
    let player = receiver.player();
    speech
        .enqueue(TurnId::new(), "hello".to_owned())
        .await
        .unwrap();
    let events = EventHub::new(8).unwrap();
    let worker = tokio::spawn(run_speech_worker(
        receiver,
        Some(tts),
        player,
        events.clone(),
        None,
    ));
    tokio::time::timeout(Duration::from_secs(2), received)
        .await
        .unwrap()
        .unwrap();
    SpeechPort::interrupt(&mut speech).await.unwrap();
    drop(speech);
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .expect("cancelled TTS must not wait for HTTP timeout")
        .unwrap();
    assert_eq!(events.current().playback, Default::default());
    release.send(()).unwrap();
    server.await.unwrap();
}
