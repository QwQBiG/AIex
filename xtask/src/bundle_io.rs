use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use ring::digest::{Context, SHA256};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub type Result<T> = std::result::Result<T, String>;

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > 16 * 1024 * 1024 {
        return Err(format!("JSON input exceeds 16 MiB: {}", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

pub fn hash_bytes(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&SHA256, bytes).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn regular_file(path: &Path) -> Result<File> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let path = absolute.as_path();
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || is_reparse(&metadata) {
            return Err(format!("linked input is not permitted: {}", path.display()));
        }
    }
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err(format!("not a regular file: {}", path.display()));
    }
    Ok(file)
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(_: &std::fs::Metadata) -> bool {
    false
}

pub fn hash_reader(reader: &mut impl Read) -> Result<(u64, String)> {
    let mut context = Context::new(&SHA256);
    let mut bytes = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        context.update(&buffer[..count]);
        bytes += count as u64;
    }
    Ok((bytes, hex(context.finish().as_ref())))
}

pub fn hash_file(path: &Path) -> Result<(u64, String)> {
    hash_reader(&mut regular_file(path)?)
}

pub fn verify_file(path: &Path, bytes: u64, sha256: &str) -> Result<()> {
    let (actual_bytes, actual_sha) = hash_file(path)?;
    if actual_bytes != bytes || actual_sha != sha256.to_ascii_lowercase() {
        return Err(format!("size/SHA256 mismatch: {}", path.display()));
    }
    Ok(())
}

pub fn create_new(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create new output {}: {error}", path.display()))
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let mut file = create_new(path)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())
}
