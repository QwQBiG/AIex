#![forbid(unsafe_code)]

mod automation;
mod conversation;
mod error;
mod event;
mod memory;
mod memory_policy;
pub use memory_policy::MemoryRecallPolicy;
mod memory_knowledge;
mod persona;
mod playback;
mod stage_telemetry;

pub use automation::{
    AUTOMATION_MAX_ACTIONS, AUTOMATION_MAX_ERROR_BYTES, AUTOMATION_MAX_FRAMES,
    AUTOMATION_MAX_MANIFEST_BYTES, AUTOMATION_MAX_PROFILE_BYTES, AUTOMATION_MAX_SNAPSHOT_BYTES,
    AUTOMATION_MAX_TARGET_BYTES, AUTOMATION_SCHEMA_VERSION, AutomationControlMode,
    AutomationPauseReason, AutomationRequest, AutomationResumeEligibility,
    AutomationSearchIncompleteReason, AutomationSearchRegion, AutomationSearchSnapshot,
    AutomationSessionPhase, AutomationSessionSnapshot, AutomationSnapshot, AutomationStepOutcome,
    AutomationTemplateMode,
};
pub use conversation::{ConversationState, Emotion, Message, Role, TurnId};
pub use error::{AppError, ErrorKind};
pub use event::{ComponentHealth, LiveResponseMode, SystemEvent};
pub use memory::{
    MemoryEntry, MemoryKind, MemoryPage, MemoryProjection, MemoryRequest, MemoryResponse,
    MemorySource,
};
pub use memory_knowledge::{
    MemoryEntity, MemoryEntityEvidence, MemoryEntityGroup, MemoryEntityKind, MemoryEntityPage,
    MemoryKnowledge, MemoryMedia, MemoryModality,
};
pub use persona::PersonaSnapshot;
pub use playback::SpeechPlaybackSnapshot;
pub use stage_telemetry::{STAGE_TELEMETRY_SCHEMA_VERSION, StageActionSummary, StageSnapshot};
