//! One-shot private worker; input is received over stdin, never command arguments.
//!
//! The selected path may refer to a network location. Checks reject an observed
//! final link, but do not prevent a non-cooperating process from changing a path
//! or file during the read. The result is not proof of source authenticity.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use ai_ex_memory::{BackupInspection, inspect_profile_backup};
use serde::{Deserialize, Serialize};

pub(crate) const WORKER_MODE: &str = "--private-inspect-memory-backup";
pub(super) const MAX_FRAME: usize = 8192;
const MAX_BACKUP_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub path: String,
    pub profile: String,
    pub comparison_space: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Reply {
    Ready { summary: BackupInspection },
    Invalid,
}

/// The caller must arrange process isolation and enforce a deadline.
pub(crate) fn run_stdio() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut length = [0_u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(io::Error::other("invalid frame"));
    }
    let mut frame = vec![0; length];
    input.read_exact(&mut frame)?;
    let reply = match serde_json::from_slice::<Request>(&frame) {
        Ok(request) => inspect(request),
        Err(_) => Reply::Invalid,
    };
    let encoded = serde_json::to_vec(&reply).map_err(|_| io::Error::other("invalid reply"))?;
    let mut output = io::stdout().lock();
    output.write_all(&(encoded.len() as u32).to_be_bytes())?;
    output.write_all(&encoded)?;
    output.flush()
}

fn inspect(request: Request) -> Reply {
    match inspect_inner(request) {
        Some(summary) => Reply::Ready { summary },
        None => Reply::Invalid,
    }
}

fn inspect_inner(request: Request) -> Option<BackupInspection> {
    inspect_with(request, read_selected)
}

fn inspect_with(
    request: Request,
    read: impl FnOnce(&Path) -> Option<Vec<u8>>,
) -> Option<BackupInspection> {
    if request.path.is_empty()
        || request.path.chars().count() > 1024
        || request.path.chars().any(char::is_control)
        || request.profile.trim().is_empty()
        || request.profile.chars().count() > 128
        || request.comparison_space.as_ref().is_some_and(|space| {
            space.trim().is_empty()
                || space.chars().count() > 256
                || space.chars().any(char::is_control)
        })
    {
        return None;
    }
    let path = Path::new(&request.path);
    if !path.is_absolute() {
        return None;
    }
    let bytes = read(path)?;
    inspect_profile_backup(
        &bytes,
        &request.profile,
        request.comparison_space.as_deref(),
    )
    .ok()
}

fn read_selected(path: &Path) -> Option<Vec<u8>> {
    if !plain_file(&fs::symlink_metadata(path).ok()?) {
        return None;
    }
    let file = open_file(path).ok()?;
    if !plain_file(&file.metadata().ok()?) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BACKUP_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_BACKUP_BYTES {
        return None;
    }
    Some(bytes)
}

fn open_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the final component itself; its attributes are checked below.
        options.custom_flags(0x0020_0000);
    }
    options.open(path)
}

fn plain_file(metadata: &Metadata) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[path = "backup_inspection_worker_tests.rs"]
mod tests;
