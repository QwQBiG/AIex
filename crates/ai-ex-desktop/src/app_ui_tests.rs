use super::*;

use crate::headless_snapshot;
#[path = "app_memory_tests.rs"]
mod memory_tests;

fn app() -> (
    DesktopApp,
    std::sync::mpsc::Sender<WorkerEvent>,
    tokio::sync::mpsc::UnboundedReceiver<WorkerCommand>,
) {
    let (events, receiver) = std::sync::mpsc::channel();
    let (commands, sent) = tokio::sync::mpsc::unbounded_channel();
    let mut app = DesktopApp::with_storage(
        WorkerHandle {
            commands,
            events: receiver,
        },
        false,
        None,
    );
    app.state.connection = ConnectionState::Connected;
    app.persona_synced = true;
    (app, events, sent)
}

fn render(
    app: &mut DesktopApp,
    context: &egui::Context,
    size: [f32; 2],
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(size[0], size[1]),
            )),
            events,
            ..Default::default()
        },
        |ui| app.show_contents(ui),
    )
}

fn label_position(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("label is visible: {label}"))
}

fn click(app: &mut DesktopApp, context: &egui::Context, position: egui::Pos2) {
    for pressed in [true, false] {
        render(
            app,
            context,
            [1000.0, 720.0],
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

#[test]
fn leaving_native_page_cancels_a_pending_file_drop() {
    let (mut app, _events, _sent) = app();
    app.native_mode = true;
    app.page = layout::Page::Native;
    app.native.state = Some(ai_ex_domain::NativeState {
        configured: true,
        configuration_path: "synthetic.toml".into(),
        preview_active: false,
        preview_generation: 0,
    });
    app.native.packages = vec![ai_ex_domain::NativePackage {
        id: "native-cpu".into(),
        name: "运行组件".into(),
        revision: "test".into(),
        kind: ai_ex_domain::NativePackageKind::Runtime,
        state: ai_ex_domain::NativePackageState::NotPrepared,
        bytes: 1,
        installed: false,
        active_operation: None,
        error: None,
        required_packages: Vec::new(),
        licenses: Vec::new(),
    }];
    let context = egui::Context::default();
    configure_appearance(&context);
    render(&mut app, &context, [1000.0, 720.0], Vec::new());
    let output = render(&mut app, &context, [1000.0, 720.0], Vec::new());
    click(
        &mut app,
        &context,
        label_position(&output, "准备拖入此组件"),
    );
    let output = render(&mut app, &context, [1000.0, 720.0], Vec::new());
    label_position(&output, "等待拖入：运行组件");
    click(&mut app, &context, label_position(&output, "取消拖入"));
    assert!(!app.native.waiting_for_drop());
    let output = render(&mut app, &context, [1000.0, 720.0], Vec::new());
    click(
        &mut app,
        &context,
        label_position(&output, "准备拖入此组件"),
    );
    let output = render(&mut app, &context, [1000.0, 720.0], Vec::new());
    label_position(&output, "等待拖入：运行组件");
    click(&mut app, &context, label_position(&output, "聊天"));
    assert_eq!(app.page, layout::Page::Conversation);
    app.page = layout::Page::Native;
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 720.0),
            )),
            dropped_files: vec![egui::DroppedFile {
                path: Some(r"D:\late.zip".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
        |ui| app.show_contents(ui),
    );
    assert!(
        app.native
            .import_paths
            .get("native-cpu")
            .is_none_or(String::is_empty)
    );
}

#[test]
fn leaving_closing_or_emergency_cancels_due_preview_before_polling() {
    use crate::native_desktop_panel::tests::{
        arm_expired_for_app_test, preview_armed_for_app_test, running_preview_panel,
    };
    for reason in ["navigation", "page", "close", "emergency"] {
        let (mut app, _events, mut sent) = app();
        let (mut panel, snapshot) = running_preview_panel();
        arm_expired_for_app_test(&mut panel);
        app.state.runtime.instance_id = Some(snapshot.instance_id);
        app.native_desktop = panel;
        app.page = layout::Page::Settings;
        let context = egui::Context::default();
        match reason {
            "navigation" => app.request_navigation(&context, crate::navigation::Destination::Home),
            "page" => {
                app.page = layout::Page::Conversation;
                render(&mut app, &context, [1000.0, 720.0], vec![]);
            }
            "close" => app.handle_close_request(&context, true),
            "emergency" => {
                assert!(app.send(WorkerCommand::EmergencyStop));
                assert!(matches!(sent.try_recv(), Ok(WorkerCommand::EmergencyStop)));
            }
            _ => unreachable!(),
        }
        assert!(!preview_armed_for_app_test(&app.native_desktop), "{reason}");
        assert!(
            !matches!(
                app.native_desktop.poll(),
                Some(WorkerCommand::NativeDesktopPreview { .. })
            ),
            "{reason}"
        );
    }
}

#[test]
fn empty_chat_suggestion_only_fills_the_draft_and_status_remains_visible() {
    let (mut app, _events, mut sent) = app();
    let context = egui::Context::default();
    configure_appearance(&context);
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    let output = render(&mut app, &context, [1000.0, 720.0], vec![]);
    label_position(&output, "已连接");
    let suggestion = "你好，今天我们来聊聊吧。";
    click(&mut app, &context, label_position(&output, suggestion));
    assert_eq!(app.input, suggestion);
    assert!(sent.try_recv().is_err());
    app.state.connection = ConnectionState::Disconnected;
    let output = render(&mut app, &context, [1000.0, 720.0], vec![]);
    label_position(&output, "未连接");
    label_position(&output, "尚未连接，草稿可以先写");
    assert_eq!(app.input, suggestion);
}

#[test]
fn sending_requires_synchronization_and_keeps_drafts_when_the_worker_has_stopped() {
    let (mut app, _events, mut sent) = app();
    app.input = "  先保留这段草稿  ".to_owned();
    app.state.needs_resync = true;
    app.submit();
    assert_eq!(app.input, "  先保留这段草稿  ");
    assert!(sent.try_recv().is_err());
    app.state.needs_resync = false;
    app.persona_synced = false;
    app.submit();
    assert!(sent.try_recv().is_err());
    app.persona_synced = true;
    app.submit();
    assert!(app.input.is_empty());
    assert!(
        matches!(sent.try_recv().unwrap(), WorkerCommand::Submit(text) if text == "先保留这段草稿")
    );
    drop(sent);
    app.input = "线程关闭时也保留".to_owned();
    app.submit();
    assert_eq!(app.input, "线程关闭时也保留");
    assert!(app.last_error.is_some());
}

#[test]
fn narrow_chat_shows_a_short_exchange_without_clipping() {
    let (mut app, _events, _sent) = app();
    app.state.turns.push(ai_ex_ui_model::UiTurn {
        turn_id: ai_ex_domain::TurnId::new(),
        user_text: "今天学习了很久，想休息一下再继续。".into(),
        assistant_text:
            "好啊，我陪你。先让肩膀放松一下，喝口水也好。\n等你想继续了，我们再一起看看剩下的安排。"
                .into(),
        status: ai_ex_ui_model::TurnStatus::Completed,
    });
    let context = egui::Context::default();
    configure_appearance(&context);
    render(&mut app, &context, [720.0, 600.0], vec![]);
    let output = render(&mut app, &context, [720.0, 600.0], vec![]);
    for label in [
        "你",
        app.state.turns[0].user_text.as_str(),
        app.state.turns[0].assistant_text.as_str(),
    ] {
        let (clip, rect) = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => Some((
                    shape.clip_rect,
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing conversation text: {label}"));
        assert!(
            clip.contains_rect(rect),
            "{label}: rect={rect:?}, clip={clip:?}"
        );
    }
}

#[test]
fn long_messages_leave_send_visible_in_wide_and_small_windows() {
    for size in [[720.0, 520.0], [1100.0, 720.0]] {
        let (mut app, _events, _sent) = app();
        app.input = "很长的多行草稿\n".repeat(240);
        app.state.turns.push(ai_ex_ui_model::UiTurn {
            turn_id: ai_ex_domain::TurnId::new(),
            user_text: "一条很长的消息\n".repeat(60),
            assistant_text: "这是一段回复。\n".repeat(100),
            status: ai_ex_ui_model::TurnStatus::Completed,
        });
        let context = egui::Context::default();
        configure_appearance(&context);
        render(&mut app, &context, size, vec![]);
        let output = render(&mut app, &context, size, vec![]);
        let send = label_position(&output, "发送");
        assert!(
            send.y > size[1] - 170.0 && send.y < size[1] - 8.0,
            "send y={}",
            send.y
        );
        assert!(send.x >= 0.0 && send.x < size[0]);
        assert!(output.shapes.iter().all(|shape| !matches!(&shape.shape,
            egui::epaint::Shape::Text(text) if text.galley.job.text == "新手控制台")));
        for primitive in context.tessellate(output.shapes, output.pixels_per_point) {
            if let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive {
                assert!(mesh.is_valid());
            }
        }
    }
}

#[test]
fn composer_first_line_and_send_stay_inside_the_clip_when_resizing() {
    for connected in [true, false] {
        let (mut app, _events, _sent) = app();
        app.input = "首行草稿必须完整可见".to_owned();
        if !connected {
            app.state.connection = ConnectionState::Disconnected;
        }
        let context = egui::Context::default();
        configure_appearance(&context);
        for size in [[1120.0, 720.0], [720.0, 600.0], [720.0, 520.0]] {
            render(&mut app, &context, size, vec![]);
            let output = render(&mut app, &context, size, vec![]);
            for label in [app.input.as_str(), "发送"] {
                let (clip, rect) = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == label => Some((
                            shape.clip_rect,
                            egui::Rect::from_min_size(text.pos, text.galley.size()),
                        )),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("visible composer text: {label}"));
                assert!(
                    clip.contains_rect(rect),
                    "{label}: rect={rect:?}, clip={clip:?}, size={size:?}, connected={connected}"
                );
                assert!(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, size.into()).contains_rect(rect)
                );
                let (frame_clip, frame) = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Rect(frame)
                            if frame.fill != egui::Color32::TRANSPARENT
                                && frame.rect.contains_rect(rect) =>
                        {
                            Some((shape.clip_rect, frame.rect))
                        }
                        _ => None,
                    })
                    .min_by(|(_, left), (_, right)| left.area().total_cmp(&right.area()))
                    .unwrap_or_else(|| panic!("visible composer frame: {label}"));
                assert!(
                    frame_clip.contains_rect(frame),
                    "{label} frame={frame:?}, clip={frame_clip:?}, size={size:?}, connected={connected}"
                );
                assert!(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, size.into()).contains_rect(frame),
                    "{label} frame must fit the window: {frame:?}, size={size:?}, connected={connected}"
                );
            }
        }
    }
}

#[test]
fn navigation_keeps_drafts_and_requires_confirmation_before_leaving_the_window() {
    let (mut app, _events, _sent) = app();
    app.input = "尚未发送的内容".to_owned();
    let context = egui::Context::default();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    let output = render(&mut app, &context, [1000.0, 720.0], vec![]);
    click(&mut app, &context, label_position(&output, "设置与诊断"));
    assert_eq!(app.page, layout::Page::Settings);
    assert_eq!(app.input, "尚未发送的内容");
    let output = render(&mut app, &context, [1000.0, 720.0], vec![]);
    click(&mut app, &context, label_position(&output, "连接设置"));
    assert_eq!(
        app.pending_navigation,
        Some(crate::navigation::Destination::Setup)
    );
    assert_eq!(app.navigation.take(), None);
    let output = render(&mut app, &context, [1000.0, 720.0], vec![]);
    click(&mut app, &context, label_position(&output, "继续编辑"));
    assert_eq!(app.pending_navigation, None);
    assert_eq!(app.input, "尚未发送的内容");
}

#[test]
fn backup_drop_waiting_is_cleared_when_leaving_memory_page() {
    let (mut app, _events, _sent) = app();
    app.page = layout::Page::Memory;
    app.state.runtime.instance_id = Some(uuid::Uuid::new_v4());
    let context = egui::Context::default();
    app.synchronize_desktop_controls();
    app.backup_inspection.arm_drop();
    assert!(app.backup_inspection.waiting_for_drop());
    app.page = layout::Page::Conversation;
    let mut remaining = 0;
    let _ = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 720.0),
            )),
            dropped_files: vec![egui::DroppedFile {
                path: Some("C:\\synthetic\\backup.jsonl".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
        |ui| {
            app.show_contents(ui);
            remaining = ui.input(|input| input.raw.dropped_files.len());
        },
    );
    assert!(!app.backup_inspection.waiting_for_drop());
    assert_eq!(remaining, 1);
}

#[test]
fn runtime_timing_tracks_the_current_connection_turn_and_event_sequence() {
    use ai_ex_control::{RuntimeTiming, TurnTiming};
    use ai_ex_domain::TurnId;
    use ai_ex_observability::SequencedEvent;

    let (mut app, events, _sent) = app();
    let instance = uuid::Uuid::new_v4();
    let turn = TurnId::new();
    app.state.runtime.instance_id = Some(instance);
    app.state.runtime.speech_turn = Some(turn);
    app.state.runtime.last_sequence = 1;
    let context = egui::Context::default();
    let timing = |sequence, turn_id| RuntimeTiming {
        instance_id: instance,
        sequence,
        turn: Some(TurnTiming {
            turn_id,
            first_chunk_ms: Some(12),
            first_playback_request_ms: None,
        }),
    };
    events
        .send(WorkerEvent::RuntimeTiming {
            generation: 0,
            timing: timing(1, turn),
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(
        app.runtime_timing
            .visible(&app.state.runtime, true, false)
            .is_some()
    );
    events
        .send(WorkerEvent::Events(vec![SequencedEvent {
            sequence: 2,
            event: SystemEvent::SpeechCancelled,
        }]))
        .unwrap();
    events
        .send(WorkerEvent::RuntimeTiming {
            generation: 0,
            timing: timing(1, turn),
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(
        app.runtime_timing
            .visible(&app.state.runtime, true, false)
            .is_none()
    );
    let next = TurnId::new();
    events
        .send(WorkerEvent::Events(vec![SequencedEvent {
            sequence: 3,
            event: SystemEvent::TurnStarted {
                turn_id: next,
                user_text: String::new(),
            },
        }]))
        .unwrap();
    events
        .send(WorkerEvent::RuntimeTiming {
            generation: 0,
            timing: timing(3, next),
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(
        app.runtime_timing
            .visible(&app.state.runtime, true, false)
            .is_some()
    );
    events
        .send(WorkerEvent::VoiceSamplingGeneration(1))
        .unwrap();
    events
        .send(WorkerEvent::RuntimeTiming {
            generation: 0,
            timing: timing(3, next),
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(
        app.runtime_timing
            .visible(&app.state.runtime, true, false)
            .is_none()
    );
}

#[test]
fn native_input_status_rejects_old_instances_and_connections() {
    let (mut app, events, _sent) = app();
    app.native_mode = true;
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    app.state.runtime.instance_id = Some(first);
    let context = egui::Context::default();
    let has = |output: &egui::FullOutput, label: &str| {
        output.shapes.iter().any(|shape| {
            matches!(
                &shape.shape, egui::Shape::Text(text) if text.galley.job.text == label
            )
        })
    };
    events
        .send(WorkerEvent::NativeInputStatus {
            generation: 0,
            instance_id: first,
            status: NativeInputStatus::Ready,
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(has(
        &render(&mut app, &context, [1000.0, 720.0], vec![]),
        "语音输入已打开"
    ));
    events
        .send(WorkerEvent::VoiceSamplingGeneration(1))
        .unwrap();
    events
        .send(WorkerEvent::NativeInputStatus {
            generation: 0,
            instance_id: first,
            status: NativeInputStatus::Ready,
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert_eq!(app.native_input_status, NativeInputStatus::Unknown);
    events
        .send(WorkerEvent::NativeInputStatus {
            generation: 1,
            instance_id: second,
            status: NativeInputStatus::Unavailable,
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert_eq!(app.native_input_status, NativeInputStatus::Unknown);
    events
        .send(WorkerEvent::NativeInputStatus {
            generation: 1,
            instance_id: first,
            status: NativeInputStatus::Unavailable,
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(has(
        &render(&mut app, &context, [1000.0, 720.0], vec![]),
        "无法启用语音输入，请检查设备、权限和识别模型"
    ));
    events
        .send(WorkerEvent::NativeInputStatusUnavailable(1))
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(has(
        &render(&mut app, &context, [1000.0, 720.0], vec![]),
        "语音输入状态未知"
    ));
    events
        .send(WorkerEvent::NativeInputStatus {
            generation: 1,
            instance_id: first,
            status: NativeInputStatus::Ready,
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    events
        .send(WorkerEvent::HistoryGap(
            ai_ex_observability::RuntimeSnapshot {
                instance_id: Some(second),
                ..Default::default()
            },
        ))
        .unwrap();
    events
        .send(WorkerEvent::NativeInputStatus {
            generation: 1,
            instance_id: first,
            status: NativeInputStatus::Ready,
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert_eq!(app.native_input_status, NativeInputStatus::Unknown);
    events.send(WorkerEvent::Connection(false)).unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert_eq!(app.native_input_status, NativeInputStatus::Unknown);
}

#[test]
fn voice_input_indicator_uses_confirmed_fresh_activity_without_changing_dialogue() {
    let (mut app, events, _sent) = app();
    let instance = uuid::Uuid::new_v4();
    app.state.runtime.instance_id = Some(instance);
    let context = egui::Context::default();
    let has_indicator = |output: &egui::FullOutput| {
        output.shapes.iter().any(|shape| matches!(
        &shape.shape, egui::Shape::Text(text) if text.galley.job.text == "正在接收或识别语音"
    ))
    };
    let snapshot = ai_ex_observability::RuntimeSnapshot {
        instance_id: Some(instance),
        voice_input: ai_ex_domain::VoiceInputState {
            revision: 1,
            active: true,
        },
        ..Default::default()
    };
    events
        .send(WorkerEvent::VoiceInputSnapshot {
            generation: 0,
            snapshot: snapshot.clone(),
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(has_indicator(&render(
        &mut app,
        &context,
        [1000.0, 720.0],
        vec![]
    )));
    assert_eq!(
        app.state.runtime.state,
        ai_ex_domain::ConversationState::Idle
    );
    app.state.needs_resync = true;
    assert!(!has_indicator(&render(
        &mut app,
        &context,
        [1000.0, 720.0],
        vec![]
    )));
    app.state.needs_resync = false;
    events.send(WorkerEvent::VoiceInputUnavailable(0)).unwrap();
    assert!(!has_indicator(&render(
        &mut app,
        &context,
        [1000.0, 720.0],
        vec![]
    )));
    events
        .send(WorkerEvent::VoiceSamplingGeneration(1))
        .unwrap();
    events
        .send(WorkerEvent::VoiceInputSnapshot {
            generation: 0,
            snapshot: snapshot.clone(),
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(!app.voice_input_fresh);
    events
        .send(WorkerEvent::VoiceInputSnapshot {
            generation: 1,
            snapshot: snapshot.clone(),
        })
        .unwrap();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    assert!(app.voice_input_fresh);
    events.send(WorkerEvent::Connection(false)).unwrap();
    events
        .send(WorkerEvent::VoiceInputSnapshot {
            generation: 0,
            snapshot,
        })
        .unwrap();
    assert!(!has_indicator(&render(
        &mut app,
        &context,
        [1000.0, 720.0],
        vec![]
    )));
}

#[test]
fn close_request_preserves_draft_until_explicit_exit() {
    let (mut app, _events, _sent) = app();
    app.input = "尚未发送的内容".into();
    let context = egui::Context::default();
    let frame = |app: &mut DesktopApp, requested, events| {
        context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                app.handle_close_request(ui.ctx(), requested);
                app.show_close_confirmation(ui.ctx());
            },
        )
    };
    let output = frame(&mut app, true, vec![]);
    assert!(
        output
            .viewport_output
            .values()
            .any(|v| v.commands.contains(&egui::ViewportCommand::CancelClose))
    );
    let output = frame(&mut app, false, vec![]);
    let pos = label_position(&output, "返回继续编辑");
    for pressed in [true, false] {
        frame(
            &mut app,
            false,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    assert!(!app.confirm_close);
    assert_eq!(app.input, "尚未发送的内容");
    frame(&mut app, true, vec![]);
    let output = frame(&mut app, false, vec![]);
    let pos = label_position(&output, "仍要退出");
    let mut closed = false;
    for pressed in [true, false] {
        let output = frame(
            &mut app,
            false,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        closed |= output
            .viewport_output
            .values()
            .any(|v| v.commands.contains(&egui::ViewportCommand::Close));
    }
    assert!(closed && app.allow_close);
}

#[test]
fn close_request_handles_retained_messages_and_navigation_without_duplicate_dialogs() {
    let (mut app, _events, _sent) = app();
    let context = egui::Context::default();
    let output = context.run_ui(egui::RawInput::default(), |ui| {
        app.handle_close_request(ui.ctx(), true)
    });
    assert!(
        !output
            .viewport_output
            .values()
            .any(|v| v.commands.contains(&egui::ViewportCommand::CancelClose))
    );
    assert!(!app.confirm_close);
    app.retained_messages
        .push_back(navigation::RetainedMessage {
            text: "结果待核对".into(),
            state: navigation::DeliveryState::Uncertain,
        });
    app.request_navigation(&context, crate::navigation::Destination::Setup);
    assert!(app.pending_navigation.is_some());
    let output = context.run_ui(egui::RawInput::default(), |ui| {
        app.handle_close_request(ui.ctx(), true)
    });
    assert!(
        output
            .viewport_output
            .values()
            .any(|v| v.commands.contains(&egui::ViewportCommand::CancelClose))
    );
    assert!(app.confirm_close);
    assert!(app.pending_navigation.is_none());
    app.request_navigation(&context, crate::navigation::Destination::Home);
    assert!(app.pending_navigation.is_none());
    assert!(app.navigation.take().is_none());
    assert_eq!(app.retained_messages[0].text, "结果待核对");
}

#[test]
fn close_interception_clears_navigation_requested_before_a_new_draft() {
    let (mut app, _events, _sent) = app();
    let context = egui::Context::default();
    app.request_navigation(&context, crate::navigation::Destination::Setup);
    app.input = "异步出现的草稿".into();
    let output = context.run_ui(egui::RawInput::default(), |ui| {
        app.handle_close_request(ui.ctx(), true);
    });
    assert!(
        output
            .viewport_output
            .values()
            .any(|v| v.commands.contains(&egui::ViewportCommand::CancelClose))
    );
    assert!(app.confirm_close);
    assert!(app.navigation.take().is_none());
    assert_eq!(app.input, "异步出现的草稿");
}

#[test]
fn rejected_messages_restore_empty_drafts_and_preserve_newer_typing() {
    let (mut app, events, _sent) = app();
    events
        .send(WorkerEvent::SubmitRejected {
            text: "第一条".to_owned(),
            error: "queue full".to_owned(),
        })
        .unwrap();
    app.drain_events();
    assert_eq!(app.input, "第一条");
    app.input = "刚刚写的新文字".to_owned();
    events
        .send(WorkerEvent::SubmitRejected {
            text: "第二条".to_owned(),
            error: "interrupted before send".to_owned(),
        })
        .unwrap();
    app.drain_events();
    assert_eq!(app.input, "刚刚写的新文字");
    assert_eq!(
        app.retained_messages
            .front()
            .map(|message| message.text.as_str()),
        Some("第二条")
    );
}

#[test]
fn uncertain_delivery_keeps_original_text_without_automatically_retrying() {
    let (mut app, events, mut sent) = app();
    let text = "请先核对是否已经收到";
    events
        .send(WorkerEvent::SubmitUncertain {
            text: text.to_owned(),
            error: "request timed out after sending".to_owned(),
        })
        .unwrap();
    app.drain_events();
    assert!(app.input.is_empty());
    let retained = app.retained_messages.front().unwrap();
    assert_eq!(retained.text, text);
    assert!(retained.state == navigation::DeliveryState::Uncertain);
    assert!(sent.try_recv().is_err());
    app.show_retained = true;
    let context = egui::Context::default();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    let output = render(&mut app, &context, [1000.0, 720.0], vec![]);
    click(
        &mut app,
        &context,
        label_position(&output, "确认未收到，恢复草稿"),
    );
    assert_eq!(app.input, text);
    assert!(app.retained_messages.is_empty());
    assert!(
        sent.try_recv().is_err(),
        "restoring a draft must not send it"
    );
}

fn enter(modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn plain_enter_and_ime_confirmation_do_not_submit_but_ctrl_enter_does() {
    let (mut app, _events, mut sent) = app();
    let context = egui::Context::default();
    render(&mut app, &context, [1000.0, 720.0], vec![]);
    context.memory_mut(|memory| memory.request_focus(egui::Id::new(chat::COMPOSER_ID)));
    app.input = "first line".to_owned();
    render(
        &mut app,
        &context,
        [1000.0, 720.0],
        vec![enter(egui::Modifiers::NONE)],
    );
    assert!(sent.try_recv().is_err());
    assert!(app.input.contains('\n'));
    render(
        &mut app,
        &context,
        [1000.0, 720.0],
        vec![egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "ni".to_owned(),
            active_range_chars: None,
        })],
    );
    render(
        &mut app,
        &context,
        [1000.0, 720.0],
        vec![
            egui::Event::Ime(egui::ImeEvent::Commit("你".to_owned())),
            enter(egui::Modifiers::CTRL),
        ],
    );
    assert!(
        sent.try_recv().is_err(),
        "confirming composition must not submit"
    );
    assert!(!app.input.is_empty());
    render(
        &mut app,
        &context,
        [1000.0, 720.0],
        vec![enter(egui::Modifiers::CTRL)],
    );
    assert!(
        matches!(sent.try_recv().unwrap(), WorkerCommand::Submit(text) if text.contains("first line"))
    );
    assert!(app.input.is_empty());
}

#[test]
fn character_import_finishes_while_the_chat_page_stays_selected() {
    let (mut app, _events, _sent) = app();
    let context = egui::Context::default();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../config/characters/companion.toml");
    app.character_files
        .begin(&context, crate::character_files::FileAction::Import(path));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while app.character_files.is_loading() {
        render(&mut app, &context, [1000.0, 720.0], vec![]);
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(app.page, layout::Page::Conversation);
    assert_eq!(app.persona.profile_id, "aiex.companion");
    assert!(app.persona_dirty);
    assert_eq!(app.active_persona.profile_id, "default");
}

#[test]
fn changing_character_identity_clears_previous_chat_but_revisions_keep_it() {
    let (mut app, events, _sent) = app();
    app.state.turns.push(ai_ex_ui_model::UiTurn {
        turn_id: ai_ex_domain::TurnId::new(),
        user_text: "old conversation".to_owned(),
        assistant_text: "old reply".to_owned(),
        status: ai_ex_ui_model::TurnStatus::Completed,
    });
    let revised = PersonaSnapshot {
        revision: 2,
        ..app.active_persona.clone()
    };
    events
        .send(WorkerEvent::PersonaApplied(revised.clone()))
        .unwrap();
    app.drain_events();
    assert_eq!(app.state.turns.len(), 1);
    app.input = "draft survives".to_owned();
    events
        .send(WorkerEvent::PersonaApplied(PersonaSnapshot {
            profile_id: "a-different-character".to_owned(),
            ..revised
        }))
        .unwrap();
    app.drain_events();
    assert!(app.state.turns.is_empty());
    assert_eq!(app.input, "draft survives");
}

#[test]
#[ignore = "writes optional UI review images into target without opening windows"]
fn write_chat_layout_snapshots() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/desktop-ui-review-{stamp}"));
    std::fs::create_dir(&directory).unwrap();
    for (name, size) in [
        ("chat-wide.png", [1120, 720]),
        ("chat-narrow.png", [720, 600]),
    ] {
        let (mut app, _events, _sent) = app();
        app.active_persona.name = "澄".to_owned();
        app.input = "那我们先把今天的安排理一下吧。".to_owned();
        app.state.turns.push(ai_ex_ui_model::UiTurn {
            turn_id: ai_ex_domain::TurnId::new(), user_text: "今天学习了很久，想休息一下再继续。".to_owned(),
            assistant_text: "好啊，我陪你。先让肩膀放松一下，喝口水也好。\n等你想继续了，我们再一起看看剩下的安排。".to_owned(),
            status: ai_ex_ui_model::TurnStatus::Completed,
        });
        let context = egui::Context::default();
        configure_appearance(&context);
        let points = [size[0] as f32, size[1] as f32];
        let mut output = render(&mut app, &context, points, vec![]);
        output.append(render(&mut app, &context, points, vec![]));
        let path = directory.join(name);
        headless_snapshot::save(&context, output, size, &path).unwrap();
        println!("{}", path.display());
    }
}
