use std::fs;
use std::io::Write;
use std::time::Duration;

use ai_ex_model_catalog::{ArtifactKind, License, ModelManifest};

use super::*;
use crate::{ArtifactSource, DownloadLocation, RuntimeWorker};

mod network;
mod storage;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("native models 中文 {}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn store(&self) -> PathBuf {
        self.0.join("managed")
    }
    fn model_source(&self) -> PathBuf {
        let root = self.0.join("offline 模型");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("weights.bin"), b"native model fixture").unwrap();
        fs::write(root.join("LICENSE.txt"), b"synthetic fixture only").unwrap();
        root
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn artifact(path: &str, kind: ArtifactKind, content: &str) -> Artifact {
    Artifact {
        path: path.into(),
        kind,
        bytes: content.len() as u64,
        sha256: crate::files::hash_bytes(content.as_bytes()),
    }
}

fn sources(artifacts: &[Artifact], content: &[&str]) -> PackageSource {
    PackageSource::Files {
        files: artifacts
            .iter()
            .zip(content)
            .map(|(file, content)| ArtifactSource {
                path: file.path.clone(),
                location: DownloadLocation::Inline {
                    content: (*content).into(),
                },
            })
            .collect(),
    }
}

fn catalog() -> DistributionCatalog {
    let licenses = vec![License {
        id: "MIT".into(),
        source_url: "https://example.invalid/fixture/LICENSE".into(),
    }];
    let artifacts = vec![
        artifact("weights.bin", ArtifactKind::Model, "native model fixture"),
        artifact(
            "LICENSE.txt",
            ArtifactKind::License,
            "synthetic fixture only",
        ),
    ];
    let runtime_artifacts = vec![
        artifact(
            "bge/worker.exe",
            ArtifactKind::NativeRuntime,
            "not an executable",
        ),
        artifact("bge/ort.dll", ArtifactKind::NativeRuntime, "not a DLL"),
        artifact(
            "LICENSE.txt",
            ArtifactKind::License,
            "synthetic fixture only",
        ),
    ];
    DistributionCatalog {
        schema_version: 1,
        models: vec![ModelPackage {
            name: "测试模型".into(),
            source: sources(
                &artifacts,
                &["native model fixture", "synthetic fixture only"],
            ),
            manifest: ModelManifest {
                id: "bge-small-zh-v1.5".into(),
                revision: "fixed-model-1".into(),
                backend: ModelBackend::OnnxEmbedding,
                capabilities: vec![ModelCapability::Embedding],
                artifacts,
                licenses: licenses.clone(),
                runtimes: vec![RuntimeRequirement {
                    id: "native-cpu".into(),
                    revision: "fixed-runtime-1".into(),
                }],
            },
        }],
        runtimes: vec![RuntimePackage {
            name: "测试运行包".into(),
            source: sources(
                &runtime_artifacts,
                &["not an executable", "not a DLL", "synthetic fixture only"],
            ),
            manifest: RuntimeManifest {
                id: "native-cpu".into(),
                revision: "fixed-runtime-1".into(),
                protocol_version: 1,
                backends: vec![ModelBackend::OnnxEmbedding],
                artifacts: runtime_artifacts,
                licenses,
                workers: vec![RuntimeWorker {
                    backend: ModelBackend::OnnxEmbedding,
                    executable: "bge/worker.exe".into(),
                    runtime_library: Some("bge/ort.dll".into()),
                }],
                ollama_executable: None,
            },
        }],
    }
}

fn snapshot(manager: &ModelManager, id: &str) -> PackageSnapshot {
    manager
        .snapshot()
        .unwrap()
        .into_iter()
        .find(|package| package.id == id)
        .unwrap()
}

async fn completed(manager: &ModelManager, operation: Uuid) {
    let result = tokio::time::timeout(Duration::from_secs(5), manager.wait(operation))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        result.phase,
        OperationPhase::Completed,
        "{:?}",
        result.error
    );
}

#[tokio::test]
async fn imported_model_waits_for_runtime_and_leases_protect_both() {
    let fixture = Fixture::new();
    let source = fixture.model_source();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    completed(
        &manager,
        manager
            .start_import("bge-small-zh-v1.5", &source)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::NotPrepared
    );
    assert!(snapshot(&manager, "bge-small-zh-v1.5").installed);
    assert!(manager.acquire("bge-small-zh-v1.5").is_err());
    completed(
        &manager,
        manager.start_download("native-cpu").await.unwrap(),
    )
    .await;
    let lease = manager.acquire("bge-small-zh-v1.5").unwrap();
    assert_eq!(snapshot(&manager, "native-cpu").state, PackageState::InUse);
    assert!(manager.remove("native-cpu").await.is_err());
    assert!(manager.remove("bge-small-zh-v1.5").await.is_err());
    assert!(lease.required_runtime("native-cpu").is_some());
    assert_eq!(
        fs::read(lease.data_directory().join("weights.bin")).unwrap(),
        b"native model fixture"
    );
    drop(lease);
    completed(&manager, manager.remove("native-cpu").await.unwrap()).await;
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::NotPrepared
    );
    assert!(snapshot(&manager, "bge-small-zh-v1.5").installed);
    completed(
        &manager,
        manager.start_download("native-cpu").await.unwrap(),
    )
    .await;
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::Ready
    );
    manager.shutdown().await.unwrap();
}

#[test]
fn rejects_floating_inputs_unsafe_ids_and_incompatible_runtime() {
    let mut value = catalog();
    value.validate().unwrap();
    value.models[0].manifest.id = "..".into();
    assert!(value.validate().is_err());
    let mut value = catalog();
    value.runtimes[0].manifest.protocol_version = 2;
    assert!(value.validate().is_err());
    let mut value = catalog();
    value.models[0].manifest.runtimes[0].revision = "other".into();
    assert!(value.validate().is_err());
    let mut value = catalog();
    if let PackageSource::Files { files } = &mut value.models[0].source {
        files[0].location = DownloadLocation::Https {
            url: "https://huggingface.co/org/model/resolve/main/model.onnx".into(),
        };
    }
    assert!(value.validate().is_err());
    let mut json = serde_json::to_value(catalog()).unwrap();
    json["auto_execute"] = serde_json::json!(true);
    assert!(serde_json::from_value::<DistributionCatalog>(json).is_err());
}

#[test]
fn runtime_dll_isolation_uses_windows_case_comparison() {
    let mut value = catalog();
    let runtime = &mut value.runtimes[0];
    runtime.manifest.backends.push(ModelBackend::Qwen3TtsOnnx);
    runtime.manifest.workers.push(RuntimeWorker {
        backend: ModelBackend::Qwen3TtsOnnx,
        executable: "BGE/qwen.exe".into(),
        runtime_library: Some("bge/ort.dll".into()),
    });
    runtime.manifest.artifacts.push(artifact(
        "BGE/qwen.exe",
        ArtifactKind::NativeRuntime,
        "qwen fixture",
    ));
    if let PackageSource::Files { files } = &mut runtime.source {
        files.push(ArtifactSource {
            path: "BGE/qwen.exe".into(),
            location: DownloadLocation::Inline {
                content: "qwen fixture".into(),
            },
        });
    }
    assert!(
        value
            .validate()
            .unwrap_err()
            .message
            .contains("separate DLL directories")
    );
}
