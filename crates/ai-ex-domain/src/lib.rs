#![forbid(unsafe_code)]

/// Bounds for optional long-sentence splitting, measured in Unicode scalars.
pub const SOFT_SPLIT_MIN_CHARS: usize = 4;
pub const SOFT_SPLIT_MAX_CHARS: usize = 512;
pub const SOFT_SPLIT_DEFAULT_CHARS: usize = 10;

mod automation;
mod conversation;
mod desktop_runtime;
pub use desktop_runtime::*;
mod error;
mod event;
mod memory;
mod memory_policy;
pub use memory_policy::MemoryRecallPolicy;
mod memory_knowledge;
mod native_control;
mod native_desktop;
pub use native_desktop::*;
mod persona;
mod playback;
mod stage_telemetry;
pub use native_control::{
    NATIVE_MAX_DOCUMENT_BYTES, NATIVE_MAX_ERROR_BYTES, NATIVE_MAX_PACKAGES, NativeLicense,
    NativeOperation, NativeOperationPhase, NativePackage, NativePackageKind, NativePackageState,
    NativeReply, NativeRequest, NativeState,
};

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
    MemoryBackupState, MemoryBackupStatus, MemoryEntry, MemoryKind, MemoryPage, MemoryProjection,
    MemoryRequest, MemoryResponse, MemorySource, MemoryTextField, MemoryTextSegment,
};
pub use memory_knowledge::{
    ImportedMemoryEntity, MemoryEntity, MemoryEntityEvidence, MemoryEntityGroup, MemoryEntityKind,
    MemoryEntityOrigin, MemoryEntityPage, MemoryEntitySource, MemoryEntitySourceStatus,
    MemoryEntitySubject, MemoryKnowledge, MemoryMedia, MemoryModality,
};
pub use persona::PersonaSnapshot;
pub use playback::SpeechPlaybackSnapshot;
pub use stage_telemetry::{STAGE_TELEMETRY_SCHEMA_VERSION, StageActionSummary, StageSnapshot};
