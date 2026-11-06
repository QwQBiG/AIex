use super::*;

#[derive(Clone, Copy)]
pub(super) enum WriteIntent {
    Rewrite,
    AppendUnlessLegacy,
}

pub(super) fn check_rewrite(records: &[MemoryRecord]) -> Result<(), AppError> {
    if records.iter().any(|record| record.saved_entities_changed) {
        return Err(AppError::invalid_transition(
            ai_ex_domain::MEMORY_SAVED_ENTITIES_CHANGED,
        ));
    }
    Ok(())
}

impl MemoryStore {
    /// Call only while holding write_lock. Readers remain independent snapshots;
    /// the first mutation refreshes that snapshot under an OS writer lease.
    pub(super) async fn ensure_writer(&self, intent: WriteIntent) -> Result<(), AppError> {
        let mut lease = self.inner.writer_lease.lock().await;
        if lease.is_some() {
            if matches!(intent, WriteIntent::Rewrite)
                || self.inner.legacy_format.load(Ordering::Acquire)
            {
                check_rewrite(&self.inner.records.read().await)?;
            }
            return Ok(());
        }
        if let Some(parent) = self.inner.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| AppError::unavailable(error.to_string()))?;
        }
        let mut lock_path = self.inner.path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(lock_path))
            .map_err(|error| AppError::unavailable(error.to_string()))?;
        file.try_lock().map_err(|_| {
            AppError::unavailable(
                "memory has another active writer; stop it before editing or rebuilding this store",
            )
        })?;
        let refreshed = document::read(&self.inner.path).await?;
        if matches!(intent, WriteIntent::Rewrite) || refreshed.legacy {
            check_rewrite(&refreshed.records)?;
        }
        let mut records = self.inner.records.write().await;
        let mut provenance = self.inner.provenance.write().await;
        *records = refreshed.records;
        *provenance = refreshed.provenance;
        self.inner
            .legacy_format
            .store(refreshed.legacy, Ordering::Release);
        *lease = Some(file);
        Ok(())
    }

    /// Defense in depth for callers that serialize the complete shared store.
    pub(super) async fn check_shared_rewrite(&self) -> Result<(), AppError> {
        check_rewrite(&self.inner.records.read().await)
    }

    pub async fn format_backup_path(&self) -> Option<PathBuf> {
        self.inner.format_backup.read().await.clone()
    }

    /// Only called after an admitted mutation, under the writer lease.
    pub(super) async fn prepare_format_backup(&self) -> Result<(), AppError> {
        if !self.inner.legacy_format.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut backup = self.inner.format_backup.write().await;
        if backup.is_none() {
            let mut path = self.inner.path.as_os_str().to_os_string();
            path.push(format!(".v1-backup-{}.jsonl", Uuid::new_v4()));
            let path = PathBuf::from(path);
            let original = tokio::fs::read(&self.inner.path)
                .await
                .map_err(|e| AppError::unavailable(e.to_string()))?;
            write_new_synced(&path, &original)
                .await
                .map_err(|e| AppError::unavailable(format!("memory format backup failed: {e}")))?;
            *backup = Some(path);
        }
        Ok(())
    }
}
