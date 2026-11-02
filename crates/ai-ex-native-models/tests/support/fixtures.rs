use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ai_ex_config::native::{ModelSelection, NativeBackend};
use ai_ex_model_catalog::{
    Artifact, ArtifactKind, License, ModelBackend, ModelCapability, ModelManifest,
    RuntimeRequirement,
};
use ai_ex_model_manager::{
    ArtifactSource, DistributionCatalog, DownloadLocation, ModelManager, ModelPackage,
    OperationPhase, PackageSource, RuntimeManifest, RuntimePackage, RuntimeWorker,
};
use ai_ex_native_models::{NativeWorkerOptions, NativeWorkerPool};
use uuid::Uuid;

pub struct Fixture {
    pub root: PathBuf,
    pub manager: ModelManager,
    pub pool: NativeWorkerPool,
    pub selection: ModelSelection,
}
impl Fixture {
    pub async fn new(speech: bool, startup_hold: bool) -> Self {
        let root = std::env::temp_dir().join(format!("native-provider 中文 {}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let model_source = root.join("model-source");
        fs::create_dir(&model_source).unwrap();
        let runtime_source = root.join("runtime-source");
        fs::create_dir_all(runtime_source.join("worker")).unwrap();
        let exe = if startup_hold {
            "worker/startup-hold.exe"
        } else {
            "worker/native-model-test-worker.exe"
        };
        fs::copy(
            env!("CARGO_BIN_EXE_native-model-test-worker"),
            runtime_source.join(exe),
        )
        .unwrap();
        if !speech {
            fs::write(runtime_source.join("worker/onnxruntime.dll"), b"fixture").unwrap();
        }
        fs::write(runtime_source.join("LICENSE.txt"), b"fixture").unwrap();
        fs::create_dir(runtime_source.join("sources")).unwrap();
        fs::write(
            runtime_source.join("sources/component.zip"),
            b"opaque source fixture",
        )
        .unwrap();
        let mut runtime_files = vec![
            artifact(&runtime_source, exe, ArtifactKind::NativeRuntime),
            artifact(&runtime_source, "LICENSE.txt", ArtifactKind::License),
        ];
        if !speech {
            runtime_files.push(artifact(
                &runtime_source,
                "worker/onnxruntime.dll",
                ArtifactKind::NativeRuntime,
            ));
        }
        runtime_files.push(artifact(
            &runtime_source,
            "sources/component.zip",
            ArtifactKind::SourceArchive,
        ));
        fs::write(model_source.join("weights.bin"), b"fixture").unwrap();
        fs::write(model_source.join("LICENSE.txt"), b"fixture").unwrap();
        let model_files = vec![
            artifact(&model_source, "weights.bin", ArtifactKind::Model),
            artifact(&model_source, "LICENSE.txt", ArtifactKind::License),
        ];
        let model_backend = if speech {
            ModelBackend::SherpaKokoro
        } else {
            ModelBackend::OnnxEmbedding
        };
        let config_backend = if speech {
            NativeBackend::SherpaKokoro
        } else {
            NativeBackend::OnnxEmbedding
        };
        let licenses = vec![License {
            id: "MIT".into(),
            source_url: "https://example.invalid/fixture/LICENSE.txt".into(),
        }];
        let model = ModelManifest {
            id: "fixture-model".into(),
            revision: "fixture-v1".into(),
            backend: model_backend,
            capabilities: vec![if speech {
                ModelCapability::SpeechPreset
            } else {
                ModelCapability::Embedding
            }],
            artifacts: model_files.clone(),
            licenses: licenses.clone(),
            runtimes: vec![RuntimeRequirement {
                id: "fixture-runtime".into(),
                revision: "fixture-v1".into(),
            }],
        };
        let runtime = RuntimeManifest {
            id: "fixture-runtime".into(),
            revision: "fixture-v1".into(),
            protocol_version: 1,
            backends: vec![model_backend],
            artifacts: runtime_files.clone(),
            licenses,
            workers: vec![RuntimeWorker {
                backend: model_backend,
                executable: exe.into(),
                runtime_library: (!speech).then(|| "worker/onnxruntime.dll".into()),
            }],
            ollama_executable: None,
        };
        let manager = ModelManager::open(
            root.join("packages"),
            DistributionCatalog {
                schema_version: 1,
                models: vec![ModelPackage {
                    name: "Fixture model".into(),
                    manifest: model,
                    source: sources(&model_files),
                }],
                runtimes: vec![RuntimePackage {
                    name: "Fixture runtime".into(),
                    manifest: runtime,
                    source: sources(&runtime_files),
                }],
            },
        )
        .await
        .unwrap();
        for (id, source) in [
            ("fixture-runtime", runtime_source),
            ("fixture-model", model_source),
        ] {
            let operation = manager.start_import(id, source).await.unwrap();
            let outcome = manager.wait(operation).await.unwrap();
            assert_eq!(outcome.phase, OperationPhase::Completed, "{outcome:?}");
        }
        let pool = NativeWorkerPool::with_options(
            manager.clone(),
            root.join("jobs"),
            root.join("voices"),
            NativeWorkerOptions {
                ready_timeout: Duration::from_secs(60),
                request_timeout: Duration::from_secs(30),
            },
        )
        .unwrap();
        let selection = ModelSelection {
            backend: config_backend,
            model_id: "fixture-model".into(),
            runtime_id: "fixture-runtime".into(),
        };
        Self {
            root,
            manager,
            pool,
            selection,
        }
    }
    pub async fn finish(self) {
        self.pool.shutdown().await.unwrap();
        self.manager.shutdown().await.unwrap();
        let root = self.root.clone();
        drop(self);
        assert!(root.starts_with(std::env::temp_dir()));
        fs::remove_dir_all(root).unwrap();
    }
    pub async fn marker(&self, name: &str) -> PathBuf {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                for entry in fs::read_dir(self.root.join("jobs")).unwrap() {
                    let path = entry.unwrap().path().join(name);
                    if path.is_file() {
                        return path;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
}

fn artifact(root: &Path, path: &str, kind: ArtifactKind) -> Artifact {
    let bytes = fs::read(root.join(path)).unwrap();
    Artifact {
        path: path.into(),
        kind,
        bytes: bytes.len() as u64,
        sha256: ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}
fn sources(artifacts: &[Artifact]) -> PackageSource {
    PackageSource::Files {
        files: artifacts
            .iter()
            .map(|artifact| ArtifactSource {
                path: artifact.path.clone(),
                location: DownloadLocation::Https {
                    url: format!("https://example.invalid/fixture/{}", artifact.path),
                },
            })
            .collect(),
    }
}
