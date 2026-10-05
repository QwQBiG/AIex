use std::collections::BTreeSet;
use std::ops::Deref;

use ai_ex_domain::{AppError, MemoryEntry};
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

/// Persistence includes enrichment; control responses expose only `entry`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct MemoryRecord {
    #[serde(flatten)]
    pub entry: MemoryEntry,
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
