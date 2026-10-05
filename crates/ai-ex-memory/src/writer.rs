use super::*;

impl MemoryStore {
    /// Call only while holding write_lock. Readers remain independent snapshots;
    /// the first mutation refreshes that snapshot under an OS writer lease.
    pub(super) async fn ensure_writer(&self) -> Result<(), AppError> {
        let mut lease = self.inner.writer_lease.lock().await;
        if lease.is_some() {
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
        let refreshed = read_records(&self.inner.path).await?;
        *self.inner.records.write().await = refreshed;
        *lease = Some(file);
        Ok(())
    }
}

pub(super) async fn read_records(path: &Path) -> Result<Vec<MemoryRecord>, AppError> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => return Err(AppError::unavailable(error.to_string())),
    };
    let mut records = Vec::new();
    for (line_number, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut record: MemoryRecord = serde_json::from_str(line).map_err(|error| {
            AppError::protocol(format!("invalid memory line {}: {error}", line_number + 1))
        })?;
        record.rebuild()?;
        records.push(record);
    }
    Ok(records)
}
