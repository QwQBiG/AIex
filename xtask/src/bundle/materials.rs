use std::collections::HashSet;
use std::path::Path;

use ai_ex_model_catalog::{Artifact, ArtifactKind, License};
use serde::Deserialize;

use crate::bundle_io::{Result, hash_file, read_json, verify_file};
use crate::bundle_zip::Entry;
use crate::catalog_build::{FileRecord, Materials};

#[derive(Deserialize)]
struct RustInputs {
    lockfile: String,
    lockfile_sha256: String,
    first_party: Vec<OwnPackage>,
    crates: Vec<RegistryPackage>,
}

#[derive(Deserialize)]
struct OwnPackage {
    license_text: String,
    license_bytes: u64,
    license_sha256: String,
}

#[derive(Deserialize)]
struct RegistryPackage {
    name: String,
    version: String,
    source_url: String,
    materials: Vec<FileRecord>,
}

pub fn append(
    directory: &Path,
    project: &Path,
    commit: &str,
    entries: &mut Vec<Entry>,
) -> Result<Vec<License>> {
    let materials: Materials = read_json(&directory.join("materials.json"))?;
    let mut licenses = Vec::new();
    let mut seen = HashSet::new();
    for material in &materials.materials {
        material.text(directory)?;
        add(directory, &material.file, entries, &mut seen)?;
        licenses.push(material.license());
    }
    let rust: RustInputs = read_json(&directory.join("rust-workers.json"))?;
    ai_ex_model_catalog::validate_relative_path(&rust.lockfile)
        .map_err(|error| error.to_string())?;
    let (_, lock_sha) = hash_file(&project.join(&rust.lockfile))?;
    if lock_sha != rust.lockfile_sha256 {
        return Err("worker Cargo.lock differs from source/license inventory".into());
    }
    for package in &rust.crates {
        if package.materials.is_empty() {
            return Err(format!("missing Rust license materials: {}", package.name));
        }
        for material in &package.materials {
            add(directory, material, entries, &mut seen)?;
        }
        licenses.push(License {
            id: format!("rust-{}-{}", package.name, package.version),
            source_url: package.source_url.clone(),
        });
    }
    let mut own_seen = HashSet::new();
    for own in &rust.first_party {
        ai_ex_model_catalog::validate_relative_path(&own.license_text)
            .map_err(|error| error.to_string())?;
        if own_seen.insert(own.license_text.clone()) {
            let input = project.join(&own.license_text);
            verify_file(&input, own.license_bytes, &own.license_sha256)?;
            entries.push(Entry {
                input: input.into(),
                artifact: Artifact {
                    path: format!("licenses/project/{}.txt", own.license_text),
                    kind: ArtifactKind::License,
                    bytes: own.license_bytes,
                    sha256: own.license_sha256.clone(),
                },
            });
        }
    }
    licenses.push(License {
        id: "project-MIT".into(),
        source_url: format!("https://github.com/QwQBiG/AIex/blob/{commit}/LICENSE"),
    });
    for name in [
        "README.md",
        "models.json",
        "materials.json",
        "runtime-files.json",
        "ollama-cpu-files.json",
        "rust-workers.json",
        "sources.json",
    ] {
        let input = directory.join(name);
        let (bytes, sha256) = hash_file(&input)?;
        entries.push(Entry {
            input: input.into(),
            artifact: Artifact {
                path: format!("distribution/{name}"),
                kind: ArtifactKind::Configuration,
                bytes,
                sha256,
            },
        });
    }
    Ok(licenses)
}

fn add(
    directory: &Path,
    file: &FileRecord,
    entries: &mut Vec<Entry>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    ai_ex_model_catalog::validate_relative_path(&file.path).map_err(|error| error.to_string())?;
    if seen.insert(file.path.to_uppercase()) {
        let input = directory.join(&file.path);
        verify_file(&input, file.bytes, &file.sha256)?;
        // Preserve original text bytes and path hierarchy; a text suffix permits all original license names.
        entries.push(Entry {
            input: input.into(),
            artifact: file.artifact(
                format!("materials/{}.txt", file.path),
                ArtifactKind::License,
            ),
        });
    }
    Ok(())
}
