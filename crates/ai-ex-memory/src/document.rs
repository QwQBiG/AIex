use serde::Deserialize;

use super::provenance::{Provenance, SCHEMA_VERSION};
use super::*;

pub(super) struct Document {
    pub records: Vec<MemoryRecord>,
    pub provenance: Provenance,
    pub legacy: bool,
}

#[derive(Deserialize)]
struct Header {
    schema_version: Option<u32>,
    record_type: Option<String>,
}

#[derive(Deserialize)]
struct CandidateRow {
    candidate: ai_ex_domain::ImportedMemoryEntity,
}

#[derive(Deserialize)]
struct SourceRow {
    source: ai_ex_domain::MemoryEntitySource,
}

pub(super) async fn read(path: &Path) -> Result<Document, AppError> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => return Err(AppError::unavailable(error.to_string())),
    };
    parse(&content)
}

pub(super) fn parse(content: &str) -> Result<Document, AppError> {
    let mut document = Document {
        records: Vec::new(),
        provenance: Provenance::default(),
        legacy: false,
    };
    let mut ids = std::collections::BTreeSet::new();
    for (line_number, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let invalid = |error: String| {
            AppError::protocol(format!("invalid memory line {}: {error}", line_number + 1))
        };
        let header: Header = serde_json::from_str(line).map_err(|e| invalid(e.to_string()))?;
        let kind = header.record_type.as_deref();
        if kind.is_none() && header.schema_version.is_some() {
            return Err(invalid(
                "versioned memory record requires a record type".into(),
            ));
        }
        if kind.is_some() && header.schema_version != Some(SCHEMA_VERSION) {
            return Err(invalid("unsupported memory document version".into()));
        }
        match kind {
            None | Some("memory") => {
                document.legacy |= kind.is_none();
                let mut record: MemoryRecord =
                    serde_json::from_str(line).map_err(|e| invalid(e.to_string()))?;
                let saved_entities = std::mem::take(&mut record.knowledge.entities);
                record.rebuild()?;
                record.saved_entities_changed =
                    !saved_entities.is_empty() && saved_entities != record.knowledge.entities;
                if !ids.insert((record.profile_id.clone(), record.id)) {
                    return Err(invalid("duplicate memory identity".into()));
                }
                document.records.push(record);
            }
            Some("entity_candidate") => {
                let row: CandidateRow =
                    serde_json::from_str(line).map_err(|e| invalid(e.to_string()))?;
                document.provenance.candidates.push(row.candidate);
            }
            Some("entity_source") => {
                let row: SourceRow =
                    serde_json::from_str(line).map_err(|e| invalid(e.to_string()))?;
                document.provenance.sources.push(row.source);
            }
            Some(_) => return Err(invalid("unknown memory document record type".into())),
        }
    }
    document.provenance.validate()?;
    // Stale or absent references remain visible as candidates, never as live evidence.
    document.provenance.reconcile(&[], &document.records);
    Ok(document)
}
