use crate::{AudioFrame, EnergyVad, VadConfig, VadEvent};

const FRAME_SAMPLES: usize = 160;

fn config() -> VadConfig {
    VadConfig {
        start_threshold: 0.2,
        continue_threshold: 0.1,
        start_frames: 2,
        end_silence_frames: 2,
        max_utterance_frames: 100,
    }
}

fn frame(level: f32) -> AudioFrame {
    AudioFrame::new(vec![level; FRAME_SAMPLES], 16_000, 1).expect("valid audio frame")
}

#[test]
fn noise_does_not_start_speech() {
    let mut vad = EnergyVad::new(config()).expect("valid VAD configuration");

    assert!(vad.process(frame(0.05)).expect("process noise").is_none());
    assert!(vad.process(frame(0.19)).expect("process noise").is_none());
    assert!(!vad.speech_in_progress());
}

#[test]
fn hysteresis_emits_a_complete_utterance() {
    let mut vad = EnergyVad::new(config()).expect("valid VAD configuration");
    assert!(!vad.speech_in_progress());

    assert!(vad.process(frame(0.3)).expect("first hot frame").is_none());
    assert!(!vad.speech_in_progress());
    assert!(matches!(
        vad.process(frame(0.3)).expect("second hot frame"),
        Some(VadEvent::SpeechStarted)
    ));
    assert!(vad.speech_in_progress());
    assert!(matches!(
        vad.process(frame(0.01)).expect("first silent frame"),
        Some(VadEvent::SpeechContinued)
    ));
    assert!(vad.speech_in_progress());
    let Some(VadEvent::SpeechEnded(utterance)) =
        vad.process(frame(0.01)).expect("second silent frame")
    else {
        panic!("expected completed utterance");
    };

    assert!(!vad.speech_in_progress());
    assert_eq!(utterance.samples.len(), FRAME_SAMPLES * 4);
    assert_eq!(utterance.sample_rate, 16_000);
    assert_eq!(utterance.channels, 1);
}

#[test]
fn format_change_inside_candidate_is_rejected() {
    let mut vad = EnergyVad::new(config()).expect("valid VAD configuration");
    vad.process(frame(0.3)).expect("first hot frame");
    let changed =
        AudioFrame::new(vec![0.3; FRAME_SAMPLES], 48_000, 1).expect("valid changed frame");

    assert!(vad.process(changed).is_err());
}

#[test]
fn maximum_utterance_resets_speech_in_progress() {
    let mut config = config();
    config.max_utterance_frames = 3;
    let mut vad = EnergyVad::new(config).expect("valid VAD configuration");
    assert!(vad.process(frame(0.3)).unwrap().is_none());
    assert!(matches!(
        vad.process(frame(0.3)).unwrap(),
        Some(VadEvent::SpeechStarted)
    ));
    assert!(vad.speech_in_progress());
    let Some(VadEvent::SpeechEnded(utterance)) = vad.process(frame(0.3)).unwrap() else {
        panic!("maximum utterance must end speech");
    };
    assert_eq!(utterance.samples.len(), FRAME_SAMPLES * 3);
    assert!(!vad.speech_in_progress());
    assert!(vad.process(frame(0.3)).unwrap().is_none());
    assert!(!vad.speech_in_progress());
}

#[test]
fn flush_resets_candidates_and_started_speech() {
    let mut vad = EnergyVad::new(config()).expect("valid VAD configuration");
    assert!(vad.process(frame(0.3)).unwrap().is_none());
    assert!(!vad.speech_in_progress());
    assert!(vad.flush().is_none());
    assert!(!vad.speech_in_progress());

    assert!(vad.process(frame(0.3)).unwrap().is_none());
    assert!(matches!(
        vad.process(frame(0.3)).unwrap(),
        Some(VadEvent::SpeechStarted)
    ));
    assert!(vad.speech_in_progress());
    let Some(VadEvent::SpeechEnded(utterance)) = vad.flush() else {
        panic!("flushing started speech must return its utterance");
    };
    assert_eq!(utterance.samples.len(), FRAME_SAMPLES * 2);
    assert!(!vad.speech_in_progress());
    assert!(vad.flush().is_none());
    assert!(!vad.speech_in_progress());
}
