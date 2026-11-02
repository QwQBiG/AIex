use std::collections::{BTreeMap, BTreeSet};

use ai_ex_domain::{ImportedMemoryEntity, MemoryEntitySource, MemoryEntitySourceStatus};
use serde::Serialize;

use super::*;

pub(super) const SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Default)]
pub(super) struct Provenance {
    pub candidates: Vec<ImportedMemoryEntity>,
    pub sources: Vec<MemoryEntitySource>,
}

impl Provenance {
    pub fn validate(&self) -> Result<(), AppError> {
        let mut ids = BTreeSet::new();
        for candidate in &self.candidates {
            candidate.validate()?;
            if !ids.insert((&candidate.profile_id, candidate.id)) {
                return Err(AppError::protocol("duplicate imported entity identity"));
            }
        }
        let mut edges = BTreeSet::new();
        for source in &self.sources {
            source.validate()?;
            let identity = source
                .memory_id
                .map_or_else(|| source.legacy_memory_id.clone(), |id| id.to_string());
            if !ids.contains(&(&source.profile_id, source.entity_id))
                || !edges.insert((&source.profile_id, source.entity_id, identity))
            {
                return Err(AppError::protocol(
                    "missing entity or duplicate source association",
                ));
            }
        }
        Ok(())
    }

    pub fn reconcile(&mut self, old: &[MemoryRecord], new: &[MemoryRecord]) {
        let old: BTreeMap<_, _> = old.iter().map(|r| ((&r.profile_id, r.id), r)).collect();
        let new: BTreeMap<_, _> = new.iter().map(|r| ((&r.profile_id, r.id), r)).collect();
        for source in &mut self.sources {
            if source.status == MemoryEntitySourceStatus::Corrected
                && source.memory_id.is_some_and(|id| {
                    old.contains_key(&(&source.profile_id, id))
                        && !new.contains_key(&(&source.profile_id, id))
                })
            {
                source.status = MemoryEntitySourceStatus::Forgotten;
            }
            if source.status != MemoryEntitySourceStatus::Active {
                continue;
            }
            let Some(id) = source.memory_id else { continue };
            let identity = (&source.profile_id, id);
            let Some(current) = new.get(&identity) else {
                source.status = if old.contains_key(&identity) {
                    MemoryEntitySourceStatus::Forgotten
                } else {
                    MemoryEntitySourceStatus::Missing
                };
                continue;
            };
            if source.memory_revision == Some(current.revision) {
                continue;
            }
            let same_contribution = old.get(&identity).is_some_and(|previous| {
                previous.user_text == current.user_text
                    && previous.assistant_text == current.assistant_text
                    && previous.source == current.source
            });
            if same_contribution {
                // Importance maintenance may advance a revision without changing evidence.
                source.memory_revision = Some(current.revision);
            } else {
                source.status = MemoryEntitySourceStatus::Corrected;
            }
        }
    }

    pub fn for_profile(&self, profile: &str) -> Self {
        Self {
            candidates: self
                .candidates
                .iter()
                .filter(|c| c.profile_id == profile)
                .cloned()
                .collect(),
            sources: self
                .sources
                .iter()
                .filter(|s| s.profile_id == profile)
                .cloned()
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct MemoryRow<'a> {
    schema_version: u32,
    record_type: &'static str,
    #[serde(flatten)]
    record: &'a MemoryRecord,
}

pub(super) fn memory_line(record: &MemoryRecord) -> Result<String, AppError> {
    serde_json::to_string(&MemoryRow {
        schema_version: SCHEMA_VERSION,
        record_type: "memory",
        record,
    })
    .map_err(|error| AppError::protocol(error.to_string()))
}

pub(super) fn serialize_document(
    records: &[MemoryRecord],
    provenance: &Provenance,
) -> Result<String, AppError> {
    let mut content = serialize_records(records)?;
    for candidate in &provenance.candidates {
        let value = serde_json::json!({"schema_version":SCHEMA_VERSION,
            "record_type":"entity_candidate", "candidate": candidate});
        content.push_str(
            &serde_json::to_string(&value).map_err(|e| AppError::protocol(e.to_string()))?,
        );
        content.push('\n');
    }
    for source in &provenance.sources {
        let value = serde_json::json!({"schema_version":SCHEMA_VERSION,
            "record_type":"entity_source", "source": source});
        content.push_str(
            &serde_json::to_string(&value).map_err(|e| AppError::protocol(e.to_string()))?,
        );
        content.push('\n');
    }
    Ok(content)
}
