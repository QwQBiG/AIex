use super::*;
use ai_ex_domain::{
    MemoryEntity, MemoryEntityEvidence, MemoryEntityGroup, MemoryEntityPage, MemoryMedia,
    MemoryModality, MemoryRequest, MemoryResponse,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct MediaStats {
    pub total: usize,
    pub text: usize,
    pub audio: usize,
    pub visual: usize,
    pub multimodal: usize,
}

/// The old multimodal module's local importance heuristic; no media inference.
pub fn observation_media(
    text: &str,
    audio_features: Option<Map<String, Value>>,
    visual_features: Option<Map<String, Value>>,
    tags: Vec<String>,
    metadata: Map<String, Value>,
) -> Result<MemoryMedia, AppError> {
    let modality = match (audio_features.is_some(), visual_features.is_some()) {
        (false, false) => MemoryModality::Text,
        (true, false) => MemoryModality::Audio,
        (false, true) => MemoryModality::Visual,
        (true, true) => MemoryModality::Multimodal,
    };
    let entities = audio_features
        .as_ref()
        .and_then(|features| features.get("entities"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
        + visual_features
            .as_ref()
            .and_then(|features| features.get("objects"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
    let importance =
        (policy::legacy_multimodal_importance(modality, text.chars().count(), entities) * 1000.0)
            .round() as u16;
    let media = MemoryMedia {
        modality,
        tags,
        audio_features,
        visual_features,
        metadata,
        importance,
    };
    media.validate()?;
    Ok(media)
}

fn modality(record: &MemoryRecord) -> MemoryModality {
    record
        .knowledge
        .media
        .as_ref()
        .map_or(MemoryModality::Text, |media| media.modality)
}
fn importance(record: &MemoryRecord) -> u16 {
    record
        .knowledge
        .media
        .as_ref()
        .map_or(0, |media| media.importance)
}

impl MemoryStore {
    pub async fn media_stats(&self) -> MediaStats {
        let profile_id = self.inner.profile_id.read().await;
        let records = self.inner.records.read().await;
        let mut stats = MediaStats::default();
        for record in records
            .iter()
            .filter(|record| record.profile_id == *profile_id)
        {
            stats.total += 1;
            match modality(record) {
                MemoryModality::Text => stats.text += 1,
                MemoryModality::Audio => stats.audio += 1,
                MemoryModality::Visual => stats.visual += 1,
                MemoryModality::Multimodal => stats.multimodal += 1,
            }
        }
        stats
    }

    pub async fn clear_media(&mut self, target: MemoryModality) -> Result<usize, AppError> {
        if !self.inner.enabled {
            return Ok(0);
        }
        let profile_id = self.inner.profile_id.read().await;
        let _guard = self.inner.write_lock.lock().await;
        self.ensure_writer().await?;
        let records = self.inner.records.read().await;
        let retained: Vec<_> = records
            .iter()
            .filter(|record| record.profile_id != *profile_id || modality(record) != target)
            .cloned()
            .collect();
        let removed = records.len() - retained.len();
        drop(records);
        if removed > 0 {
            self.replace_records(retained).await?;
        }
        Ok(removed)
    }

    pub(super) async fn knowledge_query(
        &self,
        request: &MemoryRequest,
        profile_id: &str,
    ) -> Result<Option<MemoryResponse>, AppError> {
        match request {
            MemoryRequest::Entities {
                query,
                offset,
                limit,
                ..
            } => {
                let records = self.inner.records.read().await;
                let page = entity_page(
                    &records,
                    profile_id,
                    self.inner.enabled,
                    query,
                    *offset,
                    *limit,
                )?;
                Ok(Some(MemoryResponse::Entities(page)))
            }
            MemoryRequest::Evidence {
                key, offset, limit, ..
            } => {
                let records = self.inner.records.read().await;
                let mut matches: Vec<_> = records
                    .iter()
                    .enumerate()
                    .filter(|(_, record)| {
                        record.profile_id == profile_id
                            && record
                                .knowledge
                                .entities
                                .iter()
                                .any(|entity| entity.key == *key)
                    })
                    .collect();
                matches.sort_by_key(|(index, record)| {
                    std::cmp::Reverse((record.updated_ms.unwrap_or(record.created_ms), *index))
                });
                let page = management::page_for(
                    profile_id,
                    self.inner.enabled,
                    *offset,
                    *limit,
                    matches.into_iter().map(|(_, record)| record).collect(),
                )?;
                Ok(Some(MemoryResponse::Page(page)))
            }
            MemoryRequest::QueryMedia {
                query,
                modality: filter,
                tags,
                min_importance,
                offset,
                limit,
                ..
            } => {
                let query = query.trim().to_lowercase();
                let tags: Vec<_> = tags.iter().map(|tag| tag.trim().to_lowercase()).collect();
                let records = self.inner.records.read().await;
                let mut matches: Vec<_> = records
                    .iter()
                    .enumerate()
                    .filter(|(_, record)| record.profile_id == profile_id)
                    .filter(|(_, record)| filter.is_none_or(|target| target == modality(record)))
                    .filter(|(_, record)| importance(record) >= *min_importance)
                    .filter(|(_, record)| record.lowercase_text.contains(&query))
                    .filter(|(_, record)| {
                        tags.is_empty()
                            || record.knowledge.media.as_ref().is_some_and(|media| {
                                media
                                    .tags
                                    .iter()
                                    .any(|tag| tags.contains(&tag.trim().to_lowercase()))
                            })
                    })
                    .collect();
                matches.sort_by_key(|(index, record)| {
                    std::cmp::Reverse((
                        importance(record),
                        record.updated_ms.unwrap_or(record.created_ms),
                        *index,
                    ))
                });
                let page = management::page_for(
                    profile_id,
                    self.inner.enabled,
                    *offset,
                    *limit,
                    matches.into_iter().map(|(_, record)| record).collect(),
                )?;
                Ok(Some(MemoryResponse::Page(page)))
            }
            _ => Ok(None),
        }
    }
}

fn entity_page(
    records: &[MemoryRecord],
    profile_id: &str,
    enabled: bool,
    query: &str,
    offset: usize,
    limit: usize,
) -> Result<MemoryEntityPage, AppError> {
    type EvidenceRef<'a> = (usize, &'a MemoryRecord, &'a MemoryEntity);
    let mut groups: BTreeMap<&str, Vec<EvidenceRef<'_>>> = BTreeMap::new();
    for (index, record) in records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.profile_id == profile_id)
    {
        for entity in &record.knowledge.entities {
            groups
                .entry(&entity.key)
                .or_default()
                .push((index, record, entity));
        }
    }
    let query = query.trim().to_lowercase();
    let mut groups: Vec<_> = groups
        .into_iter()
        .filter(|(key, evidence)| {
            query.is_empty()
                || key.to_lowercase().contains(&query)
                || evidence.iter().any(|(_, _, entity)| {
                    entity.name.to_lowercase().contains(&query)
                        || entity.value.to_lowercase().contains(&query)
                })
        })
        .map(|(key, mut evidence)| {
            evidence.sort_by_key(|(index, record, entity)| {
                std::cmp::Reverse((
                    record.source != MemorySource::Automatic,
                    record.updated_ms.unwrap_or(record.created_ms),
                    *index,
                    entity.confidence,
                ))
            });
            let (_, _, current) = evidence[0];
            let first = evidence
                .iter()
                .map(|(_, record, _)| record.created_ms)
                .min()
                .unwrap_or(0);
            let last = evidence
                .iter()
                .map(|(_, record, _)| record.updated_ms.unwrap_or(record.created_ms))
                .max()
                .unwrap_or(0);
            let total_mentions = evidence.len();
            let evidence = evidence
                .into_iter()
                .take(32)
                .map(|(_, record, entity)| MemoryEntityEvidence {
                    memory_id: record.id,
                    revision: record.revision,
                    source: record.source,
                    value: entity.value.clone(),
                    confidence: entity.confidence,
                })
                .collect();
            MemoryEntityGroup {
                key: key.to_owned(),
                kind: current.kind,
                name: current.name.clone(),
                current_value: current.value.clone(),
                first_mentioned_ms: first.min(u64::MAX as u128) as u64,
                last_updated_ms: last.min(u64::MAX as u128) as u64,
                total_mentions,
                evidence,
            }
        })
        .collect();
    groups.sort_by(|a, b| {
        b.last_updated_ms
            .cmp(&a.last_updated_ms)
            .then_with(|| a.key.cmp(&b.key))
    });
    let mut page = MemoryEntityPage {
        profile_id: profile_id.to_owned(),
        enabled,
        total: groups.len(),
        offset,
        entities: Vec::new(),
    };
    for group in groups.into_iter().skip(offset).take(limit) {
        page.entities.push(group);
        if serde_json::to_vec(&page)
            .map_err(|error| AppError::protocol(error.to_string()))?
            .len()
            <= 48 * 1024
        {
            continue;
        }
        if page.entities.len() == 1 {
            while page.entities[0].evidence.len() > 1
                && serde_json::to_vec(&page)
                    .map_err(|error| AppError::protocol(error.to_string()))?
                    .len()
                    > 48 * 1024
            {
                page.entities[0].evidence.pop();
            }
        } else {
            page.entities.pop();
        }
        break;
    }
    Ok(page)
}
