use serde::{Deserialize, Serialize};

use super::{ModelSelection, NativeBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceLanguage {
    Zh,
    En,
}

impl VoiceLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoicePreset {
    pub id: String,
    pub label: String,
    pub language: VoiceLanguage,
    pub model: ModelSelection,
    pub source: VoiceSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum VoiceSource {
    Preset { speaker: String },
    ReferenceAudio { audio_id: String, text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedVoiceMetadata {
    pub id: String,
    pub label: String,
    pub language: VoiceLanguage,
    pub model: ModelSelection,
    pub source: SharedVoiceSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SharedVoiceSource {
    Preset { speaker: String },
    ReferenceRequired,
}

impl VoicePreset {
    /// Character sharing includes neither reference audio IDs nor transcripts.
    pub fn share_metadata(&self) -> SharedVoiceMetadata {
        SharedVoiceMetadata {
            id: self.id.clone(),
            label: self.label.clone(),
            language: self.language,
            model: self.model.clone(),
            source: match &self.source {
                VoiceSource::Preset { speaker } => SharedVoiceSource::Preset {
                    speaker: speaker.clone(),
                },
                VoiceSource::ReferenceAudio { .. } => SharedVoiceSource::ReferenceRequired,
            },
        }
    }

    pub fn private_reference_id(&self) -> Option<&str> {
        match &self.source {
            VoiceSource::ReferenceAudio { audio_id, .. } => Some(audio_id),
            VoiceSource::Preset { .. } => None,
        }
    }
}

impl Default for VoicePreset {
    fn default() -> Self {
        Self {
            id: "builtin-zh".into(),
            label: "默认中文声音".into(),
            language: VoiceLanguage::Zh,
            model: ModelSelection::cpu(NativeBackend::SherpaKokoro, "kokoro-v1.1-zh-en"),
            source: VoiceSource::Preset {
                speaker: "zf_079".into(),
            },
        }
    }
}
