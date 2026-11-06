use super::*;
use ai_ex_domain::TurnId;
use uuid::Uuid;

fn sample(instance: Uuid, sequence: u64, turn: TurnId) -> RuntimeTiming {
    RuntimeTiming {
        instance_id: instance,
        sequence,
        turn: Some(TurnTiming {
            turn_id: turn,
            first_chunk_ms: Some(15),
            first_playback_request_ms: Some(40),
        }),
    }
}

#[test]
fn timing_only_shows_for_current_instance_turn_and_seen_event_prefix() {
    let instance = Uuid::new_v4();
    let turn = TurnId::new();
    let mut runtime = RuntimeSnapshot {
        instance_id: Some(instance),
        speech_turn: Some(turn),
        last_sequence: 5,
        ..Default::default()
    };
    let mut view = TimingView::default();
    view.accept(sample(Uuid::new_v4(), 5, turn), Some(instance));
    assert!(view.visible(&runtime, true, false).is_none());
    view.accept(sample(instance, 7, turn), Some(instance));
    assert!(view.visible(&runtime, true, false).is_none());
    runtime.last_sequence = 7;
    assert_eq!(
        view.visible(&runtime, true, false).unwrap().first_chunk_ms,
        Some(15)
    );
    assert!(view.visible(&runtime, false, false).is_none());
    assert!(view.visible(&runtime, true, true).is_none());
    runtime.speech_turn = Some(TurnId::new());
    assert!(view.visible(&runtime, true, false).is_none());
    runtime.speech_turn = Some(turn);
    runtime.speech_cancelled = true;
    assert!(view.visible(&runtime, true, false).is_none());
}

#[test]
fn cancelled_preview_or_new_turn_cannot_revive_older_measurements() {
    let instance = Uuid::new_v4();
    let turn = TurnId::new();
    let next = TurnId::new();
    let mut runtime = RuntimeSnapshot {
        instance_id: Some(instance),
        speech_turn: Some(turn),
        last_sequence: 3,
        ..Default::default()
    };
    let mut view = TimingView::default();
    view.accept(sample(instance, 3, turn), Some(instance));
    view.observe(
        4,
        &SystemEvent::TurnFinished {
            turn_id: turn,
            full_text: String::new(),
        },
    );
    runtime.last_sequence = 4;
    assert!(view.visible(&runtime, true, false).is_some());
    view.observe(5, &SystemEvent::SpeechCancelled);
    runtime.last_sequence = 5;
    runtime.speech_cancelled = true;
    view.accept(sample(instance, 4, turn), Some(instance));
    assert!(view.visible(&runtime, true, false).is_none());
    view.observe(
        6,
        &SystemEvent::TurnStarted {
            turn_id: next,
            user_text: String::new(),
        },
    );
    runtime.last_sequence = 6;
    runtime.speech_turn = Some(next);
    runtime.speech_cancelled = false;
    view.accept(sample(instance, 5, next), Some(instance));
    assert!(view.visible(&runtime, true, false).is_none());
    view.accept(sample(instance, 6, next), Some(instance));
    assert!(view.visible(&runtime, true, false).is_some());
    view.observe(
        7,
        &SystemEvent::SpeechPreviewStarted {
            turn_id: TurnId::new(),
        },
    );
    assert!(view.visible(&runtime, true, false).is_none());
}

#[test]
fn status_failure_does_not_erase_the_event_fence() {
    let instance = Uuid::new_v4();
    let turn = TurnId::new();
    let runtime = RuntimeSnapshot {
        instance_id: Some(instance),
        speech_turn: Some(turn),
        last_sequence: 12,
        ..Default::default()
    };
    let mut view = TimingView::default();
    view.observe(10, &SystemEvent::SpeechCancelled);
    view.unavailable();
    view.accept(sample(instance, 9, turn), Some(instance));
    assert!(view.visible(&runtime, true, false).is_none());
    view.accept(sample(instance, 12, turn), Some(instance));
    assert!(view.visible(&runtime, true, false).is_some());
    view.clear();
    assert!(view.visible(&runtime, true, false).is_none());
}
