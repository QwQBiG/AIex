use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use ai_ex_automation::{
    ActionResult, AuditRecord, AuditSink, AuditStage, AutomationAction, AutomationCoordinator,
    AutomationPort, DesktopBounds, DryRunAutomationPort, FrameGeometry, MatchOptions, PixelRect,
    PointerButton, ReflexController, ReflexExecutionPhase, ReflexFailureStage, ReflexMode,
    ReflexOptions, ReflexOutcome, ReflexState, ScreenFrame, TemplateMatcher, run_reflex_step,
};
use ai_ex_domain::AppError;
use ai_ex_safety::{ActionRequest, Capability, Permit, SafetyGate, SafetyPolicy};
use async_trait::async_trait;

fn template() -> ScreenFrame {
    let mut rgba = Vec::with_capacity(20 * 20 * 4);
    for y in 0..20_u32 {
        for x in 0..20_u32 {
            let gray = ((x * 37 + y * 61 + x * y * 13) % 251 + 1) as u8;
            rgba.extend_from_slice(&[gray, gray, gray, 255]);
        }
    }
    ScreenFrame::new(20, 20, rgba).unwrap()
}

fn frame(width: u32, height: u32, location: Option<(u32, u32)>) -> ScreenFrame {
    let mut rgba = vec![0; (width * height * 4) as usize];
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    if let Some((left, top)) = location {
        let template = template();
        assert!(left + template.width <= width && top + template.height <= height);
        for y in 0..template.height {
            let from = (y * template.width * 4) as usize;
            let to = ((top + y) * width * 4 + left * 4) as usize;
            rgba[to..to + template.width as usize * 4]
                .copy_from_slice(&template.rgba[from..from + template.width as usize * 4]);
        }
    }
    ScreenFrame::new(width, height, rgba).unwrap()
}

fn matched_frame() -> ScreenFrame {
    frame(60, 60, Some((7, 9)))
}

fn controller(options: ReflexOptions) -> ReflexController {
    let matcher = TemplateMatcher::new(
        &template(),
        MatchOptions {
            threshold: 0.999_999,
            ..MatchOptions::default()
        },
    )
    .unwrap();
    ReflexController::new("game".to_owned(), matcher, options).unwrap()
}

fn geometry() -> FrameGeometry {
    FrameGeometry::new(
        -100,
        50,
        1.5,
        2.0,
        DesktopBounds::new(-200, 0, 1000, 600).unwrap(),
    )
    .unwrap()
}

fn gate() -> Arc<SafetyGate> {
    Arc::new(SafetyGate::new(SafetyPolicy {
        automation_enabled: true,
        allowed_capabilities: BTreeSet::from([Capability::ScreenRead, Capability::MouseInput]),
        allowed_targets: vec!["game".to_owned()],
    }))
}

struct Port {
    inner: DryRunAutomationPort,
    actions: Arc<Mutex<Vec<AutomationAction>>>,
    fail: Option<&'static str>,
    stop_after_move: Option<Arc<SafetyGate>>,
}

#[async_trait]
impl AutomationPort for Port {
    async fn execute(
        &mut self,
        permit: &Permit,
        action: &AutomationAction,
    ) -> Result<ActionResult, AppError> {
        permit.ensure_active()?;
        self.actions.lock().unwrap().push(action.clone());
        let label = action.audit_label();
        if self.fail.is_some_and(|prefix| label.starts_with(prefix)) {
            return Err(AppError::unavailable(
                "injected adapter failure after an action may have executed",
            ));
        }
        let result = self.inner.execute(permit, action).await?;
        if matches!(action, AutomationAction::MovePointer { .. })
            && let Some(gate) = &self.stop_after_move
        {
            gate.trigger_emergency_stop();
        }
        Ok(result)
    }
}

struct Audit {
    records: Arc<Mutex<Vec<AuditRecord>>>,
    fail_completion: Option<&'static str>,
}

#[async_trait]
impl AuditSink for Audit {
    async fn record(&mut self, record: AuditRecord) -> Result<(), AppError> {
        let fail = record.stage == AuditStage::Completed
            && self
                .fail_completion
                .is_some_and(|prefix| record.action.starts_with(prefix));
        self.records.lock().unwrap().push(record);
        if fail {
            Err(AppError::unavailable("injected audit persistence failure"))
        } else {
            Ok(())
        }
    }
}

struct Harness {
    coordinator: AutomationCoordinator<Port, Audit>,
    actions: Arc<Mutex<Vec<AutomationAction>>>,
    records: Arc<Mutex<Vec<AuditRecord>>>,
}

fn harness(
    frames: Vec<ScreenFrame>,
    fail: Option<&'static str>,
    stop_after_move: bool,
    fail_completion: Option<&'static str>,
) -> Harness {
    let gate = gate();
    let actions = Arc::new(Mutex::new(Vec::new()));
    let records = Arc::new(Mutex::new(Vec::new()));
    let coordinator = AutomationCoordinator::new(
        Arc::clone(&gate),
        Port {
            inner: DryRunAutomationPort::with_frames(128, frames).unwrap(),
            actions: Arc::clone(&actions),
            fail,
            stop_after_move: stop_after_move.then_some(gate),
        },
        Audit {
            records: Arc::clone(&records),
            fail_completion,
        },
    );
    Harness {
        coordinator,
        actions,
        records,
    }
}

fn options(mode: ReflexMode) -> ReflexOptions {
    ReflexOptions {
        mode,
        ..ReflexOptions::default()
    }
}

#[tokio::test]
async fn click_once_completes_only_after_three_audited_actions_and_never_captures_again() {
    let mut harness = harness(vec![matched_frame()], None, false, None);
    let mut controller = controller(options(ReflexMode::ClickOnce));
    let report = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 100)
        .await
        .unwrap();
    assert_eq!(report.state, ReflexState::Completed);
    assert_eq!(report.outcome, ReflexOutcome::Completed);
    assert_eq!((report.frames, report.actions, report.misses), (1, 2, 0));
    assert_eq!(
        report.matched_bounds,
        Some(PixelRect {
            x: 7,
            y: 9,
            width: 20,
            height: 20
        })
    );
    assert_eq!(report.absolute, Some((-74.5, 88.0)));
    let normalized = report.normalized.unwrap();
    assert!((normalized.0 - 125.5 / 999.0).abs() < 1e-12);
    assert!((normalized.1 - 88.0 / 599.0).abs() < 1e-12);
    assert_eq!(report.receipts.len(), 3);
    assert!(
        report
            .receipts
            .iter()
            .all(|receipt| receipt.completion_audit_persisted)
    );
    let actions = harness.actions.lock().unwrap().clone();
    assert_eq!(actions[0], AutomationAction::CaptureScreen);
    let AutomationAction::MovePointer { x, y } = actions[1] else {
        panic!("second action must move")
    };
    assert!(
        (f64::from(x) - normalized.0).abs() < 1e-6 && (f64::from(y) - normalized.1).abs() < 1e-6
    );
    assert_eq!(
        actions[2],
        AutomationAction::Click {
            button: PointerButton::Left
        }
    );
    let audit = harness.records.lock().unwrap().clone();
    assert_eq!(audit.len(), 9);
    for (receipt, records) in report.receipts.iter().zip(audit.chunks_exact(3)) {
        assert!(
            records
                .iter()
                .all(|record| record.action_id == receipt.action_id)
        );
        assert_eq!(
            records
                .iter()
                .map(|record| record.stage)
                .collect::<Vec<_>>(),
            vec![
                AuditStage::Requested,
                AuditStage::Authorized,
                AuditStage::Completed
            ]
        );
    }
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("rgba") && !json.contains("ScreenCaptured"));
    let inactive = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 200)
        .await
        .unwrap();
    assert_eq!(inactive.outcome, ReflexOutcome::Inactive);
    assert!(inactive.receipts.is_empty());
    assert_eq!(harness.actions.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn repeat_interval_uses_last_capture_and_waiting_consumes_no_frame() {
    let mut harness = harness(
        vec![matched_frame(), frame(60, 60, Some((20, 20)))],
        None,
        false,
        None,
    );
    let mut controller = controller(options(ReflexMode::ClickRepeat));
    let first = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 0)
        .await
        .unwrap();
    assert_eq!(first.outcome, ReflexOutcome::Matched);
    for timestamp in [10, 49] {
        let waiting = run_reflex_step(
            &mut controller,
            &mut harness.coordinator,
            &geometry(),
            timestamp,
        )
        .await
        .unwrap();
        assert_eq!(waiting.outcome, ReflexOutcome::Waiting);
        assert_eq!((waiting.frames, waiting.actions), (1, 2));
        assert!(waiting.receipts.is_empty());
    }
    let second = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!((second.frames, second.actions), (2, 4));
    assert_eq!(second.matched_bounds.unwrap().x, 20);
    assert_eq!(harness.actions.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn hover_moves_without_click_and_respects_frame_budget() {
    let mut harness = harness(vec![matched_frame(), matched_frame()], None, false, None);
    let mut controller = controller(ReflexOptions {
        max_frames: 2,
        ..options(ReflexMode::Hover)
    });
    run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 0)
        .await
        .unwrap();
    let report = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!(report.state, ReflexState::BudgetExhausted);
    assert_eq!((report.frames, report.actions), (2, 2));
    assert!(
        harness
            .actions
            .lock()
            .unwrap()
            .iter()
            .all(|action| !matches!(action, AutomationAction::Click { .. }))
    );
    run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 100)
        .await
        .unwrap();
    assert_eq!(harness.actions.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn move_and_click_adapter_failures_stop_without_completion_or_retry() {
    for (label, stage, count) in [
        ("move_pointer", ReflexFailureStage::Move, 2),
        ("click", ReflexFailureStage::Click, 3),
    ] {
        let mut harness = harness(
            vec![matched_frame(), matched_frame()],
            Some(label),
            false,
            None,
        );
        let mut controller = controller(options(ReflexMode::ClickOnce));
        let failure = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 0)
            .await
            .unwrap_err();
        assert_eq!(failure.stage, stage);
        assert_eq!(
            failure.execution_phase,
            Some(ReflexExecutionPhase::DuringExecution)
        );
        assert!(failure.action_id.is_some());
        assert_eq!(failure.report.state, ReflexState::Failed);
        assert_eq!(failure.report.receipts.len(), count - 1);
        assert_eq!(failure.report.actions, count as u64 - 1);
        assert_eq!(harness.actions.lock().unwrap().len(), count);
        let later = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 50)
            .await
            .unwrap();
        assert_eq!(later.state, ReflexState::Failed);
        assert_eq!(later.outcome, ReflexOutcome::Inactive);
        assert_eq!(harness.actions.lock().unwrap().len(), count);
    }
}

#[tokio::test]
async fn emergency_stop_after_move_prevents_the_click_at_the_same_coordinator() {
    let mut harness = harness(vec![matched_frame()], None, true, None);
    let mut controller = controller(options(ReflexMode::ClickOnce));
    let failure = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 0)
        .await
        .unwrap_err();
    assert_eq!(failure.stage, ReflexFailureStage::Click);
    assert_eq!(
        failure.execution_phase,
        Some(ReflexExecutionPhase::BeforeExecution)
    );
    assert_eq!(failure.report.state, ReflexState::Failed);
    assert_eq!((failure.report.frames, failure.report.actions), (1, 1));
    assert_eq!(failure.report.receipts.len(), 2);
    assert_eq!(harness.actions.lock().unwrap().len(), 2);
    assert!(
        harness
            .actions
            .lock()
            .unwrap()
            .iter()
            .all(|action| !matches!(action, AutomationAction::Click { .. }))
    );
    assert_eq!(
        harness.records.lock().unwrap().last().unwrap().stage,
        AuditStage::Rejected
    );
}

#[tokio::test]
async fn missing_completion_audit_at_any_stage_prevents_subsequent_work() {
    for (label, count) in [("capture_screen", 1), ("move_pointer", 2), ("click", 3)] {
        let mut harness = harness(vec![matched_frame()], None, false, Some(label));
        let mut controller = controller(options(ReflexMode::ClickOnce));
        let failure = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 0)
            .await
            .unwrap_err();
        assert_eq!(failure.stage, ReflexFailureStage::CompletionAudit);
        assert_eq!(failure.report.state, ReflexState::Failed);
        assert_eq!(failure.report.receipts.len(), count);
        assert!(
            !failure
                .report
                .receipts
                .last()
                .unwrap()
                .completion_audit_persisted
        );
        assert_eq!(harness.actions.lock().unwrap().len(), count);
        run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 50)
            .await
            .unwrap();
        assert_eq!(harness.actions.lock().unwrap().len(), count);
        assert!(
            serde_json::to_value(&failure).unwrap()["report"]
                .get("rgba")
                .is_none()
        );
    }
}

#[tokio::test]
async fn pointer_budget_stops_between_move_and_click_without_fake_completion() {
    let mut harness = harness(vec![matched_frame(), matched_frame()], None, false, None);
    let mut controller = controller(ReflexOptions {
        max_actions: 1,
        ..options(ReflexMode::ClickOnce)
    });
    let report = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 0)
        .await
        .unwrap();
    assert_eq!(report.state, ReflexState::BudgetExhausted);
    assert_eq!((report.frames, report.actions), (1, 1));
    assert_eq!(report.receipts.len(), 2);
    assert_eq!(harness.actions.lock().unwrap().len(), 2);
    run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!(harness.actions.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn three_tracking_misses_reacquire_within_the_original_fixed_search_region() {
    let fixed = PixelRect {
        x: 20,
        y: 5,
        width: 320,
        height: 60,
    };
    let mut frames = vec![frame(360, 80, Some((30, 15)))];
    frames.extend((0..4).map(|_| frame(360, 80, Some((300, 15)))));
    let mut harness = harness(frames, None, false, None);
    let mut controller = controller(ReflexOptions {
        search_region: Some(fixed),
        ..options(ReflexMode::Hover)
    });
    run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 0)
        .await
        .unwrap();
    for miss in 1..=3 {
        let report = run_reflex_step(
            &mut controller,
            &mut harness.coordinator,
            &geometry(),
            miss * 50,
        )
        .await
        .unwrap();
        assert_eq!(report.outcome, ReflexOutcome::Missed);
        assert_eq!(report.misses, miss);
        assert_eq!(report.receipts.len(), 1);
        let searched = report.search_region.unwrap();
        assert!(searched.x >= fixed.x && searched.y >= fixed.y);
        assert!(searched.x + searched.width <= fixed.x + fixed.width);
        assert!(searched.y + searched.height <= fixed.y + fixed.height);
        assert!(searched.width <= 200 && searched.height <= 200);
        if miss == 3 {
            assert_eq!(report.state, ReflexState::Searching);
        }
    }
    let recovered = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 200)
        .await
        .unwrap();
    assert_eq!(recovered.search_region, Some(fixed));
    assert_eq!(recovered.matched_bounds.unwrap().x, 300);
    assert_eq!(recovered.misses, 0);
    assert_eq!(recovered.state, ReflexState::Tracking);
    assert_eq!((recovered.frames, recovered.actions), (5, 2));
}

#[tokio::test]
async fn ten_misses_report_lost_without_pointer_actions_and_the_next_frame_can_recover() {
    let mut frames = (0..10).map(|_| frame(60, 60, None)).collect::<Vec<_>>();
    frames.push(matched_frame());
    let mut harness = harness(frames, None, false, None);
    let mut controller = controller(options(ReflexMode::ClickRepeat));
    for miss in 1..=10 {
        let report = run_reflex_step(
            &mut controller,
            &mut harness.coordinator,
            &geometry(),
            (miss - 1) * 50,
        )
        .await
        .unwrap();
        assert_eq!(report.misses, miss);
        assert_eq!(report.actions, 0);
        assert_eq!(report.receipts.len(), 1);
        if miss == 10 {
            assert_eq!(report.state, ReflexState::Lost);
        }
    }
    assert!(
        harness
            .actions
            .lock()
            .unwrap()
            .iter()
            .all(|action| action == &AutomationAction::CaptureScreen)
    );
    let recovered = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 500)
        .await
        .unwrap();
    assert_eq!(recovered.state, ReflexState::Tracking);
    assert_eq!(
        (recovered.misses, recovered.frames, recovered.actions),
        (0, 11, 2)
    );
}

#[tokio::test]
async fn regressing_timestamps_stop_before_capture_and_do_not_retry() {
    let mut harness = harness(vec![matched_frame(), matched_frame()], None, false, None);
    let mut controller = controller(options(ReflexMode::Hover));
    run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 100)
        .await
        .unwrap();
    run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 120)
        .await
        .unwrap();
    let failure = run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 119)
        .await
        .unwrap_err();
    assert_eq!(failure.stage, ReflexFailureStage::Timestamp);
    assert_eq!(failure.report.state, ReflexState::Failed);
    assert_eq!(harness.actions.lock().unwrap().len(), 2);
    run_reflex_step(&mut controller, &mut harness.coordinator, &geometry(), 200)
        .await
        .unwrap();
    assert_eq!(harness.actions.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn frame_sequences_move_owned_pixels_preserve_queue_checks_and_fail_when_exhausted() {
    let gate = gate();
    let permit = gate
        .authorize(ActionRequest {
            capability: Capability::ScreenRead,
            target: "game".to_owned(),
            rationale: "owned frame test".to_owned(),
        })
        .unwrap();
    let first = matched_frame();
    let first_pointer = first.rgba.as_ptr();
    let second = frame(60, 60, Some((20, 20)));
    let expected_second = second.clone();
    let mut port = DryRunAutomationPort::with_frames(1, vec![first, second]).unwrap();
    let ActionResult::ScreenCaptured(captured) = port
        .execute(&permit, &AutomationAction::CaptureScreen)
        .await
        .unwrap()
    else {
        panic!("frame expected")
    };
    assert_eq!(
        captured.rgba.as_ptr(),
        first_pointer,
        "sequence capture must move the owned pixel buffer"
    );
    assert!(
        port.execute(&permit, &AutomationAction::CaptureScreen)
            .await
            .unwrap_err()
            .message
            .contains("queue is full")
    );
    port.clear();
    assert_eq!(
        port.execute(&permit, &AutomationAction::CaptureScreen)
            .await
            .unwrap(),
        ActionResult::ScreenCaptured(expected_second)
    );
    port.clear();
    assert!(
        port.execute(&permit, &AutomationAction::CaptureScreen)
            .await
            .unwrap_err()
            .message
            .contains("exhausted")
    );
    assert!(port.is_empty());
    let repeated = matched_frame();
    let mut repeat = DryRunAutomationPort::with_screen(2, repeated.clone()).unwrap();
    for _ in 0..2 {
        assert_eq!(
            repeat
                .execute(&permit, &AutomationAction::CaptureScreen)
                .await
                .unwrap(),
            ActionResult::ScreenCaptured(repeated.clone())
        );
    }
    assert!(
        DryRunAutomationPort::with_frames(
            2,
            vec![ScreenFrame {
                width: 1,
                height: 1,
                rgba: vec![]
            }]
        )
        .is_err()
    );
    assert!(DryRunAutomationPort::with_frames(0, vec![]).is_err());
}

#[tokio::test]
async fn invalid_options_and_geometry_cannot_produce_pointer_actions() {
    for invalid in [
        ReflexOptions {
            interval_ms: 0,
            ..ReflexOptions::default()
        },
        ReflexOptions {
            max_frames: 0,
            ..ReflexOptions::default()
        },
        ReflexOptions {
            max_actions: 0,
            ..ReflexOptions::default()
        },
        ReflexOptions {
            search_region: Some(PixelRect {
                x: u32::MAX,
                y: 0,
                width: 2,
                height: 1,
            }),
            ..ReflexOptions::default()
        },
    ] {
        let matcher = TemplateMatcher::new(&template(), MatchOptions::default()).unwrap();
        assert!(ReflexController::new("game".to_owned(), matcher, invalid).is_err());
    }
    let mut harness = harness(vec![matched_frame()], None, false, None);
    let mut controller = controller(options(ReflexMode::ClickOnce));
    let mut invalid = geometry();
    invalid.scale_x = f64::NAN;
    let failure = run_reflex_step(&mut controller, &mut harness.coordinator, &invalid, 0)
        .await
        .unwrap_err();
    assert_eq!(failure.stage, ReflexFailureStage::Geometry);
    assert_eq!(failure.report.state, ReflexState::Failed);
    assert_eq!(
        harness.actions.lock().unwrap().as_slice(),
        &[AutomationAction::CaptureScreen]
    );
}
