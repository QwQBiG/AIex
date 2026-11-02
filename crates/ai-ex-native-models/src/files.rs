use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ai_ex_domain::AppError;
use uuid::Uuid;

pub(crate) const MAX_AUDIO_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) fn error(error: impl std::fmt::Display) -> AppError {
    AppError::configuration(format!("native resource file: {error}"))
}

pub(crate) fn regular(path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path).map_err(error)?;
    if is_link(&metadata) || !metadata.is_file() {
        return Err(error("expected a regular resource file"));
    }
    Ok(())
}

fn is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub(crate) fn directory(path: &Path) -> Result<PathBuf, AppError> {
    if !path.is_absolute() {
        return Err(error("native resource roots must be absolute"));
    }
    fs::create_dir_all(path).map_err(error)?;
    let metadata = fs::symlink_metadata(path).map_err(error)?;
    if is_link(&metadata) || !metadata.is_dir() {
        return Err(error("native resource root cannot be a link"));
    }
    fs::canonicalize(path).map_err(error)
}

pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(error)?;
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .map_err(error)
}

pub(crate) fn read_audio(path: &Path) -> Result<Vec<u8>, AppError> {
    regular(path)?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(error)?
        .take(MAX_AUDIO_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() as u64 > MAX_AUDIO_BYTES {
        return Err(error("WAV exceeds 32 MiB"));
    }
    Ok(bytes)
}

/// Only removes the newly created UUID directory under its canonical owner.
pub(crate) struct PrivateDirectory {
    pub path: PathBuf,
    owner: PathBuf,
}
impl PrivateDirectory {
    pub fn create(owner: &Path, prefix: &str) -> Result<Arc<Self>, AppError> {
        let path = owner.join(format!("{prefix}-{}", Uuid::new_v4()));
        fs::create_dir(&path).map_err(error)?;
        Ok(Arc::new(Self {
            path,
            owner: owner.to_owned(),
        }))
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        if let (Ok(metadata), Ok(resolved)) = (
            fs::symlink_metadata(&self.path),
            fs::canonicalize(&self.path),
        ) && !is_link(&metadata)
            && resolved.starts_with(&self.owner)
            && resolved != self.owner
        {
            let _cleanup = fs::remove_dir_all(&self.path);
        }
    }
}

pub(crate) struct RequestFiles {
    pub directory: Arc<PrivateDirectory>,
    pub relative_prefix: String,
    _worker: Arc<PrivateDirectory>,
}
impl RequestFiles {
    pub fn matches_worker(&self, worker: &Arc<PrivateDirectory>) -> bool {
        Arc::ptr_eq(&self._worker, worker)
    }

    pub fn new(worker: Arc<PrivateDirectory>) -> Result<Self, AppError> {
        let directory = PrivateDirectory::create(&worker.path, "request")?;
        let relative_prefix = directory
            .path
            .file_name()
            .and_then(|part| part.to_str())
            .ok_or_else(|| error("invalid request directory"))?
            .to_owned();
        Ok(Self {
            directory,
            relative_prefix,
            _worker: worker,
        })
    }
    pub fn relative(&self, filename: &str) -> String {
        format!("{}/{}", self.relative_prefix, filename)
    }
}
