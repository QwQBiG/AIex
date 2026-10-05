use std::collections::BTreeSet;
use std::ops::Deref;

use ai_ex_domain::{AppError, MemoryEntry, MemoryKnowledge};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct MemoryEmbedding {
    pub space: String,
    pub values: Vec<f32>,
}

impl MemoryEmbedding {
    pub fn normalized(&self) -> Result<Vec<f32>, AppError> {
        if self.space.trim().is_empty() || self.space.chars().count() > 256 {
            return Err(AppError::protocol("invalid memory embedding space"));
        }
        if self.values.is_empty()
            || self.values.len() > 8192
            || self.values.iter().any(|v| !v.is_finite())
        {
            return Err(AppError::protocol(
                "invalid memory embedding dimensions or values",
            ));
        }
        let norm = self
            .values
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt();
        if norm == 0.0 {
            return Err(AppError::protocol("memory embedding must be nonzero"));
        }
        Ok(self
            .values
            .iter()
            .map(|v| (f64::from(*v) / norm) as f32)
            .collect())
    }
}

/// Persistence includes enrichment; control pages expose bounded knowledge alongside entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct MemoryRecord {
    #[serde(flatten)]
    pub entry: MemoryEntry,
    #[serde(default, skip_serializing_if = "knowledge_empty")]
    pub knowledge: MemoryKnowledge,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<MemoryEmbedding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_metadata: Option<serde_json::Value>,
    #[serde(skip)]
    pub terms: BTreeSet<String>,
    #[serde(skip)]
    pub lowercase_text: String,
    #[serde(skip)]
    pub normalized: Vec<f32>,
}

// Read-only field access keeps existing persistence/profile code clear.
impl Deref for MemoryRecord {
    type Target = MemoryEntry;
    fn deref(&self) -> &Self::Target {
        &self.entry
    }
}

impl MemoryRecord {
    pub fn new(entry: MemoryEntry) -> Self {
        let mut record = Self {
            entry,
            knowledge: MemoryKnowledge::default(),
            embedding: None,
            legacy_metadata: None,
            terms: BTreeSet::new(),
            lowercase_text: String::new(),
            normalized: Vec::new(),
        };
        record.rebuild().expect("text-only memory is valid");
        record
    }

    pub fn rebuild(&mut self) -> Result<(), AppError> {
        if let Some(media) = &self.knowledge.media {
            media.validate()?;
        }
        // Historical summaries/events may contain generated or third-party text.
        // Only the explicit legacy user_input is eligible for personal candidates.
        let evidence_text = if self.knowledge.media.is_some()
            && self.source == ai_ex_domain::MemorySource::Automatic
        {
            // Observed captions/transcripts do not establish who is speaking.
            ""
        } else {
            match &self.legacy_metadata {
                Some(metadata) => metadata
                    .get("user_input")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
                None => &self.user_text,
            }
        };
        self.knowledge.entities = crate::entities::extract_entities(evidence_text);
        self.knowledge.validate()?;
        self.lowercase_text = searchable_text(self).to_lowercase();
        self.terms = terms(&self.lowercase_text);
        self.normalized = self
            .embedding
            .as_ref()
            .map(MemoryEmbedding::normalized)
            .transpose()?
            .unwrap_or_default();
        Ok(())
    }
}

pub fn default_profile() -> String {
    "default".to_owned()
}
pub(crate) fn searchable_text(record: &MemoryRecord) -> String {
    format!("{} {}", record.user_text, record.assistant_text)
}

pub(crate) fn terms(text: &str) -> BTreeSet<String> {
    let lowercase = text.to_lowercase();
    let mut result: BTreeSet<String> = lowercase
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_owned)
        .collect();
    for character in lowercase
        .chars()
        .filter(|character| !character.is_whitespace())
    {
        if !character.is_ascii_punctuation() {
            result.insert(character.to_string());
        }
    }
    result
}

pub(crate) fn knowledge_empty(knowledge: &MemoryKnowledge) -> bool {
    knowledge.entities.is_empty() && knowledge.media.is_none()
}
