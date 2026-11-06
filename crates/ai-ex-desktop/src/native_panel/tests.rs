use super::*;
use ai_ex_domain::{NativeLicense, NativePackageKind, NativePackageState};

fn panel() -> NativePanel {
    NativePanel {
        state: Some(NativeState {
            configured: true,
            configuration_path: "config/native-v1.toml".into(),
            preview_active: false,
            preview_generation: 0,
        }),
        config: Some(NativeConfig::default()),
        selected_voice: "builtin-zh".into(),
        packages: ["native-cpu", "qwen3-4b", "kokoro-v1.1-zh-en"]
            .into_iter()
            .map(|id| NativePackage {
                id: id.into(),
                name: format!("{id}（界面示例）"),
                revision: "0.9.0-alpha.1".into(),
                kind: if id == "native-cpu" {
                    NativePackageKind::Runtime
                } else {
                    NativePackageKind::Model
                },
                state: NativePackageState::NotPrepared,
                bytes: 1024 * 1024,
                installed: false,
                active_operation: None,
                error: None,
                required_packages: Vec::new(),
                licenses: vec![NativeLicense {
                    id: "MIT".into(),
                    source_url: "https://example.org/v1/LICENSE".into(),
                }],
            })
            .collect(),
        ..Default::default()
    }
}

#[test]
fn microphone_discovery_is_explicit_and_old_connection_replies_are_ignored() {
    let mut panel = panel();
    assert!(panel.input_devices.is_none());
    assert!(!mutating(&NativeRequest::InputDevices));
    let old = id(panel.begin(NativeRequest::InputDevices).unwrap());
    panel.disconnected();
    panel.receive(
        old,
        Ok(NativeReply::InputDevices(vec![NativeInputDevice {
            name: "过期设备".into(),
            ambiguous: false,
        }])),
    );
    assert!(panel.input_devices.is_none());
    let fresh = id(panel.begin(NativeRequest::InputDevices).unwrap());
    panel.receive(
        fresh,
        Ok(NativeReply::InputDevices(vec![NativeInputDevice {
            name: "合成设备".into(),
            ambiguous: true,
        }])),
    );
    assert!(panel.input_devices.as_ref().unwrap()[0].ambiguous);
}

#[test]
fn microphone_choice_is_saved_with_switches_and_requires_service_restart() {
    let mut panel = panel();
    let baseline = panel.config.clone().unwrap();
    let first = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(
        first,
        Ok(NativeReply::Settings {
            revision: baseline.revision,
            document: baseline.to_toml().unwrap(),
        }),
    );
    panel.microphone_input_draft = Some(Some(NativeMicrophoneInput::Named {
        name: "合成麦克风".into(),
    }));
    let mut switches = baseline.devices;
    switches.microphone_enabled = true;
    panel.device_draft = Some(switches);
    let request = panel.voice_settings_request().unwrap();
    let NativeRequest::SaveSettings {
        expected_revision,
        document,
    } = &request
    else {
        panic!("expected settings update");
    };
    assert_eq!(*expected_revision, baseline.revision);
    let mut committed = NativeConfig::parse(document).unwrap();
    assert_eq!(
        committed.microphone_input,
        panel.microphone_input_draft.clone().unwrap()
    );
    assert!(committed.devices.microphone_enabled);
    let id = id(panel.begin(request).unwrap());
    committed.revision += 1;
    panel.receive(
        id,
        Ok(NativeReply::Settings {
            revision: committed.revision,
            document: committed.to_toml().unwrap(),
        }),
    );
    assert!(panel.microphone_input_draft.is_none());
    assert!(panel.device_draft.is_none());
    assert!(panel.microphone_restart_required());
    panel.disconnected();
    assert!(panel.microphone_restart_required());
    panel.runtime_restarted();
    assert!(!panel.microphone_restart_required());
}

#[test]
fn pending_or_unconfirmed_settings_cannot_be_edited() {
    for lost_reply in [false, true] {
        let mut panel = panel();
        panel.device_draft = Some(ai_ex_config::native::NativeDevices {
            microphone_enabled: true,
            playback_enabled: false,
        });
        panel.speech_cleanup_draft = Some(Some(NativeSpeechCleanup::default()));
        let request = panel.voice_settings_request().unwrap();
        let request_id = id(panel.begin(request).unwrap());
        if lost_reply {
            panel.receive(request_id, Err(AppError::connectivity("synthetic")));
        }
        let context = egui::Context::default();
        let draw = |panel: &mut NativePanel, events| {
            context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 1600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| panel.device_settings(ui, &mut Vec::new()),
            )
        };
        for label in ["启用麦克风，进行语音输入", "为本地伙伴单独设置"] {
            let output = draw(&mut panel, Vec::new());
            let position = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap();
            for pressed in [true, false] {
                draw(
                    &mut panel,
                    vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }
        assert!(panel.device_draft.unwrap().microphone_enabled);
        assert_eq!(
            panel.speech_cleanup_draft,
            Some(Some(NativeSpeechCleanup::default()))
        );
    }
}

#[test]
fn settings_query_before_or_after_timeout_cannot_confirm_an_old_revision() {
    let mut panel = panel();
    let original = panel.config.clone().unwrap();
    let old_query = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.device_draft = Some(ai_ex_config::native::NativeDevices {
        microphone_enabled: true,
        playback_enabled: false,
    });
    let request = panel.voice_settings_request().unwrap();
    let NativeRequest::SaveSettings { document, .. } = &request else {
        panic!("expected settings update")
    };
    let mut saved = NativeConfig::parse(document).unwrap();
    let save = id(panel.begin(request).unwrap());
    panel.receive(
        old_query,
        Ok(NativeReply::Settings {
            revision: original.revision,
            document: original.to_toml().unwrap(),
        }),
    );
    assert!(panel.device_draft.unwrap().microphone_enabled);
    let stale = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(save, Err(AppError::connectivity("synthetic")));
    panel.receive(
        stale,
        Ok(NativeReply::Settings {
            revision: original.revision,
            document: original.to_toml().unwrap(),
        }),
    );
    assert!(panel.uncertain && panel.saving_settings());
    assert!(panel.device_draft.unwrap().microphone_enabled);
    let later = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(
        later,
        Ok(NativeReply::Settings {
            revision: original.revision,
            document: original.to_toml().unwrap(),
        }),
    );
    assert!(panel.uncertain && panel.saving_settings());
    assert!(panel.status.contains("重启服务"));
    let later = id(panel.begin(NativeRequest::Settings).unwrap());
    saved.revision += 1;
    panel.receive(
        later,
        Ok(NativeReply::Settings {
            revision: saved.revision,
            document: saved.to_toml().unwrap(),
        }),
    );
    assert!(!panel.uncertain);
    assert!(panel.device_draft.is_none());
}

#[test]
fn instance_change_preserves_unknown_save_until_explicit_recovery() {
    let mut panel = panel();
    panel.device_draft = Some(ai_ex_config::native::NativeDevices {
        microphone_enabled: true,
        playback_enabled: false,
    });
    let request = panel.voice_settings_request().unwrap();
    panel.begin(request).unwrap();
    panel.runtime_restarted();
    assert!(panel.uncertain && panel.saving_settings());
    assert!(panel.device_draft.unwrap().microphone_enabled);
    panel.disconnected();
    assert!(panel.uncertain && panel.saving_settings());
    let stop = id(panel.begin(NativeRequest::StopPreview).unwrap());
    panel.receive(stop, Err(AppError::unavailable("synthetic")));
    assert!(panel.uncertain && panel.saving_settings());
    assert!(panel.confirm_old_service_stopped());
    assert!(panel.uncertain && panel.saving_settings());
    let query = id(panel.begin(NativeRequest::Settings).unwrap());
    let current = NativeConfig::default();
    panel.receive(
        query,
        Ok(NativeReply::Settings {
            revision: current.revision,
            document: current.to_toml().unwrap(),
        }),
    );
    assert!(!panel.uncertain);
    assert!(panel.device_draft.unwrap().microphone_enabled);
}

fn begin_space_check(panel: &mut NativePanel, package_id: &str) -> u64 {
    panel.close_download_confirmation();
    panel.download_confirmation = Some(package_id.to_owned());
    let mut requests = Vec::new();
    panel.request_storage_space(&mut requests);
    assert_eq!(requests.len(), 1);
    let request = requests.pop().unwrap();
    assert!(!mutating(&request));
    id(panel.begin(request).unwrap())
}

fn space_labels(panel: &mut NativePanel) -> Vec<String> {
    let context = egui::Context::default();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 1600.0),
            )),
            ..Default::default()
        },
        |ui| panel.package(ui, "qwen3-4b", &mut Vec::new()),
    );
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn storage_space_confirmation_distinguishes_unknown_insufficient_and_snapshot() {
    let mut panel = panel();
    let revision = panel.config.as_ref().unwrap().revision;
    let request = begin_space_check(&mut panel, "qwen3-4b");
    assert!(!panel.has_pending());
    assert!(
        space_labels(&mut panel)
            .iter()
            .any(|text| text.contains("可用空间：未知"))
    );
    panel.receive(
        request,
        Ok(NativeReply::StorageSpace {
            revision,
            available_bytes: Some(100),
        }),
    );
    let labels = space_labels(&mut panel);
    assert!(labels.iter().any(|text| text.contains("当前可用空间小于")));
    assert!(labels.iter().any(|text| text.contains("临时空间")));
    let request = begin_space_check(&mut panel, "qwen3-4b");
    panel.receive(
        request,
        Ok(NativeReply::StorageSpace {
            revision,
            available_bytes: Some(2 * 1024 * 1024),
        }),
    );
    assert!(
        !space_labels(&mut panel)
            .iter()
            .any(|text| text.contains("当前可用空间小于"))
    );
    assert!(
        space_labels(&mut panel)
            .iter()
            .any(|text| text == "确认下载")
    );
}

#[test]
fn storage_space_old_replies_cannot_cross_target_close_settings_or_disconnect() {
    let mut panel = panel();
    let revision = panel.config.as_ref().unwrap().revision;
    let old = begin_space_check(&mut panel, "qwen3-4b");
    let current = begin_space_check(&mut panel, "native-cpu");
    panel.receive(
        old,
        Ok(NativeReply::StorageSpace {
            revision,
            available_bytes: Some(1),
        }),
    );
    assert_eq!(panel.storage_space.as_ref().unwrap().available_bytes, None);
    panel.close_download_confirmation();
    panel.receive(
        current,
        Ok(NativeReply::StorageSpace {
            revision,
            available_bytes: Some(1),
        }),
    );
    assert!(panel.storage_space.is_none());
    let old = begin_space_check(&mut panel, "qwen3-4b");
    let settings = id(panel.begin(NativeRequest::Settings).unwrap());
    let config = panel.config.as_ref().unwrap();
    panel.receive(
        settings,
        Ok(NativeReply::Settings {
            revision,
            document: config.to_toml().unwrap(),
        }),
    );
    panel.receive(
        old,
        Ok(NativeReply::StorageSpace {
            revision,
            available_bytes: Some(1),
        }),
    );
    assert!(panel.storage_space.is_none());
    let old = begin_space_check(&mut panel, "qwen3-4b");
    panel.disconnected();
    panel.receive(
        old,
        Ok(NativeReply::StorageSpace {
            revision,
            available_bytes: Some(1),
        }),
    );
    assert!(panel.download_confirmation.is_none());
    assert!(panel.storage_space.is_none());
}

#[test]
fn storage_space_failure_leaves_download_confirmation_usable() {
    let mut panel = panel();
    let request = begin_space_check(&mut panel, "qwen3-4b");
    panel.receive(request, Err(AppError::connectivity("fixture")));
    assert!(!panel.uncertain);
    assert!(
        space_labels(&mut panel)
            .iter()
            .any(|text| text.contains("可用空间：未知"))
    );
    assert!(
        space_labels(&mut panel)
            .iter()
            .any(|text| text == "确认下载")
    );
}

#[test]
fn storage_space_is_requested_by_opening_download_confirmation() {
    let mut panel = panel();
    let revision = panel.config.as_ref().unwrap().revision;
    let context = egui::Context::default();
    let render = |panel: &mut NativePanel, events| {
        let mut requests = Vec::new();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 1600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| panel.package(ui, "qwen3-4b", &mut requests),
        );
        (output, requests)
    };
    let (output, requests) = render(&mut panel, Vec::new());
    assert!(requests.is_empty());
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "下载准备" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .expect("download button");
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
    assert_eq!(
        sent,
        vec![NativeRequest::StorageSpace {
            expected_revision: revision
        }]
    );
    assert_eq!(panel.download_confirmation.as_deref(), Some("qwen3-4b"));
}

#[test]
fn asynchronous_voice_import_selects_confirmed_voice_and_preserves_a_changed_draft() {
    use ai_ex_config::native::{ModelSelection, NativeBackend, VoiceLanguage, VoiceSource};
    use ai_ex_domain::NativeOperationPhase;
    for change_draft in [false, true] {
        let mut panel = panel();
        panel.profile_id = "default".into();
        panel.voice_name = "合成音色".into();
        panel.reference_path = r"C:\synthetic.wav".into();
        panel.reference_text = "合成对应原文".into();
        let request = NativeRequest::ImportVoice {
            name: panel.voice_name.clone(),
            path: panel.reference_path.clone(),
            text: panel.reference_text.clone(),
            language: "zh".into(),
            expected_revision: 1,
        };
        let request_id = id(panel.begin(request).unwrap());
        let mut operation = NativeOperation {
            id: Uuid::new_v4(),
            package_id: "voice-import".into(),
            phase: NativeOperationPhase::Importing,
            completed_bytes: 0,
            total_bytes: 0,
            error: None,
        };
        panel.receive(request_id, Ok(NativeReply::Operation(operation.clone())));
        assert!(panel.voice_imports.contains_key(&operation.id));
        if change_draft {
            panel.reference_text = "用户正在填写下一份原文".into();
        }
        let request_id = id(panel
            .begin(NativeRequest::Operation { id: operation.id })
            .unwrap());
        operation.phase = NativeOperationPhase::Completed;
        panel.receive(request_id, Ok(NativeReply::Operation(operation)));
        let mut config = panel.config.clone().unwrap();
        let request_id = id(panel.begin(NativeRequest::Settings).unwrap());
        panel.receive(
            request_id,
            Ok(NativeReply::Settings {
                revision: 1,
                document: config.to_toml().unwrap(),
            }),
        );
        assert!(
            panel.completed_voice_import.is_some(),
            "a stale settings reply cannot confirm import"
        );
        config.revision = 2;
        let mut voice = config.voices[0].clone();
        voice.id = "confirmed-import".into();
        voice.label = "合成音色".into();
        voice.language = VoiceLanguage::Zh;
        voice.model = ModelSelection::cpu(NativeBackend::Qwen3TtsOnnx, "qwen3-tts-base-0.6b");
        voice.source = VoiceSource::ReferenceAudio {
            audio_id: Uuid::new_v4().to_string(),
            text: "合成对应原文".into(),
        };
        config.voices.push(voice);
        let request_id = id(panel.begin(NativeRequest::Settings).unwrap());
        panel.receive(
            request_id,
            Ok(NativeReply::Settings {
                revision: 2,
                document: config.to_toml().unwrap(),
            }),
        );
        assert_eq!(panel.selected_voice, "confirmed-import");
        assert!(panel.completed_voice_import.is_none() && panel.voice_imports.is_empty());
        if change_draft {
            assert_eq!(panel.reference_text, "用户正在填写下一份原文");
            assert_eq!(panel.reference_path, r"C:\synthetic.wav");
        } else {
            assert!(panel.reference_path.is_empty() && panel.reference_text.is_empty());
        }
        let (_, commands) = render(&mut panel, &egui::Context::default(), Vec::new());
        assert!(commands.iter().all(
            |command| matches!(command, WorkerCommand::Native { request,.. } if !mutating(request))
        ));
    }
}

fn id(command: WorkerCommand) -> u64 {
    match command {
        WorkerCommand::Native { request_id, .. } => request_id,
        _ => panic!("native command"),
    }
}

fn render(
    panel: &mut NativePanel,
    context: &egui::Context,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Vec<WorkerCommand>) {
    let mut commands = Vec::new();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 1000.0),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            commands = panel.show(ui, "default", true);
        },
    );
    (output, commands)
}

fn click_label(panel: &mut NativePanel, context: &egui::Context, label: &str) {
    let (output, _) = render(panel, context, Vec::new());
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing button: {label}"));
    for pressed in [true, false] {
        let _ = render(
            panel,
            context,
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
    }
}

#[test]
fn dropping_fills_only_the_explicitly_armed_model_draft_once() {
    let mut panel = panel();
    let context = egui::Context::default();
    let _ = render(&mut panel, &context, Vec::new());
    let file = egui::DroppedFile {
        path: Some(r"D:\离线资源\运行包.zip".into()),
        ..Default::default()
    };
    let _ = context.run_ui(
        egui::RawInput {
            dropped_files: vec![file.clone()],
            ..Default::default()
        },
        |ui| {
            let _ = panel.show(ui, "default", true);
            assert_eq!(ui.input(|input| input.raw.dropped_files.len()), 1);
        },
    );
    assert!(
        panel
            .import_paths
            .get("native-cpu")
            .is_none_or(String::is_empty)
    );
    click_label(&mut panel, &context, "准备拖入此组件");
    assert_eq!(
        panel.drop_target,
        Some(DropTarget::Package("native-cpu".into()))
    );
    let mut commands = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            focused: false,
            events: vec![egui::Event::PointerMoved(egui::pos2(999.0, 999.0))],
            dropped_files: vec![file],
            ..Default::default()
        },
        |ui| commands = panel.show(ui, "default", true),
    );
    assert_eq!(panel.import_paths["native-cpu"], r"D:\离线资源\运行包.zip");
    assert!(panel.status.contains("核对后点击"));
    assert!(panel.drop_target.is_none());
    assert!(commands.iter().all(
        |command| matches!(command, WorkerCommand::Native { request, .. } if !mutating(request))
    ));

    let _ = context.run_ui(
        egui::RawInput {
            dropped_files: vec![egui::DroppedFile {
                path: Some(r"D:\wrong.zip".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
        |ui| {
            let _ = panel.show(ui, "default", true);
        },
    );
    assert_eq!(panel.import_paths["native-cpu"], r"D:\离线资源\运行包.zip");
}

#[test]
fn dropped_paths_reject_multiple_items_and_missing_system_paths() {
    let context = egui::Context::default();
    let mut panel = panel();
    for files in [
        vec![egui::DroppedFile::default()],
        vec![egui::DroppedFile::default(), egui::DroppedFile::default()],
    ] {
        panel.arm_drop(DropTarget::Package("native-cpu".into()));
        let _ = context.run_ui(
            egui::RawInput {
                dropped_files: files,
                ..Default::default()
            },
            |ui| {
                let _ = panel.show(ui, "default", true);
            },
        );
        assert!(panel.drop_target.is_none());
        assert!(!panel.status.is_empty());
        assert!(
            panel
                .import_paths
                .get("native-cpu")
                .is_none_or(String::is_empty)
        );
    }
}

#[test]
fn dropping_a_reference_wav_preserves_the_voice_draft_without_importing() {
    let mut panel = panel();
    panel.step = 2;
    panel.reference_text = "已经填写的原文".into();
    let context = egui::Context::default();
    let (output, _) = render(&mut panel, &context, Vec::new());
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == "用参考音频制作专属声音" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .expect("reference audio section is visible");
    for pressed in [true, false] {
        let _ = render(
            &mut panel,
            &context,
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
    }
    for _ in 0..20 {
        let _ = render(&mut panel, &context, Vec::new());
    }
    click_label(&mut panel, &context, "准备拖入参考 WAV");
    assert_eq!(panel.drop_target, Some(DropTarget::ReferenceWav));
    let mut commands = Vec::new();
    let _ = context.run_ui(
        egui::RawInput {
            dropped_files: vec![egui::DroppedFile {
                path: Some(r"D:\声音\参考.wav".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
        |ui| commands = panel.show(ui, "default", true),
    );
    assert_eq!(panel.reference_path, r"D:\声音\参考.wav");
    assert!(panel.drop_target.is_none());
    assert!(panel.status.contains("核对后点击"));
    assert_eq!(panel.reference_text, "已经填写的原文");
    assert!(commands.iter().all(
        |command| matches!(command, WorkerCommand::Native { request, .. } if !mutating(request))
    ));
}

#[test]
fn armed_drop_is_cancelled_by_user_step_change_and_disconnect() {
    let mut panel = panel();
    let context = egui::Context::default();
    click_label(&mut panel, &context, "准备拖入此组件");
    assert!(panel.drop_target.is_some());
    panel.cancel_drop();
    assert!(panel.drop_target.is_none());
    click_label(&mut panel, &context, "准备拖入此组件");
    click_label(&mut panel, &context, "2 · 语音开关");
    assert_eq!(panel.step, 1);
    assert!(panel.drop_target.is_none());
    panel.arm_drop(DropTarget::ReferenceWav);
    panel.disconnected();
    assert!(panel.drop_target.is_none());
}

#[test]
fn opening_is_read_only_and_download_requires_the_visible_confirmation() {
    let mut panel = panel();
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    panel.download_confirmation = Some("qwen3-4b".into());
    let render_scrolled = |panel: &mut NativePanel, events| {
        let mut commands = Vec::new();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 1000.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                egui::ScrollArea::vertical()
                    .vertical_scroll_offset(400.0)
                    .show(ui, |ui| {
                        commands = panel.show(ui, "default", true);
                    });
            },
        );
        (output, commands)
    };
    let (output, commands) = render_scrolled(&mut panel, Vec::new());
    assert!(commands.iter().all(
        |command| matches!(command, WorkerCommand::Native { request, .. } if !mutating(request))
    ));
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == "确认下载" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .expect("download confirmation is visible");
    let mut sent = Vec::new();
    for pressed in [true, false] {
        let (_, commands) = render_scrolled(
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
        sent.extend(commands);
    }
    assert!(sent.iter().any(|command| matches!(command, WorkerCommand::Native { request: NativeRequest::Download { package_id }, .. } if package_id == "qwen3-4b")));
}

#[test]
fn download_confirmation_waits_for_uncertain_or_pending_operations() {
    let mut panel = panel();
    panel.download_confirmation = Some("qwen3-4b".into());
    panel.uncertain = true;
    let context = egui::Context::default();
    let render_card = |panel: &mut NativePanel, events| {
        let mut requests = Vec::new();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 1600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| panel.package(ui, "qwen3-4b", &mut requests),
        );
        (output, requests)
    };
    let click = |panel: &mut NativePanel, label: &str| {
        let (output, _) = render_card(panel, Vec::new());
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("button visible");
        let mut result = Vec::new();
        for pressed in [true, false] {
            let (_, requests) = render_card(
                panel,
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
            result.extend(requests);
        }
        result
    };
    let (output, _) = render_card(&mut panel, Vec::new());
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("操作结果待核对"))));
    assert!(click(&mut panel, "确认下载").is_empty());
    assert_eq!(panel.download_confirmation.as_deref(), Some("qwen3-4b"));
    panel.uncertain = false;
    let pending = id(panel.begin(NativeRequest::CreateDefaults).unwrap());
    assert!(click(&mut panel, "确认下载").is_empty());
    assert!(panel.download_confirmation.is_some());
    panel.pending.remove(&pending);
    assert!(
        matches!(click(&mut panel, "确认下载").as_slice(), [NativeRequest::Download { package_id }] if package_id == "qwen3-4b")
    );
    assert!(panel.download_confirmation.is_none());
    panel.download_confirmation = Some("qwen3-4b".into());
    panel.uncertain = true;
    assert!(click(&mut panel, "暂不下载").is_empty());
    assert!(panel.download_confirmation.is_none());
}

#[test]
fn opening_download_confirmation_scrolls_it_into_view() {
    let mut panel = panel();
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    let render_scroll = |panel: &mut NativePanel, events| {
        context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 1000.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                egui::ScrollArea::vertical().animated(false).show(ui, |ui| {
                    panel.show(ui, "default", true);
                });
            },
        )
    };
    let output = render_scroll(&mut panel, Vec::new());
    let position = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "下载准备" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .next_back()
        .expect("second package download button");
    for pressed in [true, false] {
        let _ = render_scroll(
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
    }
    assert_eq!(panel.download_confirmation.as_deref(), Some("qwen3-4b"));
    let output = render_scroll(&mut panel, Vec::new());
    assert!(
        output.shapes.iter().any(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "确认下载" => shape
                .clip_rect
                .contains(text.pos + text.galley.size() * 0.5),
            _ => false,
        }),
        "confirmation must scroll into the visible area"
    );
}

#[test]
fn uncertain_download_keeps_waiting_after_a_package_snapshot() {
    let mut panel = panel();
    let request = NativeRequest::Download {
        package_id: "qwen3-4b".into(),
    };
    let request_id = id(panel.begin(request.clone()).unwrap());
    panel.receive(
        request_id,
        Err(AppError::connectivity("synthetic lost reply")),
    );
    assert!(panel.uncertain);
    assert!(panel.begin(request.clone()).is_none());
    let (_, commands) = render(&mut panel, &egui::Context::default(), Vec::new());
    assert!(commands.iter().all(
        |command| matches!(command, WorkerCommand::Native { request, .. } if !mutating(request))
    ));
    let operation = Uuid::new_v4();
    let mut packages = panel.packages.clone();
    packages[1].active_operation = Some(operation);
    let request_id = commands
        .into_iter()
        .find_map(|command| match command {
            WorkerCommand::Native {
                request_id,
                request: NativeRequest::Packages,
            } => Some(request_id),
            _ => None,
        })
        .unwrap();
    panel.receive(request_id, Ok(NativeReply::Packages(packages)));
    assert!(panel.uncertain);
    assert_eq!(panel.package_unconfirmed.len(), 1);
    assert!(panel.retained_package_operations.contains_key(&operation));
    assert!(panel.begin(request.clone()).is_none());
    panel.last_poll = None;
    let (_, commands) = render(&mut panel, &egui::Context::default(), Vec::new());
    assert!(commands.iter().any(|command| matches!(command, WorkerCommand::Native { request: NativeRequest::Operation { id }, .. } if *id == operation)));
    assert!(!commands.iter().any(|command| matches!(
        command,
        WorkerCommand::Native {
            request: NativeRequest::Download { .. },
            ..
        }
    )));
}

#[test]
fn observed_terminal_operation_survives_disconnect_while_request_is_unknown() {
    let mut panel = panel();
    let download = NativeRequest::Download {
        package_id: "qwen3-4b".into(),
    };
    let download_id = id(panel.begin(download.clone()).unwrap());
    panel.receive(
        download_id,
        Err(AppError::connectivity("synthetic lost reply")),
    );
    let operation_id = Uuid::new_v4();
    let mut packages = panel.packages.clone();
    packages[1].active_operation = Some(operation_id);
    let query_id = id(panel.begin(NativeRequest::Packages).unwrap());
    panel.receive(query_id, Ok(NativeReply::Packages(packages)));
    let query_id = id(panel
        .begin(NativeRequest::Operation { id: operation_id })
        .unwrap());
    panel.receive(
        query_id,
        Ok(NativeReply::Operation(NativeOperation {
            id: operation_id,
            package_id: "qwen3-4b".into(),
            phase: ai_ex_domain::NativeOperationPhase::Completed,
            completed_bytes: 1,
            total_bytes: 1,
            error: None,
        })),
    );
    panel.disconnected();
    assert_eq!(panel.package_unconfirmed.get(&download_id), Some(&download));
    assert!(panel.uncertain);
    assert_eq!(
        panel
            .retained_package_operations
            .get(&operation_id)
            .unwrap(),
        "qwen3-4b"
    );
    assert_eq!(
        panel.retained_package_terminal.get(&operation_id),
        Some(&ai_ex_domain::NativeOperationPhase::Completed)
    );
    let output = egui::Context::default().run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 1200.0),
            )),
            ..Default::default()
        },
        |ui| {
            panel.show(ui, "default", false);
        },
    );
    let labels = output.shapes.iter().filter_map(|shape| match &shape.shape {
        egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
        _ => None,
    });
    assert!(labels.into_iter().any(|text| {
        text.contains(&operation_id.to_string()) && text.contains("曾观察到已完成")
    }));
}

#[test]
fn earlier_and_later_package_queries_cannot_settle_an_unknown_mutation() {
    for request in [
        NativeRequest::Download {
            package_id: "qwen3-4b".into(),
        },
        NativeRequest::Import {
            package_id: "qwen3-4b".into(),
            path: "C:\\synthetic\\private-marker.zip".into(),
        },
        NativeRequest::Remove {
            package_id: "qwen3-4b".into(),
        },
    ] {
        let mut panel = panel();
        let old_query = id(panel.begin(NativeRequest::Packages).unwrap());
        let mutation = id(panel.begin(request.clone()).unwrap());
        panel.receive(
            mutation,
            Err(AppError::connectivity("synthetic lost reply")),
        );
        let packages = panel.packages.clone();
        panel.receive(old_query, Ok(NativeReply::Packages(packages.clone())));
        assert!(panel.uncertain);
        assert!(panel.begin(request.clone()).is_none());
        let new_query = id(panel.begin(NativeRequest::Packages).unwrap());
        panel.receive(new_query, Ok(NativeReply::Packages(packages)));
        assert!(panel.uncertain);
        assert_eq!(panel.package_unconfirmed.get(&mutation), Some(&request));
        assert!(panel.begin(request).is_none());
    }
}

#[test]
fn disconnect_preserves_pending_package_actions_known_ids_and_drafts() {
    let mut panel = panel();
    let operation = Uuid::new_v4();
    panel.packages[1].active_operation = Some(operation);
    panel.import_paths.insert(
        "qwen3-4b".into(),
        "C:\\synthetic\\private-marker.zip".into(),
    );
    panel.device_draft = Some(ai_ex_config::native::NativeDevices {
        microphone_enabled: true,
        playback_enabled: false,
    });
    let request = NativeRequest::Import {
        package_id: "qwen3-4b".into(),
        path: "C:\\synthetic\\private-marker.zip".into(),
    };
    let pending = id(panel.begin(request.clone()).unwrap());
    panel.disconnected();
    assert_eq!(panel.package_unconfirmed.get(&pending), Some(&request));
    assert_eq!(
        panel.retained_package_operations.get(&operation).unwrap(),
        "qwen3-4b"
    );
    assert!(panel.device_draft.unwrap().microphone_enabled);
    assert_eq!(
        panel.import_paths["qwen3-4b"],
        "C:\\synthetic\\private-marker.zip"
    );
    panel.receive(pending, Ok(NativeReply::Accepted));
    let query = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(
        query,
        Ok(NativeReply::Settings {
            revision: 1,
            document: NativeConfig::default().to_toml().unwrap(),
        }),
    );
    assert!(panel.uncertain);
    assert!(panel.begin(request).is_none());
    assert!(panel.retained_package_operations.contains_key(&operation));
}

#[test]
fn unknown_cancel_retains_its_id_and_cannot_be_sent_twice() {
    let mut panel = panel();
    let operation = Uuid::new_v4();
    panel.packages[1].active_operation = Some(operation);
    let request = NativeRequest::Cancel { id: operation };
    let pending = id(panel.begin(request.clone()).unwrap());
    panel.receive(pending, Err(AppError::connectivity("synthetic lost reply")));
    assert!(panel.uncertain);
    assert_eq!(
        panel.retained_package_operations.get(&operation).unwrap(),
        "qwen3-4b"
    );
    assert!(panel.begin(request).is_none());
    assert!(
        panel
            .begin(NativeRequest::Cancel { id: Uuid::new_v4() })
            .is_some()
    );
    panel.disconnected();
    assert!(panel.uncertain);
    assert!(panel.retained_package_operations.contains_key(&operation));
}

#[test]
fn unknown_save_and_cancel_remain_independent_across_disconnect() {
    let mut panel = panel();
    let operation = Uuid::new_v4();
    panel.packages[1].active_operation = Some(operation);
    let save = NativeRequest::SaveSettings {
        expected_revision: 1,
        document: NativeConfig::default().to_toml().unwrap(),
    };
    let cancel = NativeRequest::Cancel { id: operation };
    let save_id = id(panel.begin(save.clone()).unwrap());
    let cancel_id = id(panel.begin(cancel.clone()).unwrap());
    panel.receive(
        cancel_id,
        Err(AppError::connectivity("synthetic lost cancel")),
    );
    panel.receive(save_id, Err(AppError::connectivity("synthetic lost save")));
    assert!(matches!(
        panel.unconfirmed,
        Some(NativeRequest::SaveSettings { .. })
    ));
    assert_eq!(panel.package_unconfirmed.get(&cancel_id), Some(&cancel));
    panel.disconnected();
    assert!(matches!(
        panel.unconfirmed,
        Some(NativeRequest::SaveSettings { .. })
    ));
    assert_eq!(panel.package_unconfirmed.get(&cancel_id), Some(&cancel));
    let query = id(panel.begin(NativeRequest::Settings).unwrap());
    let newer = NativeConfig {
        revision: 2,
        ..NativeConfig::default()
    };
    panel.receive(
        query,
        Ok(NativeReply::Settings {
            revision: 2,
            document: newer.to_toml().unwrap(),
        }),
    );
    assert!(panel.unconfirmed.is_none());
    assert!(panel.uncertain);
    assert_eq!(panel.package_unconfirmed.get(&cancel_id), Some(&cancel));
    assert!(panel.retained_package_operations.contains_key(&operation));
    assert!(panel.begin(cancel).is_none());
}

#[test]
fn disconnected_recovery_displays_ids_without_private_import_paths() {
    let mut panel = panel();
    let operation = Uuid::new_v4();
    panel.packages[1].active_operation = Some(operation);
    let request = NativeRequest::Import {
        package_id: "qwen3-4b".into(),
        path: "C:\\synthetic\\private-marker.zip".into(),
    };
    let pending = id(panel.begin(request).unwrap());
    panel.receive(pending, Err(AppError::connectivity("synthetic lost reply")));
    panel.disconnected();
    let context = egui::Context::default();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 1200.0),
            )),
            ..Default::default()
        },
        |ui| {
            panel.show(ui, "default", false);
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        labels
            .iter()
            .any(|text| text.contains("退出本应用") && text.contains("原服务已停止"))
    );
    assert!(
        labels
            .iter()
            .any(|text| text.contains(&operation.to_string()))
    );
    assert!(!labels.iter().any(|text| text.contains("private-marker")));
}

#[test]
fn disconnected_import_path_requires_explicit_expand_and_copy() {
    let mut panel = panel();
    let path = "C:\\synthetic\\old model.zip";
    let request = NativeRequest::Import {
        package_id: "qwen3-4b".into(),
        path: path.into(),
    };
    let pending = id(panel.begin(request).unwrap());
    panel.receive(pending, Err(AppError::connectivity("synthetic lost reply")));
    panel.disconnected();
    let context = egui::Context::default();
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        context.style_mut_of(theme, |style| style.animation_time = 0.0);
    }
    let frame = |panel: &mut NativePanel, events| {
        context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 1200.0),
                )),
                events,
                ..Default::default()
            },
            |ui| assert!(panel.show(ui, "default", false).is_empty()),
        )
    };
    let position = |output: &egui::FullOutput, label: &str| {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("requested control is visible")
    };
    let initial = frame(&mut panel, Vec::new());
    assert!(!initial.shapes.iter().any(|shape| matches!(
        &shape.shape, egui::Shape::Text(text) if text.galley.job.text == path
    )));
    let expand = position(&initial, "查看导入路径");
    for pressed in [true, false] {
        frame(
            &mut panel,
            vec![
                egui::Event::PointerMoved(expand),
                egui::Event::PointerButton {
                    pos: expand,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
    }
    let expanded = frame(&mut panel, Vec::new());
    assert!(expanded.shapes.iter().any(|shape| matches!(
        &shape.shape, egui::Shape::Text(text) if text.galley.job.text == path
    )));
    let copy = position(&expanded, "复制导入路径");
    frame(
        &mut panel,
        vec![
            egui::Event::PointerMoved(copy),
            egui::Event::PointerButton {
                pos: copy,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
    );
    let copied = frame(
        &mut panel,
        vec![
            egui::Event::PointerMoved(copy),
            egui::Event::PointerButton {
                pos: copy,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ],
    );
    assert!(
        copied
            .platform_output
            .commands
            .iter()
            .any(|command| matches!(
                command, egui::OutputCommand::CopyText(text) if text == path
            ))
    );
    assert_eq!(panel.package_unconfirmed.len(), 1);
}

#[test]
fn settings_keep_device_drafts_until_save_ack_and_reject_revision_disagreement() {
    let mut panel = panel();
    panel.device_draft = Some(ai_ex_config::native::NativeDevices {
        microphone_enabled: false,
        playback_enabled: true,
    });
    let request_id = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(
        request_id,
        Ok(NativeReply::Settings {
            revision: 2,
            document: NativeConfig::default().to_toml().unwrap(),
        }),
    );
    assert_eq!(panel.config.as_ref().unwrap().revision, 1);
    assert!(panel.device_draft.is_some());
    let document = panel.config.as_ref().unwrap().to_toml().unwrap();
    let request_id = id(panel
        .begin(NativeRequest::SaveSettings {
            expected_revision: 1,
            document: document.clone(),
        })
        .unwrap());
    let committed = NativeConfig {
        revision: 2,
        devices: panel.device_draft.unwrap(),
        ..Default::default()
    };
    panel.receive(
        request_id,
        Ok(NativeReply::Settings {
            revision: 2,
            document: committed.to_toml().unwrap(),
        }),
    );
    assert!(panel.device_draft.is_none());
    assert!(panel.config.as_ref().unwrap().devices.playback_enabled);
}

#[test]
fn native_models_and_voice_pages_render_without_a_window_or_devices() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!("native-onboarding-review-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    for (step, name) in [(0, "models"), (1, "devices"), (2, "voices"), (3, "preview")] {
        let mut panel = panel();
        panel.step = step;
        let context = egui::Context::default();
        crate::ui::theme::configure_appearance(&context);
        let (mut output, _) = render(&mut panel, &context, Vec::new());
        let (next, _) = render(&mut panel, &context, Vec::new());
        output.append(next);
        assert!(
            !output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::epaint::Shape::Text(text) if text.galley.job.text.contains("widget ID"))),
            "repeated controls must have distinct persistent identities"
        );
        let path = directory.join(format!("{name}.png"));
        crate::headless_snapshot::save(&context, output, [1000, 1000], &path).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 1000);
    }
    eprintln!("native onboarding snapshots: {}", directory.display());
}

#[test]
fn reconnect_retires_old_operations_and_late_replies_but_preserves_user_drafts() {
    let mut panel = panel();
    let operation = NativeOperation {
        id: Uuid::new_v4(),
        package_id: "qwen3-4b".into(),
        phase: ai_ex_domain::NativeOperationPhase::Downloading,
        completed_bytes: 0,
        total_bytes: 1024,
        error: None,
    };
    panel.operations.insert(operation.id, operation.clone());
    panel.preview_operation = Some(operation.id);
    panel.reference_text = "保留尚未导入的对应原文".into();
    let old_request = id(panel
        .begin(NativeRequest::Operation { id: operation.id })
        .unwrap());
    panel.disconnected();
    assert!(panel.uncertain && panel.operations.is_empty() && panel.preview_operation.is_none());
    assert!(panel.config.is_none() && panel.packages.is_empty());
    panel.receive(old_request, Ok(NativeReply::Operation(operation)));
    assert!(panel.operations.is_empty());
    let state = id(panel.begin(NativeRequest::State).unwrap());
    panel.receive(
        state,
        Ok(NativeReply::State(NativeState {
            configured: true,
            configuration_path: "config/native-v1.toml".into(),
            preview_active: false,
            preview_generation: 0,
        })),
    );
    let settings = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(
        settings,
        Ok(NativeReply::Settings {
            revision: 1,
            document: NativeConfig::default().to_toml().unwrap(),
        }),
    );
    assert!(!panel.uncertain);
    assert!(panel.config.is_some());
    assert_eq!(panel.reference_text, "保留尚未导入的对应原文");
}

#[test]
fn microphone_change_needs_a_new_runtime_and_survives_a_connection_retry() {
    let mut panel = panel();
    let first = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(
        first,
        Ok(NativeReply::Settings {
            revision: 1,
            document: NativeConfig::default().to_toml().unwrap(),
        }),
    );
    let mut next = NativeConfig::default();
    next.devices.microphone_enabled = true;
    let save = id(panel
        .begin(NativeRequest::SaveSettings {
            expected_revision: 1,
            document: next.to_toml().unwrap(),
        })
        .unwrap());
    next.revision = 2;
    panel.receive(
        save,
        Ok(NativeReply::Settings {
            revision: 2,
            document: next.to_toml().unwrap(),
        }),
    );
    assert!(panel.microphone_restart_required());
    assert!(panel.status.contains("重启服务后生效"));
    panel.disconnected();
    let reload = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(
        reload,
        Ok(NativeReply::Settings {
            revision: 2,
            document: next.to_toml().unwrap(),
        }),
    );
    assert!(panel.microphone_restart_required());
    panel.runtime_restarted();
    let reload = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(
        reload,
        Ok(NativeReply::Settings {
            revision: 2,
            document: next.to_toml().unwrap(),
        }),
    );
    assert!(!panel.microphone_restart_required());
}

#[test]
fn connecting_to_running_service_does_not_claim_microphone_settings_are_active() {
    let mut panel = panel();
    let settings = id(panel.begin(NativeRequest::Settings).unwrap());
    panel.receive(
        settings,
        Ok(NativeReply::Settings {
            revision: 1,
            document: NativeConfig::default().to_toml().unwrap(),
        }),
    );
    let context = egui::Context::default();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 720.0),
            )),
            ..Default::default()
        },
        |ui| panel.text_chat_entry(ui),
    );
    assert!(output.shapes.iter().any(|shape| matches!(
        &shape.shape,
        egui::Shape::Text(text) if text.galley.job.text.contains("当前服务使用的麦克风设置无法确认")
    )));
}

#[test]
fn enabling_sound_only_saves_the_flag_and_preserves_an_unsaved_microphone_draft() {
    let mut panel = panel();
    panel.step = 3;
    panel.device_draft = Some(ai_ex_config::native::NativeDevices {
        microphone_enabled: true,
        playback_enabled: false,
    });
    for package in &mut panel.packages {
        package.installed = true;
        package.state = NativePackageState::Ready;
    }
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    let (output, _) = render(&mut panel, &context, Vec::new());
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == "启用对话声音" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .expect("sound activation is an explicit user action");
    let mut sent = Vec::new();
    for pressed in [true, false] {
        let (_, commands) = render(
            &mut panel,
            &context,
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
        sent.extend(commands);
    }
    assert!(!sent.iter().any(|command| matches!(
        command,
        WorkerCommand::Native {
            request: NativeRequest::PreviewVoice { .. },
            ..
        }
    )));
    let (request_id, mut config) = sent
        .into_iter()
        .find_map(|command| match command {
            WorkerCommand::Native {
                request_id,
                request: NativeRequest::SaveSettings { document, .. },
            } => Some((request_id, NativeConfig::parse(&document).unwrap())),
            _ => None,
        })
        .expect("activation saves confirmed configuration first");
    assert!(config.devices.playback_enabled && !config.devices.microphone_enabled);
    config.revision += 1;
    panel.receive(
        request_id,
        Ok(NativeReply::Settings {
            revision: config.revision,
            document: config.to_toml().unwrap(),
        }),
    );
    assert!(
        panel.device_draft.unwrap().microphone_enabled
            && panel.device_draft.unwrap().playback_enabled
    );
    assert!(!panel.microphone_restart_required());
}

#[test]
fn preview_click_carries_the_confirmed_service_generation() {
    let mut panel = panel();
    panel.step = 3;
    panel.refresh = false;
    panel.last_poll = Some(Instant::now());
    panel.state.as_mut().unwrap().preview_generation = 41;
    panel.config.as_mut().unwrap().devices.playback_enabled = true;
    for package in &mut panel.packages {
        package.installed = true;
        package.state = NativePackageState::Ready;
    }
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    let (output, _) = render(&mut panel, &context, Vec::new());
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == "播放试听" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap();
    let mut sent = Vec::new();
    for pressed in [true, false] {
        let (_, commands) = render(
            &mut panel,
            &context,
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
        sent.extend(commands);
    }
    assert!(sent.iter().any(|command| matches!(
        command,
        WorkerCommand::Native {
            request: NativeRequest::PreviewVoice {
                expected_generation: 41,
                ..
            },
            ..
        }
    )));
}
