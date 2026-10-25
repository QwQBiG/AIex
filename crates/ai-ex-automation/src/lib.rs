#![forbid(unsafe_code)]

mod chat_pause;
mod coordinator;
mod dry_run;
mod frame_geometry;
mod game_profile;
mod plugin_port;
mod plugin_protocol;
mod ports;
mod reflex;
mod reflex_control;
mod reflex_runner;
mod reflex_session;
mod replay;
mod template_matcher;
mod template_search;
mod types;

pub use chat_pause::{
    ChatPauseOptions, ChatPauseOutcome, ChatPausePolicy, ChatPauseReport, ResumeEligibility,
};
pub use coordinator::{AutomationCoordinator, GuardedExecutionFailure};
pub use dry_run::DryRunAutomationPort;
pub use frame_geometry::{DesktopBounds, FrameGeometry};
pub use game_profile::GameProfile;
pub use plugin_port::{AutomationPluginTransport, PluginAutomationPort};
pub use plugin_protocol::{
    AUTOMATION_PLUGIN_SCHEMA_VERSION, AutomationPluginRequest, AutomationPluginRequestKind,
    AutomationPluginResponse, AutomationPluginResponseKind,
};
pub use ports::{AuditSink, AutomationPort};
pub use reflex::{
    ReflexController, ReflexExecutionPhase, ReflexFailure, ReflexFailureStage, ReflexMode,
    ReflexOptions, ReflexOutcome, ReflexReceipt, ReflexState, ReflexStepReport,
};
pub use reflex_control::{
    OwnedReflexPause, ReflexControl, ReflexControlMode, ReflexControlSnapshot, ReflexInactive,
    ReflexInactiveReason, ReflexLease, ReflexPauseClaim, ReflexPauseResume,
};
pub use reflex_runner::run_reflex_step;
pub use reflex_session::{
    ReflexInterruption, ReflexSession, ReflexSessionOutcome, ReflexSessionReport,
};
pub use replay::{AUTOMATION_SCHEMA_VERSION, AutomationReplayRecord, parse_jsonl};
pub use template_matcher::{MatchOptions, PixelPoint, PixelRect, TemplateMatch, TemplateMatcher};
pub use template_search::{
    TemplateMatchSearch, TemplateSearchComplete, TemplateSearchIncomplete,
    TemplateSearchIncompleteReason, TemplateSearchProgress, TemplateSearchProof,
    TemplateSearchStep,
};
pub use types::{
    ActionResult, AuditRecord, AuditStage, AutomationAction, ExecutionFailure, ExecutionPhase,
    ExecutionReceipt, PointerButton, ScreenFrame,
};

#[cfg(test)]
mod tests;
