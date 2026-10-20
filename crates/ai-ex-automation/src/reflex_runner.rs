use ai_ex_domain::AppError;

use crate::{
    ActionResult, AuditSink, AutomationAction, AutomationCoordinator, AutomationPort,
    ExecutionPhase, FrameGeometry, PixelRect, PointerButton, ReflexController,
    ReflexExecutionPhase, ReflexFailure, ReflexFailureStage, ReflexMode, ReflexOutcome,
    ReflexReceipt, ReflexState, ReflexStepReport, ScreenFrame,
};

/// Advance one offline timestamp. Every external action uses this same coordinator.
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
    let mut report = controller.report(timestamp_ms);
    if matches!(
        controller.state,
        ReflexState::Completed | ReflexState::BudgetExhausted | ReflexState::Failed
    ) {
        return Ok(report);
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
        return Ok(report);
    }
    if controller
        .last_capture_timestamp
        .is_some_and(|previous| timestamp_ms - previous < controller.options.interval_ms)
    {
        report.outcome = ReflexOutcome::Waiting;
        return Ok(report);
    }
    controller.last_capture_timestamp = Some(timestamp_ms);
    let captured = execute(
        controller,
        coordinator,
        &mut report,
        AutomationAction::CaptureScreen,
        ReflexFailureStage::Capture,
    )
    .await?;
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
        return Ok(report);
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
    let moved = execute(
        controller,
        coordinator,
        &mut report,
        AutomationAction::MovePointer { x, y },
        ReflexFailureStage::Move,
    )
    .await?;
    if moved != ActionResult::Completed {
        return Err(controller.fail(
            &mut report,
            ReflexFailureStage::Move,
            None,
            None,
            "pointer move returned an unexpected result",
        ));
    }
    if controller.options.mode != ReflexMode::Hover {
        if controller.actions >= controller.options.max_actions {
            controller.exhaust_budget(&mut report);
            return Ok(report);
        }
        let clicked = execute(
            controller,
            coordinator,
            &mut report,
            AutomationAction::Click {
                button: PointerButton::Left,
            },
            ReflexFailureStage::Click,
        )
        .await?;
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
    if controller.options.mode == ReflexMode::ClickOnce {
        controller.state = ReflexState::Completed;
        report.outcome = ReflexOutcome::Completed;
    } else if controller.budget_exhausted() {
        controller.exhaust_budget(&mut report);
    } else {
        report.outcome = ReflexOutcome::Matched;
    }
    controller.sync_report(&mut report);
    Ok(report)
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
) -> Result<ActionResult, ReflexFailure> {
    let is_capture = action == AutomationAction::CaptureScreen;
    let target = controller.target.clone();
    let receipt = match coordinator
        .execute(target, "bounded template reflex step", action.clone())
        .await
    {
        Ok(receipt) => receipt,
        Err(failure) => {
            if failure.phase == ExecutionPhase::DuringExecution {
                // An adapter error cannot prove the external action did not happen.
                if is_capture {
                    controller.frames += 1;
                } else {
                    controller.actions += 1;
                }
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
    };
    if is_capture {
        controller.frames += 1;
    } else {
        controller.actions += 1;
    }
    report.receipts.push(ReflexReceipt {
        action_id: receipt.action_id,
        action,
        completion_audit_persisted: receipt.completion_audit_persisted,
    });
    controller.sync_report(report);
    if !receipt.completion_audit_persisted {
        return Err(controller.fail(report, ReflexFailureStage::CompletionAudit, Some(ReflexExecutionPhase::DuringExecution), Some(receipt.action_id), "action executed but its completion audit was not persisted; reflex stopped without retry"));
    }
    Ok(receipt.result)
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
