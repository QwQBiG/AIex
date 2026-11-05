use super::*;
use crate::worker::WorkerCommand;
use ai_ex_config::native::{ModelSelection, NativeBackend};
use ai_ex_domain::{NativeReply, NativeRequest};

fn fixture() -> (NativePanel, VoicePreset) {
    let voice = VoicePreset {
        id: "personal".into(),
        label: "我的声音".into(),
        language: VoiceLanguage::Zh,
        model: ModelSelection::cpu(NativeBackend::Qwen3TtsOnnx, "qwen3-tts-base-0.6b"),
        source: VoiceSource::ReferenceAudio {
            audio_id: "synthetic-reference".into(),
            text: "原录音文字".into(),
        },
    };
    let mut config = NativeConfig::default();
    config.voices.push(voice.clone());
    config
        .character_voices
        .insert("person-a".into(), voice.id.clone());
    let panel = NativePanel {
        config: Some(config),
        selected_voice: voice.id.clone(),
        ..Default::default()
    };
    (panel, voice)
}

fn settings_reply(panel: &mut NativePanel, config: &NativeConfig) {
    let WorkerCommand::Native { request_id, .. } = panel.begin(NativeRequest::Settings).unwrap()
    else {
        panic!("expected native request")
    };
    panel.receive(
        request_id,
        Ok(NativeReply::Settings {
            revision: config.revision,
            document: config.to_toml().unwrap(),
        }),
    );
}

#[test]
fn voice_edit_reuses_reference_and_preserves_bindings_through_save() {
    let (mut panel, voice) = fixture();
    let mut edit = VoiceEdit::new(&voice).unwrap();
    edit.label = "更正后的名称".into();
    edit.text = "更正后的录音文字".into();
    edit.language = VoiceLanguage::En;
    panel.voice_edit = Some(edit);
    panel.selected_voice = "builtin-zh".into();
    assert!(panel.has_unsaved());
    let mut config = panel.config.clone().unwrap();
    config.revision = 2;
    config.devices.playback_enabled = true;
    settings_reply(&mut panel, &config);
    let edit = panel.voice_edit.as_ref().unwrap();
    let request = edit.save_request(panel.config.as_ref().unwrap()).unwrap();
    let NativeRequest::SaveSettings {
        expected_revision,
        document,
    } = &request
    else {
        panic!("expected settings save")
    };
    assert_eq!(*expected_revision, 2);
    let mut saved = NativeConfig::parse(document).unwrap();
    assert!(saved.devices.playback_enabled);
    assert_eq!(saved.character_voices, config.character_voices);
    let edited = saved
        .voices
        .iter()
        .find(|voice| voice.id == "personal")
        .unwrap();
    assert_eq!(edited.label, "更正后的名称");
    assert_eq!(edited.language, VoiceLanguage::En);
    assert_eq!(
        edited.source,
        VoiceSource::ReferenceAudio {
            audio_id: "synthetic-reference".into(),
            text: "更正后的录音文字".into()
        }
    );
    let WorkerCommand::Native { request_id, .. } = panel.begin(request).unwrap() else {
        panic!("expected native request")
    };
    saved.revision = 3;
    panel.receive(
        request_id,
        Ok(NativeReply::Settings {
            revision: saved.revision,
            document: saved.to_toml().unwrap(),
        }),
    );
    assert!(panel.voice_edit.is_none());
    assert!(!panel.has_unsaved());
}

#[test]
fn changed_or_stale_voice_keeps_the_users_draft() {
    let (mut panel, voice) = fixture();
    let mut edit = VoiceEdit::new(&voice).unwrap();
    edit.text = "用户草稿".into();
    panel.voice_edit = Some(edit);
    let mut remote = panel.config.clone().unwrap();
    remote.revision = 3;
    remote
        .voices
        .iter_mut()
        .find(|voice| voice.id == "personal")
        .unwrap()
        .label = "远端名称".into();
    settings_reply(&mut panel, &remote);
    assert!(
        panel
            .voice_edit
            .as_ref()
            .unwrap()
            .save_request(&remote)
            .is_err()
    );
    let stale = NativeConfig {
        revision: 2,
        ..remote.clone()
    };
    settings_reply(&mut panel, &stale);
    assert_eq!(panel.config.as_ref().unwrap().revision, 3);
    assert_eq!(panel.voice_edit.as_ref().unwrap().text, "用户草稿");
    panel.disconnected();
    assert_eq!(panel.voice_edit.as_ref().unwrap().text, "用户草稿");
    assert!(panel.has_unsaved());
}

#[test]
fn editor_save_is_explicit_and_conflicts_do_not_submit() {
    let (mut panel, voice) = fixture();
    let mut edit = VoiceEdit::new(&voice).unwrap();
    edit.text = "更正的录音文字".into();
    panel.voice_edit = Some(edit);
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    let render = |panel: &mut NativePanel, events| {
        let mut requests = Vec::new();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |ui| panel.voice_editor(ui, &mut requests),
        );
        (output, requests)
    };
    let (output, requests) = render(&mut panel, Vec::new());
    assert!(requests.is_empty());
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "保存声音修改" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .expect("save button");
    let mut sent = Vec::new();
    for pressed in [true, false] {
        let (_, requests) = render(
            &mut panel,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
        sent.extend(requests);
    }
    assert!(matches!(
        sent.as_slice(),
        [NativeRequest::SaveSettings { .. }]
    ));
    let current = panel.config.as_mut().unwrap();
    current
        .voices
        .iter_mut()
        .find(|voice| voice.id == "personal")
        .unwrap()
        .label = "外部修改".into();
    let (_, requests) = render(&mut panel, Vec::new());
    assert!(requests.is_empty());
    assert!(
        panel
            .voice_edit
            .as_ref()
            .unwrap()
            .save_request(panel.config.as_ref().unwrap())
            .is_err()
    );
    assert_eq!(panel.voice_edit.as_ref().unwrap().text, "更正的录音文字");
}

#[test]
fn unfinished_voice_edit_is_explained_before_preview_or_chat() {
    let (mut panel, voice) = fixture();
    let mut edit = VoiceEdit::new(&voice).unwrap();
    edit.text = "尚未保存的原文".into();
    panel.voice_edit = Some(edit);
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    let mut requests = Vec::new();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 900.0),
            )),
            ..Default::default()
        },
        |ui| {
            panel.text_chat_entry(ui);
            panel.preview_and_bind(ui, "person-a", &mut requests);
        },
    );
    let labels: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(labels.contains(&"声音修改尚未保存，进入聊天后仍使用已保存的声音。"));
    assert!(labels.contains(&"声音修改尚未保存；试听和聊天将使用已保存的声音。"));
    assert!(requests.is_empty());
}

#[test]
fn invalid_voice_draft_is_explained_in_place_and_kept_until_fixed() {
    let (mut panel, voice) = fixture();
    let mut edit = VoiceEdit::new(&voice).unwrap();
    edit.label = "  ".into();
    panel.voice_edit = Some(edit);
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    let render = |panel: &mut NativePanel, events| {
        let mut requests = Vec::new();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |ui| panel.voice_editor(ui, &mut requests),
        );
        (output, requests)
    };
    let (output, requests) = render(&mut panel, Vec::new());
    assert!(requests.is_empty());
    assert!(
        output
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
        if text.galley.job.text == "请填写声音名称，最多 128 个字符，不能包含控制字符。"))
    );
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "保存声音修改" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap();
    for pressed in [true, false] {
        let (_, requests) = render(
            &mut panel,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
        assert!(requests.is_empty());
    }
    assert_eq!(panel.voice_edit.as_ref().unwrap().label, "  ");
    let edit = panel.voice_edit.as_mut().unwrap();
    edit.label = "新的声音".into();
    edit.text = " ".into();
    let (output, _) = render(&mut panel, Vec::new());
    assert!(
        output
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
        if text.galley.job.text == "请填写有效的录音对应文字；内容过长时请适当缩短。"))
    );
    panel.voice_edit.as_mut().unwrap().text = "录音内容".into();
    let (output, requests) = render(&mut panel, Vec::new());
    assert!(requests.is_empty());
    assert!(
        !output
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
        if text.galley.job.text.starts_with("请填写")))
    );
}
