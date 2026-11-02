#![forbid(unsafe_code)]

mod config;
mod job;
mod runner;

#[cfg(test)]
mod tests;

pub use config::VerifiedConfig;
pub use job::JobDirectory;
pub use runner::{Engine, serve};

pub use ai_ex_domain::AppError;
pub use ai_ex_inference::{
    AudioFile, InferenceOutput, InferenceRequest, InferenceTask, VoiceSelection,
};
pub use ai_ex_model_catalog::{ArtifactKind, ModelBackend};
