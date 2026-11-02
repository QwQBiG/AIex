use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ai_ex_domain::AppError;

use super::{NativeConfig, ResolvedNativeStorage, storage::absolute};

mod io;
use io::{read_bytes, write_new};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct NativeConfigStore {
    path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct NativeConfigUpdate {
    pub config: NativeConfig,
    pub backup: PathBuf,
}

impl NativeConfigStore {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, AppError> {
        Ok(Self {
            path: absolute(path.as_ref())?,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<NativeConfig, AppError> {
        parse_bytes(&read_bytes(&self.path)?)
    }

    pub fn resolve_storage(
        &self,
        config: &NativeConfig,
    ) -> Result<ResolvedNativeStorage, AppError> {
        config.storage.resolve(&self.path)
    }

    pub fn create_new(&self, config: &NativeConfig) -> Result<(), AppError> {
        let text = config.to_toml()?;
        let _lock = self.lock()?;
        write_new(&self.path, text.as_bytes())
    }

    /// Writers coordinate with an OS lock. Only the expected revision may move
    /// forward; the original bytes are backed up before same-directory replace.
    pub fn update(
        &self,
        expected_revision: u64,
        replacement: &NativeConfig,
    ) -> Result<NativeConfigUpdate, AppError> {
        replacement.validate()?;
        if replacement.revision != expected_revision {
            return Err(AppError::invalid_transition(
                "replacement native revision is stale",
            ));
        }
        let _lock = self.lock()?;
        let original = read_bytes(&self.path)?;
        let current = parse_bytes(&original)?;
        if current.revision != expected_revision {
            return Err(AppError::invalid_transition(
                "native configuration changed; reload before saving",
            ));
        }
        let mut next = replacement.clone();
        next.revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| AppError::configuration("native configuration revision exhausted"))?;
        let text = next.to_toml()?;
        let backup = self.sibling(&format!(".r{}.backup", current.revision))?;
        if backup.try_exists().map_err(failure)? {
            if read_bytes(&backup)? != original {
                return Err(AppError::configuration(
                    "native revision backup already exists with different content",
                ));
            }
        } else {
            write_new(&backup, &original)?;
        }
        let temporary = self.sibling(&format!(
            ".r{}.{}-{}.tmp",
            next.revision,
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ))?;
        write_new(&temporary, text.as_bytes())?;
        let staged = StagedFile(temporary);
        fs::rename(&staged.0, &self.path).map_err(failure)?;
        Ok(NativeConfigUpdate {
            config: next,
            backup,
        })
    }

    fn sibling(&self, suffix: &str) -> Result<PathBuf, AppError> {
        let mut name = self
            .path
            .file_name()
            .ok_or_else(|| AppError::configuration("native config requires a filename"))?
            .to_os_string();
        name.push(suffix);
        Ok(self.path.with_file_name(name))
    }

    fn lock(&self) -> Result<File, AppError> {
        let path = self.sibling(".lock")?;
        io::check_regular_if_present(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(failure)?;
        file.try_lock().map_err(|_| {
            AppError::invalid_transition("native configuration is being saved by another writer")
        })?;
        Ok(file)
    }
}

struct StagedFile(PathBuf);
impl Drop for StagedFile {
    fn drop(&mut self) {
        let _cleanup = fs::remove_file(&self.0);
    }
}

fn parse_bytes(bytes: &[u8]) -> Result<NativeConfig, AppError> {
    let source = std::str::from_utf8(bytes)
        .map_err(|_| AppError::configuration("native configuration is not UTF-8"))?;
    NativeConfig::parse(source)
}

fn failure(error: std::io::Error) -> AppError {
    AppError::configuration(format!("native configuration file: {error}"))
}
