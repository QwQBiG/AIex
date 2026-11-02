//! Independent native settings. Parsing never starts devices or requires models.

mod migration;
mod storage;
mod store;
mod validation;
mod voice;

#[cfg(test)]
mod tests;

pub use migration::{
    LegacyReferenceCandidate, NativeMigrationNotice, NativeMigrationPreview, preview_legacy_file,
};
pub use storage::{NativeStorage, ResolvedNativeStorage};
pub use store::{NativeConfigStore, NativeConfigUpdate};
pub use validation::validate_resource_id;
pub use voice::{SharedVoiceMetadata, SharedVoiceSource, VoiceLanguage, VoicePreset, VoiceSource};

use ai_ex_domain::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const NATIVE_CONFIG_FILENAME: &str = "native-v1.toml";
pub const NATIVE_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_RUNTIME_ID: &str = "native-cpu";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeConfig {
    pub schema_version: u16,
    pub revision: u64,
    #[serde(default)]
    pub storage: NativeStorage,
    #[serde(default)]
    pub devices: NativeDevices,
    pub dialogue: Option<ModelSelection>,
    pub transcription: Option<ModelSelection>,
    pub embedding: Option<ModelSelection>,
    pub active_voice: Option<String>,
    #[serde(default)]
    pub character_voices: BTreeMap<String, String>,
    #[serde(default)]
    pub voices: Vec<VoicePreset>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NativeDevices {
    pub microphone_enabled: bool,
    pub playback_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub backend: NativeBackend,
    pub model_id: String,
    pub runtime_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeBackend {
    Ollama,
    SherpaWhisper,
    SherpaKokoro,
    Qwen3TtsOnnx,
    OnnxEmbedding,
}

impl ModelSelection {
    pub fn cpu(backend: NativeBackend, model_id: impl Into<String>) -> Self {
        Self {
            backend,
            model_id: model_id.into(),
            runtime_id: DEFAULT_RUNTIME_ID.into(),
        }
    }
}

impl Default for NativeConfig {
    fn default() -> Self {
        Self {
            schema_version: NATIVE_SCHEMA_VERSION,
            revision: 1,
            storage: NativeStorage::default(),
            devices: NativeDevices::default(),
            dialogue: Some(ModelSelection::cpu(NativeBackend::Ollama, "qwen3-4b")),
            transcription: Some(ModelSelection::cpu(
                NativeBackend::SherpaWhisper,
                "whisper-small-int8",
            )),
            embedding: Some(ModelSelection::cpu(
                NativeBackend::OnnxEmbedding,
                "bge-small-zh-v1.5",
            )),
            active_voice: Some("builtin-zh".into()),
            character_voices: BTreeMap::new(),
            voices: vec![VoicePreset::default()],
        }
    }
}

impl NativeConfig {
    pub fn parse(source: &str) -> Result<Self, AppError> {
        if source.len() > crate::MAX_CONFIG_BYTES {
            return Err(AppError::configuration(
                "native configuration exceeds 1 MiB",
            ));
        }
        let config: Self = toml::from_str(source).map_err(|error| {
            AppError::configuration(format!(
                "invalid native TOML at byte range {:?}",
                error.span()
            ))
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn to_toml(&self) -> Result<String, AppError> {
        self.validate()?;
        let text = toml::to_string_pretty(self)
            .map_err(|_| AppError::configuration("cannot serialize native configuration"))?;
        if text.len() > crate::MAX_CONFIG_BYTES {
            return Err(AppError::configuration(
                "native configuration exceeds 1 MiB",
            ));
        }
        Ok(text)
    }

    pub fn active_voice_preset(&self) -> Option<&VoicePreset> {
        self.active_voice
            .as_ref()
            .and_then(|id| self.voices.iter().find(|voice| voice.id == *id))
    }

    pub fn voice_for_profile(&self, profile_id: &str) -> Option<&VoicePreset> {
        self.character_voices
            .get(profile_id)
            .and_then(|id| self.voices.iter().find(|voice| voice.id == *id))
            .or_else(|| self.active_voice_preset())
    }
}
