#![forbid(unsafe_code)]

mod actor;
mod engine;
mod policy;
mod ports;
mod protocol_adapter;
mod runtime;
#[cfg(test)]
mod runtime_tests;
mod stage_ports;

pub use actor::{PendingTurn, RuntimeHandle, spawn_runtime};
pub use engine::ConversationEngine;
pub use policy::ConversationPolicy;
pub use ports::{
    AvatarPort, EmbeddingPort, EventSink, LanguageModelPort, MemoryPort, ModelRequest, SpeechPort,
};
pub use protocol_adapter::LegacyModelBackend;
pub use runtime::{Runtime, RuntimeControl, TurnOutcome};
pub use stage_ports::{StageAvatarPort, StageJournal, StageOutput, StageSpeechPort};
