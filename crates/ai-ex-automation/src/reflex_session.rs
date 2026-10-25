//! Owned control boundary for a reflex controller.
//!
//! Each step uses one immutable generation lease through its awaits. Pause/resume
//! cannot revive old work, and stop is permanent for the owned control session.
//! Already-issued adapter/native actions cannot be assumed cancelled; their partial
//! receipts and consumed budgets remain in a completed runner report. Dropping a
//! started step instead marks the controller failed and charges a possibly dispatched
//! action without fabricating a receipt. Use control.stop() and drain the step when
//! its complete report and completion audit are required.

use ai_ex_domain::AppError;
use serde::Serialize;
use uuid::Uuid;

use crate::reflex_control::{ReflexControl, ReflexControlSnapshot, ReflexInactive};
use crate::reflex_runner::run_controlled_reflex_step;
use crate::{
    AuditSink, AutomationCoordinator, AutomationPort, FrameGeometry, PixelRect, ReflexController,
    ReflexFailure, ReflexOptions, ReflexState, ReflexStepReport,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexSessionOutcome {
    /// An ordinary runner report, including waiting or terminal inactivity.
    Advanced,
    /// Control interrupted the step; its partial execution report is retained.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReflexInterruption {
    pub inactive: ReflexInactive,
    /// A prepared action rejected before adapter dispatch, when applicable.
    pub action_id: Option<Uuid>,
    /// None if no rejection audit was attempted; otherwise its persistence result.
    pub rejection_audit_persisted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReflexSessionReport {
    pub outcome: ReflexSessionOutcome,
    /// Includes all completed or possibly executed work, without budget rollback.
    pub step: ReflexStepReport,
    /// Present exactly when the session outcome is Interrupted.
    pub interruption: Option<ReflexInterruption>,
}

/// Owns its controller and control handle without exposing a mutable bypass.
///
/// `control` returns clones of this session's handle. A stopped session requires a
/// new session and controller. Target generation invalidation is available through
/// the handle; target and matcher remain fixed in this wrapper, while geometry is
/// supplied at each step boundary. Invalidation does not replace the target.
pub struct ReflexSession {
    controller: ReflexController,
    control: ReflexControl,
    fixed_region: Option<PixelRect>,
}

impl ReflexSession {
    pub fn new(controller: ReflexController) -> Self {
        let fixed_region = controller.options().search_region;
        Self {
            controller,
            control: ReflexControl::new(),
            fixed_region,
        }
    }

    pub fn control(&self) -> ReflexControl {
        self.control.clone()
    }

    /// Execution progress; pause/stop mode is available from the control snapshot.
    pub fn state(&self) -> ReflexState {
        self.controller.state()
    }

    pub fn options(&self) -> &ReflexOptions {
        self.controller.options()
    }

    /// Install a frame-local focus only for this exact running generation.
    ///
    /// Some is clipped to the immutable search region supplied at construction;
    /// None restores that original region, including an originally unrestricted
    /// frame. Invalid/empty regions fail before mutation. A stale, paused, stopped,
    /// or terminal session returns None without changing controller or control.
    /// A successful update consumes its ticket, discards tracking, and preserves
    /// misses, budgets, capture cadence and monotonic timestamps. It never resumes
    /// control or changes the target, matcher, or coordinate geometry.
    pub fn refocus_if_current(
        &mut self,
        expected: ReflexControlSnapshot,
        region: Option<PixelRect>,
    ) -> Result<Option<ReflexControlSnapshot>, AppError> {
        let region = bounded_focus(region, self.fixed_region)?;
        if matches!(
            self.controller.state(),
            ReflexState::Completed | ReflexState::BudgetExhausted | ReflexState::Failed
        ) {
            return Ok(None);
        }
        // Exclusive session access keeps the terminal check stable. The control
        // mutex covers the provenance check, generation advance and installation
        // together, so independent pause/stop commands cannot split that commit.
        let controller = &mut self.controller;
        self.control
            .advance_running_if_current(expected, |snapshot| {
                controller.options.search_region = region;
                controller.tracking_region = None;
                controller.tracking_misses = 0;
                if controller.state == ReflexState::Tracking {
                    controller.state = ReflexState::Searching;
                }
                controller.control_generation = Some((snapshot.session_id, snapshot.generation));
            })
    }

    /// Advance with the session's original control allocation.
    ///
    /// A control interruption retains partial receipts/counters and discards stale
    /// tracking/frame reuse. A resumed step must capture a fresh frame. Adapter and
    /// audit failures retain the ordinary runner's failure semantics; an audited,
    /// confirmed ClickOnce completion must not be replayed after a late pause.
    #[allow(
        clippy::result_large_err,
        reason = "failure retains the partial execution report and audit receipts"
    )]
    pub async fn step<P: AutomationPort, A: AuditSink>(
        &mut self,
        coordinator: &mut AutomationCoordinator<P, A>,
        geometry: &FrameGeometry,
        timestamp_ms: u64,
    ) -> Result<ReflexSessionReport, ReflexFailure> {
        run_controlled_reflex_step(
            &mut self.controller,
            &self.control,
            coordinator,
            geometry,
            timestamp_ms,
        )
        .await
    }
}

fn bounded_focus(
    requested: Option<PixelRect>,
    fixed: Option<PixelRect>,
) -> Result<Option<PixelRect>, AppError> {
    let Some(requested) = requested else {
        return Ok(fixed);
    };
    crate::reflex::validate_region(requested)?;
    let Some(fixed) = fixed else {
        return Ok(Some(requested));
    };
    // Both rectangles were validated before their checked exclusive ends are used.
    let right = requested
        .x
        .checked_add(requested.width)
        .zip(fixed.x.checked_add(fixed.width))
        .map(|(requested, fixed)| requested.min(fixed))
        .ok_or_else(|| AppError::configuration("reflex focus bounds overflow"))?;
    let bottom = requested
        .y
        .checked_add(requested.height)
        .zip(fixed.y.checked_add(fixed.height))
        .map(|(requested, fixed)| requested.min(fixed))
        .ok_or_else(|| AppError::configuration("reflex focus bounds overflow"))?;
    let x = requested.x.max(fixed.x);
    let y = requested.y.max(fixed.y);
    if right <= x || bottom <= y {
        return Err(AppError::configuration(
            "reflex focus does not intersect the original search region",
        ));
    }
    Ok(Some(PixelRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }))
}
