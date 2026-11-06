use super::*;

fn reply(id: Uuid, revision: u64) -> NativeDesktopReply {
    NativeDesktopReply {
        snapshot: NativeDesktopSnapshot {
            instance_id: id,
            revision,
            execution_mode: "native".into(),
            available: true,
            session: None,
        },
        windows: Vec::new(),
        vision_preview: None,
    }
}

#[test]
fn native_start_is_explicit_bounded_and_separate_from_template_permissions() {
    let id = Uuid::new_v4();
    let mut panel = NativeDesktopPanel::default();
    panel.synchronize(Some(id), true);
    assert!(panel.poll().is_none());
    assert!(panel.start_request().is_none());
    let WorkerCommand::NativeDesktop { request_id, .. } =
        panel.begin(NativeDesktopRequest::Status).unwrap()
    else {
        unreachable!()
    };
    panel.receive(request_id, Ok(reply(id, 3)));
    panel.windows = vec![NativeWindow {
        handle: 1,
        process_id: 2,
        process_started_ticks: 3,
        title: "fixture".into(),
        scope: "fixture.exe".into(),
        left: -1280,
        top: 0,
        width: 800,
        height: 600,
        dpi: 144,
        foreground: false,
    }];
    panel.selection = Some(0);
    panel.max_frames = u32::MAX;
    panel.max_actions = u32::MAX;
    let NativeDesktopRequest::StartNative {
        max_frames,
        max_actions,
        expected_revision,
        window,
        ..
    } = panel.start_request().unwrap()
    else {
        unreachable!()
    };
    assert_eq!((max_frames, max_actions, expected_revision), (256, 4096, 3));
    assert_eq!(window.left, -1280);
    panel.synchronize(Some(id), false);
    assert!(panel.start_request().is_none());
    let context = egui::Context::default();
    let output = context.run_ui(Default::default(), |ui| {
        assert!(panel.show(ui).is_empty());
    });
    assert!(!output.shapes.is_empty());
}

#[test]
fn manual_action_drafts_round_trip_and_only_render_without_injecting_input() {
    use ai_ex_domain::{NativeDesktopAction as Action, NativePointerButton as Button};
    let window = NativeWindow {
        handle: 1,
        process_id: 2,
        process_started_ticks: 3,
        title: "dedicated fixture".into(),
        scope: "fixture.exe".into(),
        left: -1280,
        top: -200,
        width: 800,
        height: 600,
        dpi: 144,
        foreground: false,
    };
    let centered = action::ActionDraft::centered(&window);
    assert_eq!(
        centered.action(),
        Some(Action::Click {
            x: -880,
            y: 100,
            button: Button::Left
        })
    );
    let actions = [
        Action::Click {
            x: -800,
            y: 0,
            button: Button::Right,
        },
        Action::TypeText {
            text: "专用窗口测试".into(),
        },
        Action::KeyChord {
            keys: vec!["Ctrl".into(), "A".into()],
        },
        Action::Drag {
            from_x: -1000,
            from_y: 0,
            to_x: -800,
            to_y: 100,
            button: Button::Left,
            steps: 24,
            duration_ms: 500,
        },
        Action::Scroll {
            horizontal: true,
            delta: -120,
        },
        Action::Wait { milliseconds: 200 },
        Action::Noop,
    ];
    let context = egui::Context::default();
    for expected in actions {
        let mut draft = action::ActionDraft::from_action(&expected);
        assert_eq!(draft.action(), Some(expected.clone()));
        let output = context.run_ui(Default::default(), |ui| draft.show(ui));
        assert!(!output.shapes.is_empty());
        assert_eq!(draft.action(), Some(expected));
    }
    assert!(
        action::ActionDraft::from_action(&Action::TypeText {
            text: String::new()
        })
        .action()
        .is_none()
    );
    assert!(
        action::ActionDraft::from_action(&Action::Scroll {
            horizontal: false,
            delta: 0
        })
        .action()
        .is_none()
    );
}

pub(crate) fn running_preview_panel() -> (NativeDesktopPanel, NativeDesktopSnapshot) {
    use ai_ex_domain::{NativeDesktopPhase, NativeDesktopSession};
    let instance = Uuid::new_v4();
    let session = NativeDesktopSession {
        session_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        window: NativeWindow {
            handle: 1,
            process_id: 2,
            process_started_ticks: 3,
            title: "合成窗口".into(),
            scope: "fixture.exe".into(),
            left: 0,
            top: 0,
            width: 800,
            height: 600,
            dpi: 96,
            foreground: true,
        },
        geometry_revision: 1,
        phase: NativeDesktopPhase::Running,
        frames: 0,
        actions: 0,
        max_frames: 10,
        max_actions: 10,
        frame_id: None,
        last_operation: None,
        last_outcome: None,
        goal: "合成目标".into(),
        armed_action: None,
    };
    let snapshot = NativeDesktopSnapshot {
        instance_id: instance,
        revision: 1,
        execution_mode: "native".into(),
        available: true,
        session: Some(session),
    };
    let mut panel = NativeDesktopPanel::default();
    panel.synchronize(Some(instance), true);
    panel.confirmed = Some(snapshot.clone());
    panel.fresh = true;
    panel.manual_mode = false;
    panel.goal = "合成目标".into();
    (panel, snapshot)
}

pub(crate) fn arm_expired_for_app_test(panel: &mut NativeDesktopPanel) {
    panel.arm_preview();
    panel.preview_arm.as_mut().unwrap().send_at = Instant::now();
}

pub(crate) fn preview_armed_for_app_test(panel: &NativeDesktopPanel) -> bool {
    panel.preview_arm.is_some()
}

#[test]
fn preview_requires_explicit_delay_and_discards_cancelled_results() {
    use ai_ex_domain::{NativeDesktopAction, NativeVisionPreview};
    let (mut panel, mut snapshot) = running_preview_panel();
    panel.arm_preview();
    assert!(panel.preview_arm.is_some());
    panel.cancel_preview();
    assert!(panel.preview_arm.is_none());
    assert!(panel.poll().is_none());
    panel.arm_preview();
    panel.preview_arm.as_mut().unwrap().send_at = Instant::now();
    let WorkerCommand::NativeDesktopPreview {
        request_id,
        request,
        generation,
        guard,
        ..
    } = panel.poll().unwrap()
    else {
        panic!("expected delayed preview")
    };
    let NativeDesktopRequest::PreviewVision { operation_id, .. } = request else {
        panic!("preview only")
    };
    panel.cancel_preview();
    assert_ne!(guard.load(std::sync::atomic::Ordering::Acquire), generation);
    snapshot.revision = 3;
    let stale = NativeDesktopReply {
        snapshot: snapshot.clone(),
        windows: vec![],
        vision_preview: Some(NativeVisionPreview {
            operation_id,
            action: NativeDesktopAction::Noop,
        }),
    };
    panel.receive(request_id, Ok(stale));
    assert!(panel.preview.is_none());
    assert_eq!(panel.confirmed.as_ref().unwrap().revision, 1);

    panel.arm_preview();
    panel.preview_arm.as_mut().unwrap().send_at = Instant::now();
    let WorkerCommand::NativeDesktopPreview {
        request_id,
        request,
        ..
    } = panel.poll().unwrap()
    else {
        panic!("preview")
    };
    let NativeDesktopRequest::PreviewVision { operation_id, .. } = request else {
        panic!("preview")
    };
    let text = "私人测试内容";
    panel.receive(
        request_id,
        Ok(NativeDesktopReply {
            snapshot: snapshot.clone(),
            windows: vec![],
            vision_preview: Some(NativeVisionPreview {
                operation_id,
                action: NativeDesktopAction::TypeText { text: text.into() },
            }),
        }),
    );
    let shown = panel.preview.as_ref().unwrap();
    assert!(!preview::summary(&shown.value.action).contains(text));
    assert_eq!(shown.revision, 3);
    let WorkerCommand::NativeDesktop { request_id, .. } =
        panel.begin(NativeDesktopRequest::Status).unwrap()
    else {
        panic!("status")
    };
    snapshot.revision += 1;
    panel.receive(
        request_id,
        Ok(NativeDesktopReply {
            snapshot,
            windows: vec![],
            vision_preview: None,
        }),
    );
    assert!(panel.preview.is_none());
}

#[test]
fn preview_is_cancelled_when_state_or_connection_changes() {
    let (mut panel, mut snapshot) = running_preview_panel();
    panel.arm_preview();
    let WorkerCommand::NativeDesktop { request_id, .. } =
        panel.begin(NativeDesktopRequest::Status).unwrap()
    else {
        panic!("status")
    };
    snapshot.revision += 1;
    panel.receive(
        request_id,
        Ok(NativeDesktopReply {
            snapshot,
            windows: vec![],
            vision_preview: None,
        }),
    );
    assert!(panel.preview_arm.is_none());
    panel.arm_preview();
    assert!(panel.preview_arm.is_some());
    panel.synchronize(panel.instance, false);
    assert!(panel.preview_arm.is_none());
}

#[test]
fn delayed_poll_never_sends_an_expired_preview() {
    let (mut panel, _) = running_preview_panel();
    panel.arm_preview();
    panel.preview_arm.as_mut().unwrap().send_at = Instant::now() - Duration::from_secs(3);
    assert!(panel.poll().is_none());
    assert!(panel.preview_arm.is_none());
    assert!(panel.pending.is_empty());
    assert!(panel.preview_message.unwrap().contains("未发送"));
}
