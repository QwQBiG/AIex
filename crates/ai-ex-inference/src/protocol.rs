use ai_ex_domain::AppError;
use ai_ex_model_catalog::validate_relative_path;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WORKER_PROTOCOL_VERSION: u16 = 1;
const MAX_TEXT_BYTES: usize = 128 * 1024;
const MAX_EMBEDDING_DIMENSIONS: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestIdentity {
    pub session_id: Uuid,
    pub request_id: Uuid,
    pub generation: u64,
}

impl RequestIdentity {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.session_id.is_nil() || self.request_id.is_nil() {
            return Err(AppError::protocol("inference identity must not be nil"));
        }
        Ok(())
    }
}

/// Relative to the private job directory supplied when starting the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioFile {
    pub path: String,
}

impl AudioFile {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_relative_path(&self.path)?;
        if !self.path.to_ascii_lowercase().ends_with(".wav") {
            return Err(AppError::protocol("inference audio must be a WAV file"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VoiceSelection {
    Preset { speaker: String },
    Reference { audio: AudioFile, text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InferenceTask {
    Transcribe {
        audio: AudioFile,
        language: String,
    },
    Synthesize {
        text: String,
        language: String,
        voice: VoiceSelection,
        output: AudioFile,
    },
    Embed {
        text: String,
        space: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceRequest {
    pub identity: RequestIdentity,
    pub model_id: String,
    pub task: InferenceTask,
}

impl InferenceRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        self.identity.validate()?;
        validate_text(&self.model_id, 256, "model identifier")?;
        match &self.task {
            InferenceTask::Transcribe { audio, language } => {
                audio.validate()?;
                validate_text(language, 32, "language")
            }
            InferenceTask::Synthesize {
                text,
                language,
                voice,
                output,
            } => {
                validate_text(text, MAX_TEXT_BYTES, "synthesis text")?;
                validate_text(language, 32, "language")?;
                output.validate()?;
                match voice {
                    VoiceSelection::Preset { speaker } => validate_text(speaker, 128, "speaker"),
                    VoiceSelection::Reference { audio, text } => {
                        audio.validate()?;
                        // Use Unicode case comparison for Windows resource names.
                        // Both paths have already passed the slash-only syntax check.
                        if audio.path.to_uppercase() == output.path.to_uppercase() {
                            return Err(AppError::protocol("reference audio cannot be output"));
                        }
                        validate_text(text, MAX_TEXT_BYTES, "reference text")
                    }
                }
            }
            InferenceTask::Embed { text, space } => {
                validate_text(text, MAX_TEXT_BYTES, "embedding text")?;
                validate_text(space, 256, "embedding space")
            }
        }
    }
}

fn validate_text(value: &str, maximum: usize, label: &str) -> Result<(), AppError> {
    if value.trim().is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(AppError::protocol(format!("invalid inference {label}")));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerCommand {
    Run { request: InferenceRequest },
    Cancel { identity: RequestIdentity },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InferenceOutput {
    Transcript {
        text: String,
    },
    /// A standard RIFF/WAVE file with a 16-byte fmt chunk, mono PCM16 samples,
    /// and the declared sample rate. Workers must canonicalize native output
    /// before replying; a WAV suffix alone does not establish this contract.
    Audio {
        file: AudioFile,
        sample_rate: u32,
    },
    Embedding {
        space: String,
        values: Vec<f32>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerReply {
    Ready {
        protocol_version: u16,
    },
    Completed {
        identity: RequestIdentity,
        output: InferenceOutput,
    },
    Failed {
        identity: RequestIdentity,
        error: AppError,
    },
    Cancelled {
        identity: RequestIdentity,
    },
}

impl WorkerReply {
    pub fn validate(&self) -> Result<(), AppError> {
        match self {
            Self::Ready { .. } => Ok(()),
            Self::Cancelled { identity } => identity.validate(),
            Self::Failed { identity, error } => {
                identity.validate()?;
                validate_text(&error.message, 4096, "error message")
            }
            Self::Completed { identity, output } => {
                identity.validate()?;
                output.validate()
            }
        }
    }
}

impl InferenceOutput {
    pub(crate) fn matches_task(&self, task: &InferenceTask) -> bool {
        match (self, task) {
            (Self::Transcript { .. }, InferenceTask::Transcribe { .. }) => true,
            (
                Self::Embedding { space, .. },
                InferenceTask::Embed {
                    space: expected, ..
                },
            ) => space == expected,
            (Self::Audio { file, .. }, InferenceTask::Synthesize { output, .. }) => {
                file.path == output.path
            }
            _ => false,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        match self {
            Self::Transcript { text } if text.len() <= MAX_TEXT_BYTES => Ok(()),
            Self::Transcript { .. } => Err(AppError::protocol("worker transcript is too large")),
            Self::Audio { file, sample_rate } => {
                file.validate()?;
                if *sample_rate == 0 {
                    return Err(AppError::protocol(
                        "worker audio sample rate must be positive",
                    ));
                }
                Ok(())
            }
            Self::Embedding { space, values } => {
                validate_text(space, 256, "embedding space")?;
                if values.is_empty()
                    || values.len() > MAX_EMBEDDING_DIMENSIONS
                    || values.iter().any(|value| !value.is_finite())
                    || values.iter().all(|value| *value == 0.0)
                {
                    return Err(AppError::protocol("invalid worker embedding vector"));
                }
                Ok(())
            }
        }
    }
}
