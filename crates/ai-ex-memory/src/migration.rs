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
    pub changed_saved_entity_records: usize,
}

fn report(document: &document::Document) -> MemoryMigrationReport {
    MemoryMigrationReport {
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
        changed_saved_entity_records: document
            .records
            .iter()
            .filter(|record| record.saved_entities_changed)
            .count(),
    }
}

/// Inspect without writing or producing an upgraded document.
pub fn inspect_memory_jsonl(content: &str) -> Result<MemoryMigrationReport, AppError> {
    Ok(report(&document::parse(content)?))
}

/// Return a new document only when saved entities survive reconstruction.
pub fn migrate_jsonl(content: &str) -> Result<(String, MemoryMigrationReport), AppError> {
    let document = document::parse(content)?;
    let report = report(&document);
    writer::check_rewrite(&document.records)?;
    let output = provenance::serialize_document(&document.records, &document.provenance)?;
    Ok((output, report))
}
