use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ai_ex_model_catalog::{Artifact, ArtifactKind, License, ModelBackend};
use ai_ex_model_manager::{DistributionCatalog, RuntimeManifest, RuntimeWorker};

use crate::bundle_io::{hash_bytes, read_json, write_json};
use crate::bundle_zip::Entry;

fn directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("native-bundle-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&path).expect("new test directory");
    path
}

fn entries(path: &std::path::Path) -> Vec<Entry> {
    [
        "sherpa/worker.exe",
        "qwen/worker.exe",
        "bge/worker.exe",
        "ollama/ollama.exe",
        "LICENSE.txt",
        "qwen/onnxruntime.dll",
        "bge/onnxruntime.dll",
    ]
    .iter()
    .enumerate()
    .map(|(index, name)| {
        let content = format!("fixture-{index}");
        let input = path.join(format!("input{index}.txt"));
        std::fs::write(&input, content.as_bytes()).expect("test fixture");
        Entry {
            input: input.into(),
            artifact: Artifact {
                path: (*name).into(),
                bytes: content.len() as u64,
                sha256: hash_bytes(content.as_bytes()),
                kind: if name.ends_with(".exe") || name.ends_with(".dll") {
                    ArtifactKind::NativeRuntime
                } else {
                    ArtifactKind::License
                },
            },
        }
    })
    .collect()
}

fn manifest(entries: &[Entry]) -> RuntimeManifest {
    use ModelBackend as B;
    RuntimeManifest {
        id: crate::catalog_build::RUNTIME_ID.into(),
        revision: crate::catalog_build::REVISION.into(),
        protocol_version: 1,
        backends: vec![
            B::Ollama,
            B::SherpaWhisper,
            B::SherpaKokoro,
            B::Qwen3TtsOnnx,
            B::OnnxEmbedding,
        ],
        artifacts: entries.iter().map(|entry| entry.artifact.clone()).collect(),
        licenses: vec![License {
            id: "MIT".into(),
            source_url: "https://example.org/v1/LICENSE".into(),
        }],
        workers: [
            (B::SherpaWhisper, "sherpa/worker.exe"),
            (B::SherpaKokoro, "sherpa/worker.exe"),
            (B::Qwen3TtsOnnx, "qwen/worker.exe"),
            (B::OnnxEmbedding, "bge/worker.exe"),
        ]
        .into_iter()
        .map(|(backend, executable)| RuntimeWorker {
            backend,
            executable: executable.into(),
            runtime_library: match backend {
                B::Qwen3TtsOnnx => Some("qwen/onnxruntime.dll".into()),
                B::OnnxEmbedding => Some("bge/onnxruntime.dll".into()),
                _ => None,
            },
        })
        .collect(),
        ollama_executable: Some("ollama/ollama.exe".into()),
    }
}

#[test]
fn archive_round_trip_rejects_changed_digest_and_existing_output() {
    let path = directory();
    let entries = entries(&path);
    let archive = path.join("runtime.zip");
    crate::bundle_zip::write_archive(&archive, &entries).expect("write ZIP");
    let mut manifest = manifest(&entries);
    crate::bundle_zip::verify_archive(&archive, &manifest.artifacts).expect("verify ZIP");
    assert!(crate::bundle_zip::write_archive(&archive, &entries).is_err());
    manifest.artifacts[0].sha256 = "0".repeat(64);
    assert!(crate::bundle_zip::verify_archive(&archive, &manifest.artifacts).is_err());
    std::fs::remove_dir_all(path).expect("remove own test directory");
}

#[test]
fn real_fixed_catalog_has_complete_models_verified_inline_text_and_no_moving_urls() {
    let path = directory();
    let entries = entries(&path);
    let runtime = manifest(&entries);
    let manifest_path = path.join("runtime.json");
    let archive = path.join("runtime.zip");
    let output = path.join("catalog.json");
    write_json(&manifest_path, &runtime).expect("runtime manifest");
    crate::bundle_zip::write_archive(&archive, &entries).expect("runtime ZIP");
    let distribution = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|path| path.join("docs/native-distribution"))
        .find(|path| path.is_dir())
        .expect("repository distribution inputs");
    let (_, passed) = crate::catalog_build::build(&distribution, &manifest_path, &archive, &output)
        .expect("fixed catalog");
    assert!(passed);
    let catalog: DistributionCatalog = read_json(&output).expect("read generated catalog");
    catalog
        .validate()
        .expect("full schema validation including inline hashes");
    assert_eq!(catalog.models.len(), 5);
    assert_eq!(
        catalog
            .models
            .iter()
            .map(|model| model.manifest.artifacts.len())
            .sum::<usize>(),
        430
    );
    let dialogue = catalog
        .models
        .iter()
        .find(|model| model.manifest.id == "qwen3-4b")
        .expect("dialogue");
    assert_eq!(dialogue.manifest.artifacts.len(), 6);
    assert!(crate::catalog_build::build(&distribution, &manifest_path, &archive, &output).is_err());
    let text = std::fs::read_to_string(&output).expect("catalog text");
    assert!(!text.contains("/resolve/main/") && !text.contains("/latest/"));
    assert!(text.contains("/releases/download/v0.9.0-alpha.1/"));
    std::fs::remove_dir_all(path).expect("remove own test directory");
}

#[test]
fn command_options_require_unique_named_paths() {
    let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(
        crate::command_paths(
            &args(&["--output", "new.zip", "--spec", "spec.json"]),
            &["--spec", "--output"]
        )
        .is_ok()
    );
    assert!(
        crate::command_paths(
            &args(&["--spec", "a", "--spec", "b"]),
            &["--spec", "--output"]
        )
        .is_err()
    );
    assert!(crate::command_paths(&args(&["--wrong", "a"]), &["--spec"]).is_err());
}

#[test]
fn corresponding_sources_remain_an_opaque_verified_runtime_artifact() {
    let path = directory();
    let sources = path.join("sources.zip");
    crate::bundle_zip::write_source_archive(
        &sources,
        &[Entry::generated(
            "rust/example-1.0.0.crate",
            b"opaque source bytes".to_vec(),
        )],
    )
    .unwrap();
    let (bytes, sha256) = crate::bundle_io::hash_file(&sources).unwrap();
    let input = serde_json::json!({"source":sources,"bytes":bytes,"sha256":sha256});
    let mut entries = entries(&path);
    crate::bundle::append_sources(
        &mut entries,
        Some(&serde_json::from_value(input.clone()).unwrap()),
    )
    .unwrap();
    let runtime = manifest(&entries);
    runtime.validate().unwrap();
    let source = runtime
        .artifacts
        .iter()
        .find(|item| item.kind == ArtifactKind::SourceArchive)
        .unwrap();
    assert_eq!(source.path, "sources/corresponding-sources.zip");
    assert_eq!(runtime.artifacts.len(), 8);
    let output = path.join("runtime.zip");
    crate::bundle_zip::write_archive(&output, &entries).unwrap();
    crate::bundle_zip::verify_archive(&output, &runtime.artifacts).unwrap();
    let mut bad = input;
    bad["sha256"] = serde_json::json!("0".repeat(64));
    assert!(
        crate::bundle::append_sources(&mut entries, Some(&serde_json::from_value(bad).unwrap()))
            .is_err()
    );
    std::fs::remove_dir_all(path).unwrap();
}
