use ai_ex_domain::AppError;

use crate::coordinator::GuardedExecutionFailure;
use crate::reflex_control::{ReflexControl, ReflexInactive, ReflexLease};
use crate::reflex_session::{ReflexInterruption, ReflexSessionOutcome, ReflexSessionReport};
use crate::{
    ActionResult, AuditSink, AutomationAction, AutomationCoordinator, AutomationPort,
    ExecutionPhase, FrameGeometry, PixelRect, PointerButton, ReflexController,
    ReflexExecutionPhase, ReflexFailure, ReflexFailureStage, ReflexMode, ReflexOutcome,
    ReflexReceipt, ReflexState, ReflexStepReport, ScreenFrame,
};

/// Advance one offline timestamp through the ordinary, uncontrolled coordinator path.
/// Waiting and terminal reports consume no frame and issue no action.
#[allow(
    clippy::result_large_err,
    reason = "failure retains the partial execution report and audit receipts"
)]
pub async fn run_reflex_step<P: AutomationPort, A: AuditSink>(
    controller: &mut ReflexController,
    coordinator: &mut AutomationCoordinator<P, A>,
    geometry: &FrameGeometry,
    timestamp_ms: u64,
) -> Result<ReflexStepReport, ReflexFailure> {
    run_step(controller, coordinator, geometry, timestamp_ms, None)
        .await
        .map(|report| report.step)
}

#[allow(
    clippy::result_large_err,
    reason = "failure retains the partial execution report and audit receipts"
)]
pub(crate) async fn run_controlled_reflex_step<P: AutomationPort, A: AuditSink>(
    controller: &mut ReflexController,
    control: &ReflexControl,
    coordinator: &mut AutomationCoordinator<P, A>,
    geometry: &FrameGeometry,
    timestamp_ms: u64,
) -> Result<ReflexSessionReport, ReflexFailure> {
    if terminal(controller) {
        return Ok(advanced(controller.report(timestamp_ms)));
    }
    // Acquire exactly once. Pause/resume cannot replace this in-flight generation.
    let lease = match control.begin_step() {
        Ok(lease) => lease,
        Err(inactive) => {
            let report = controller.report(timestamp_ms);
            return Ok(interrupted(
                controller,
                report,
                ReflexInterruption {
                    inactive,
                    action_id: None,
                    rejection_audit_persisted: None,
                },
            ));
        }
    };
    let issued = lease.snapshot();
    let generation = (issued.session_id, issued.generation);
    if controller.control_generation != Some(generation) {
        // Control can change entirely between steps, without an observed interruption.
        // A first controlled step also discards any tracking inherited from raw use.
        clear_tracking(controller);
    }
    controller.control_generation = Some(generation);
    run_step(
        controller,
        coordinator,
        geometry,
        timestamp_ms,
        Some(&lease),
    )
    .await
}

/// Advance one offline timestamp. Every external action uses this same coordinator.
/// Waiting and terminal reports consume no frame and issue no action.
#[allow(
    clippy::result_large_err,
    reason = "failure retains the partial execution report and audit receipts"
)]
async fn run_step<P: AutomationPort, A: AuditSink>(
    controller: &mut ReflexController,
    coordinator: &mut AutomationCoordinator<P, A>,
    geometry: &FrameGeometry,
    timestamp_ms: u64,
    lease: Option<&ReflexLease>,
) -> Result<ReflexSessionReport, ReflexFailure> {
    let mut report = controller.report(timestamp_ms);
    if matches!(
        controller.state,
        ReflexState::Completed | ReflexState::BudgetExhausted | ReflexState::Failed
    ) {
        return Ok(advanced(report));
    }
    if controller
        .last_timestamp
        .is_some_and(|previous| timestamp_ms < previous)
    {
        return Err(controller.fail(
            &mut report,
            ReflexFailureStage::Timestamp,
            None,
            None,
            "reflex timestamps must be monotonic",
        ));
    }
    controller.last_timestamp = Some(timestamp_ms);
    if controller.budget_exhausted() {
        controller.exhaust_budget(&mut report);
        return Ok(advanced(report));
    }
    if controller
        .last_capture_timestamp
        .is_some_and(|previous| timestamp_ms - previous < controller.options.interval_ms)
    {
        report.outcome = ReflexOutcome::Waiting;
        return Ok(advanced(report));
    }
    let captured = match execute(
        controller,
        coordinator,
        &mut report,
        AutomationAction::CaptureScreen,
        ReflexFailureStage::Capture,
        lease,
    )
    .await?
    {
        ActionAdvance::Executed(result) => result,
        ActionAdvance::Interrupted(interruption) => {
            return Ok(interrupted(controller, report, interruption));
        }
    };
    let ActionResult::ScreenCaptured(frame) = captured else {
        return Err(controller.fail(
            &mut report,
            ReflexFailureStage::Capture,
            None,
            None,
            "capture action did not return a screen frame",
        ));
    };
    let frame = ScreenFrame::new(frame.width, frame.height, frame.rgba).map_err(|error| {
        controller.fail(
            &mut report,
            ReflexFailureStage::Capture,
            None,
            None,
            error.message,
        )
    })?;
    if let Some(interruption) = check_lease(lease) {
        return Ok(interrupted(controller, report, interruption));
    }
    let region = search_region(controller, &frame).map_err(|error| {
        controller.fail(
            &mut report,
            ReflexFailureStage::Match,
            None,
            None,
            error.message,
        )
    })?;
    report.search_region = Some(region);
    let matched = controller
        .matcher
        .find_match(&frame, Some(region))
        .map_err(|error| {
            controller.fail(
                &mut report,
                ReflexFailureStage::Match,
                None,
                None,
                error.message,
            )
        })?;
    report.score = Some(matched.score);
    report.matched_bounds = matched.bounds;
    if let Some(interruption) = check_lease(lease) {
        return Ok(interrupted(controller, report, interruption));
    }
    let Some(bounds) = matched.bounds else {
        controller.misses = controller.misses.saturating_add(1);
        if controller.tracking_region.is_some() {
            controller.tracking_misses += 1;
            if controller.tracking_misses >= 3 {
                controller.tracking_region = None;
                controller.tracking_misses = 0;
            }
        }
        controller.state = if controller.misses >= 10 {
            ReflexState::Lost
        } else if controller.tracking_region.is_some() {
            ReflexState::Tracking
        } else {
            ReflexState::Searching
        };
        report.outcome = ReflexOutcome::Missed;
        if controller.budget_exhausted() {
            controller.exhaust_budget(&mut report);
        }
        controller.sync_report(&mut report);
        return Ok(advanced(report));
    };
    controller.misses = 0;
    controller.tracking_misses = 0;
    controller.state = ReflexState::Tracking;
    let center = bounds.center();
    let tracking_x = center.x.saturating_sub(100);
    let tracking_y = center.y.saturating_sub(100);
    controller.tracking_region = Some(PixelRect {
        x: tracking_x,
        y: tracking_y,
        width: 200.min(frame.width - tracking_x),
        height: 200.min(frame.height - tracking_y),
    });
    let absolute = geometry.absolute(center).map_err(|error| {
        controller.fail(
            &mut report,
            ReflexFailureStage::Geometry,
            None,
            None,
            error.message,
        )
    })?;
    let normalized = geometry.normalize(center).map_err(|error| {
        controller.fail(
            &mut report,
            ReflexFailureStage::Geometry,
            None,
            None,
            error.message,
        )
    })?;
    report.absolute = Some(absolute);
    report.normalized = Some(normalized);
    if !normalized.0.is_finite()
        || !normalized.1.is_finite()
        || !(0.0..=1.0).contains(&normalized.0)
        || !(0.0..=1.0).contains(&normalized.1)
    {
        return Err(controller.fail(
            &mut report,
            ReflexFailureStage::Geometry,
            None,
            None,
            "normalized reflex coordinates are outside the pointer range",
        ));
    }
    let (x, y) = (normalized.0 as f32, normalized.1 as f32);
    if !x.is_finite() || !y.is_finite() {
        return Err(controller.fail(
            &mut report,
            ReflexFailureStage::Geometry,
            None,
            None,
            "normalized reflex coordinates cannot be represented as f32",
        ));
    }
    let moved = match execute(
        controller,
        coordinator,
        &mut report,
        AutomationAction::MovePointer { x, y },
        ReflexFailureStage::Move,
        lease,
    )
    .await?
    {
        ActionAdvance::Executed(result) => result,
        ActionAdvance::Interrupted(interruption) => {
            return Ok(interrupted(controller, report, interruption));
        }
    };
    if moved != ActionResult::Completed {
        return Err(controller.fail(
            &mut report,
            ReflexFailureStage::Move,
            None,
            None,
            "pointer move returned an unexpected result",
        ));
    }
    if let Some(interruption) = check_lease(lease) {
        return Ok(interrupted(controller, report, interruption));
    }
    if controller.options.mode != ReflexMode::Hover {
        if controller.actions >= controller.options.max_actions {
            controller.exhaust_budget(&mut report);
            return Ok(advanced(report));
        }
        let clicked = match execute(
            controller,
            coordinator,
            &mut report,
            AutomationAction::Click {
                button: PointerButton::Left,
            },
            ReflexFailureStage::Click,
            lease,
        )
        .await?
        {
            ActionAdvance::Executed(result) => result,
            ActionAdvance::Interrupted(interruption) => {
                return Ok(interrupted(controller, report, interruption));
            }
        };
        if clicked != ActionResult::Completed {
            return Err(controller.fail(
                &mut report,
                ReflexFailureStage::Click,
                None,
                None,
                "pointer click returned an unexpected result",
            ));
        }
    }
    // A successful, audited ClickOnce is final even if control changed during its audit.
    // Repeat clicks retain their execution receipts, then observe the original lease.
    if controller.options.mode == ReflexMode::ClickRepeat
        && let Some(interruption) = check_lease(lease)
    {
        return Ok(interrupted(controller, report, interruption));
    }
    if controller.options.mode == ReflexMode::ClickOnce {
        controller.state = ReflexState::Completed;
        report.outcome = ReflexOutcome::Completed;
    } else if controller.budget_exhausted() {
        controller.exhaust_budget(&mut report);
    } else {
        report.outcome = ReflexOutcome::Matched;
    }
    controller.sync_report(&mut report);
    Ok(advanced(report))
}

#[derive(Debug)]
enum ActionAdvance {
    Executed(ActionResult),
    Interrupted(ReflexInterruption),
}
/// A dropped action future cannot return a trustworthy receipt to its caller.
/// Conservatively retain possible dispatch and prevent a later step from retrying.
struct PendingExecution<'a> {
    controller: &'a mut ReflexController,
    report: &'a mut ReflexStepReport,
    is_capture: bool,
    dispatch_armed: bool,
    disarmed: bool,
}

impl PendingExecution<'_> {
    fn disarm(mut self) {
        self.disarmed = true;
    }
}

impl Drop for PendingExecution<'_> {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        if self.dispatch_armed {
            record_execution(self.controller, self.report, self.is_capture);
        }
        self.controller.state = ReflexState::Failed;
        clear_tracking(self.controller);
        self.report.outcome = ReflexOutcome::Inactive;
        self.controller.sync_report(self.report);
    }
}

#[allow(
    clippy::result_large_err,
    reason = "failure retains the partial execution report and audit receipts"
)]
async fn execute<P: AutomationPort, A: AuditSink>(
    controller: &mut ReflexController,
    coordinator: &mut AutomationCoordinator<P, A>,
    report: &mut ReflexStepReport,
    action: AutomationAction,
    stage: ReflexFailureStage,
    lease: Option<&ReflexLease>,
) -> Result<ActionAdvance, ReflexFailure> {
    let is_capture = action == AutomationAction::CaptureScreen;
    let target = controller.target.clone();
    let mut pending = PendingExecution {
        controller: &mut *controller,
        report: &mut *report,
        is_capture,
        dispatch_armed: false,
        disarmed: false,
    };
    let mut first_check = true;
    let result = coordinator
        .execute_guarded(
            target,
            "bounded template reflex step",
            action.clone(),
            || {
                if let Some(lease) = lease {
                    lease.ensure_current()?;
                }
                // The coordinator checks once before pre-audits, then once immediately
                // before adapter dispatch with no intervening await on success.
                if first_check {
                    first_check = false;
                } else {
                    pending.dispatch_armed = true;
                }
                Ok::<(), ReflexInactive>(())
            },
        )
        .await;
    // There is no await between disarming and the existing receipt/failure accounting.
    pending.disarm();
    let receipt = match result {
        Ok(receipt) => receipt,
        Err(GuardedExecutionFailure::Execution(failure)) => {
            if failure.phase == ExecutionPhase::DuringExecution {
                // An adapter error cannot prove the external action did not happen.
                record_execution(controller, report, is_capture);
            }
            let phase = match failure.phase {
                ExecutionPhase::BeforeExecution => ReflexExecutionPhase::BeforeExecution,
                ExecutionPhase::DuringExecution => ReflexExecutionPhase::DuringExecution,
            };
            return Err(controller.fail(
                report,
                stage,
                Some(phase),
                Some(failure.action_id),
                failure.error.message,
            ));
        }
        Err(GuardedExecutionFailure::GuardRejected {
            action_id,
            error,
            rejection_audit_persisted,
        }) => {
            if !rejection_audit_persisted {
                return Err(controller.fail(
                    report,
                    stage,
                    Some(ReflexExecutionPhase::BeforeExecution),
                    Some(action_id),
                    format!("control rejected action ({error}) but its rejection audit was not persisted; reflex stopped without dispatch"),
                ));
            }
            return Ok(ActionAdvance::Interrupted(ReflexInterruption {
                inactive: error,
                action_id: Some(action_id),
                rejection_audit_persisted: Some(true),
            }));
        }
    };
    record_execution(controller, report, is_capture);
    report.receipts.push(ReflexReceipt {
        action_id: receipt.action_id,
        action,
        completion_audit_persisted: receipt.completion_audit_persisted,
    });
    controller.sync_report(report);
    if !receipt.completion_audit_persisted {
        return Err(controller.fail(report, ReflexFailureStage::CompletionAudit, Some(ReflexExecutionPhase::DuringExecution), Some(receipt.action_id), "action executed but its completion audit was not persisted; reflex stopped without retry"));
    }
    Ok(ActionAdvance::Executed(receipt.result))
}

fn record_execution(
    controller: &mut ReflexController,
    report: &ReflexStepReport,
    is_capture: bool,
) {
    if is_capture {
        controller.frames += 1;
        // A rejected prepared capture must not consume the next capture interval.
        controller.last_capture_timestamp = Some(report.timestamp_ms);
    } else {
        controller.actions += 1;
    }
}

fn terminal(controller: &ReflexController) -> bool {
    matches!(
        controller.state,
        ReflexState::Completed | ReflexState::BudgetExhausted | ReflexState::Failed
    )
}

fn check_lease(lease: Option<&ReflexLease>) -> Option<ReflexInterruption> {
    lease
        .and_then(|lease| lease.ensure_current().err())
        .map(|inactive| ReflexInterruption {
            inactive,
            action_id: None,
            rejection_audit_persisted: None,
        })
}

fn advanced(step: ReflexStepReport) -> ReflexSessionReport {
    ReflexSessionReport {
        outcome: ReflexSessionOutcome::Advanced,
        step,
        interruption: None,
    }
}

fn clear_tracking(controller: &mut ReflexController) {
    controller.tracking_region = None;
    controller.tracking_misses = 0;
    if controller.state == ReflexState::Tracking {
        controller.state = ReflexState::Searching;
    }
}

fn interrupted(
    controller: &mut ReflexController,
    mut report: ReflexStepReport,
    interruption: ReflexInterruption,
) -> ReflexSessionReport {
    clear_tracking(controller);
    report.outcome = ReflexOutcome::Inactive;
    controller.sync_report(&mut report);
    ReflexSessionReport {
        outcome: ReflexSessionOutcome::Interrupted,
        step: report,
        interruption: Some(interruption),
    }
}

fn search_region(
    controller: &mut ReflexController,
    frame: &ScreenFrame,
) -> Result<PixelRect, AppError> {
    let requested = controller.options.search_region.unwrap_or(PixelRect {
        x: 0,
        y: 0,
        width: frame.width,
        height: frame.height,
    });
    if requested.x >= frame.width || requested.y >= frame.height {
        return Err(AppError::configuration(
            "reflex search region is outside the captured frame",
        ));
    }
    let base = PixelRect {
        width: requested.width.min(frame.width - requested.x),
        height: requested.height.min(frame.height - requested.y),
        ..requested
    };
    if let Some(tracking) = controller.tracking_region {
        let left = base.x.max(tracking.x);
        let top = base.y.max(tracking.y);
        let right = (base.x + base.width).min(tracking.x + tracking.width);
        let bottom = (base.y + base.height).min(tracking.y + tracking.height);
        if right > left && bottom > top {
            return Ok(PixelRect {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
            });
        }
        controller.tracking_region = None;
        controller.tracking_misses = 0;
    }
    Ok(base)
}
