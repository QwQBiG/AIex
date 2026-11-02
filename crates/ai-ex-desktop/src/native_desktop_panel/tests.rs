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
