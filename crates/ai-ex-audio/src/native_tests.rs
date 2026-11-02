use super::*;
use crate::SpeechQueue;
use ai_ex_core::SpeechPort;
use ai_ex_domain::TurnId;

fn silent_wav(duration_ms: u32) -> Vec<u8> {
    let bytes = duration_ms * 16 * 2;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&16000_u32.to_le_bytes());
    wav.extend_from_slice(&32000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&bytes.to_le_bytes());
    wav.resize(44 + bytes as usize, 0);
    wav
}

#[tokio::test]
async fn invalid_or_cancelled_audio_never_reports_playback() {
    let (mut queue, mut receiver) = SpeechQueue::new(1).unwrap();
    queue
        .enqueue(TurnId::new(), "test".to_owned())
        .await
        .unwrap();
    let job = receiver.receive().await.unwrap();
    let player = receiver.player();
    assert!(
        player
            .play_wav_observed(&job, b"not a WAV".to_vec(), |_| panic!(
                "invalid audio must not play"
            ))
            .await
            .is_err()
    );
    SpeechPort::interrupt(&mut queue).await.unwrap();
    player
        .play_wav_observed(&job, silent_wav(120), |_| {
            panic!("cancelled audio must not play")
        })
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires a real output device; plays generated silence only"]
async fn native_silent_playback_reports_progress_completion_and_interruption() {
    for interrupt in [false, true] {
        let (mut queue, mut receiver) = SpeechQueue::new(1).unwrap();
        let turn_id = TurnId::new();
        queue
            .enqueue(turn_id, "silent device probe".to_owned())
            .await
            .unwrap();
        let job = receiver.receive().await.unwrap();
        let player = receiver.player();
        let (events, mut observed) = tokio::sync::mpsc::unbounded_channel();
        let duration = if interrupt { 2000 } else { 200 };
        let task = tokio::spawn(async move {
            player
                .play_wav_observed(&job, silent_wav(duration), move |snapshot| {
                    let _ = events.send(snapshot);
                })
                .await
        });
        let first = tokio::time::timeout(Duration::from_secs(5), observed.recv())
            .await
            .unwrap()
            .expect("playback started");
        assert!(first.active);
        assert_eq!(first.turn_id, Some(turn_id));
        assert_eq!(first.duration_ms, duration as u64);
        let stopped = SpeechPlaybackSnapshot {
            turn_id: first.turn_id,
            sentence_id: first.sentence_id,
            ..Default::default()
        };
        if interrupt {
            SpeechPort::interrupt(&mut queue).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("playback must stop promptly")
            .unwrap()
            .unwrap();
        let mut progressed = false;
        let mut last = first;
        while let Some(snapshot) = observed.recv().await {
            assert_eq!(snapshot.mouth_level, 0, "silence keeps mouth closed");
            progressed |= snapshot.position_ms > 0;
            last = snapshot;
        }
        assert_eq!(last, stopped);
        assert!(interrupt || progressed);
    }
}
