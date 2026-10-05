use ai_ex_domain::{MemoryPage, MemoryRequest, MemoryResponse};

use super::*;

impl MemoryStore {
    pub async fn manage(&mut self, request: MemoryRequest) -> Result<MemoryResponse, AppError> {
        request.validate()?;
        let profile_id = self.inner.profile_id.read().await;
        if request.profile_id() != *profile_id {
            return Err(AppError::invalid_transition(
                "memory profile changed; reload the current character's memories",
            ));
        }
        if let Some(response) = self.knowledge_query(&request, &profile_id).await? {
            return Ok(response);
        }
        if let MemoryRequest::List {
            query,
            kind,
            offset,
            limit,
            ..
        } = request
        {
            let query = query.trim().to_lowercase();
            let records = self.inner.records.read().await;
            let mut matches: Vec<_> = records
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.profile_id == *profile_id)
                .filter(|(_, entry)| kind.is_none_or(|kind| kind == entry.kind))
                .filter(|(_, entry)| entry.lowercase_text.contains(&query))
                .collect();
            matches.sort_by_key(|(index, entry)| {
                std::cmp::Reverse((entry.updated_ms.unwrap_or(entry.created_ms), *index))
            });
            let page = page_for(
                &profile_id,
                self.inner.enabled,
                offset,
                limit,
                matches.into_iter().map(|(_, entry)| entry).collect(),
            )?;
            return Ok(MemoryResponse::Page(page));
        }
        if !self.inner.enabled {
            return Err(AppError::unavailable(
                "memory is disabled in the service configuration",
            ));
        }
        let _write_guard = self.inner.write_lock.lock().await;
        self.ensure_writer().await?;
        let mut records = self.inner.records.read().await.clone();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let media = match &request {
            MemoryRequest::RememberMedia { media, .. }
            | MemoryRequest::CorrectMedia { media, .. } => Some(media.clone()),
            _ => None,
        };
        match request {
            MemoryRequest::Remember { text, .. } | MemoryRequest::RememberMedia { text, .. } => {
                let mut record = MemoryRecord::new(MemoryEntry {
                    id: Uuid::new_v4(),
                    profile_id: profile_id.clone(),
                    turn_id: TurnId::new(),
                    created_ms: now,
                    updated_ms: None,
                    kind: MemoryKind::Persona,
                    user_text: text.trim().to_owned(),
                    assistant_text: String::new(),
                    source: MemorySource::UserNote,
                    revision: 1,
                });
                record.knowledge.media = media;
                self.enrich(&mut record).await?;
                records.push(record);
            }
            MemoryRequest::Correct {
                id,
                expected_revision,
                text,
                ..
            }
            | MemoryRequest::CorrectMedia {
                id,
                expected_revision,
                text,
                ..
            } => {
                let index = find_record(&records, &profile_id, id, expected_revision)?;
                let entry = &mut records[index].entry;
                entry.revision = entry
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| AppError::invalid_transition("memory revision limit reached"))?;
                entry.user_text = text.trim().to_owned();
                entry.assistant_text.clear();
                entry.kind = MemoryKind::Persona;
                entry.source = MemorySource::UserCorrection;
                entry.updated_ms = Some(now.max(entry.updated_ms.unwrap_or(entry.created_ms)));
                records[index].legacy_metadata = None;
                records[index].knowledge.media = media;
                self.enrich(&mut records[index]).await?;
            }
            MemoryRequest::Forget {
                id,
                expected_revision,
                ..
            } => {
                let index = find_record(&records, &profile_id, id, expected_revision)?;
                records.remove(index);
            }
            _ => unreachable!("read-only requests handled before mutation"),
        }
        self.replace_records(records).await?;
        Ok(MemoryResponse::Changed)
    }

    pub(super) async fn replace_records(&self, records: Vec<MemoryRecord>) -> Result<(), AppError> {
        let content = serialize_records(&records)?;
        if let Some(parent) = self.inner.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| AppError::unavailable(error.to_string()))?;
        }
        let temporary = self
            .inner
            .path
            .with_extension(format!("{}.tmp", Uuid::new_v4()));
        write_new_synced(&temporary, content.as_bytes())
            .await
            .map_err(|error| AppError::unavailable(error.to_string()))?;
        if let Err(error) = tokio::fs::rename(&temporary, &self.inner.path).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(AppError::unavailable(error.to_string()));
        }
        *self.inner.records.write().await = records;
        Ok(())
    }
}

fn serialized_size(page: &MemoryPage) -> Result<usize, AppError> {
    serde_json::to_vec(page)
        .map(|bytes| bytes.len())
        .map_err(|error| AppError::protocol(error.to_string()))
}

fn preview_entry(entry: &MemoryRecord) -> MemoryEntry {
    let mut preview = entry.entry.clone();
    let user_budget = 4000 - entry.assistant_text.chars().take(2000).count();
    preview.user_text = entry.user_text.chars().take(user_budget).collect();
    let assistant_budget = 4000 - preview.user_text.chars().count();
    preview.assistant_text = entry
        .assistant_text
        .chars()
        .take(assistant_budget)
        .collect();
    preview
}

fn find_record(
    records: &[MemoryRecord],
    profile_id: &str,
    id: Uuid,
    revision: u64,
) -> Result<usize, AppError> {
    let index = records
        .iter()
        .position(|entry| entry.profile_id == profile_id && entry.id == id)
        .ok_or_else(|| AppError::invalid_transition("memory no longer exists in this character"))?;
    if records[index].revision != revision {
        return Err(AppError::invalid_transition(
            "memory changed; reload it before editing",
        ));
    }
    Ok(index)
}

pub(super) fn page_for(
    profile_id: &str,
    enabled: bool,
    offset: usize,
    limit: usize,
    records: Vec<&MemoryRecord>,
) -> Result<MemoryPage, AppError> {
    let mut page = MemoryPage {
        profile_id: profile_id.to_owned(),
        enabled,
        total: records.len(),
        offset,
        entries: Vec::new(),
        truncated_ids: Vec::new(),
        knowledge: Default::default(),
    };
    for entry in records.into_iter().skip(offset).take(limit) {
        page.entries.push(entry.entry.clone());
        if !record::knowledge_empty(&entry.knowledge) {
            page.knowledge.insert(entry.id, entry.knowledge.clone());
        }
        if serialized_size(&page)? <= 48 * 1024 {
            continue;
        }
        page.entries.pop();
        page.knowledge.remove(&entry.id);
        if page.entries.is_empty() {
            page.entries.push(preview_entry(entry));
            if !record::knowledge_empty(&entry.knowledge) {
                page.knowledge.insert(entry.id, entry.knowledge.clone());
            }
            page.truncated_ids.push(entry.id);
            if serialized_size(&page)? > 48 * 1024 {
                return Err(AppError::protocol(
                    "memory preview exceeds transport budget",
                ));
            }
        }
        break;
    }
    Ok(page)
}
