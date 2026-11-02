use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ai_ex_model_catalog::ArtifactKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::bundle_io::{Result, hash_file, hash_reader, read_json, regular_file, verify_file};
use crate::bundle_zip::Entry;
use crate::catalog_build::FileRecord;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    schema_version: u16,
    distribution_directory: PathBuf,
    project_commit: String,
    project_archive: Input,
    archives: Vec<Input>,
    build_materials: Vec<Input>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Input {
    #[serde(skip_serializing)]
    source: PathBuf,
    #[serde(flatten)]
    file: FileRecord,
    source_url: Option<String>,
}

#[derive(Deserialize)]
struct NativeSources {
    archives: Vec<NativeArchive>,
}
#[derive(Deserialize)]
struct NativeArchive {
    bytes: u64,
    sha256: String,
    source_url: String,
}
#[derive(Deserialize)]
struct RustSources {
    crates: Vec<RustArchive>,
}
#[derive(Deserialize)]
struct RustArchive {
    name: String,
    version: String,
    registry_checksum: String,
    source_url: String,
}

pub fn build(spec: &Path, output: &Path) -> Result<(Value, bool)> {
    if output.exists() {
        return Err("sources output must be a new ZIP path".into());
    }
    let spec: Spec = read_json(spec)?;
    if spec.schema_version != 1
        || spec.project_commit.len() != 40
        || !spec
            .project_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("sources require schema 1 and the exact project commit".into());
    }
    let (native_count, registry_count) = check_archives(&spec)?;
    check_project(&spec.project_archive, &spec.project_commit)?;
    check_distribution(&spec.project_archive, &spec.distribution_directory)?;
    if spec.build_materials.is_empty() || spec.build_materials.len() > 64 {
        return Err("sources require an explicit bounded build-material list".into());
    }
    for name in [
        "llvm-mingw-20240619-build-llvm.sh",
        "llvm-mingw-20240619-build-mingw-w64.sh",
        "llvm-mingw-20240619-build-mingw-w64-libraries.sh",
    ] {
        if !spec
            .build_materials
            .iter()
            .any(|input| input.file.path == format!("build/{name}"))
        {
            return Err(format!("missing fixed build recipe: {name}"));
        }
    }
    if spec.build_materials.iter().any(|input| {
        input
            .source
            .file_name()
            .is_some_and(|name| name == "native-catalog.json")
            || input.file.path.ends_with("config/native-catalog.json")
    }) {
        return Err("generated native catalog must stay outside corresponding sources".into());
    }
    let provenance = json!({"schema_version":1,"project_commit":spec.project_commit,
        "native_archives":native_count,"registry_archives":registry_count,
        "project_archive":spec.project_archive,"archives":spec.archives,
        "build_materials":spec.build_materials});
    let mut entries = Vec::new();
    for input in spec
        .archives
        .iter()
        .chain(std::iter::once(&spec.project_archive))
        .chain(&spec.build_materials)
    {
        ai_ex_model_catalog::validate_relative_path(&input.file.path).map_err(|e| e.to_string())?;
        verify_file(&input.source, input.file.bytes, &input.file.sha256)?;
        entries.push(Entry {
            input: input.source.clone().into(),
            artifact: input
                .file
                .artifact(input.file.path.clone(), ArtifactKind::SourceArchive),
        });
    }
    entries.push(Entry::generated(
        "source-inputs.json",
        serde_json::to_vec_pretty(&provenance).map_err(|e| e.to_string())?,
    ));
    entries.sort_by(|a, b| a.artifact.path.cmp(&b.artifact.path));
    crate::bundle_zip::write_source_archive(output, &entries)?;
    let artifacts = entries
        .iter()
        .map(|entry| entry.artifact.clone())
        .collect::<Vec<_>>();
    crate::bundle_zip::verify_archive(output, &artifacts)?;
    let (bytes, sha256) = hash_file(output)?;
    Ok((
        json!({"status":"passed","output":output,"bytes":bytes,"sha256":sha256,
        "artifacts":entries.len(),"project_commit":spec.project_commit,
        "native_archives":native_count,"registry_archives":registry_count}),
        true,
    ))
}

fn check_archives(spec: &Spec) -> Result<(usize, usize)> {
    let native: NativeSources = read_json(&spec.distribution_directory.join("sources.json"))?;
    let rust: RustSources = read_json(&spec.distribution_directory.join("rust-workers.json"))?;
    if native.archives.is_empty()
        || rust.crates.is_empty()
        || spec.archives.len() != native.archives.len() + rust.crates.len()
    {
        return Err(
            "sources must cover every archive in the fixed native and registry inventories".into(),
        );
    }
    let mut used = HashSet::new();
    for archive in &native.archives {
        let (index, input) = spec
            .archives
            .iter()
            .enumerate()
            .find(|(_, input)| {
                input.source_url.as_deref() == Some(&archive.source_url)
                    && input.file.bytes == archive.bytes
                    && input.file.sha256 == archive.sha256
            })
            .ok_or_else(|| format!("missing native source: {}", archive.source_url))?;
        if !input.file.path.starts_with("native/") || !used.insert(index) {
            return Err("native source entries must be distinct beneath native/".into());
        }
    }
    for archive in &rust.crates {
        let path = format!("rust/{}-{}.crate", archive.name, archive.version);
        let (index, _) = spec
            .archives
            .iter()
            .enumerate()
            .find(|(_, input)| {
                input.file.path == path
                    && input.file.sha256 == archive.registry_checksum
                    && input.source_url.as_deref() == Some(&archive.source_url)
            })
            .ok_or_else(|| format!("missing registry source: {path}"))?;
        if !used.insert(index) {
            return Err("duplicate registry source".into());
        }
    }
    Ok((native.archives.len(), rust.crates.len()))
}

fn check_project(input: &Input, commit: &str) -> Result<()> {
    verify_file(&input.source, input.file.bytes, &input.file.sha256)?;
    let mut archive =
        zip::ZipArchive::new(regular_file(&input.source)?).map_err(|e| e.to_string())?;
    if archive.comment() != commit.as_bytes() {
        return Err("project ZIP must be git archive of the declared commit (ZIP comment)".into());
    }
    if archive.by_name("config/native-catalog.json").is_ok() {
        return Err("project archive must exclude generated config/native-catalog.json".into());
    }
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "LICENSE",
        "native-workers/Cargo.toml",
        "native-workers/Cargo.lock",
        "native-workers/qwen/src/main.rs",
        "native-workers/sherpa/src/main.rs",
        "native-workers/bge/src/main.rs",
        "docs/NATIVE_WORKERS.md",
        "xtask/Cargo.toml",
        "xtask/src/main.rs",
        "xtask/src/sources_build.rs",
        "docs/native-distribution/sources.json",
        "docs/native-distribution/rust-workers.json",
        "docs/native-distribution/models.json",
        "docs/native-distribution/runtime-files.json",
        "docs/native-distribution/materials.json",
        "docs/native-distribution/ollama-cpu-files.json",
    ] {
        if archive.by_name(path).is_err() {
            return Err(format!("project ZIP lacks {path}"));
        }
    }
    Ok(())
}

fn check_distribution(input: &Input, directory: &Path) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(regular_file(&input.source)?).map_err(|e| e.to_string())?;
    for name in [
        "README.md",
        "sources.json",
        "rust-workers.json",
        "models.json",
        "runtime-files.json",
        "materials.json",
        "ollama-cpu-files.json",
    ] {
        let expected = hash_file(&directory.join(name))?;
        let mut file = archive
            .by_name(&format!("docs/native-distribution/{name}"))
            .map_err(|e| format!("project distribution material {name}: {e}"))?;
        if hash_reader(&mut file)? != expected {
            return Err(format!(
                "project distribution material differs from fixed input: {name}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn project_archive_requires_commit_comment_and_build_sources() {
        let root = std::env::temp_dir().join(format!(
            "native-source-project-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let source = root.join("project.zip");
        let commit = "1".repeat(40);
        let mut zip = zip::ZipWriter::new(crate::bundle_io::create_new(&source).unwrap());
        zip.set_comment(commit.clone());
        for path in [
            "Cargo.toml",
            "Cargo.lock",
            "LICENSE",
            "native-workers/Cargo.toml",
            "native-workers/Cargo.lock",
            "native-workers/qwen/src/main.rs",
            "native-workers/sherpa/src/main.rs",
            "native-workers/bge/src/main.rs",
            "docs/NATIVE_WORKERS.md",
            "xtask/Cargo.toml",
            "xtask/src/main.rs",
            "xtask/src/sources_build.rs",
            "docs/native-distribution/sources.json",
            "docs/native-distribution/rust-workers.json",
            "docs/native-distribution/models.json",
            "docs/native-distribution/runtime-files.json",
            "docs/native-distribution/materials.json",
            "docs/native-distribution/ollama-cpu-files.json",
        ] {
            zip.start_file(path, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"synthetic source fixture").unwrap();
        }
        zip.finish().unwrap();
        let (bytes, sha256) = hash_file(&source).unwrap();
        let mut input = Input {
            source,
            file: FileRecord {
                path: "project/source.zip".into(),
                bytes,
                sha256,
            },
            source_url: None,
        };
        check_project(&input, &commit).unwrap();
        assert!(check_project(&input, &"2".repeat(40)).is_err());
        let recursive_source = root.join("recursive-project.zip");
        let mut recursive_zip =
            zip::ZipWriter::new(crate::bundle_io::create_new(&recursive_source).unwrap());
        recursive_zip.set_comment(commit.clone());
        recursive_zip
            .start_file(
                "config/native-catalog.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        recursive_zip.write_all(b"{}").unwrap();
        recursive_zip.finish().unwrap();
        let (bytes, sha256) = hash_file(&recursive_source).unwrap();
        let recursive_input = Input {
            source: recursive_source,
            file: FileRecord {
                path: "project/source.zip".into(),
                bytes,
                sha256,
            },
            source_url: None,
        };
        assert!(
            check_project(&recursive_input, &commit)
                .unwrap_err()
                .contains("must exclude generated")
        );
        let file = root.join("archive.bin");
        std::fs::write(&file, b"synthetic archive").unwrap();
        let (bytes, sha256) = hash_file(&file).unwrap();
        let mut native = Vec::new();
        let mut rust = Vec::new();
        let mut archives = Vec::new();
        for index in 0..13 {
            let url = format!("https://example.org/native/{index}.zip");
            native.push(json!({"bytes":bytes,"sha256":sha256,"source_url":url}));
            archives.push(json!({"source":file,"path":format!("native/{index}.zip"),
                "bytes":bytes,"sha256":sha256,"source_url":url}));
        }
        for index in 0..238 {
            let name = format!("fixture-{index}");
            let url = format!("https://example.org/rust/{name}-1.crate");
            rust.push(
                json!({"name":name,"version":"1","registry_checksum":sha256,"source_url":url}),
            );
            archives.push(json!({"source":file,"path":format!("rust/{name}-1.crate"),
                "bytes":bytes,"sha256":sha256,"source_url":url}));
        }
        crate::bundle_io::write_json(&root.join("sources.json"), &json!({"archives":native}))
            .unwrap();
        crate::bundle_io::write_json(&root.join("rust-workers.json"), &json!({"crates":rust}))
            .unwrap();
        for name in [
            "README.md",
            "models.json",
            "runtime-files.json",
            "materials.json",
            "ollama-cpu-files.json",
        ] {
            std::fs::write(root.join(name), b"synthetic distribution fixture").unwrap();
        }
        let final_project = root.join("project-final.zip");
        let mut initial = zip::ZipArchive::new(regular_file(&input.source).unwrap()).unwrap();
        let mut final_zip =
            zip::ZipWriter::new(crate::bundle_io::create_new(&final_project).unwrap());
        final_zip.set_comment(commit.clone());
        for index in 0..initial.len() {
            let mut file = initial.by_index(index).unwrap();
            final_zip
                .start_file(file.name(), zip::write::SimpleFileOptions::default())
                .unwrap();
            if let Some(name) = file.name().strip_prefix("docs/native-distribution/") {
                final_zip
                    .write_all(&std::fs::read(root.join(name)).unwrap())
                    .unwrap();
            } else {
                std::io::copy(&mut file, &mut final_zip).unwrap();
            }
        }
        final_zip
            .start_file(
                "docs/native-distribution/README.md",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        final_zip
            .write_all(&std::fs::read(root.join("README.md")).unwrap())
            .unwrap();
        final_zip.finish().unwrap();
        drop(initial);
        input.source = final_project;
        (input.file.bytes, input.file.sha256) = hash_file(&input.source).unwrap();
        let materials = [
            "llvm-mingw-20240619-build-llvm.sh",
            "llvm-mingw-20240619-build-mingw-w64.sh",
            "llvm-mingw-20240619-build-mingw-w64-libraries.sh",
        ]
        .map(|name| {
            json!({"source":file,
                "path":format!("build/{name}"),"bytes":bytes,"sha256":sha256,"source_url":null})
        });
        let spec = root.join("input.json");
        crate::bundle_io::write_json(
            &spec,
            &json!({"schema_version":1,"distribution_directory":root,
            "project_commit":commit,"project_archive":{"source":input.source,"path":input.file.path,
                "bytes":input.file.bytes,"sha256":input.file.sha256,"source_url":null},
            "archives":archives,"build_materials":materials}),
        )
        .unwrap();
        let output = root.join("sources.zip");
        let (report, passed) = build(&spec, &output).unwrap();
        assert!(passed);
        assert_eq!(report["artifacts"], 256);
        assert!(build(&spec, &output).is_err());
        let mut bad: Value = read_json(&spec).unwrap();
        let mut recursive = bad["build_materials"][0].clone();
        recursive["path"] = json!("build/config/native-catalog.json");
        bad["build_materials"]
            .as_array_mut()
            .unwrap()
            .push(recursive);
        let bad_spec = root.join("recursive-input.json");
        crate::bundle_io::write_json(&bad_spec, &bad).unwrap();
        let error = build(&bad_spec, &root.join("recursive.zip")).unwrap_err();
        assert!(error.contains("generated native catalog"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_metadata_requires_every_fixed_native_and_registry_archive() {
        let distribution = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("docs/native-distribution");
        let native: NativeSources = read_json(&distribution.join("sources.json")).unwrap();
        let rust: RustSources = read_json(&distribution.join("rust-workers.json")).unwrap();
        let mut archives = native
            .archives
            .into_iter()
            .enumerate()
            .map(|(index, input)| Input {
                source: PathBuf::new(),
                file: FileRecord {
                    path: format!("native/{index}.zip"),
                    bytes: input.bytes,
                    sha256: input.sha256,
                },
                source_url: Some(input.source_url),
            })
            .collect::<Vec<_>>();
        archives.extend(rust.crates.into_iter().map(|input| Input {
            source: PathBuf::new(),
            file: FileRecord {
                path: format!("rust/{}-{}.crate", input.name, input.version),
                bytes: 1,
                sha256: input.registry_checksum,
            },
            source_url: Some(input.source_url),
        }));
        let mut spec = Spec {
            schema_version: 1,
            distribution_directory: distribution,
            project_commit: "1".repeat(40),
            project_archive: Input {
                source: PathBuf::new(),
                file: FileRecord {
                    path: "project/source.zip".into(),
                    bytes: 1,
                    sha256: "0".repeat(64),
                },
                source_url: None,
            },
            archives,
            build_materials: Vec::new(),
        };
        check_archives(&spec).unwrap();
        let updated = std::env::temp_dir().join(format!(
            "native-source-extra-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&updated).unwrap();
        let native: Value = read_json(&spec.distribution_directory.join("sources.json")).unwrap();
        let mut registry: Value =
            read_json(&spec.distribution_directory.join("rust-workers.json")).unwrap();
        let checksum = crate::bundle_io::hash_bytes(b"synthetic extra build dependency");
        registry["crates"].as_array_mut().unwrap().push(json!({
            "name":"synthetic-build-extra","version":"1.0.0","registry_checksum":checksum,
            "source_url":"https://example.org/synthetic-build-extra-1.0.0.crate"
        }));
        crate::bundle_io::write_json(&updated.join("sources.json"), &native).unwrap();
        crate::bundle_io::write_json(&updated.join("rust-workers.json"), &registry).unwrap();
        spec.distribution_directory = updated.clone();
        assert!(
            check_archives(&spec).is_err(),
            "a new inventory item cannot be silently omitted"
        );
        spec.archives.push(Input {
            source: PathBuf::new(),
            file: FileRecord {
                path: "rust/synthetic-build-extra-1.0.0.crate".into(),
                bytes: 32,
                sha256: checksum,
            },
            source_url: Some("https://example.org/synthetic-build-extra-1.0.0.crate".into()),
        });
        let (_, registry_count) = check_archives(&spec).unwrap();
        assert_eq!(registry_count, registry["crates"].as_array().unwrap().len());
        spec.archives.last_mut().unwrap().file.sha256 = "0".repeat(64);
        assert!(check_archives(&spec).is_err());
        std::fs::remove_dir_all(updated).unwrap();
    }
}
