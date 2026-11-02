use std::collections::HashSet;

use ai_ex_domain::AppError;

use super::{
    ModelSelection, NATIVE_SCHEMA_VERSION, NativeBackend, NativeConfig, VoicePreset, VoiceSource,
};

impl NativeConfig {
    /// Validates syntax and combinations, never the presence of installed files.
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != NATIVE_SCHEMA_VERSION || self.revision == 0 {
            return Err(AppError::configuration(
                "unsupported native schema or zero revision",
            ));
        }
        self.storage.validate()?;
        for (selection, expected) in [
            (&self.dialogue, NativeBackend::Ollama),
            (&self.transcription, NativeBackend::SherpaWhisper),
            (&self.embedding, NativeBackend::OnnxEmbedding),
        ] {
            if let Some(selection) = selection {
                selection.validate()?;
                if selection.backend != expected {
                    return Err(AppError::configuration(
                        "native capability backend mismatch",
                    ));
                }
            }
        }
        if self.voices.len() > 64 {
            return Err(AppError::configuration(
                "native configuration exceeds 64 voices",
            ));
        }
        let mut ids = HashSet::new();
        for voice in &self.voices {
            voice.validate()?;
            if !ids.insert(voice.id.to_ascii_lowercase()) {
                return Err(AppError::configuration("duplicate native voice ID"));
            }
        }
        if let Some(id) = &self.active_voice {
            validate_resource_id(id)?;
            if !self.voices.iter().any(|voice| voice.id == *id) {
                return Err(AppError::configuration(
                    "active native voice is not defined",
                ));
            }
        }
        for (profile_id, voice_id) in &self.character_voices {
            if profile_id.trim().is_empty() || profile_id.chars().count() > 128 {
                return Err(AppError::configuration(
                    "native voice binding profile ID is outside persona bounds",
                ));
            }
            if !self.voices.iter().any(|voice| voice.id == *voice_id) {
                return Err(AppError::configuration(
                    "native character voice binding references an undefined preset",
                ));
            }
        }
        Ok(())
    }
}

impl ModelSelection {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_resource_id(&self.model_id)?;
        validate_resource_id(&self.runtime_id)?;
        let known = match self.model_id.as_str() {
            "qwen3-4b" => Some(NativeBackend::Ollama),
            "whisper-small-int8" => Some(NativeBackend::SherpaWhisper),
            "kokoro-v1.1-zh-en" => Some(NativeBackend::SherpaKokoro),
            "qwen3-tts-base-0.6b" => Some(NativeBackend::Qwen3TtsOnnx),
            "bge-small-zh-v1.5" => Some(NativeBackend::OnnxEmbedding),
            _ => None,
        };
        if known.is_some_and(|backend| backend != self.backend) {
            return Err(AppError::configuration(
                "known native model ID has a different backend",
            ));
        }
        Ok(())
    }
}

impl VoicePreset {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_resource_id(&self.id)?;
        if self.label.trim().is_empty()
            || self.label.chars().count() > 128
            || self.label.chars().any(char::is_control)
        {
            return Err(AppError::configuration(
                "voice label must contain 1..=128 readable characters",
            ));
        }
        self.model.validate()?;
        match (&self.source, self.model.backend) {
            (VoiceSource::Preset { speaker }, NativeBackend::SherpaKokoro) => {
                validate_resource_id(speaker)
            }
            (VoiceSource::ReferenceAudio { audio_id, text }, NativeBackend::Qwen3TtsOnnx) => {
                validate_resource_id(audio_id)?;
                if text.trim().is_empty() || text.len() > 64 * 1024 || text.contains('\0') {
                    return Err(AppError::configuration(
                        "reference voice requires matching transcript text of at most 64 KiB",
                    ));
                }
                Ok(())
            }
            _ => Err(AppError::configuration(
                "voice source is incompatible with its native backend",
            )),
        }
    }
}

pub fn validate_resource_id(value: &str) -> Result<(), AppError> {
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if value.is_empty()
        || value.len() > 128
        || reserved
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || value.ends_with('.')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(AppError::configuration(
            "native resource ID must be a safe ASCII identifier",
        ));
    }
    Ok(())
}
