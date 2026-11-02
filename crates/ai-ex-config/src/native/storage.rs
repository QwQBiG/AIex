use std::path::{Component, Path, PathBuf};

use ai_ex_domain::AppError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NativeStorage {
    pub packages: PathBuf,
    pub voices: PathBuf,
    pub jobs: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNativeStorage {
    pub packages: PathBuf,
    pub models: PathBuf,
    pub runtimes: PathBuf,
    pub voices: PathBuf,
    pub jobs: PathBuf,
}

impl Default for NativeStorage {
    fn default() -> Self {
        Self {
            packages: "native/packages".into(),
            voices: "native/voices".into(),
            jobs: "native/jobs".into(),
        }
    }
}

impl NativeStorage {
    pub fn validate(&self) -> Result<(), AppError> {
        for path in [&self.packages, &self.voices, &self.jobs] {
            let text = path
                .to_str()
                .ok_or_else(|| AppError::configuration("native storage path is not UTF-8"))?;
            if text.trim().is_empty()
                || text.len() > 4096
                || text.chars().any(char::is_control)
                || path.components().any(|part| part == Component::ParentDir)
                || (!path.is_absolute()
                    && (text.contains(':') || text.split(['/', '\\']).any(|part| part == "..")))
            {
                return Err(AppError::configuration(
                    "native storage requires an absolute path or a portable relative directory without parent traversal",
                ));
            }
        }
        Ok(())
    }

    /// Resolution is lexical and creates no directories or device handles.
    pub fn resolve(&self, config_file: &Path) -> Result<ResolvedNativeStorage, AppError> {
        self.validate()?;
        let config_file = absolute(config_file)?;
        let root = config_file
            .parent()
            .ok_or_else(|| AppError::configuration("native config has no parent directory"))?;
        let resolve = |path: &Path| {
            if path.is_absolute() {
                path.to_owned()
            } else {
                root.join(path)
            }
        };
        let packages = resolve(&self.packages);
        Ok(ResolvedNativeStorage {
            models: packages.join("models"),
            runtimes: packages.join("runtimes"),
            packages,
            voices: resolve(&self.voices),
            jobs: resolve(&self.jobs),
        })
    }
}

pub(super) fn absolute(path: &Path) -> Result<PathBuf, AppError> {
    if path.as_os_str().is_empty() || path.file_name().is_none() {
        return Err(AppError::configuration(
            "native configuration requires a file path",
        ));
    }
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        std::env::current_dir()
            .map(|root| root.join(path))
            .map_err(|error| {
                AppError::configuration(format!("cannot resolve configuration path: {error}"))
            })
    }
}
