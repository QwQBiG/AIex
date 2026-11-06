//! Build a review-only candidate from two explicit, already loaded backups.

#[path = "candidate_merge_json.rs"]
mod unique_json;

use std::collections::{BTreeMap, BTreeSet};

use ai_ex_domain::{AppError, MemoryEntitySource, MemoryEntitySourceStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{document, provenance::SCHEMA_VERSION};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_ROWS: usize = 50_000;
const MAX_SPACES: usize = 16;

/// Summary only: it contains no memory text, paths, identities or space names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryMergeReport {
    pub base_memory_records: usize,
    pub incoming_memory_records: usize,
    pub entity_candidates: usize,
    pub source_associations: usize,
    pub inactive_source_associations: usize,
    pub distinct_embedding_spaces: usize,
    /// An empty input has no embedded profile label; it is not authenticated.
    pub base_empty: bool,
    pub incoming_empty: bool,
}

/// Private memory bytes must not be logged or treated as an adopted store.
pub struct MemoryMergeCandidate {
    pub content: Vec<u8>,
    pub report: MemoryMergeReport,
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
        source: MemoryEntitySource,
    },
}

#[derive(Deserialize)]
struct Profile {
    profile_id: Option<String>,
}

struct Input {
    document: document::Document,
    rows: usize,
}

type SourceKey = (Uuid, String);

fn source_key(source: &MemoryEntitySource) -> SourceKey {
    let memory = source
        .memory_id
        .map_or_else(|| source.legacy_memory_id.clone(), |id| id.to_string());
    (source.entity_id, memory)
}

fn invalid() -> AppError {
    AppError::configuration("invalid or unsupported memory candidate")
}

fn parse_input(bytes: &[u8], profile: &str) -> Result<Input, AppError> {
    if bytes.len() > MAX_BYTES {
        return Err(AppError::configuration(
            "memory candidate exceeds size limit",
        ));
    }
    let content = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut rows = 0;
    let mut original_sources = BTreeMap::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        rows += 1;
        if rows > MAX_ROWS {
            return Err(AppError::configuration(
                "memory candidate exceeds row limit",
            ));
        }
        unique_json::validate(line).map_err(|_| invalid())?;
        let header: Header = serde_json::from_str(line).map_err(|_| invalid())?;
        let (version, row_profile) = match header {
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
            } => {
                original_sources.insert(source_key(&source), source.status);
                (schema_version, Some(source.profile_id))
            }
        };
        if version != Some(SCHEMA_VERSION) || row_profile.as_deref() != Some(profile) {
            return Err(AppError::configuration(
                "memory candidate profile or version does not match",
            ));
        }
    }
    // Parser diagnostics can contain untrusted private values.
    let document = document::parse(content).map_err(|_| invalid())?;
    for source in &document.provenance.sources {
        if original_sources.get(&source_key(source)) == Some(&MemoryEntitySourceStatus::Active)
            && source.status != MemoryEntitySourceStatus::Active
        {
            return Err(AppError::configuration(
                "memory candidate has unstable active sources",
            ));
        }
    }
    Ok(Input { document, rows })
}

/// Construct a separate candidate without opening files or changing any store.
///
/// Both inputs must be v2 JSONL for the explicitly supplied profile. Empty
/// inputs have no profile label and are reported as such; two empty inputs are
/// rejected. Each input's active sources must already be valid on its own.
/// Cross-input identity collisions and overlapping vector space names are
/// rejected: a matching name and dimensions do not establish semantic
/// compatibility. Input bytes are retained verbatim, with one newline inserted
/// only if needed between files. No data is merged into, or activated in, a store.
/// Unknown fields survive in the returned bytes, but a later store rewrite may
/// not preserve them; callers must not claim that adoption is lossless.
pub fn merge_profile_candidates(
    base: &[u8],
    incoming: &[u8],
    profile: &str,
) -> Result<MemoryMergeCandidate, AppError> {
    if profile.trim().is_empty() || profile.chars().count() > 128 {
        return Err(AppError::configuration("invalid memory candidate profile"));
    }
    let base_input = parse_input(base, profile)?;
    let incoming_input = parse_input(incoming, profile)?;
    let rows = base_input
        .rows
        .checked_add(incoming_input.rows)
        .ok_or_else(invalid)?;
    if rows == 0 || rows > MAX_ROWS {
        return Err(AppError::configuration(
            "memory candidate row count is unsupported",
        ));
    }
    let base_records: BTreeSet<_> = base_input.document.records.iter().map(|r| r.id).collect();
    if incoming_input
        .document
        .records
        .iter()
        .any(|r| base_records.contains(&r.id))
    {
        return Err(AppError::configuration("memory identities overlap"));
    }
    let base_entities: BTreeSet<_> = base_input
        .document
        .provenance
        .candidates
        .iter()
        .map(|c| c.id)
        .collect();
    if incoming_input
        .document
        .provenance
        .candidates
        .iter()
        .any(|c| base_entities.contains(&c.id))
    {
        return Err(AppError::configuration("entity identities overlap"));
    }
    let base_sources: BTreeSet<_> = base_input
        .document
        .provenance
        .sources
        .iter()
        .map(source_key)
        .collect();
    if incoming_input
        .document
        .provenance
        .sources
        .iter()
        .any(|s| base_sources.contains(&source_key(s)))
    {
        return Err(AppError::configuration("source associations overlap"));
    }
    let base_spaces = embedding_spaces(&base_input)?;
    let incoming_spaces = embedding_spaces(&incoming_input)?;
    if incoming_spaces
        .keys()
        .any(|space| base_spaces.contains_key(space))
    {
        return Err(AppError::configuration(
            "memory embedding spaces overlap without verified identity",
        ));
    }
    let space_count = base_spaces.len() + incoming_spaces.len();
    if space_count > MAX_SPACES {
        return Err(AppError::configuration(
            "memory embedding spaces exceed limit",
        ));
    }
    let separator = usize::from(!base.is_empty() && !incoming.is_empty() && !base.ends_with(b"\n"));
    let size = base
        .len()
        .checked_add(separator)
        .and_then(|n| n.checked_add(incoming.len()))
        .ok_or_else(invalid)?;
    if size > MAX_BYTES {
        return Err(AppError::configuration(
            "merged memory candidate exceeds size limit",
        ));
    }
    let mut content = Vec::with_capacity(size);
    content.extend_from_slice(base);
    if separator != 0 {
        content.push(b'\n');
    }
    content.extend_from_slice(incoming);
    // Recheck combined identities and source semantics without serializing them.
    let merged = parse_input(&content, profile)?;
    let report = MemoryMergeReport {
        base_memory_records: base_input.document.records.len(),
        incoming_memory_records: incoming_input.document.records.len(),
        entity_candidates: merged.document.provenance.candidates.len(),
        source_associations: merged.document.provenance.sources.len(),
        inactive_source_associations: merged
            .document
            .provenance
            .sources
            .iter()
            .filter(|source| source.status != MemoryEntitySourceStatus::Active)
            .count(),
        distinct_embedding_spaces: space_count,
        base_empty: base_input.rows == 0,
        incoming_empty: incoming_input.rows == 0,
    };
    Ok(MemoryMergeCandidate { content, report })
}

fn embedding_spaces(input: &Input) -> Result<BTreeMap<&str, usize>, AppError> {
    let mut spaces = BTreeMap::new();
    for record in &input.document.records {
        if let Some(embedding) = &record.embedding
            && let Some(dimensions) =
                spaces.insert(embedding.space.as_str(), embedding.values.len())
            && dimensions != embedding.values.len()
        {
            return Err(AppError::configuration(
                "memory embedding dimensions conflict",
            ));
        }
    }
    Ok(spaces)
}

#[cfg(test)]
#[path = "candidate_merge_tests.rs"]
mod tests;
