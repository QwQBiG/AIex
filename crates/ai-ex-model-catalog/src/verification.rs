use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use ai_ex_domain::AppError;
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};

use crate::{Artifact, ModelManifest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationStatus {
    NotPrepared,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryVerification {
    pub model_id: String,
    pub revision: String,
    pub status: PreparationStatus,
    pub verified_bytes: u64,
    pub issues: Vec<ArtifactIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactIssue {
    pub path: String,
    pub kind: ArtifactIssueKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArtifactIssueKind {
    Missing,
    LinkNotAllowed,
    NotRegularFile,
    SizeMismatch { expected: u64, actual: u64 },
    HashMismatch { expected: String, actual: String },
    Io { operation: String, detail: String },
}

/// Verify only the declared files without modifying or executing them.
/// The caller owns a quiescent staging directory and separately resolves runtime
/// requirements. Ready describes these bytes, not a successful model inference.
pub fn verify_directory(
    manifest: &ModelManifest,
    directory: impl AsRef<Path>,
) -> Result<DirectoryVerification, AppError> {
    manifest.validate()?;
    let directory = directory.as_ref();
    let mut report = DirectoryVerification {
        model_id: manifest.id.clone(),
        revision: manifest.revision.clone(),
        status: PreparationStatus::NotPrepared,
        verified_bytes: 0,
        issues: Vec::new(),
    };
    for artifact in &manifest.artifacts {
        match verify_artifact(directory, artifact) {
            Ok(()) => {
                report.verified_bytes = report
                    .verified_bytes
                    .checked_add(artifact.bytes)
                    .ok_or_else(|| AppError::configuration("artifact total size overflow"))?;
            }
            Err(kind) => report.issues.push(ArtifactIssue {
                path: artifact.path.clone(),
                kind,
            }),
        }
    }
    if report.issues.is_empty() {
        report.status = PreparationStatus::Ready;
    }
    Ok(report)
}

fn verify_artifact(root: &Path, artifact: &Artifact) -> Result<(), ArtifactIssueKind> {
    let path = checked_file(root, &artifact.path)?;
    let mut file = File::open(path).map_err(|error| io_issue("open", error))?;
    let metadata = file
        .metadata()
        .map_err(|error| io_issue("metadata", error))?;
    if !metadata.is_file() {
        return Err(ArtifactIssueKind::NotRegularFile);
    }
    if metadata.len() != artifact.bytes {
        return Err(ArtifactIssueKind::SizeMismatch {
            expected: artifact.bytes,
            actual: metadata.len(),
        });
    }
    let actual = hash_file(&mut file, artifact.bytes)?;
    if !actual.eq_ignore_ascii_case(&artifact.sha256) {
        return Err(ArtifactIssueKind::HashMismatch {
            expected: artifact.sha256.clone(),
            actual,
        });
    }
    Ok(())
}

fn checked_file(root: &Path, relative: &str) -> Result<PathBuf, ArtifactIssueKind> {
    let metadata = fs::symlink_metadata(root).map_err(|error| io_issue("metadata", error))?;
    if is_link(&metadata) {
        return Err(ArtifactIssueKind::LinkNotAllowed);
    }
    if !metadata.is_dir() {
        return Err(ArtifactIssueKind::NotRegularFile);
    }
    let mut path = root.to_path_buf();
    let mut components = relative.split('/').peekable();
    while let Some(component) = components.next() {
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(|error| io_issue("metadata", error))?;
        if is_link(&metadata) {
            return Err(ArtifactIssueKind::LinkNotAllowed);
        }
        let valid_type = if components.peek().is_some() {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        if !valid_type {
            return Err(ArtifactIssueKind::NotRegularFile);
        }
    }
    Ok(path)
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn hash_file(file: &mut File, expected: u64) -> Result<String, ArtifactIssueKind> {
    let mut context = Context::new(&SHA256);
    let mut buffer = [0_u8; 65_536];
    let mut total = 0_u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_issue("read", error))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > expected {
            return Err(ArtifactIssueKind::SizeMismatch {
                expected,
                actual: total,
            });
        }
        context.update(&buffer[..count]);
    }
    if total != expected {
        return Err(ArtifactIssueKind::SizeMismatch {
            expected,
            actual: total,
        });
    }
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn io_issue(operation: &str, error: io::Error) -> ArtifactIssueKind {
    if error.kind() == io::ErrorKind::NotFound {
        ArtifactIssueKind::Missing
    } else {
        ArtifactIssueKind::Io {
            operation: operation.to_owned(),
            detail: error.to_string(),
        }
    }
}
