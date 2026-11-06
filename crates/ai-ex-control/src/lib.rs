#![forbid(unsafe_code)]

mod client;
#[cfg(test)]
mod desktop_runtime_tests;
#[cfg(test)]
mod native_tests;
mod protocol;
mod server;

pub use ai_ex_domain::{
    AUTOMATION_MAX_ACTIONS, AUTOMATION_MAX_ERROR_BYTES, AUTOMATION_MAX_FRAMES,
    AUTOMATION_MAX_MANIFEST_BYTES, AUTOMATION_MAX_PROFILE_BYTES, AUTOMATION_MAX_SNAPSHOT_BYTES,
    AUTOMATION_MAX_TARGET_BYTES, AUTOMATION_SCHEMA_VERSION, AutomationControlMode,
    AutomationPauseReason, AutomationRequest, AutomationResumeEligibility, AutomationSessionPhase,
    AutomationSessionSnapshot, AutomationSnapshot, AutomationStepOutcome, AutomationTemplateMode,
};
pub use ai_ex_domain::{NativeReply, NativeRequest};
pub use client::ControlClient;
pub use protocol::{
    ControlCommand, ControlPayload, ControlRequest, ControlResponse, LegacyCandidateState,
    LegacyCandidateStatus, LegacyMemoryBackupInspection, MemoryBackupInspection, MemoryReply,
    SensitivePath,
};
pub use server::{ControlBackend, ControlServer};
