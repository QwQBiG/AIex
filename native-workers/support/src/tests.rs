use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ai_ex_model_catalog::{Artifact, License, ModelCapability, ModelManifest};
use serde::Deserialize;

use crate::{ArtifactKind, AudioFile, JobDirectory, ModelBackend, VerifiedConfig};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SHA_X: &str = "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "aiex-worker-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("unique test directory");
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for entry in fs::read_dir(&self.0).expect("fixture") {
            let path = entry.expect("fixture entry").path();
            if path.is_dir() {
                for file in fs::read_dir(&path).expect("fixture data") {
                    fs::remove_file(file.expect("file").path()).expect("remove fixture file");
                }
                fs::remove_dir(path).expect("remove fixture directory");
            } else {
                fs::remove_file(path).expect("remove fixture file");
            }
        }
        fs::remove_dir(&self.0).expect("remove fixture root");
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Backend {
    model: String,
}

fn setup(fixture: &Fixture) -> PathBuf {
    let root = &fixture.0;
    fs::create_dir(root.join("data")).unwrap();
    let artifacts: Vec<_> = [
        ("data/model.onnx", ArtifactKind::Model),
        ("LICENSE.txt", ArtifactKind::License),
        ("runtime.dll", ArtifactKind::NativeRuntime),
    ]
    .into_iter()
    .map(|(path, kind)| {
        fs::write(root.join(path), b"x").unwrap();
        Artifact {
            path: path.into(),
            kind,
            bytes: 1,
            sha256: SHA_X.into(),
        }
    })
    .collect();
    let manifest = ModelManifest {
        id: "worker-test".into(),
        revision: "20261031".into(),
        backend: ModelBackend::OnnxEmbedding,
        capabilities: vec![ModelCapability::Embedding],
        artifacts,
        licenses: vec![License {
            id: "MIT".into(),
            source_url: "https://example.com/LICENSE.txt".into(),
        }],
        runtimes: vec![],
    };
    let manifest_path = root.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let config = root.join("config.json");
    fs::write(&config, serde_json::to_vec(&serde_json::json!({"schema_version":1,"manifest":manifest_path,"directory":root,"backend":{"model":"data/model.onnx"}})).unwrap()).unwrap();
    config
}

#[test]
fn worker_verifies_bytes_before_resolving_native_paths() {
    let fixture = Fixture::new();
    let config = setup(&fixture);
    let verified = VerifiedConfig::<Backend>::load(&config, ModelBackend::OnnxEmbedding).unwrap();
    assert!(
        verified
            .artifact(&verified.backend.model, ArtifactKind::Model)
            .is_ok()
    );
    assert!(
        verified
            .artifact("runtime.dll", ArtifactKind::Model)
            .is_err()
    );
    fs::write(fixture.0.join("runtime.dll"), b"corrupt").unwrap();
    assert!(VerifiedConfig::<Backend>::load(&config, ModelBackend::OnnxEmbedding).is_err());
}

#[test]
fn directory_reader_cannot_discover_unverified_files() {
    let fixture = Fixture::new();
    let config = setup(&fixture);
    let verified = VerifiedConfig::<Backend>::load(&config, ModelBackend::OnnxEmbedding).unwrap();
    assert!(verified.artifact_directory("data").is_ok());
    fs::write(fixture.0.join("data/extra.bin"), b"unverified").unwrap();
    assert!(verified.artifact_directory("data").is_err());
}

#[test]
fn audio_output_is_pcm16_and_does_not_overwrite_input() {
    let fixture = Fixture::new();
    let jobs = JobDirectory::at(fixture.0.clone());
    let file = AudioFile {
        path: "reference.wav".into(),
    };
    jobs.write_audio(&file, &[-1.0, 0.0, 1.0], 24000).unwrap();
    let first = fs::read(fixture.0.join(&file.path)).unwrap();
    assert_eq!(jobs.read_audio(&file).unwrap().1, 24000);
    assert!(jobs.write_audio(&file, &[0.5], 24000).is_err());
    assert_eq!(fs::read(fixture.0.join(&file.path)).unwrap(), first);
    assert!(
        jobs.input(&AudioFile {
            path: "../reference.wav".into()
        })
        .is_err()
    );
}

#[test]
fn recognition_and_reference_inputs_reject_float_wav() {
    let fixture = Fixture::new();
    let path = fixture.0.join("float.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 24000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    writer.write_sample(0.25_f32).unwrap();
    writer.finalize().unwrap();
    let jobs = JobDirectory::at(fixture.0.clone());
    assert!(
        jobs.read_audio(&AudioFile {
            path: "float.wav".into()
        })
        .is_err()
    );
}
