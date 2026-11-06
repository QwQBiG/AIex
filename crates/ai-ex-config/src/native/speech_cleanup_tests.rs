use super::*;

#[test]
fn speech_cleanup_override_is_optional_and_round_trips() {
    let original = NativeConfig::default();
    let saved = original.to_toml().unwrap();
    assert!(!saved.contains("speech_cleanup"));
    assert_eq!(NativeConfig::parse(&saved).unwrap().speech_cleanup, None);

    let mut updated = original;
    updated.speech_cleanup = Some(NativeSpeechCleanup {
        remove_emoji: false,
        remove_markdown: true,
        remove_parenthetical: false,
    });
    let saved = updated.to_toml().unwrap();
    assert!(saved.contains("[speech_cleanup]"));
    assert_eq!(NativeConfig::parse(&saved).unwrap(), updated);
    assert!(
        NativeConfig::parse(
            &saved.replace("[speech_cleanup]", "[speech_cleanup]\nunsupported = true")
        )
        .is_err()
    );
}
