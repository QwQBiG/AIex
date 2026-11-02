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
