use std::collections::BTreeSet;

use ai_ex_domain::AppError;
use serde::{Deserialize, Serialize};

use crate::{document, provenance::SCHEMA_VERSION};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ROWS: usize = 50_000;
const MAX_SPACES: usize = 16;

/// A bounded summary that contains no backup text, paths, identities or space names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupInspection {
    pub memory_records: usize,
    pub entity_candidates: usize,
    pub source_associations: usize,
    pub inactive_source_associations: usize,
    pub embedded_records: usize,
    pub distinct_embedding_spaces: usize,
    /// None means no comparison space was supplied.
    pub matching_embeddings: Option<usize>,
}

#[derive(Deserialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
enum Header {
    Memory {
        schema_version: Option<u32>,
        profile_id: Option<String>,
    },
    EntityCandidate {
        schema_version: Option<u32>,
        candidate: Profile,
    },
    EntitySource {
        schema_version: Option<u32>,
        source: Profile,
    },
}

#[derive(Deserialize)]
struct Profile {
    profile_id: Option<String>,
}

/// Inspect explicitly provided v2 JSONL bytes for a single profile.
///
/// This does not read or write files, restore or merge records, or inspect legacy
/// gzip backups. A fully empty backup has no verifiable profile and is rejected.
/// Source states are reported after the normal document reconciliation rules.
/// Profile labels are checked for consistency, not authenticated; v2 JSONL has
/// no whole-file integrity signature. Matching spaces does not prove compatibility.
pub fn inspect_profile_backup(
    bytes: &[u8],
    expected_profile: &str,
    comparison_space: Option<&str>,
) -> Result<BackupInspection, AppError> {
    if bytes.len() > MAX_BYTES {
        return Err(AppError::configuration("backup exceeds inspection limit"));
    }
    if expected_profile.trim().is_empty()
        || expected_profile.chars().count() > 128
        || comparison_space.is_some_and(|space| {
            space.trim().is_empty()
                || space.chars().count() > 256
                || space.chars().any(char::is_control)
        })
    {
        return Err(AppError::configuration("invalid backup inspection options"));
    }
    let content = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut rows = 0_usize;
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        rows += 1;
        if rows > MAX_ROWS {
            return Err(AppError::configuration("backup exceeds inspection limit"));
        }
        let header: Header = serde_json::from_str(line).map_err(|_| invalid())?;
        let (version, profile) = match header {
            Header::Memory {
                schema_version,
                profile_id,
            } => (schema_version, profile_id),
            Header::EntityCandidate {
                schema_version,
                candidate,
            } => (schema_version, candidate.profile_id),
            Header::EntitySource {
                schema_version,
                source,
            } => (schema_version, source.profile_id),
        };
        if version != Some(SCHEMA_VERSION) {
            return Err(invalid());
        }
        if profile.as_deref() != Some(expected_profile) {
            return Err(AppError::configuration("backup profile cannot be verified"));
        }
    }
    if rows == 0 {
        return Err(AppError::configuration("backup profile cannot be verified"));
    }
    // Never expose parser errors: untrusted fields may contain private text.
    let document = document::parse(content).map_err(|_| invalid())?;
    let mut spaces = BTreeSet::new();
    let mut embedded_records = 0;
    let mut matching_embeddings = 0;
    for embedding in document
        .records
        .iter()
        .filter_map(|record| record.embedding.as_ref())
    {
        spaces.insert(embedding.space.as_str());
        if spaces.len() > MAX_SPACES {
            return Err(AppError::configuration("backup exceeds inspection limit"));
        }
        embedded_records += 1;
        matching_embeddings += usize::from(comparison_space == Some(embedding.space.as_str()));
    }
    Ok(BackupInspection {
        memory_records: document.records.len(),
        entity_candidates: document.provenance.candidates.len(),
        source_associations: document.provenance.sources.len(),
        inactive_source_associations: document
            .provenance
            .sources
            .iter()
            .filter(|source| source.status != ai_ex_domain::MemoryEntitySourceStatus::Active)
            .count(),
        embedded_records,
        distinct_embedding_spaces: spaces.len(),
        matching_embeddings: comparison_space.map(|_| matching_embeddings),
    })
}

fn invalid() -> AppError {
    AppError::configuration("invalid or unsupported backup")
}

#[cfg(test)]
#[path = "backup_inspection_tests.rs"]
mod tests;
