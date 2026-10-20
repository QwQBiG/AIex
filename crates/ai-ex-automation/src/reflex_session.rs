//! Owned control boundary for a reflex controller.
//!
//! Each step uses one immutable generation lease through its awaits. Pause/resume
//! cannot revive old work, and stop is permanent for the owned control session.
//! Already-issued adapter/native actions cannot be assumed cancelled; their partial
//! receipts and consumed budgets remain in a completed runner report. Dropping a
//! started step instead marks the controller failed and charges a possibly dispatched
//! action without fabricating a receipt. Use control.stop() and drain the step when
//! its complete report and completion audit are required.

use serde::Serialize;
use uuid::Uuid;

use crate::reflex_control::{ReflexControl, ReflexInactive};
use crate::reflex_runner::run_controlled_reflex_step;
use crate::{
    AuditSink, AutomationCoordinator, AutomationPort, FrameGeometry, ReflexController,
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
}

impl ReflexSession {
    pub fn new(controller: ReflexController) -> Self {
        Self {
            controller,
            control: ReflexControl::new(),
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
