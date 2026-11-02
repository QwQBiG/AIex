use ai_ex_domain::{SpeechPlaybackSnapshot, SystemEvent, TurnId};
use ai_ex_observability::EventHub;

use super::{ApplyOutcome, ConnectionState, PresentationState, UiState};

fn apply(hub: &EventHub, ui: &mut UiState, event: SystemEvent) {
    hub.publish_now(event);
    for item in hub.events_since(ui.runtime.last_sequence, 32) {
        assert_eq!(ui.apply_event(item), ApplyOutcome::Applied);
    }
    assert_eq!(ui.runtime, hub.current());
    assert!(!ui.needs_resync && ui.turns.is_empty());
    assert!(ui.runtime.active_turn.is_none());
    assert_eq!(ui.runtime.turns_started, 0);
}

#[test]
fn voice_preview_replays_and_recovers_without_creating_a_chat_turn_or_history_gap() {
    let hub = EventHub::new(32).unwrap();
    let mut ui = UiState::new(8).unwrap();
    ui.connection = ConnectionState::Connected;
    ui.apply_snapshot(hub.current());
    apply(&hub, &mut ui, SystemEvent::SpeechCancelled);
    let turn_id = TurnId::new();
    let sentence_id = TurnId::new().0;
    apply(&hub, &mut ui, SystemEvent::SpeechPreviewStarted { turn_id });
    apply(
        &hub,
        &mut ui,
        SystemEvent::SpeechPlayback {
            playback: SpeechPlaybackSnapshot {
                turn_id: Some(turn_id),
                sentence_id: Some(sentence_id),
                active: true,
                text: "合成试听".into(),
                duration_ms: 1000,
                mouth_level: 600,
                ..Default::default()
            },
        },
    );
    assert_eq!(PresentationState::subtitle(&ui), Some("合成试听"));
    assert!(!ui.runtime.speech_cancelled && ui.runtime.playback.active);
    let mut reopened = UiState::new(8).unwrap();
    reopened.apply_snapshot(hub.current());
    assert!(reopened.turns.is_empty() && !reopened.needs_resync);
    assert_eq!(reopened.runtime, ui.runtime);
    apply(
        &hub,
        &mut ui,
        SystemEvent::SpeechPlayback {
            playback: SpeechPlaybackSnapshot {
                turn_id: Some(turn_id),
                sentence_id: Some(sentence_id),
                ..Default::default()
            },
        },
    );
    assert!(!ui.runtime.playback.active);
    assert_eq!(PresentationState::subtitle(&ui), None);
}
