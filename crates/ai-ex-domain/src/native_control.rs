//! Authenticated native setup DTOs. Runtime and configuration implementations stay in the service.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppError;

pub const NATIVE_MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
pub const NATIVE_MAX_PACKAGES: usize = 64;
pub const NATIVE_MAX_ERROR_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeRequest {
    State,
    Packages,
    Download {
        package_id: String,
    },
    Import {
        package_id: String,
        path: String,
    },
    Operation {
        id: Uuid,
    },
    Cancel {
        id: Uuid,
    },
    Remove {
        package_id: String,
    },
    Settings,
    SaveSettings {
        expected_revision: u64,
        document: String,
    },
    ImportVoice {
        name: String,
        path: String,
        text: String,
        language: String,
        expected_revision: u64,
    },
    PreviewVoice {
        voice_id: String,
        text: String,
        expected_generation: u64,
    },
    StopPreview,
    BindVoice {
        profile_id: String,
        voice_id: String,
        expected_revision: u64,
    },
    CreateDefaults,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeReply {
    State(NativeState),
    Packages(Vec<NativePackage>),
    Settings { revision: u64, document: String },
    Operation(NativeOperation),
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeState {
    pub configured: bool,
    pub configuration_path: String,
    pub preview_active: bool,
    pub preview_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePackage {
    pub id: String,
    pub name: String,
    pub revision: String,
    pub kind: NativePackageKind,
    pub state: NativePackageState,
    pub bytes: u64,
    pub installed: bool,
    pub active_operation: Option<Uuid>,
    pub error: Option<String>,
    pub licenses: Vec<NativeLicense>,
    pub required_packages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeLicense {
    pub id: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePackageKind {
    Model,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePackageState {
    NotPrepared,
    Downloading,
    Verifying,
    Ready,
    InUse,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeOperationPhase {
    Downloading,
    Importing,
    Verifying,
    Committing,
    Removing,
    Running,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}

impl NativeOperationPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeOperation {
    pub id: Uuid,
    pub package_id: String,
    pub phase: NativeOperationPhase,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
}

fn bounded(value: &str, max: usize, empty: bool) -> Result<(), AppError> {
    if value.len() > max || (!empty && value.trim().is_empty()) || value.contains('\0') {
        return Err(AppError::protocol(
            "native control field exceeds its bounds or is empty",
        ));
    }
    Ok(())
}

impl NativeRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        match self {
            Self::Download { package_id } | Self::Remove { package_id } => {
                bounded(package_id, 256, false)
            }
            Self::Import { package_id, path } => {
                bounded(package_id, 256, false)?;
                bounded(path, 4096, false)
            }
            Self::SaveSettings {
                expected_revision,
                document,
            } => {
                revision(*expected_revision)?;
                bounded(document, NATIVE_MAX_DOCUMENT_BYTES, false)
            }
            Self::ImportVoice {
                name,
                path,
                text,
                language,
                expected_revision,
            } => {
                revision(*expected_revision)?;
                bounded(name, 256, false)?;
                bounded(path, 4096, false)?;
                bounded(text, 64 * 1024, false)?;
                if !matches!(language.as_str(), "zh" | "en") {
                    return Err(AppError::protocol("unsupported voice language"));
                }
                Ok(())
            }
            Self::PreviewVoice { voice_id, text, .. } => {
                bounded(voice_id, 256, false)?;
                bounded(text, 4096, false)
            }
            Self::BindVoice {
                profile_id,
                voice_id,
                expected_revision,
            } => {
                revision(*expected_revision)?;
                bounded(profile_id, 512, false)?;
                if profile_id.chars().count() > 128 {
                    return Err(AppError::protocol(
                        "native voice profile ID is outside persona bounds",
                    ));
                }
                bounded(voice_id, 256, false)
            }
            _ => Ok(()),
        }
    }
}

impl NativeReply {
    pub fn validate(&self) -> Result<(), AppError> {
        match self {
            Self::State(state) => bounded(&state.configuration_path, 4096, true),
            Self::Settings {
                revision: value,
                document,
            } => {
                revision(*value)?;
                bounded(document, NATIVE_MAX_DOCUMENT_BYTES, false)
            }
            Self::Packages(packages) => {
                if packages.len() > NATIVE_MAX_PACKAGES {
                    return Err(AppError::protocol("too many native packages"));
                }
                let mut metadata_bytes = 0usize;
                for package in packages {
                    metadata_bytes += package.id.len()
                        + package.name.len()
                        + package.revision.len()
                        + package.error.as_ref().map_or(0, String::len);
                    bounded(&package.id, 256, false)?;
                    bounded(&package.name, 256, false)?;
                    bounded(&package.revision, 256, false)?;
                    diagnostic(&package.error)?;
                    if package.licenses.len() > 512 || package.required_packages.len() > 8 {
                        return Err(AppError::protocol("native package metadata exceeds bounds"));
                    }
                    for license in &package.licenses {
                        metadata_bytes += license.id.len() + license.source_url.len();
                        bounded(&license.id, 256, false)?;
                        bounded(&license.source_url, 4096, false)?;
                    }
                    for required in &package.required_packages {
                        metadata_bytes += required.len();
                        bounded(required, 256, false)?;
                    }
                    if metadata_bytes > NATIVE_MAX_DOCUMENT_BYTES {
                        return Err(AppError::protocol("native package metadata exceeds 1 MiB"));
                    }
                }
                Ok(())
            }
            Self::Operation(operation) => {
                bounded(&operation.package_id, 256, false)?;
                diagnostic(&operation.error)
            }
            Self::Accepted => Ok(()),
        }
    }
}

fn diagnostic(error: &Option<String>) -> Result<(), AppError> {
    error
        .as_ref()
        .map_or(Ok(()), |text| bounded(text, NATIVE_MAX_ERROR_BYTES, true))
}

fn revision(value: u64) -> Result<(), AppError> {
    if value == 0 {
        return Err(AppError::protocol("native revision must be nonzero"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_binding_preserves_the_existing_unicode_persona_identifier_limit() {
        let valid = "🪷".repeat(128);
        let persona = crate::PersonaSnapshot {
            profile_id: valid.clone(),
            ..Default::default()
        };
        persona.validate().unwrap();
        NativeRequest::BindVoice {
            profile_id: valid,
            voice_id: "builtin-zh".into(),
            expected_revision: 1,
        }
        .validate()
        .unwrap();
        assert!(
            NativeRequest::BindVoice {
                profile_id: "中".repeat(129),
                voice_id: "builtin-zh".into(),
                expected_revision: 1
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn typed_native_requests_round_trip_and_bound_private_fields() {
        let request = NativeRequest::ImportVoice {
            name: "声音".into(),
            path: "D:/参考.wav".into(),
            text: "这是对应原文。".into(),
            language: "zh".into(),
            expected_revision: 1,
        };
        request.validate().unwrap();
        let json = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            serde_json::from_slice::<NativeRequest>(&json).unwrap(),
            request
        );
        assert!(
            NativeRequest::SaveSettings {
                expected_revision: 0,
                document: "test".into()
            }
            .validate()
            .is_err()
        );
        assert!(
            NativeRequest::PreviewVoice {
                voice_id: "v".into(),
                text: "x".repeat(4097),
                expected_generation: 0,
            }
            .validate()
            .is_err()
        );
        assert!(
            serde_json::from_str::<NativeRequest>(
                r#"{"type":"preview_voice","voice_id":"voice","text":"hello","expected_generation":0,"private":1}"#
            )
            .is_err()
        );
    }
}
