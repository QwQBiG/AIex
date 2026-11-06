use super::*;

fn snapshot(instance: &str, revision: u64, active: bool, sequence: u64) -> RuntimeSnapshot {
    RuntimeSnapshot {
        instance_id: Some(instance.parse().unwrap()),
        voice_input: ai_ex_domain::VoiceInputState { revision, active },
        last_sequence: sequence,
        ..Default::default()
    }
}

#[test]
fn voice_input_merges_independently_of_event_sequence_and_resets_with_instance() {
    const FIRST: &str = "00000000-0000-4000-8000-000000000001";
    const SECOND: &str = "00000000-0000-4000-8000-000000000002";
    let mut ui = UiState::new(4).unwrap();
    ui.apply_snapshot(snapshot(FIRST, 2, true, 10));
    assert!(ui.merge_voice_input(&snapshot(FIRST, 3, false, 5)));
    assert_eq!(ui.runtime.last_sequence, 10);
    assert_eq!(ui.runtime.voice_input.revision, 3);
    assert!(!ui.runtime.voice_input.active);
    ui.apply_snapshot(snapshot(FIRST, 2, true, 11));
    assert_eq!(ui.runtime.voice_input.revision, 3);
    assert!(!ui.runtime.voice_input.active);
    assert!(!ui.merge_voice_input(&snapshot(SECOND, 100, true, 12)));
    ui.recover_snapshot(snapshot(SECOND, 0, false, 0));
    assert_eq!(ui.runtime.voice_input.revision, 0);
    assert!(!ui.runtime.voice_input.active);
}
