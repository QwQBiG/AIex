#![forbid(unsafe_code)]

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
