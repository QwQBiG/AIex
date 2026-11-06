use super::*;
use ai_ex_domain::{ConversationState, SystemEvent};

#[test]
fn parallel_leases_release_only_their_own_activity() {
    let events = EventHub::new(8).unwrap();
    let activity = VoiceInputActivity::new(events.clone());
    events.publish_now(SystemEvent::StateChanged {
        from: ConversationState::Idle,
        to: ConversationState::Thinking,
    });
    let first = activity.begin().unwrap();
    let second = activity.begin().unwrap();
    assert_eq!(
        events.current().voice_input,
        VoiceInputState {
            revision: 2,
            active: true
        }
    );
    drop(second);
    assert_eq!(
        events.current().voice_input,
        VoiceInputState {
            revision: 3,
            active: true
        }
    );
    events.observe_voice_input(VoiceInputState {
        revision: 1,
        active: false,
    });
    assert_eq!(
        events.current().voice_input,
        VoiceInputState {
            revision: 3,
            active: true
        }
    );
    drop(first);
    let snapshot = events.current();
    assert_eq!(
        snapshot.voice_input,
        VoiceInputState {
            revision: 4,
            active: false
        }
    );
    assert_eq!(snapshot.state, ConversationState::Thinking);
    assert_eq!(snapshot.last_sequence, 1);
}

#[test]
fn releases_remain_representable_when_revision_is_nearly_exhausted() {
    let events = EventHub::new(8).unwrap();
    let activity = VoiceInputActivity::new(events.clone());
    activity.inner.state.lock().unwrap().revision = u64::MAX - 2;
    let lease = activity.begin().unwrap();
    assert!(activity.begin().is_err());
    drop(lease);
    assert_eq!(
        events.current().voice_input,
        VoiceInputState {
            revision: u64::MAX,
            active: false
        }
    );
}
