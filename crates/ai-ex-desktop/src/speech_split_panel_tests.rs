use super::*;

fn begin(panel: &mut SpeechSplitPanel, request: SpeechSplitRequest) -> u64 {
    let WorkerCommand::SpeechSplit { request_id, .. } = panel.begin(request).unwrap() else {
        panic!("expected speech split request");
    };
    request_id
}

fn status(instance_id: Uuid, revision: u64, minimum: Option<usize>) -> SpeechSplitStatus {
    SpeechSplitStatus {
        instance_id,
        revision,
        minimum,
    }
}

#[test]
fn session_setting_is_confirmed_only_by_matching_service_response() {
    let instance = Uuid::new_v4();
    let mut panel = SpeechSplitPanel::default();
    panel.synchronize(Some(instance), true);
    let WorkerCommand::SpeechSplit {
        request_id,
        request,
    } = panel.poll().unwrap()
    else {
        panic!("expected query");
    };
    assert_eq!(request, SpeechSplitRequest::Query);
    panel.receive(request_id, Ok(status(instance, 0, None)));
    assert!(panel.fresh);
    assert!(!panel.draft_enabled);
    panel.draft_enabled = true;
    panel.draft_chars = 16;
    let request = SpeechSplitRequest::Set {
        instance_id: instance,
        expected_revision: 0,
        minimum: Some(16),
    };
    let id = begin(&mut panel, request);
    assert!(panel.has_unsaved());
    assert!(!panel.fresh);
    panel.receive(id, Ok(status(instance, 1, Some(16))));
    assert!(panel.fresh);
    assert!(!panel.has_unsaved());
    assert_eq!(panel.confirmed.as_ref().unwrap().minimum, Some(16));
}

#[test]
fn timeout_and_new_instance_keep_draft_and_require_explicit_reconciliation() {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut panel = SpeechSplitPanel::default();
    panel.synchronize(Some(first), true);
    let WorkerCommand::SpeechSplit { request_id, .. } = panel.poll().unwrap() else {
        unreachable!()
    };
    panel.receive(request_id, Ok(status(first, 3, None)));
    panel.draft_enabled = true;
    panel.draft_chars = 18;
    let id = begin(
        &mut panel,
        SpeechSplitRequest::Set {
            instance_id: first,
            expected_revision: 3,
            minimum: Some(18),
        },
    );
    panel.receive(id, Err(AppError::connectivity("synthetic timeout")));
    assert!(!panel.fresh && panel.has_unsaved());
    assert_eq!(panel.desired(), Some(18));
    assert!(panel.poll().is_none());
    let query = begin(&mut panel, SpeechSplitRequest::Query);
    panel.receive(query, Ok(status(first, 4, Some(20))));
    assert!(panel.fresh && panel.has_unsaved());
    assert_eq!(panel.desired(), Some(18));
    assert_ne!(panel.draft_base, Some((first, 4)));

    panel.synchronize(Some(second), true);
    let WorkerCommand::SpeechSplit { request_id, .. } = panel.poll().unwrap() else {
        unreachable!()
    };
    panel.receive(id, Ok(status(first, 4, Some(18))));
    panel.receive(request_id, Ok(status(second, 0, None)));
    assert_eq!(panel.desired(), Some(18));
    assert!(panel.has_unsaved());
    assert_eq!(panel.confirmed.as_ref().unwrap().instance_id, second);
    assert_ne!(panel.draft_base, Some((second, 0)));
}

#[test]
fn mismatched_or_stale_reply_is_not_confirmed_and_query_does_not_loop() {
    let instance = Uuid::new_v4();
    let mut panel = SpeechSplitPanel::default();
    panel.synchronize(Some(instance), true);
    let WorkerCommand::SpeechSplit { request_id, .. } = panel.poll().unwrap() else {
        unreachable!()
    };
    panel.receive(request_id, Ok(status(instance, 4, Some(12))));
    let query = begin(&mut panel, SpeechSplitRequest::Query);
    panel.receive(query, Ok(status(instance, 4, Some(24))));
    assert!(!panel.fresh);
    assert_eq!(panel.confirmed.as_ref().unwrap().minimum, Some(12));
    assert!(panel.poll().is_none());
    let query = begin(&mut panel, SpeechSplitRequest::Query);
    panel.receive(query, Ok(status(instance, 3, None)));
    assert!(!panel.fresh);
}

fn after_timed_out_set() -> (SpeechSplitPanel, Uuid) {
    let instance = Uuid::new_v4();
    let mut panel = SpeechSplitPanel::default();
    panel.synchronize(Some(instance), true);
    let WorkerCommand::SpeechSplit { request_id, .. } = panel.poll().unwrap() else {
        unreachable!()
    };
    panel.receive(request_id, Ok(status(instance, 0, None)));
    panel.draft_enabled = true;
    panel.draft_chars = 18;
    let id = begin(
        &mut panel,
        SpeechSplitRequest::Set {
            instance_id: instance,
            expected_revision: 0,
            minimum: Some(18),
        },
    );
    panel.receive(id, Err(AppError::connectivity("synthetic timeout")));
    (panel, instance)
}

#[test]
fn repeated_queries_do_not_clear_unknown_and_fence_uses_snapshot_not_draft() {
    let (mut panel, instance) = after_timed_out_set();
    for _ in 0..2 {
        let query = begin(&mut panel, SpeechSplitRequest::Query);
        panel.receive(query, Ok(status(instance, 0, None)));
        assert_eq!(panel.uncertain, Some((instance, 0)));
        assert_eq!(panel.desired(), Some(18));
    }
    let fence = begin(
        &mut panel,
        SpeechSplitRequest::Fence {
            instance_id: instance,
            expected_revision: 0,
            minimum: None,
        },
    );
    panel.receive(fence, Ok(status(instance, 1, None)));
    assert_eq!(panel.uncertain, None);
    assert_eq!(panel.confirmed.as_ref().unwrap().minimum, None);
    assert_eq!(panel.desired(), Some(18));
    assert_ne!(panel.draft_base, Some((instance, 1)));
    // A late success for the original request is not the pending fence reply.
    panel.receive(fence - 3, Ok(status(instance, 1, Some(18))));
    assert_eq!(panel.confirmed.as_ref().unwrap().minimum, None);
}

#[test]
fn old_set_winning_then_fence_conflict_requires_query_and_another_fence() {
    let (mut panel, instance) = after_timed_out_set();
    let query = begin(&mut panel, SpeechSplitRequest::Query);
    panel.receive(query, Ok(status(instance, 0, None)));
    let fence = begin(
        &mut panel,
        SpeechSplitRequest::Fence {
            instance_id: instance,
            expected_revision: 0,
            minimum: None,
        },
    );
    panel.receive(fence, Err(AppError::invalid_transition("old set won")));
    assert!(panel.uncertain.is_some());
    assert_eq!(panel.desired(), Some(18));
    let query = begin(&mut panel, SpeechSplitRequest::Query);
    panel.receive(query, Ok(status(instance, 1, Some(18))));
    assert!(panel.uncertain.is_some());
    assert_eq!(panel.desired(), Some(18));
    let fence = begin(
        &mut panel,
        SpeechSplitRequest::Fence {
            instance_id: instance,
            expected_revision: 1,
            minimum: Some(18),
        },
    );
    panel.receive(fence, Ok(status(instance, 2, Some(18))));
    assert_eq!(panel.uncertain, None);
    assert_eq!(panel.desired(), Some(18));
}

#[test]
fn timed_out_fence_remains_unknown_through_reconnect_and_preserves_draft() {
    let (mut panel, instance) = after_timed_out_set();
    let query = begin(&mut panel, SpeechSplitRequest::Query);
    panel.receive(query, Ok(status(instance, 0, None)));
    let fence = begin(
        &mut panel,
        SpeechSplitRequest::Fence {
            instance_id: instance,
            expected_revision: 0,
            minimum: None,
        },
    );
    panel.receive(fence, Err(AppError::connectivity("fence reply lost")));
    assert_eq!(panel.uncertain, Some((instance, 0)));
    panel.synchronize(None, false);
    panel.synchronize(Some(instance), true);
    let WorkerCommand::SpeechSplit { request_id, .. } = panel.poll().unwrap() else {
        unreachable!()
    };
    panel.receive(request_id, Ok(status(instance, 1, None)));
    assert_eq!(panel.uncertain, Some((instance, 0)));
    assert_eq!(panel.desired(), Some(18));
    let fence = begin(
        &mut panel,
        SpeechSplitRequest::Fence {
            instance_id: instance,
            expected_revision: 1,
            minimum: None,
        },
    );
    panel.receive(
        fence,
        Err(AppError::connectivity("second fence reply lost")),
    );
    assert_eq!(panel.uncertain, Some((instance, 1)));
    let query = begin(&mut panel, SpeechSplitRequest::Query);
    panel.receive(query, Ok(status(instance, 2, None)));
    assert_eq!(panel.uncertain, Some((instance, 1)));
    assert_eq!(panel.desired(), Some(18));
}

#[test]
fn new_instance_is_rechecked_and_never_revives_old_response() {
    let (mut panel, first) = after_timed_out_set();
    let second = Uuid::new_v4();
    panel.synchronize(None, false);
    assert_eq!(panel.uncertain, Some((first, 0)));
    panel.synchronize(Some(second), true);
    assert_eq!(panel.uncertain, None);
    let WorkerCommand::SpeechSplit { request_id, .. } = panel.poll().unwrap() else {
        unreachable!()
    };
    panel.receive(2, Ok(status(first, 1, Some(18))));
    panel.receive(request_id, Ok(status(second, 0, None)));
    assert_eq!(panel.confirmed.as_ref().unwrap().instance_id, second);
    assert_eq!(panel.desired(), Some(18));
    assert!(panel.has_unsaved());
    assert_ne!(panel.draft_base, Some((second, 0)));
}

#[test]
fn fence_requires_a_user_click_and_uses_the_read_snapshot() {
    let (mut panel, instance) = after_timed_out_set();
    let query = begin(&mut panel, SpeechSplitRequest::Query);
    panel.receive(query, Ok(status(instance, 0, None)));
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    let render = |panel: &mut SpeechSplitPanel, events| {
        let mut commands = Vec::new();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| commands = panel.show(ui, true),
        );
        (output, commands)
    };
    let click = |panel: &mut SpeechSplitPanel, label: &str| {
        let (output, commands) = render(panel, Vec::new());
        assert!(commands.is_empty());
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("visible control");
        let mut sent = Vec::new();
        for pressed in [true, false] {
            let (_, commands) = render(
                panel,
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
            sent.extend(commands);
        }
        sent
    };
    assert!(click(&mut panel, "朗读分段（本次会话）").is_empty());
    for _ in 0..20 {
        assert!(render(&mut panel, Vec::new()).1.is_empty());
    }
    let commands = click(&mut panel, "确认当前服务值");
    assert!(matches!(commands.as_slice(), [WorkerCommand::SpeechSplit {
        request: SpeechSplitRequest::Fence { instance_id, expected_revision: 0, minimum: None }, ..
    }] if *instance_id == instance));
    assert_eq!(panel.desired(), Some(18));
}
