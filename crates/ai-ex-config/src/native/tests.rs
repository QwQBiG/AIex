use super::*;

mod files;
mod voices;

#[test]
fn distributed_native_settings_match_defaults_without_installed_models() {
    let config =
        NativeConfig::parse(include_str!("../../../../config/native-v1.example.toml")).unwrap();
    assert_eq!(config, NativeConfig::default());
    assert!(!config.devices.microphone_enabled);
    assert!(!config.devices.playback_enabled);
    assert_eq!(
        NativeConfig::parse(&config.to_toml().unwrap()).unwrap(),
        config
    );
}

#[test]
fn microphone_selection_is_optional_versioned_and_round_trips() {
    let source = include_str!("../../../../config/native-v1.example.toml");
    let original = NativeConfig::parse(source).unwrap();
    assert_eq!(original.microphone_input, None);
    assert!(!original.to_toml().unwrap().contains("microphone_input"));
    for selection in [
        NativeMicrophoneInput::SystemDefault,
        NativeMicrophoneInput::Named {
            name: "合成麦克风 (USB)".into(),
        },
    ] {
        let mut changed = original.clone();
        changed.microphone_input = Some(selection);
        assert_eq!(
            NativeConfig::parse(&changed.to_toml().unwrap()).unwrap(),
            changed
        );
    }
}

#[test]
fn microphone_device_names_have_bounded_readable_values() {
    let mut config = NativeConfig::default();
    for name in [
        String::new(),
        "  ".into(),
        "line\nbreak".into(),
        "a".repeat(257),
    ] {
        config.microphone_input = Some(NativeMicrophoneInput::Named { name });
        assert!(config.validate().is_err());
    }
    config.microphone_input = Some(NativeMicrophoneInput::Named {
        name: "x".repeat(256),
    });
    assert!(config.validate().is_ok());
}
