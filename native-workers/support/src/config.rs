use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use ai_ex_model_catalog::{
    ArtifactKind, ModelBackend, ModelManifest, PreparationStatus, verify_directory,
};
use serde::{Deserialize, de::DeserializeOwned};

use crate::AppError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config<B> {
    schema_version: u16,
    manifest: PathBuf,
    directory: PathBuf,
    backend: B,
}

pub struct VerifiedConfig<B> {
    pub manifest: ModelManifest,
    pub backend: B,
    directory: PathBuf,
}

impl<B: DeserializeOwned> VerifiedConfig<B> {
    pub fn from_args(expected: ModelBackend) -> Result<Self, AppError> {
        Self::from_args_for(&[expected])
    }

    pub fn from_args_for(expected: &[ModelBackend]) -> Result<Self, AppError> {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.len() != 2 || args[0] != "--config" {
            return Err(AppError::configuration(
                "usage: worker --config ABSOLUTE_JSON",
            ));
        }
        Self::load_for(Path::new(&args[1]), expected)
    }

    pub fn load(path: &Path, expected: ModelBackend) -> Result<Self, AppError> {
        Self::load_for(path, &[expected])
    }

    fn load_for(path: &Path, expected: &[ModelBackend]) -> Result<Self, AppError> {
        if !path.is_absolute() {
            return Err(AppError::configuration("worker config must be absolute"));
        }
        let config: Config<B> = read_json(path)?;
        if config.schema_version != 1
            || !config.directory.is_absolute()
            || !config.manifest.is_absolute()
        {
            return Err(AppError::configuration(
                "invalid worker configuration version or paths",
            ));
        }
        let manifest: ModelManifest = read_json(&config.manifest)?;
        if !expected.contains(&manifest.backend) {
            return Err(AppError::configuration("worker model backend mismatch"));
        }
        let report = verify_directory(&manifest, &config.directory)?;
        if report.status != PreparationStatus::Ready {
            return Err(AppError::configuration(format!(
                "worker package verification failed: {:?}",
                report.issues
            )));
        }
        Ok(Self {
            manifest,
            backend: config.backend,
            directory: config.directory,
        })
    }

    pub fn artifact(&self, relative: &str, kind: ArtifactKind) -> Result<PathBuf, AppError> {
        if !self
            .manifest
            .artifacts
            .iter()
            .any(|item| item.path == relative && item.kind == kind)
        {
            return Err(AppError::configuration(
                "worker path is not a declared artifact of the required kind",
            ));
        }
        Ok(self.directory.join(relative))
    }

    /// Native directory readers may discover files not explicitly named in a
    /// configuration, so every file below a supplied directory must be covered.
    pub fn artifact_directory(&self, relative: &str) -> Result<PathBuf, AppError> {
        ai_ex_model_catalog::validate_relative_path(relative)?;
        let directory = self.directory.join(relative);
        let mut ancestor = self.directory.clone();
        for part in relative.split('/') {
            ancestor.push(part);
            let metadata = fs::symlink_metadata(&ancestor)
                .map_err(|error| AppError::configuration(error.to_string()))?;
            if metadata.file_type().is_symlink() || is_reparse(&metadata) || !metadata.is_dir() {
                return Err(AppError::configuration(
                    "native directory ancestry is not a regular directory",
                ));
            }
        }
        let mut pending = vec![directory.clone()];
        let mut count = 0;
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| AppError::configuration(error.to_string()))?;
            if metadata.file_type().is_symlink() || is_reparse(&metadata) {
                return Err(AppError::configuration(
                    "worker artifact directory cannot contain links",
                ));
            }
            if metadata.is_dir() {
                for entry in fs::read_dir(path)
                    .map_err(|error| AppError::configuration(error.to_string()))?
                {
                    count += 1;
                    if count > 100_000 {
                        return Err(AppError::configuration("worker directory is too large"));
                    }
                    pending.push(
                        entry
                            .map_err(|error| AppError::configuration(error.to_string()))?
                            .path(),
                    );
                }
            } else {
                let declared = self
                    .manifest
                    .artifacts
                    .iter()
                    .any(|item| self.directory.join(&item.path) == path);
                if !metadata.is_file() || !declared {
                    return Err(AppError::configuration(
                        "native directory contains an undeclared artifact",
                    ));
                }
            }
        }
        if !fs::metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
            return Err(AppError::configuration(
                "native artifact directory is missing",
            ));
        }
        Ok(directory)
    }
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, AppError> {
    const LIMIT: u64 = 1024 * 1024;
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|error| {
            AppError::configuration(format!("cannot read worker configuration: {error}"))
        })?;
    if bytes.len() as u64 > LIMIT {
        return Err(AppError::configuration("worker JSON exceeds one MiB"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| AppError::configuration(format!("invalid worker JSON: {error}")))
}
