use super::*;

fn reference() -> VoicePreset {
    VoicePreset {
        id: "personal".into(),
        label: "自己的声音".into(),
        language: VoiceLanguage::Zh,
        model: ModelSelection::cpu(NativeBackend::Qwen3TtsOnnx, "qwen3-tts-base-0.6b"),
        source: VoiceSource::ReferenceAudio {
            audio_id: "private-audio-42".into(),
            text: "用于本地参考的私人文本".into(),
        },
    }
}

#[test]
fn typed_voices_roundtrip_and_sharing_omits_private_material() {
    let mut config = NativeConfig::default();
    config.voices.push(reference());
    config.active_voice = Some("personal".into());
    let decoded = NativeConfig::parse(&config.to_toml().unwrap()).unwrap();
    assert_eq!(decoded, config);
    let voice = decoded.active_voice_preset().unwrap();
    assert_eq!(voice.private_reference_id(), Some("private-audio-42"));
    let shared = toml::to_string(&voice.share_metadata()).unwrap();
    assert!(shared.contains("reference_required"));
    assert!(!shared.contains("private-audio-42"));
    assert!(!shared.contains("用于本地参考的私人文本"));
    assert!(matches!(
        VoicePreset::default().share_metadata().source,
        SharedVoiceSource::Preset { .. }
    ));
}

#[test]
fn native_voices_reject_backend_source_mismatch_and_path_ids() {
    let mut voice = reference();
    voice.model = VoicePreset::default().model;
    assert!(voice.validate().is_err());
    voice = reference();
    voice.source = VoiceSource::Preset {
        speaker: "zf_079".into(),
    };
    assert!(voice.validate().is_err());
    for id in [
        "../reference.wav",
        "C:\\private\\reference.wav",
        "audio:stream",
        "CON",
        "voice.",
    ] {
        let mut voice = reference();
        voice.source = VoiceSource::ReferenceAudio {
            audio_id: id.into(),
            text: "匹配文本".into(),
        };
        assert!(voice.validate().is_err(), "{id}");
    }
    let mut voice = reference();
    voice.source = VoiceSource::ReferenceAudio {
        audio_id: "audio-42".into(),
        text: "  ".into(),
    };
    assert!(voice.validate().is_err());
}

#[test]
fn missing_installations_are_not_configuration_errors() {
    let mut config = NativeConfig::default();
    config.dialogue.as_mut().unwrap().model_id = "not-installed-custom-model".into();
    config.dialogue.as_mut().unwrap().runtime_id = "not-installed-runtime".into();
    config.validate().unwrap();
    config.dialogue.as_mut().unwrap().backend = NativeBackend::SherpaWhisper;
    assert!(config.validate().is_err());
    config = NativeConfig::default();
    config.voices.push(VoicePreset::default());
    assert!(config.validate().is_err());
    config.voices.pop();
    config.active_voice = Some("missing".into());
    assert!(config.validate().is_err());
}

#[test]
fn malformed_native_inputs_do_not_echo_private_values() {
    let sentinel = "private-audio-marker";
    let text = NativeConfig::default()
        .to_toml()
        .unwrap()
        .replace("language = \"zh\"", &format!("language = \"{sentinel}\""));
    assert!(
        !NativeConfig::parse(&text)
            .unwrap_err()
            .to_string()
            .contains(sentinel)
    );
    assert!(NativeConfig::parse(&" ".repeat(crate::MAX_CONFIG_BYTES + 1)).is_err());
}

#[test]
fn character_voice_binding_is_isolated_and_other_characters_use_default() {
    let mut config = NativeConfig::default();
    config.voices.push(reference());
    config
        .character_voices
        .insert("角色 一号".into(), "personal".into());
    config.validate().unwrap();
    assert_eq!(
        config.voice_for_profile("角色 一号").unwrap().id,
        "personal"
    );
    assert_eq!(
        config.voice_for_profile("其他人物").unwrap().id,
        "builtin-zh"
    );
    let roundtrip = NativeConfig::parse(&config.to_toml().unwrap()).unwrap();
    assert_eq!(roundtrip.character_voices, config.character_voices);
    config.voices.retain(|voice| voice.id != "personal");
    assert!(config.validate().is_err());
    config.character_voices.clear();
    config
        .character_voices
        .insert(" ".into(), "builtin-zh".into());
    assert!(config.validate().is_err());
}
