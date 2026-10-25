use ai_ex_domain::AppError;
use async_trait::async_trait;

use crate::{
    AudioFrame, DuplexController, DuplexDirective, EnergyVad, TranscriberPort, Utterance, VadConfig,
};

struct FixedTranscriber {
    transcript: String,
    calls: usize,
    fail: bool,
}

#[async_trait]
impl TranscriberPort for FixedTranscriber {
    async fn transcribe(&mut self, _utterance: Utterance) -> Result<String, AppError> {
        self.calls += 1;
        if self.fail {
            return Err(AppError::unavailable("synthetic transcription failure"));
        }
        Ok(self.transcript.clone())
    }
}

fn frame(level: f32) -> AudioFrame {
    AudioFrame::new(vec![level; 160], 16_000, 1).expect("valid frame")
}

fn controller(transcript: &str) -> DuplexController<FixedTranscriber> {
    controller_with_failure(transcript, false)
}

fn controller_with_failure(transcript: &str, fail: bool) -> DuplexController<FixedTranscriber> {
    let vad = EnergyVad::new(VadConfig {
        start_threshold: 0.2,
        continue_threshold: 0.1,
        start_frames: 2,
        end_silence_frames: 2,
        max_utterance_frames: 100,
    })
    .expect("valid VAD");
    DuplexController::new(
        vad,
        FixedTranscriber {
            transcript: transcript.to_owned(),
            calls: 0,
            fail,
        },
    )
}

#[tokio::test]
async fn speech_start_requests_barge_in_then_submits_transcript() {
    let mut controller = controller("  hello there  ");
    assert!(!controller.speech_in_progress());

    assert_eq!(controller.process_frame(frame(0.3)).await.unwrap(), None);
    assert!(!controller.speech_in_progress());
    assert_eq!(
        controller.process_frame(frame(0.3)).await.unwrap(),
        Some(DuplexDirective::InterruptCurrentTurn)
    );
    assert!(controller.speech_in_progress());
    assert_eq!(controller.process_frame(frame(0.0)).await.unwrap(), None);
    assert!(controller.speech_in_progress());
    assert_eq!(
        controller.process_frame(frame(0.0)).await.unwrap(),
        Some(DuplexDirective::SubmitTranscript("hello there".to_owned()))
    );
    assert!(!controller.speech_in_progress());
}

#[tokio::test]
async fn empty_transcript_is_not_submitted() {
    let mut controller = controller("   ");
    controller.process_frame(frame(0.3)).await.unwrap();
    controller.process_frame(frame(0.3)).await.unwrap();
    assert!(controller.speech_in_progress());
    controller.process_frame(frame(0.0)).await.unwrap();
    assert!(controller.speech_in_progress());

    assert_eq!(controller.process_frame(frame(0.0)).await.unwrap(), None);
    assert!(!controller.speech_in_progress());
    assert_eq!(controller.flush().await.unwrap(), None);
    assert!(!controller.speech_in_progress());
}

#[tokio::test]
async fn transcription_errors_leave_vad_idle_after_silence_or_flush() {
    for via_flush in [false, true] {
        let mut controller = controller_with_failure("", true);
        controller.process_frame(frame(0.3)).await.unwrap();
        assert_eq!(
            controller.process_frame(frame(0.3)).await.unwrap(),
            Some(DuplexDirective::InterruptCurrentTurn)
        );
        assert!(controller.speech_in_progress());
        let result = if via_flush {
            controller.flush().await
        } else {
            assert_eq!(controller.process_frame(frame(0.0)).await.unwrap(), None);
            assert!(controller.speech_in_progress());
            controller.process_frame(frame(0.0)).await
        };
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("synthetic transcription failure")
        );
        assert!(!controller.speech_in_progress());
        assert_eq!(controller.flush().await.unwrap(), None);
        assert!(!controller.speech_in_progress());

        assert_eq!(controller.process_frame(frame(0.3)).await.unwrap(), None);
        assert!(!controller.speech_in_progress());
        assert_eq!(
            controller.process_frame(frame(0.3)).await.unwrap(),
            Some(DuplexDirective::InterruptCurrentTurn)
        );
        assert!(controller.speech_in_progress());
    }
}
