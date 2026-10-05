use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{AppError, MemorySource};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEntityKind {
    Name,
    Preference,
    Fact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntity {
    pub kind: MemoryEntityKind,
    pub key: String,
    pub name: String,
    pub value: String,
    pub confidence: u16,
}

impl MemoryEntity {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.confidence > 1000 {
            return Err(AppError::configuration(
                "memory entity confidence must be in 0..=1000",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryModality {
    #[default]
    Text,
    Audio,
    Visual,
    Multimodal,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryMedia {
    pub modality: MemoryModality,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_features: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual_features: Option<Map<String, Value>>,
    pub metadata: Map<String, Value>,
    pub importance: u16,
}

impl MemoryMedia {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_media_tags(&self.tags)?;
        if self.importance > 1000 {
            return Err(AppError::configuration(
                "memory importance must be in 0..=1000",
            ));
        }
        let audio = self.audio_features.is_some();
        let visual = self.visual_features.is_some();
        let features_match = match self.modality {
            MemoryModality::Text => !audio && !visual,
            MemoryModality::Audio => audio && !visual,
            MemoryModality::Visual => !audio && visual,
            // The legacy add_multimodal_memory API made both feature maps optional.
            // Preserve its explicit modality even before either map is available.
            MemoryModality::Multimodal => true,
        };
        if !features_match {
            return Err(AppError::configuration(
                "memory modality does not match its audio and visual features",
            ));
        }
        let encoded = serde_json::to_vec(self)
            .map_err(|_| AppError::configuration("memory media cannot be serialized"))?;
        if encoded.len() > 8192 {
            return Err(AppError::configuration("memory media exceeds 8192 bytes"));
        }
        Ok(())
    }
}

pub(crate) fn validate_media_tags(tags: &[String]) -> Result<(), AppError> {
    if tags.len() > 16
        || tags
            .iter()
            .any(|tag| tag.trim().is_empty() || tag.chars().count() > 64)
    {
        return Err(AppError::configuration(
            "memory media supports at most 16 nonempty tags of at most 64 characters",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryKnowledge {
    pub entities: Vec<MemoryEntity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<MemoryMedia>,
}

impl MemoryKnowledge {
    pub fn validate(&self) -> Result<(), AppError> {
        for entity in &self.entities {
            entity.validate()?;
        }
        if let Some(media) = &self.media {
            media.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntityEvidence {
    pub memory_id: Uuid,
    pub revision: u64,
    pub source: MemorySource,
    pub value: String,
    pub confidence: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntityGroup {
    pub key: String,
    pub kind: MemoryEntityKind,
    pub name: String,
    pub current_value: String,
    pub first_mentioned_ms: u64,
    pub last_updated_ms: u64,
    pub evidence: Vec<MemoryEntityEvidence>,
    pub total_mentions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntityPage {
    pub profile_id: String,
    pub enabled: bool,
    pub total: usize,
    pub offset: usize,
    pub entities: Vec<MemoryEntityGroup>,
}

#[cfg(test)]
#[path = "memory_knowledge_tests.rs"]
mod tests;
