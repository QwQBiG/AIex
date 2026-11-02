use ai_ex_domain::{
    MemoryEntityEvidence, MemoryEntityGroup, MemoryEntityKind, MemoryEntityOrigin,
    MemoryEntitySourceStatus,
};

use super::provenance::Provenance;
use super::*;

fn preview(value: &str, limit: usize) -> String {
    let mut text: String = value.chars().take(limit).collect();
    if value.chars().count() > limit {
        text.push('…');
    }
    text
}

pub(super) fn source_ids(
    provenance: &Provenance,
    profile: &str,
    key: &str,
) -> Option<std::collections::BTreeSet<Uuid>> {
    let raw = key.strip_prefix("imported:")?;
    let id = Uuid::parse_str(raw).ok();
    Some(
        provenance
            .sources
            .iter()
            .filter(|source| {
                source.profile_id == profile
                    && Some(source.entity_id) == id
                    && matches!(
                        source.status,
                        MemoryEntitySourceStatus::Active | MemoryEntitySourceStatus::Corrected
                    )
            })
            .filter_map(|source| source.memory_id)
            .collect(),
    )
}

pub(super) fn groups(
    provenance: &Provenance,
    records: &[MemoryRecord],
    profile: &str,
    query: &str,
) -> Vec<MemoryEntityGroup> {
    let records: std::collections::BTreeMap<_, _> = records
        .iter()
        .filter(|record| record.profile_id == profile)
        .map(|record| (record.id, record))
        .collect();
    let mut source_groups: std::collections::BTreeMap<_, Vec<_>> =
        std::collections::BTreeMap::new();
    for source in provenance
        .sources
        .iter()
        .filter(|source| source.profile_id == profile)
    {
        source_groups
            .entry(source.entity_id)
            .or_default()
            .push(source);
    }
    let query = query.trim().to_lowercase();
    provenance
        .candidates
        .iter()
        .filter(|candidate| candidate.profile_id == profile)
        .filter_map(|candidate| {
            let metadata = candidate.legacy_record.get("metadata");
            let name = candidate
                .entity
                .as_ref()
                .map(|e| e.name.as_str())
                .or_else(|| metadata?.get("name")?.as_str())
                .or_else(|| candidate.legacy_record.get("id")?.as_str())
                .unwrap_or("未解析导入候选");
            let value = candidate
                .entity
                .as_ref()
                .map(|e| e.value.as_str())
                .or_else(|| metadata?.get("value")?.as_str())
                .or_else(|| candidate.legacy_record.get("document")?.as_str())
                .unwrap_or("");
            if !query.is_empty()
                && !name.to_lowercase().contains(&query)
                && !value.to_lowercase().contains(&query)
            {
                return None;
            }
            let mut sources = source_groups
                .get(&candidate.id)
                .cloned()
                .unwrap_or_default();
            sources.sort_by_key(|source| {
                (
                    source.status != MemoryEntitySourceStatus::Active,
                    &source.legacy_memory_id,
                )
            });
            let withdrawn = !sources.is_empty()
                && sources.iter().all(|source| {
                    matches!(
                        source.status,
                        MemoryEntitySourceStatus::Corrected | MemoryEntitySourceStatus::Forgotten
                    )
                });
            let confidence = candidate
                .entity
                .as_ref()
                .map_or(0, |entity| entity.confidence);
            let mut evidence: Vec<_> = sources
                .iter()
                .take(32)
                .map(|source| {
                    let record = source.memory_id.and_then(|id| records.get(&id));
                    MemoryEntityEvidence {
                        memory_id: source.memory_id.unwrap_or_else(Uuid::nil),
                        revision: source.memory_revision.unwrap_or(0),
                        source: record.map_or(MemorySource::Automatic, |record| record.source),
                        value: preview(value, 4096),
                        confidence,
                        source_status: source.status,
                        imported_entity_id: Some(candidate.id),
                        legacy_memory_id: Some(preview(&source.legacy_memory_id, 512)),
                    }
                })
                .collect();
            if evidence.is_empty() {
                evidence.push(MemoryEntityEvidence {
                    memory_id: Uuid::nil(),
                    revision: 0,
                    source: MemorySource::Automatic,
                    value: preview(value, 4096),
                    confidence,
                    source_status: MemoryEntitySourceStatus::Unresolved,
                    imported_entity_id: Some(candidate.id),
                    legacy_memory_id: None,
                });
            }
            Some(MemoryEntityGroup {
                key: format!("imported:{}", candidate.id),
                kind: candidate
                    .entity
                    .as_ref()
                    .map_or(MemoryEntityKind::Fact, |entity| entity.kind),
                name: preview(name, 256),
                current_value: if withdrawn {
                    String::new()
                } else {
                    preview(value, 4096)
                },
                first_mentioned_ms: candidate.first_mentioned_ms.unwrap_or(0),
                last_updated_ms: candidate.last_updated_ms.unwrap_or(0),
                evidence,
                total_mentions: sources.len(),
                subject: candidate.subject.clone(),
                origin: MemoryEntityOrigin::Imported,
            })
        })
        .collect()
}
