use std::collections::HashSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ai_ex_domain::AppError;
use ai_ex_model_catalog::{Artifact, validate_relative_path};
use ring::digest::{Context, SHA256};

pub(crate) fn io_error(error: impl std::fmt::Display) -> AppError {
    AppError::unavailable(format!("model storage: {error}"))
}

pub(crate) fn is_link(metadata: &Metadata) -> bool {
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

pub(crate) fn checked_path(root: &Path, relative: &str) -> Result<PathBuf, AppError> {
    validate_relative_path(relative)?;
    let root_metadata = fs::symlink_metadata(root).map_err(io_error)?;
    if is_link(&root_metadata) || !root_metadata.is_dir() {
        return Err(AppError::configuration(
            "model root must be a regular directory",
        ));
    }
    let mut path = root.to_path_buf();
    let mut parts = relative.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        if is_link(&metadata) || (parts.peek().is_some() && !metadata.is_dir()) {
            return Err(AppError::configuration(
                "model path contains a link or non-directory",
            ));
        }
    }
    Ok(path)
}

pub(crate) fn create_directory(root: &Path, relative: &str) -> Result<PathBuf, AppError> {
    validate_relative_path(relative)?;
    let mut path = root.to_path_buf();
    if is_link(&fs::symlink_metadata(root).map_err(io_error)?) {
        return Err(io_error("linked model root"));
    }
    for part in relative.split('/') {
        path.push(part);
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        if is_link(&metadata) || !metadata.is_dir() {
            return Err(io_error("unsafe model directory"));
        }
    }
    Ok(path)
}

pub(crate) fn new_output(root: &Path, relative: &str) -> Result<File, AppError> {
    validate_relative_path(relative)?;
    if let Some((parent, _)) = relative.rsplit_once('/') {
        create_directory(root, parent)?;
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(relative))
        .map_err(io_error)
}

pub(crate) fn verify_files(
    artifacts: &[Artifact],
    root: &Path,
    check: &dyn Fn() -> Result<(), AppError>,
) -> Result<(), AppError> {
    for artifact in artifacts {
        check()?;
        let path = checked_path(root, &artifact.path)?;
        let mut file = File::open(path).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.len() != artifact.bytes {
            return Err(io_error(format!(
                "artifact size mismatch: {}",
                artifact.path
            )));
        }
        if hash_reader(&mut file, artifact.bytes, check)? != artifact.sha256.to_ascii_lowercase() {
            return Err(io_error(format!(
                "artifact digest mismatch: {}",
                artifact.path
            )));
        }
    }
    verify_inventory(artifacts, root)
}

pub(crate) fn verify_inventory(artifacts: &[Artifact], root: &Path) -> Result<(), AppError> {
    let allowed: HashSet<_> = artifacts.iter().map(|item| item.path.as_str()).collect();
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        if is_link(&metadata) {
            return Err(io_error("package contains a link"));
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(&path).map_err(io_error)? {
                count += 1;
                if count > 100_000 {
                    return Err(io_error("package inventory exceeds limit"));
                }
                pending.push(entry.map_err(io_error)?.path());
            }
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(io_error)?
                .to_str()
                .ok_or_else(|| io_error("package path is not UTF-8"))?
                .replace('\\', "/");
            if !metadata.is_file() || !allowed.contains(relative.as_str()) {
                return Err(io_error("package contains an undeclared file"));
            }
        }
    }
    Ok(())
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&SHA256, bytes).as_ref())
}

pub(crate) fn hash_reader(
    reader: &mut impl Read,
    expected: u64,
    check: &dyn Fn() -> Result<(), AppError>,
) -> Result<String, AppError> {
    let mut context = Context::new(&SHA256);
    let mut buffer = [0_u8; 65_536];
    let mut total = 0_u64;
    loop {
        check()?;
        let count = reader.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| io_error("artifact size overflow"))?;
        if total > expected {
            return Err(io_error("artifact exceeds expected length"));
        }
        context.update(&buffer[..count]);
    }
    if total != expected {
        return Err(io_error("artifact is shorter than declared"));
    }
    Ok(hex(context.finish().as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io_error)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_error)
}
