use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::*;

const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn manifest() -> ModelManifest {
    ModelManifest {
        id: "fixture-encoder".to_owned(),
        revision: "v1.2.3".to_owned(),
        backend: ModelBackend::OnnxEmbedding,
        capabilities: vec![ModelCapability::Embedding],
        artifacts: vec![
            Artifact {
                path: "model.onnx".to_owned(),
                kind: ArtifactKind::Model,
                bytes: 3,
                sha256: ABC_SHA256.to_owned(),
            },
            Artifact {
                path: "LICENSE".to_owned(),
                kind: ArtifactKind::License,
                bytes: 3,
                sha256: ABC_SHA256.to_owned(),
            },
        ],
        licenses: vec![License {
            id: "fixture-license".to_owned(),
            source_url: "https://example.invalid/fixture-license".to_owned(),
        }],
        runtimes: vec![RuntimeRequirement {
            id: "onnxruntime".to_owned(),
            revision: "1.22.0".to_owned(),
        }],
    }
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "model-catalog-中文 space-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn prepare(&self) {
        fs::write(self.root.join("model.onnx"), b"abc").unwrap();
        fs::write(self.root.join("LICENSE"), b"abc").unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Only explicit fixture-owned paths; unexpected files are preserved.
        let _ = fs::remove_file(self.root.join("model.onnx"));
        let _ = fs::remove_file(self.root.join("LICENSE"));
        let _ = fs::remove_dir(self.root.join("linked"));
        let _ = fs::remove_dir(self.root.join("model.onnx"));
        let _ = fs::remove_dir(&self.root);
    }
}

#[test]
fn catalog_round_trips_and_rejects_unknown_schema_and_fields() {
    let mut catalog = ModelCatalog {
        schema_version: CATALOG_SCHEMA_VERSION,
        models: vec![manifest()],
    };
    catalog.validate().unwrap();
    let json = serde_json::to_value(&catalog).unwrap();
    assert_eq!(
        serde_json::from_value::<ModelCatalog>(json.clone()).unwrap(),
        catalog
    );
    let mut unknown = json;
    unknown["run_script"] = serde_json::json!("export");
    assert!(serde_json::from_value::<ModelCatalog>(unknown).is_err());
    catalog.schema_version += 1;
    assert!(catalog.validate().is_err());
}

#[test]
fn rejects_duplicate_model_ids_and_windows_artifact_paths() {
    let catalog = ModelCatalog {
        schema_version: CATALOG_SCHEMA_VERSION,
        models: vec![manifest(), manifest()],
    };
    assert!(catalog.validate().is_err());
    let mut model = manifest();
    let mut duplicate = model.artifacts[0].clone();
    duplicate.path = "MODEL.ONNX".to_owned();
    model.artifacts.push(duplicate);
    assert!(model.validate().is_err());
    let mut model = manifest();
    model.artifacts[0].path = "voices/σ.onnx".to_owned();
    let mut duplicate = model.artifacts[0].clone();
    duplicate.path = "voices/ς.onnx".to_owned();
    model.artifacts.push(duplicate);
    assert!(model.validate().is_err());
}

#[test]
fn rejects_mutable_model_and_runtime_revisions() {
    for revision in [
        "latest",
        "v1-latest",
        "main",
        "master",
        "HEAD",
        "stable",
        "nightly",
    ] {
        let mut model = manifest();
        model.revision = revision.to_owned();
        assert!(model.validate().is_err());
        model = manifest();
        model.runtimes[0].revision = revision.to_owned();
        assert!(model.validate().is_err());
    }
}

#[test]
fn requires_matching_capabilities_and_license_provenance() {
    let mut model = manifest();
    model.backend = ModelBackend::SherpaWhisper;
    assert!(model.validate().is_err());
    model.capabilities = vec![ModelCapability::Transcription];
    model.validate().unwrap();
    model.capabilities.push(ModelCapability::Transcription);
    assert!(model.validate().is_err());
    model = manifest();
    model.licenses.clear();
    assert!(model.validate().is_err());
    model = manifest();
    model
        .artifacts
        .retain(|artifact| artifact.kind != ArtifactKind::License);
    assert!(model.validate().is_err());
    model = manifest();
    model.licenses[0].source_url = "http://example.invalid/license".to_owned();
    assert!(model.validate().is_err());
}

#[test]
fn accepts_kokoro_frontend_data_and_rejects_conversion_scripts() {
    let mut artifact = Artifact {
        path: "date-zh.fst".into(),
        kind: ArtifactKind::Configuration,
        bytes: 3,
        sha256: ABC_SHA256.into(),
    };
    for path in [
        "date-zh.fst",
        "dict/jieba.dict.utf8",
        "espeak-ng-data/zh_dict",
    ] {
        artifact.path = path.into();
        artifact.validate().unwrap();
    }
    artifact.path = "dict/generate_user_dict.py".into();
    assert!(artifact.validate().is_err());
}

#[test]
fn license_only_manifest_cannot_become_ready() {
    let fixture = Fixture::new();
    fixture.prepare();
    let mut model = manifest();
    model
        .artifacts
        .retain(|artifact| artifact.kind == ArtifactKind::License);
    assert!(model.validate().is_err());
    assert!(verify_directory(&model, &fixture.root).is_err());
}

#[test]
fn rejects_scripts_executables_as_data_and_invalid_hash_metadata() {
    for path in [
        "../model.onnx",
        "export.py",
        "export.PY",
        "x.psm1",
        "runner.exe",
        "native.dll",
    ] {
        let mut artifact = manifest().artifacts[0].clone();
        artifact.path = path.to_owned();
        assert!(artifact.validate().is_err(), "accepted {path}");
    }
    let mut artifact = manifest().artifacts[0].clone();
    artifact.kind = ArtifactKind::NativeRuntime;
    assert!(artifact.validate().is_err());
    artifact.path = "runtime/native.dll".to_owned();
    artifact.validate().unwrap();
    artifact.sha256 = "1234".to_owned();
    assert!(artifact.validate().is_err());
    artifact.sha256 = "g".repeat(64);
    assert!(artifact.validate().is_err());
    artifact.sha256 = ABC_SHA256.to_owned();
    artifact.bytes = 0;
    assert!(artifact.validate().is_err());
}

#[test]
fn missing_files_are_not_prepared_and_verification_creates_nothing() {
    let fixture = Fixture::new();
    let report = verify_directory(&manifest(), &fixture.root).unwrap();
    assert_eq!(report.status, PreparationStatus::NotPrepared);
    assert_eq!(report.verified_bytes, 0);
    assert_eq!(report.issues.len(), 2);
    assert!(
        report
            .issues
            .iter()
            .all(|issue| issue.kind == ArtifactIssueKind::Missing)
    );
    assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
    let absent = fixture.root.join("not-created");
    assert_eq!(
        verify_directory(&manifest(), &absent).unwrap().status,
        PreparationStatus::NotPrepared
    );
    assert!(!absent.exists());
}

#[test]
fn known_sha256_files_are_ready_and_unchanged() {
    let fixture = Fixture::new();
    fixture.prepare();
    let mut model = manifest();
    model.artifacts[0].sha256 = ABC_SHA256.to_ascii_uppercase();
    let report = verify_directory(&model, &fixture.root).unwrap();
    assert_eq!(report.status, PreparationStatus::Ready);
    assert_eq!(report.verified_bytes, 6);
    assert!(report.issues.is_empty());
    assert_eq!(fs::read(fixture.root.join("model.onnx")).unwrap(), b"abc");
    assert_eq!(fs::read(fixture.root.join("LICENSE")).unwrap(), b"abc");
}

#[test]
fn reports_size_and_same_size_hash_mismatches_separately() {
    let fixture = Fixture::new();
    fixture.prepare();
    fs::write(fixture.root.join("model.onnx"), b"abcd").unwrap();
    let report = verify_directory(&manifest(), &fixture.root).unwrap();
    assert_eq!(report.status, PreparationStatus::NotPrepared);
    assert_eq!(report.verified_bytes, 3);
    assert_eq!(
        report.issues[0].kind,
        ArtifactIssueKind::SizeMismatch {
            expected: 3,
            actual: 4
        }
    );
    fs::write(fixture.root.join("model.onnx"), b"abd").unwrap();
    let report = verify_directory(&manifest(), &fixture.root).unwrap();
    assert!(matches!(
        report.issues[0].kind,
        ArtifactIssueKind::HashMismatch { .. }
    ));
    assert_eq!(fs::read(fixture.root.join("model.onnx")).unwrap(), b"abd");
}

#[test]
fn refuses_a_directory_in_place_of_an_artifact() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("model.onnx")).unwrap();
    fs::write(fixture.root.join("LICENSE"), b"abc").unwrap();
    let report = verify_directory(&manifest(), &fixture.root).unwrap();
    assert_eq!(report.status, PreparationStatus::NotPrepared);
    assert_eq!(report.issues[0].kind, ArtifactIssueKind::NotRegularFile);
}

#[test]
fn verifies_all_chunks_and_detects_a_late_byte_change() {
    let fixture = Fixture::new();
    fixture.prepare();
    let mut bytes: Vec<u8> = (0..131_073).map(|index| (index % 251) as u8).collect();
    let mut model = manifest();
    model.artifacts[0].bytes = bytes.len() as u64;
    model.artifacts[0].sha256 = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    fs::write(fixture.root.join("model.onnx"), &bytes).unwrap();
    assert_eq!(
        verify_directory(&model, &fixture.root).unwrap().status,
        PreparationStatus::Ready
    );
    bytes[131_072] ^= 1;
    fs::write(fixture.root.join("model.onnx"), &bytes).unwrap();
    let report = verify_directory(&model, &fixture.root).unwrap();
    assert!(matches!(
        report.issues[0].kind,
        ArtifactIssueKind::HashMismatch { .. }
    ));
}

#[cfg(windows)]
#[test]
fn refuses_a_windows_directory_junction() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let source = Fixture::new();
    source.prepare();
    let fixture = Fixture::new();
    fs::write(fixture.root.join("LICENSE"), b"abc").unwrap();
    let junction = fixture.root.join("linked");
    let output = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&source.root)
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction creation failed: {:?}",
        output.status
    );
    let mut model = manifest();
    model.artifacts[0].path = "linked/model.onnx".to_owned();
    let report = verify_directory(&model, &fixture.root).unwrap();
    assert_eq!(report.status, PreparationStatus::NotPrepared);
    assert_eq!(report.issues[0].kind, ArtifactIssueKind::LinkNotAllowed);
    assert_eq!(fs::read(source.root.join("model.onnx")).unwrap(), b"abc");
    fs::remove_dir(&junction).unwrap();
}
