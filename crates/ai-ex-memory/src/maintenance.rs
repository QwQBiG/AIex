use super::*;
use crate::policy::{PolicyMemory, legacy_adjusted_importance, legacy_cleanup_candidates};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MediaMaintenanceReport {
    pub profile: String,
    pub considered: usize,
    pub protected_confirmed: usize,
    pub updated: usize,
    pub archived: usize,
    pub applied: bool,
    pub entries: Vec<MediaMaintenanceEntry>,
}
#[derive(Debug, Serialize)]
pub struct MediaMaintenanceEntry {
    pub id: Uuid,
    pub revision: u64,
    pub before: u16,
    pub after: u16,
    pub archive: bool,
}

impl MemoryStore {
    /// Explicit maintenance: adjust automatic media importance, then preview cleanup
    /// using those adjusted values. Confirmed notes/corrections are always protected.
    /// Applying requires a new archive of every original affected record before replacement.
    pub async fn maintain_media(
        &mut self,
        now_ms: u128,
        max_memories: usize,
        apply: bool,
        archive: Option<&Path>,
    ) -> Result<MediaMaintenanceReport, AppError> {
        if !self.inner.enabled {
            return Err(AppError::unavailable("memory is disabled"));
        }
        if max_memories == 0 {
            return Err(AppError::configuration("media capacity must be positive"));
        }
        if !apply && archive.is_some() {
            return Err(AppError::configuration(
                "archive requires applied maintenance",
            ));
        }
        let profile = self.inner.profile_id.read().await;
        // Preview neither claims a writer nor creates a lock/backup file.
        let _guard = if apply {
            Some(self.inner.write_lock.lock().await)
        } else {
            None
        };
        if apply {
            self.ensure_writer().await?;
        }
        let mut records = self.inner.records.read().await.clone();
        let mut report = MediaMaintenanceReport {
            profile: profile.clone(),
            considered: 0,
            protected_confirmed: 0,
            updated: 0,
            archived: 0,
            applied: false,
            entries: Vec::new(),
        };
        let mut positions = Vec::new();
        let mut policies = Vec::new();
        for (index, record) in records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.profile_id == *profile)
        {
            let Some(media) = &record.knowledge.media else {
                continue;
            };
            if record.source != MemorySource::Automatic {
                report.protected_confirmed += 1;
                continue;
            }
            let count = record
                .legacy_metadata
                .as_ref()
                .and_then(|m| m.get("access_count"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let score = legacy_adjusted_importance(
                f64::from(media.importance) / 1000.0,
                count,
                record.created_ms,
                now_ms,
            )?;
            positions.push(index);
            policies.push(PolicyMemory {
                created_ms: record.created_ms,
                importance: score,
            });
        }
        report.considered = positions.len();
        let removals = legacy_cleanup_candidates(&policies, now_ms, max_memories)?;
        let mut originals = Vec::new();
        let mut remove_ids = std::collections::BTreeSet::new();
        for (position, index) in positions.into_iter().enumerate() {
            let record = &mut records[index];
            let before = record
                .knowledge
                .media
                .as_ref()
                .expect("selected media")
                .importance;
            let after = (policies[position].importance * 1000.0).round() as u16;
            let remove = removals.contains(&position);
            if before == after && !remove {
                continue;
            }
            originals.push(record.clone());
            report.entries.push(MediaMaintenanceEntry {
                id: record.id,
                revision: record.revision,
                before,
                after,
                archive: remove,
            });
            if remove {
                remove_ids.insert(record.id);
                report.archived += 1;
            } else {
                record.entry.revision = record
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| AppError::invalid_transition("memory revision limit reached"))?;
                record.entry.updated_ms =
                    Some(now_ms.max(record.updated_ms.unwrap_or(record.created_ms)));
                record
                    .knowledge
                    .media
                    .as_mut()
                    .expect("selected media")
                    .importance = after;
                record.rebuild()?;
                report.updated += 1;
            }
        }
        if apply && !originals.is_empty() {
            let archive = archive.ok_or_else(|| {
                AppError::configuration("changed media require --archive-memory with a new path")
            })?;
            let content = serialize_records(&originals)?;
            if let Some(parent) = archive.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| AppError::unavailable(e.to_string()))?;
            }
            write_new_synced(archive, content.as_bytes())
                .await
                .map_err(|e| AppError::unavailable(e.to_string()))?;
            records.retain(|record| {
                !(record.profile_id == *profile && remove_ids.contains(&record.id))
            });
            self.replace_records(records).await?;
            self.inner
                .recent_memory_ids
                .write()
                .await
                .retain(|(p, id)| p != &*profile || !remove_ids.contains(id));
            report.applied = true;
        }
        Ok(report)
    }
}
