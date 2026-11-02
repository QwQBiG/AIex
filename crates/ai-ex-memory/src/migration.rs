use serde::Serialize;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryMigrationReport {
    pub output_schema_version: u32,
    pub contains_legacy_records: bool,
    pub memory_records: usize,
    pub imported_entity_candidates: usize,
    pub source_associations: usize,
    pub inactive_source_associations: usize,
}

/// Validates an existing store without acquiring a writer or touching its files.
/// The caller publishes the returned v2 content only to an explicitly chosen output.
pub fn migrate_jsonl(content: &str) -> Result<(String, MemoryMigrationReport), AppError> {
    let document = document::parse(content)?;
    let report = MemoryMigrationReport {
        output_schema_version: provenance::SCHEMA_VERSION,
        contains_legacy_records: document.legacy,
        memory_records: document.records.len(),
        imported_entity_candidates: document.provenance.candidates.len(),
        source_associations: document.provenance.sources.len(),
        inactive_source_associations: document
            .provenance
            .sources
            .iter()
            .filter(|source| source.status != ai_ex_domain::MemoryEntitySourceStatus::Active)
            .count(),
    };
    let output = provenance::serialize_document(&document.records, &document.provenance)?;
    Ok((output, report))
}
