use std::sync::atomic::{AtomicU64, Ordering};

use ai_ex_domain::SpeechPlaybackSnapshot;

use super::*;

fn playback(turn: TurnId, sentence: uuid::Uuid, active: bool) -> SpeechPlaybackSnapshot {
    SpeechPlaybackSnapshot {
        turn_id: Some(turn),
        sentence_id: Some(sentence),
        active,
        ..Default::default()
    }
}

#[test]
fn preview_intent_restores_expression_without_creating_a_conversation() {
    let hub = EventHub::new(32).unwrap();
    let cancelled = TurnId::new();
    hub.publish_now(SystemEvent::TurnStarted {
        turn_id: cancelled,
        user_text: "旧对话".into(),
    });
    hub.publish_now(SystemEvent::TurnInterrupted {
        turn_id: cancelled,
        reason: "试听".into(),
    });
    assert!(hub.current().speech_cancelled);
    let preview = TurnId::new();
    let preview_sentence = uuid::Uuid::new_v4();
    let generation = AtomicU64::new(7);
    assert!(hub.publish_if(
        SystemEvent::SpeechPreviewStarted { turn_id: preview },
        || generation.load(Ordering::Acquire) == 7
    ));
    hub.publish_now(SystemEvent::SpeechPlayback {
        playback: playback(preview, preview_sentence, true),
    });
    let state = hub.current();
    assert!(state.playback.active);
    assert_eq!(state.turns_started, 1);
    assert_eq!(state.active_turn, None);
    assert_eq!(state.turns_interrupted, 1);

    generation.store(8, Ordering::Release);
    let next = TurnId::new();
    let next_sentence = uuid::Uuid::new_v4();
    hub.publish_now(SystemEvent::TurnStarted {
        turn_id: next,
        user_text: "新对话".into(),
    });
    hub.publish_now(SystemEvent::SpeechPlayback {
        playback: playback(next, next_sentence, true),
    });
    let sequence = hub.current().last_sequence;
    assert!(!hub.publish_if(
        SystemEvent::SpeechPreviewStarted { turn_id: preview },
        || generation.load(Ordering::Acquire) == 7
    ));
    assert_eq!(hub.current().last_sequence, sequence);
    // Identity matching also protects projections from a delayed unguarded stop.
    hub.publish_now(SystemEvent::SpeechPlayback {
        playback: playback(preview, preview_sentence, false),
    });
    assert_eq!(hub.current().playback.sentence_id, Some(next_sentence));
    assert!(hub.current().playback.active);
    hub.publish_now(SystemEvent::SpeechPlayback {
        playback: playback(next, next_sentence, false),
    });
    assert!(!hub.current().playback.active);
    assert_eq!(hub.current().active_turn, Some(next));
}

#[test]
fn rejected_publication_allocates_no_sequence_or_history_entry() {
    let hub = EventHub::new(4).unwrap();
    let mut receiver = hub.subscribe();
    assert!(!hub.publish_if(
        SystemEvent::SpeechPreviewStarted {
            turn_id: TurnId::new()
        },
        || false
    ));
    assert_eq!(hub.current().last_sequence, 0);
    assert!(hub.events_since(0, 4).is_empty());
    assert!(receiver.try_recv().is_err());
    hub.publish_now(SystemEvent::SpeechCancelled);
    assert_eq!(receiver.try_recv().unwrap().sequence, 1);
}
