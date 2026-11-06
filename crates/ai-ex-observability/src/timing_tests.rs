use std::time::{Duration, Instant};

use ai_ex_domain::{SpeechPlaybackSnapshot, SystemEvent, TurnId};

use super::TimingTracker;
use crate::{EventHub, RuntimeSnapshot};

fn observe(
    tracker: &mut TimingTracker,
    snapshot: &mut RuntimeSnapshot,
    event: SystemEvent,
    now: Instant,
) {
    snapshot.observe_speech_event(&event);
    tracker.observe(&event, snapshot, now);
}

fn start(turn_id: TurnId) -> SystemEvent {
    SystemEvent::TurnStarted {
        turn_id,
        user_text: "synthetic private input".into(),
    }
}

fn chunk(turn_id: TurnId, text: &str) -> SystemEvent {
    SystemEvent::ModelChunk {
        turn_id,
        text: text.into(),
    }
}

fn playback(turn_id: TurnId) -> SystemEvent {
    SystemEvent::SpeechPlayback {
        playback: SpeechPlaybackSnapshot {
            turn_id: Some(turn_id),
            active: true,
            sentence_id: Some(uuid::Uuid::new_v4()),
            text: "synthetic private speech".into(),
            ..Default::default()
        },
    }
}

#[test]
fn first_fragment_and_delayed_playback_are_measured_once_without_content() {
    let turn = TurnId::new();
    let now = Instant::now();
    let mut tracker = TimingTracker::default();
    let mut snapshot = RuntimeSnapshot::default();
    observe(&mut tracker, &mut snapshot, start(turn), now);
    observe(&mut tracker, &mut snapshot, chunk(turn, ""), now);
    observe(
        &mut tracker,
        &mut snapshot,
        chunk(turn, "synthetic private response"),
        now + Duration::from_millis(123),
    );
    observe(
        &mut tracker,
        &mut snapshot,
        SystemEvent::TurnFinished {
            turn_id: turn,
            full_text: "synthetic private response".into(),
        },
        now + Duration::from_millis(150),
    );
    observe(
        &mut tracker,
        &mut snapshot,
        playback(turn),
        now + Duration::from_millis(275),
    );
    observe(
        &mut tracker,
        &mut snapshot,
        playback(turn),
        now + Duration::from_millis(400),
    );
    let timing = tracker.current().unwrap();
    assert_eq!(timing.first_chunk_ms, Some(123));
    assert_eq!(timing.first_playback_request_ms, Some(275));
    assert!(!serde_json::to_string(&timing).unwrap().contains("private"));
}

#[test]
fn cancellation_preview_and_new_turn_discard_old_events() {
    let first = TurnId::new();
    let second = TurnId::new();
    let now = Instant::now();
    let mut tracker = TimingTracker::default();
    let mut snapshot = RuntimeSnapshot::default();
    observe(&mut tracker, &mut snapshot, start(first), now);
    observe(
        &mut tracker,
        &mut snapshot,
        SystemEvent::SpeechCancelled,
        now,
    );
    observe(&mut tracker, &mut snapshot, playback(first), now);
    assert!(tracker.current().is_none());
    observe(&mut tracker, &mut snapshot, start(second), now);
    observe(&mut tracker, &mut snapshot, playback(first), now);
    observe(&mut tracker, &mut snapshot, chunk(first, "late"), now);
    let timing = tracker.current().unwrap();
    assert_eq!(timing.turn_id, second);
    assert_eq!(timing.first_chunk_ms, None);
    assert_eq!(timing.first_playback_request_ms, None);
    observe(
        &mut tracker,
        &mut snapshot,
        SystemEvent::SpeechPreviewStarted {
            turn_id: TurnId::new(),
        },
        now,
    );
    observe(&mut tracker, &mut snapshot, playback(second), now);
    assert!(tracker.current().is_none());
}

#[test]
fn query_returns_the_event_prefix_and_instance_without_changing_events() {
    let hub = EventHub::new(8).unwrap();
    let turn = TurnId::new();
    let initial = hub.runtime_timing();
    assert_eq!(initial.sequence, 0);
    assert!(initial.turn.is_none());
    hub.publish_now(start(turn));
    hub.publish_now(chunk(turn, "test"));
    let timing = hub.runtime_timing();
    assert_eq!(timing.instance_id, initial.instance_id);
    assert_eq!(timing.sequence, hub.current().last_sequence);
    assert_eq!(timing.turn.unwrap().turn_id, turn);
    assert_eq!(hub.events_since(0, 8).len(), 2);
    assert!(!hub.publish_if(playback(turn), || false));
    assert_eq!(hub.runtime_timing().sequence, timing.sequence);
}
