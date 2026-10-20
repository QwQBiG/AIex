use ai_ex_domain::AppError;
use serde::Serialize;
use uuid::Uuid;

use crate::{AutomationAction, PixelRect, TemplateMatcher};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexMode {
    ClickOnce,
    ClickRepeat,
    Hover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReflexOptions {
    pub mode: ReflexMode,
    pub interval_ms: u64,
    pub max_frames: u64,
    /// Pointer moves and clicks; capture has the separate frame budget.
    pub max_actions: u64,
    pub search_region: Option<PixelRect>,
}

impl Default for ReflexOptions {
    fn default() -> Self {
        Self {
            mode: ReflexMode::ClickOnce,
            interval_ms: 50,
            max_frames: 300,
            max_actions: 600,
            search_region: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexState {
    Searching,
    Tracking,
    Lost,
    Completed,
    BudgetExhausted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexOutcome {
    Waiting,
    Matched,
    Missed,
    Completed,
    BudgetExhausted,
    Inactive,
}

/// Successful execution receipts for this step, without captured RGBA bytes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReflexReceipt {
    pub action_id: Uuid,
    pub action: AutomationAction,
    pub completion_audit_persisted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReflexStepReport {
    pub state: ReflexState,
    pub outcome: ReflexOutcome,
    pub timestamp_ms: u64,
    pub matched_bounds: Option<PixelRect>,
    pub score: Option<f64>,
    /// The effective, frame-local region searched by this step.
    pub search_region: Option<PixelRect>,
    pub misses: u64,
    /// Captures executed or possibly executed before an adapter failure.
    pub frames: u64,
    /// Pointer actions executed or possibly executed before an adapter failure.
    pub actions: u64,
    pub absolute: Option<(f64, f64)>,
    pub normalized: Option<(f64, f64)>,
    pub receipts: Vec<ReflexReceipt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexFailureStage {
    Timestamp,
    Capture,
    Match,
    Geometry,
    Move,
    Click,
    CompletionAudit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexExecutionPhase {
    BeforeExecution,
    DuringExecution,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReflexFailure {
    pub stage: ReflexFailureStage,
    pub execution_phase: Option<ReflexExecutionPhase>,
    pub action_id: Option<Uuid>,
    pub message: String,
    pub report: ReflexStepReport,
}

impl std::fmt::Display for ReflexFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "reflex {:?}: {}", self.stage, self.message)
    }
}

impl std::error::Error for ReflexFailure {}

pub struct ReflexController {
    pub(crate) target: String,
    pub(crate) matcher: TemplateMatcher,
    pub(crate) options: ReflexOptions,
    pub(crate) state: ReflexState,
    pub(crate) misses: u64,
    pub(crate) frames: u64,
    pub(crate) actions: u64,
    pub(crate) tracking_region: Option<PixelRect>,
    pub(crate) tracking_misses: u8,
    pub(crate) last_timestamp: Option<u64>,
    pub(crate) last_capture_timestamp: Option<u64>,
    pub(crate) control_generation: Option<(Uuid, u64)>,
}

impl ReflexController {
    pub fn new(
        target: String,
        matcher: TemplateMatcher,
        options: ReflexOptions,
    ) -> Result<Self, AppError> {
        if target.trim().is_empty() || target.len() > 512 || target.chars().any(char::is_control) {
            return Err(AppError::configuration("reflex target is invalid"));
        }
        if options.interval_ms == 0 || options.max_frames == 0 || options.max_actions == 0 {
            return Err(AppError::configuration(
                "reflex interval and budgets must be positive",
            ));
        }
        if let Some(region) = options.search_region {
            validate_region(region)?;
        }
        Ok(Self {
            target,
            matcher,
            options,
            state: ReflexState::Searching,
            misses: 0,
            frames: 0,
            actions: 0,
            tracking_region: None,
            tracking_misses: 0,
            last_timestamp: None,
            last_capture_timestamp: None,
            control_generation: None,
        })
    }

    pub fn state(&self) -> ReflexState {
        self.state
    }

    pub fn options(&self) -> &ReflexOptions {
        &self.options
    }

    pub(crate) fn report(&self, timestamp_ms: u64) -> ReflexStepReport {
        ReflexStepReport {
            state: self.state,
            outcome: ReflexOutcome::Inactive,
            timestamp_ms,
            matched_bounds: None,
            score: None,
            search_region: None,
            misses: self.misses,
            frames: self.frames,
            actions: self.actions,
            absolute: None,
            normalized: None,
            receipts: Vec::new(),
        }
    }

    pub(crate) fn sync_report(&self, report: &mut ReflexStepReport) {
        report.state = self.state;
        report.misses = self.misses;
        report.frames = self.frames;
        report.actions = self.actions;
    }

    pub(crate) fn budget_exhausted(&self) -> bool {
        self.frames >= self.options.max_frames || self.actions >= self.options.max_actions
    }

    pub(crate) fn exhaust_budget(&mut self, report: &mut ReflexStepReport) {
        self.state = ReflexState::BudgetExhausted;
        report.outcome = ReflexOutcome::BudgetExhausted;
        self.sync_report(report);
    }

    pub(crate) fn fail(
        &mut self,
        report: &mut ReflexStepReport,
        stage: ReflexFailureStage,
        execution_phase: Option<ReflexExecutionPhase>,
        action_id: Option<Uuid>,
        message: impl Into<String>,
    ) -> ReflexFailure {
        self.state = ReflexState::Failed;
        report.outcome = ReflexOutcome::Inactive;
        self.sync_report(report);
        ReflexFailure {
            stage,
            execution_phase,
            action_id,
            message: message.into(),
            report: report.clone(),
        }
    }
}

pub(crate) fn validate_region(region: PixelRect) -> Result<(), AppError> {
    if region.width == 0
        || region.height == 0
        || region.x.checked_add(region.width).is_none()
        || region.y.checked_add(region.height).is_none()
    {
        return Err(AppError::configuration("reflex search region is invalid"));
    }
    Ok(())
}
