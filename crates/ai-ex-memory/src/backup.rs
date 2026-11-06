use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ai_ex_domain::{AppError, MemoryBackupState, MemoryBackupStatus, MemoryResponse};
use uuid::Uuid;

use super::{MemoryStore, document, provenance};

/// A completed backup of the selected profile, written beside the memory file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryBackup {
    pub path: PathBuf,
    pub memory_count: usize,
    /// The completed file is valid, but the operation could not remove its temporary link.
    pub temporary_retained: bool,
}

impl MemoryStore {
    /// Back up the selected profile without modifying the active memory file.
    /// Other writers must use the same sidecar lock; unrelated file replacement is not guarded.
    pub async fn backup_snapshot(
        &self,
        expected_profile: &str,
        backup_id: Uuid,
    ) -> Result<MemoryBackup, AppError> {
        if !self.inner.enabled {
            return Err(AppError::unavailable("memory is disabled"));
        }
        if expected_profile.trim().is_empty()
            || expected_profile.chars().count() > 128
            || backup_id.is_nil()
        {
            return Err(AppError::configuration("invalid memory backup request"));
        }
        let profile = self.inner.profile_id.read().await;
        if *profile != expected_profile {
            return Err(AppError::invalid_transition(
                "memory profile changed; refresh before backup",
            ));
        }
        let _write = self.inner.write_lock.lock().await;
        let lease = self.inner.writer_lease.lock().await;
        let source = std::path::absolute(&self.inner.path)
            .map_err(|error| AppError::unavailable(format!("memory backup path: {error}")))?;
        let (temporary, destination) = backup_paths(&source, backup_id)?;
        let path = destination
            .to_str()
            .ok_or_else(|| AppError::configuration("memory backup path is not UTF-8"))?;
        if path.len() > 2048
            || serde_json::to_vec(path).map_or(true, |encoded| encoded.len() > 8192)
        {
            return Err(AppError::configuration(
                "memory backup path exceeds the response budget",
            ));
        }
        let metadata = tokio::fs::metadata(&source)
            .await
            .map_err(|error| AppError::unavailable(format!("memory backup source: {error}")))?;
        if !metadata.is_file() {
            return Err(AppError::unavailable("memory backup source is not a file"));
        }
        let (records, provenance) = if lease.is_some() {
            let records = self.inner.records.read().await;
            let provenance = self.inner.provenance.read().await.for_profile(&profile);
            let records = records
                .iter()
                .filter(|record| record.profile_id == *profile)
                .cloned()
                .collect::<Vec<_>>();
            (records, provenance)
        } else {
            let mut lock_name = source.as_os_str().to_os_string();
            lock_name.push(".lock");
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(PathBuf::from(lock_name))
                .map_err(|error| AppError::unavailable(format!("memory backup lock: {error}")))?;
            lock.try_lock().map_err(|_| {
                AppError::unavailable("memory has another active writer; stop it before backing up")
            })?;
            let document = document::read(&source).await?;
            let records = document
                .records
                .into_iter()
                .filter(|record| record.profile_id == *profile)
                .collect::<Vec<_>>();
            (records, document.provenance.for_profile(&profile))
        };
        drop(lease);
        drop(_write);
        drop(profile);
        let count = records.len();
        let content = provenance::serialize_document(&records, &provenance)?;
        let result = tokio::task::spawn_blocking(move || {
            publish(&temporary, &destination, content.as_bytes())
        })
        .await
        .map_err(|_| AppError::unavailable("memory backup task stopped"))?
        .map_err(|error| AppError::unavailable(format!("memory backup failed: {error}")))?;
        Ok(MemoryBackup {
            path: result.0,
            memory_count: count,
            temporary_retained: result.1,
        })
    }

    pub(super) async fn backup_status(&self, profile: &str, id: Uuid) -> MemoryBackupStatus {
        self.inner
            .backup_operations
            .lock()
            .await
            .iter()
            .find(|item| item.profile_id == profile && item.backup_id == id)
            .cloned()
            .unwrap_or_else(|| MemoryBackupStatus {
                profile_id: profile.to_owned(),
                backup_id: id,
                state: MemoryBackupState::Unknown,
            })
    }

    pub(super) async fn managed_backup(
        &self,
        profile: &str,
        id: Uuid,
    ) -> Result<MemoryResponse, AppError> {
        {
            let mut operations = self.inner.backup_operations.lock().await;
            if let Some(existing) = operations.iter().find(|entry| entry.backup_id == id) {
                if existing.profile_id != profile {
                    return Err(AppError::invalid_transition(
                        "backup ID belongs to another request",
                    ));
                }
                return Ok(MemoryResponse::BackupStatus(existing.clone()));
            }
            if operations.len() >= 64 {
                let removable = operations
                    .iter()
                    .position(|entry| !matches!(entry.state, MemoryBackupState::Running));
                if let Some(index) = removable {
                    operations.remove(index);
                } else {
                    return Err(AppError::unavailable(
                        "too many memory backups are in progress",
                    ));
                }
            }
            operations.push_back(MemoryBackupStatus {
                profile_id: profile.to_owned(),
                backup_id: id,
                state: MemoryBackupState::Running,
            });
        }
        let state = match self.backup_snapshot(profile, id).await {
            Ok(backup) => MemoryBackupState::Completed {
                path: backup.path.to_string_lossy().into_owned(),
                memory_count: backup.memory_count,
                temporary_retained: backup.temporary_retained,
            },
            Err(error) => MemoryBackupState::Failed {
                message: error.message.chars().take(512).collect(),
            },
        };
        let status = MemoryBackupStatus {
            profile_id: profile.to_owned(),
            backup_id: id,
            state,
        };
        if let Some(entry) = self
            .inner
            .backup_operations
            .lock()
            .await
            .iter_mut()
            .find(|entry| entry.backup_id == id)
        {
            *entry = status.clone();
        }
        Ok(MemoryResponse::BackupStatus(status))
    }
}

fn backup_paths(source: &Path, id: Uuid) -> Result<(PathBuf, PathBuf), AppError> {
    let name = source
        .file_name()
        .ok_or_else(|| AppError::configuration("memory path has no filename"))?;
    let sibling = |suffix: &str| {
        let mut name = OsString::from(name);
        name.push(format!(".backup-{id}.{suffix}"));
        source.with_file_name(name)
    };
    Ok((sibling("partial"), sibling("jsonl")))
}

fn publish(temporary: &Path, destination: &Path, content: &[u8]) -> io::Result<(PathBuf, bool)> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)?;
    let result = (|| {
        file.write_all(content)?;
        file.flush()?;
        file.sync_all()?;
        fs::hard_link(temporary, destination)
    })();
    drop(file);
    let cleanup = fs::remove_file(temporary);
    match (result, cleanup) {
        (Ok(()), cleanup) => Ok((destination.to_owned(), cleanup.is_err())),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(io::Error::new(
            error.kind(),
            format!("{error}; temporary backup could not be removed: {cleanup}"),
        )),
    }
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
