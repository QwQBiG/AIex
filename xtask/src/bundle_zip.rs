use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ai_ex_model_catalog::Artifact;
use zip::write::SimpleFileOptions;

use crate::bundle_io::{Result, create_new, hash_reader, regular_file};

pub struct Entry {
    pub artifact: Artifact,
    pub input: Input,
}

pub enum Input {
    File(PathBuf),
    Bytes(Vec<u8>),
}

impl From<PathBuf> for Input {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}

impl Entry {
    pub fn generated(path: &str, content: Vec<u8>) -> Self {
        Self {
            artifact: Artifact {
                path: path.into(),
                kind: ai_ex_model_catalog::ArtifactKind::Configuration,
                bytes: content.len() as u64,
                sha256: crate::bundle_io::hash_bytes(&content),
            },
            input: Input::Bytes(content),
        }
    }
}

pub fn write_archive(output: &Path, entries: &[Entry]) -> Result<()> {
    for entry in entries {
        entry
            .artifact
            .validate()
            .map_err(|error| error.to_string())?;
    }
    write_entries(output, entries, zip::CompressionMethod::Deflated)
}

pub fn write_source_archive(output: &Path, entries: &[Entry]) -> Result<()> {
    for entry in entries {
        ai_ex_model_catalog::validate_relative_path(&entry.artifact.path)
            .map_err(|error| error.to_string())?;
        if entry.artifact.bytes == 0
            || entry.artifact.sha256.len() != 64
            || !entry
                .artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("source ZIP requires nonempty files with SHA256".into());
        }
    }
    write_entries(output, entries, zip::CompressionMethod::Stored)
}

fn write_entries(
    output: &Path,
    entries: &[Entry],
    compression: zip::CompressionMethod,
) -> Result<()> {
    let mut writer = zip::ZipWriter::new(create_new(output)?);
    let options = SimpleFileOptions::default()
        .compression_method(compression)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let mut names = HashSet::new();
    for entry in entries {
        if !names.insert(entry.artifact.path.to_uppercase()) {
            return Err(format!("duplicate ZIP entry: {}", entry.artifact.path));
        }
        let mut input: Box<dyn Read + '_> = match &entry.input {
            Input::File(path) => Box::new(regular_file(path)?),
            Input::Bytes(bytes) => Box::new(bytes.as_slice()),
        };
        writer
            .start_file(&entry.artifact.path, options)
            .map_err(|error| error.to_string())?;
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        let mut bytes = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = input.read(&mut buffer).map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            context.update(&buffer[..count]);
            bytes += count as u64;
            writer
                .write_all(&buffer[..count])
                .map_err(|error| error.to_string())?;
        }
        let sha256: String = context
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if bytes != entry.artifact.bytes || !sha256.eq_ignore_ascii_case(&entry.artifact.sha256) {
            return Err(format!(
                "input changed while archiving: {}",
                entry.artifact.path
            ));
        }
    }
    writer
        .finish()
        .and_then(|file| file.sync_all().map_err(zip::result::ZipError::Io))
        .map_err(|error| error.to_string())
}

pub fn verify_archive(path: &Path, artifacts: &[Artifact]) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(regular_file(path)?).map_err(|error| error.to_string())?;
    if archive.len() != artifacts.len() {
        return Err("ZIP entry count differs from runtime manifest".into());
    }
    let mut names = HashSet::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|error| error.to_string())?;
        if !names.insert(file.name().to_uppercase()) {
            return Err("duplicate ZIP entry".into());
        }
        let expected = artifacts
            .iter()
            .find(|artifact| artifact.path == file.name())
            .ok_or_else(|| format!("undeclared ZIP entry: {}", file.name()))?;
        if !file.is_file()
            || file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("ZIP must contain regular files only".into());
        }
        if file.size() != expected.bytes {
            return Err(format!("ZIP size mismatch: {}", file.name()));
        }
        let (bytes, sha256) = hash_reader(&mut file)?;
        if bytes != expected.bytes || !sha256.eq_ignore_ascii_case(&expected.sha256) {
            return Err(format!("ZIP hash mismatch: {}", expected.path));
        }
    }
    Ok(())
}
