use super::*;
use ai_ex_domain::{NativeDesktopAction, NativeVisionOperation, NativeVisionPreparation};

fn response(
    snapshot: NativeDesktopSnapshot,
    preparation: Option<NativeVisionPreparation>,
    proposal: Option<NativeVisionProposal>,
    operation: Option<NativeVisionOperation>,
) -> NativeDesktopReply {
    NativeDesktopReply {
        snapshot,
        windows: vec![],
        vision_preview: None,
        vision_preparation: preparation,
        vision_proposal: proposal,
        vision_operation: operation,
    }
}

fn ready_proposal() -> (NativeDesktopPanel, NativeDesktopSnapshot, Uuid) {
    let (mut panel, mut snapshot) = crate::native_desktop_panel::tests::running_preview_panel();
    panel.arm_proposal_preparation();
    panel.proposal.arm.as_mut().unwrap().send_at = Instant::now();
    let WorkerCommand::NativeDesktopGuarded {
        request_id,
        request,
        ..
    } = panel.poll().unwrap()
    else {
        panic!("prepare")
    };
    let NativeDesktopRequest::PrepareVisionProposal { operation_id, .. } = request else {
        panic!("prepare only")
    };
    snapshot.revision = 4;
    panel.receive(
        request_id,
        Ok(response(
            snapshot.clone(),
            Some(NativeVisionPreparation {
                proposal_id: operation_id,
                state: NativeVisionPreparationState::Ready,
            }),
            Some(NativeVisionProposal {
                proposal_id: operation_id,
                action: NativeDesktopAction::TypeText {
                    text: "合成\u{202e}测试".into(),
                },
                remaining_ms: 120_000,
            }),
            None,
        )),
    );
    assert!(panel.proposal.review.is_some());
    (panel, snapshot, operation_id)
}

#[test]
fn preparing_and_confirming_are_explicit_bounded_and_never_include_action() {
    let (mut panel, snapshot, proposal_id) = ready_proposal();
    assert!(panel.proposal.operation.is_none());
    panel.arm_proposal_confirmation();
    panel.cancel_proposal_countdown();
    assert!(panel.proposal.operation.is_none());
    assert!(panel.proposal.review.is_some());
    panel.arm_proposal_confirmation();
    panel.proposal.arm.as_mut().unwrap().send_at = Instant::now();
    let WorkerCommand::NativeDesktopGuarded {
        request_id,
        request,
        ..
    } = panel.poll().unwrap()
    else {
        panic!("confirm")
    };
    let NativeDesktopRequest::ConfirmVisionProposal {
        instance_id,
        session_id,
        proposal_id: actual,
        operation_id,
        expected_revision,
    } = request
    else {
        panic!("confirmation only")
    };
    assert_eq!(actual, proposal_id);
    assert_ne!(proposal_id, operation_id);
    assert_eq!(instance_id, snapshot.instance_id);
    assert_eq!(session_id, snapshot.session.as_ref().unwrap().session_id);
    assert_eq!(expected_revision, snapshot.revision);
    assert!(panel.proposal.review.is_none());
    assert_eq!(
        panel.proposal.operation.as_ref().unwrap().state,
        NativeVisionOperationState::Unknown
    );
    panel.receive(
        request_id,
        Err(AppError::connectivity("synthetic lost reply")),
    );
    assert!(panel.proposal_operation_unresolved());
    assert!(
        matches!(panel.query_operation(), Some(NativeDesktopRequest::GetVisionOperation { operation_id: id, .. }) if id == operation_id)
    );
    assert!(panel.proposal.arm.is_none());
}

#[test]
fn stale_preparation_replies_cannot_revive_or_hide_ready_result() {
    let (mut panel, mut snapshot) = crate::native_desktop_panel::tests::running_preview_panel();
    panel.arm_proposal_preparation();
    panel.proposal.arm.as_mut().unwrap().send_at = Instant::now();
    let WorkerCommand::NativeDesktopGuarded {
        request_id,
        request,
        ..
    } = panel.poll().unwrap()
    else {
        panic!("prepare")
    };
    let NativeDesktopRequest::PrepareVisionProposal { operation_id, .. } = request else {
        panic!("prepare")
    };
    let WorkerCommand::NativeDesktop {
        request_id: query_id,
        ..
    } = panel.begin(panel.query_preparation().unwrap()).unwrap()
    else {
        panic!("query")
    };
    snapshot.revision = 3;
    panel.receive(
        request_id,
        Ok(response(
            snapshot,
            Some(NativeVisionPreparation {
                proposal_id: operation_id,
                state: NativeVisionPreparationState::Ready,
            }),
            Some(NativeVisionProposal {
                proposal_id: operation_id,
                action: NativeDesktopAction::Noop,
                remaining_ms: 120_000,
            }),
            None,
        )),
    );
    panel.receive(query_id, Err(AppError::connectivity("late query failure")));
    assert!(panel.proposal.review.is_some());
    assert!(panel.proposal.message.unwrap().contains("完整审阅"));
}

#[test]
fn operation_unknown_after_pending_rejects_late_pending_but_accepts_final_results() {
    for terminal in [
        NativeVisionOperationState::Completed,
        NativeVisionOperationState::Rejected,
    ] {
        let (mut panel, snapshot, proposal_id) = ready_proposal();
        panel.arm_proposal_confirmation();
        panel.proposal.arm.as_mut().unwrap().send_at = Instant::now();
        let WorkerCommand::NativeDesktopGuarded {
            request_id,
            request,
            ..
        } = panel.poll().unwrap()
        else {
            panic!("confirm")
        };
        let NativeDesktopRequest::ConfirmVisionProposal { operation_id, .. } = request else {
            panic!("confirmation only")
        };
        panel.receive(
            request_id,
            Err(AppError::connectivity("synthetic lost reply")),
        );
        let query = panel.query_operation().unwrap();
        let reply = |state| {
            response(
                snapshot.clone(),
                None,
                None,
                Some(NativeVisionOperation {
                    operation_id,
                    proposal_id,
                    state,
                }),
            )
        };
        panel.receive_proposal(
            10,
            query.clone(),
            Ok(reply(NativeVisionOperationState::Pending)),
        );
        assert_eq!(
            panel.proposal.operation.as_ref().unwrap().state,
            NativeVisionOperationState::Pending
        );
        panel.receive_proposal(
            11,
            query.clone(),
            Ok(reply(NativeVisionOperationState::Unknown)),
        );
        assert_eq!(
            panel.proposal.operation.as_ref().unwrap().state,
            NativeVisionOperationState::Unknown
        );
        assert!(panel.proposal.message.unwrap().contains("结果未知"));
        panel.receive_proposal(
            12,
            query.clone(),
            Ok(reply(NativeVisionOperationState::Pending)),
        );
        assert_eq!(
            panel.proposal.operation.as_ref().unwrap().state,
            NativeVisionOperationState::Unknown
        );
        panel.receive_proposal(13, query.clone(), Ok(reply(terminal)));
        assert_eq!(panel.proposal.operation.as_ref().unwrap().state, terminal);
        let terminal_message = panel.proposal.message;
        panel.receive_proposal(
            14,
            query.clone(),
            Ok(reply(NativeVisionOperationState::Unknown)),
        );
        panel.receive_proposal(15, query, Err(AppError::connectivity("late query failure")));
        assert_eq!(panel.proposal.operation.as_ref().unwrap().state, terminal);
        assert_eq!(panel.proposal.message, terminal_message);
    }
}

#[test]
fn query_error_keeps_operation_unknown_until_a_terminal_reply() {
    let (mut panel, snapshot, proposal_id) = ready_proposal();
    panel.arm_proposal_confirmation();
    panel.proposal.arm.as_mut().unwrap().send_at = Instant::now();
    let WorkerCommand::NativeDesktopGuarded {
        request_id,
        request,
        ..
    } = panel.poll().unwrap()
    else {
        panic!("confirm")
    };
    let NativeDesktopRequest::ConfirmVisionProposal { operation_id, .. } = request else {
        panic!("confirmation only")
    };
    let query = panel.query_operation().unwrap();
    let reply = |state| {
        response(
            snapshot.clone(),
            None,
            None,
            Some(NativeVisionOperation {
                operation_id,
                proposal_id,
                state,
            }),
        )
    };
    panel.receive(request_id, Ok(reply(NativeVisionOperationState::Pending)));
    panel.receive_proposal(
        10,
        query.clone(),
        Err(AppError::connectivity("synthetic lost query")),
    );
    assert_eq!(
        panel.proposal.operation.as_ref().unwrap().state,
        NativeVisionOperationState::Unknown
    );
    assert!(panel.proposal.message.unwrap().contains("结果未知"));
    panel.receive_proposal(
        11,
        query.clone(),
        Ok(reply(NativeVisionOperationState::Pending)),
    );
    assert_eq!(
        panel.proposal.operation.as_ref().unwrap().state,
        NativeVisionOperationState::Unknown
    );
    panel.receive_proposal(12, query, Ok(reply(NativeVisionOperationState::Completed)));
    assert_eq!(
        panel.proposal.operation.as_ref().unwrap().state,
        NativeVisionOperationState::Completed
    );
}

#[test]
fn closing_or_disconnecting_keeps_the_operation_number_and_marks_pending_unknown() {
    let (mut panel, snapshot, proposal_id) = ready_proposal();
    panel.arm_proposal_confirmation();
    panel.proposal.arm.as_mut().unwrap().send_at = Instant::now();
    let WorkerCommand::NativeDesktopGuarded {
        request_id,
        request,
        ..
    } = panel.poll().unwrap()
    else {
        panic!("confirm")
    };
    let NativeDesktopRequest::ConfirmVisionProposal { operation_id, .. } = request else {
        panic!("confirmation only")
    };
    panel.receive(
        request_id,
        Ok(response(
            snapshot.clone(),
            None,
            None,
            Some(NativeVisionOperation {
                operation_id,
                proposal_id,
                state: NativeVisionOperationState::Pending,
            }),
        )),
    );
    panel.cancel_preview();
    let operation = panel.proposal.operation.as_ref().unwrap();
    assert_eq!(operation.id, operation_id);
    assert_eq!(operation.proposal_id, proposal_id);
    assert_eq!(operation.state, NativeVisionOperationState::Unknown);
    assert!(panel.proposal_operation_unresolved());
    panel.synchronize(None, false);
    assert!(panel.query_operation().is_none());
    panel.synchronize(Some(snapshot.instance_id), true);
    assert!(
        matches!(panel.query_operation(), Some(NativeDesktopRequest::GetVisionOperation {
        operation_id: id, ..
    }) if id == operation_id)
    );
    assert_eq!(
        panel.proposal.operation.as_ref().unwrap().state,
        NativeVisionOperationState::Unknown
    );
}

#[test]
fn late_errors_from_an_older_operation_do_not_change_the_current_one() {
    let (mut panel, snapshot, proposal_id) = ready_proposal();
    panel.arm_proposal_confirmation();
    panel.proposal.arm.as_mut().unwrap().send_at = Instant::now();
    let WorkerCommand::NativeDesktopGuarded {
        request_id,
        request,
        ..
    } = panel.poll().unwrap()
    else {
        panic!("confirm")
    };
    let NativeDesktopRequest::ConfirmVisionProposal { operation_id, .. } = request else {
        panic!("confirmation only")
    };
    panel.receive(
        request_id,
        Ok(response(
            snapshot.clone(),
            None,
            None,
            Some(NativeVisionOperation {
                operation_id,
                proposal_id,
                state: NativeVisionOperationState::Pending,
            }),
        )),
    );
    let stale_query = NativeDesktopRequest::GetVisionOperation {
        instance_id: snapshot.instance_id,
        session_id: snapshot.session.as_ref().unwrap().session_id,
        proposal_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
    };
    let message = panel.proposal.message;
    panel.receive_proposal(
        90,
        stale_query,
        Err(AppError::connectivity("late query failure")),
    );
    assert_eq!(
        panel.proposal.operation.as_ref().unwrap().state,
        NativeVisionOperationState::Pending
    );
    assert_eq!(panel.proposal.message, message);
}

#[test]
fn scroll_review_explains_the_pointer_position() {
    let context = egui::Context::default();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        },
        |ui| {
            view::action_review(
                ui,
                &NativeDesktopAction::Scroll {
                    horizontal: false,
                    delta: 120,
                },
            )
        },
    );
    assert!(output.shapes.iter().any(|shape| matches!(
        &shape.shape,
        egui::Shape::Text(text) if text.galley.job.text == "滚动作用于执行时鼠标所在位置，请将鼠标移到目标区域。"
    )));
}
