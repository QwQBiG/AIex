use super::*;
use ai_ex_model_catalog::{
    Artifact, ArtifactKind, License, ModelBackend, ModelCapability, ModelManifest,
    RuntimeRequirement,
};
use ai_ex_model_manager::{
    ArtifactSource, DownloadLocation, ModelPackage, PackageSource, RuntimeManifest, RuntimePackage,
    RuntimeWorker,
};

fn artifacts(files: &[(&str, ArtifactKind)]) -> Vec<Artifact> {
    files
        .iter()
        .map(|(path, kind)| Artifact {
            path: (*path).into(),
            kind: *kind,
            bytes: 1,
            sha256: "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881".into(),
        })
        .collect()
}
fn source(files: &[Artifact]) -> PackageSource {
    PackageSource::Files {
        files: files
            .iter()
            .map(|file| ArtifactSource {
                path: file.path.clone(),
                location: DownloadLocation::Inline {
                    content: "x".into(),
                },
            })
            .collect(),
    }
}
fn catalog() -> DistributionCatalog {
    let model = artifacts(&[
        ("weights.bin", ArtifactKind::Model),
        ("LICENSE.txt", ArtifactKind::License),
    ]);
    let runtime = artifacts(&[
        ("worker/fake.exe", ArtifactKind::NativeRuntime),
        ("LICENSE.txt", ArtifactKind::License),
    ]);
    let licenses = vec![License {
        id: "fixture".into(),
        source_url: "https://example.invalid/fixed-v1/LICENSE.txt".into(),
    }];
    DistributionCatalog {
        schema_version: 1,
        models: vec![ModelPackage {
            name: "Synthetic voice".into(),
            source: source(&model),
            manifest: ModelManifest {
                id: "kokoro-v1.1-zh-en".into(),
                revision: "fixture-v1".into(),
                backend: ModelBackend::SherpaKokoro,
                capabilities: vec![ModelCapability::SpeechPreset],
                artifacts: model,
                licenses: licenses.clone(),
                runtimes: vec![RuntimeRequirement {
                    id: "native-cpu".into(),
                    revision: "fixture-v1".into(),
                }],
            },
        }],
        runtimes: vec![RuntimePackage {
            name: "Synthetic runtime".into(),
            source: source(&runtime),
            manifest: RuntimeManifest {
                id: "native-cpu".into(),
                revision: "fixture-v1".into(),
                protocol_version: 1,
                backends: vec![ModelBackend::SherpaKokoro],
                artifacts: runtime,
                licenses,
                workers: vec![RuntimeWorker {
                    backend: ModelBackend::SherpaKokoro,
                    executable: "worker/fake.exe".into(),
                    runtime_library: None,
                }],
                ollama_executable: None,
            },
        }],
    }
}

async fn generation(runtime: &Arc<NativeRuntime>) -> u64 {
    let NativeReply::State(state) = runtime.request(NativeRequest::State).await.unwrap() else {
        panic!("state reply")
    };
    state.preview_generation
}
fn preview(expected_generation: u64) -> NativeRequest {
    NativeRequest::PreviewVoice {
        voice_id: "builtin-zh".into(),
        text: "你好".into(),
        expected_generation,
    }
}

#[tokio::test]
async fn preview_admission_and_stop_never_reuse_an_old_confirmed_generation() {
    let fixture = Fixture::new();
    let mut config = NativeConfig {
        dialogue: None,
        transcription: None,
        embedding: None,
        ..Default::default()
    };
    config.devices.playback_enabled = true;
    NativeConfigStore::new(fixture.path())
        .unwrap()
        .create_new(&config)
        .unwrap();
    let runtime = NativeRuntime::start_inner(fixture.path(), "profile".into(), Ok(catalog()));
    runtime.wait_ready(Duration::from_secs(5)).await.unwrap();
    let manager = runtime.manager().unwrap();
    for id in ["native-cpu", "kokoro-v1.1-zh-en"] {
        let operation = manager.start_download(id).await.unwrap();
        assert_eq!(
            manager.wait(operation).await.unwrap().phase,
            ai_ex_model_manager::OperationPhase::Completed
        );
    }
    let (queue, receiver) = ai_ex_audio::SpeechQueue::new(2).unwrap();
    let events = ai_ex_observability::EventHub::new(16).unwrap();
    runtime.attach_preview(queue, receiver.player(), events.clone());
    let initial = generation(&runtime).await;
    let NativeReply::Operation(operation) = runtime.request(preview(initial)).await.unwrap() else {
        panic!("preview operation")
    };
    assert_eq!(generation(&runtime).await, initial + 1);
    runtime.request(NativeRequest::StopPreview).await.unwrap();
    assert_eq!(generation(&runtime).await, initial + 2);
    assert!(
        runtime
            .operations
            .get(operation.id)
            .unwrap()
            .unwrap()
            .phase
            .is_terminal()
    );
    let rejected = runtime.request(preview(initial)).await.unwrap_err();
    assert_eq!(rejected.kind, ai_ex_domain::ErrorKind::InvalidTransition);
    assert!(rejected.message.contains("试听状态已改变"));
    assert_eq!(generation(&runtime).await, initial + 2);
    assert_eq!(events.current().turns_started, 0);
    assert!(!events.current().playback.active);
    // The declared executable is a single synthetic byte. It cannot infer or
    // return audio, so successful admission never opens an output device.
    if let Err(error) = runtime.shutdown().await {
        assert!(error.message.contains("cannot start native worker"));
    }
    drop(receiver);
}

#[tokio::test]
async fn a_stop_acknowledged_before_an_inflight_preview_prevents_its_late_start() {
    use ai_ex_control::{
        ControlBackend, ControlClient, ControlCommand, ControlPayload, ControlServer,
    };
    use async_trait::async_trait;

    struct Delayed {
        runtime: Arc<NativeRuntime>,
        received: Notify,
        release: Notify,
    }
    #[async_trait]
    impl ControlBackend for Delayed {
        async fn execute(&self, command: ControlCommand) -> Result<ControlPayload, AppError> {
            let ControlCommand::Native { request } = command else {
                return Err(AppError::protocol("unexpected test command"));
            };
            if matches!(request, NativeRequest::PreviewVoice { .. }) {
                self.received.notify_one();
                self.release.notified().await;
            }
            self.runtime
                .request(request)
                .await
                .map(|reply| ControlPayload::Native(Box::new(reply)))
        }
    }

    let fixture = Fixture::new();
    let runtime = fixture.start().await;
    runtime
        .request(NativeRequest::CreateDefaults)
        .await
        .unwrap();
    let backend = Arc::new(Delayed {
        runtime: runtime.clone(),
        received: Notify::new(),
        release: Notify::new(),
    });
    let token = "0123456789abcdef0123456789abcdef";
    let server = ControlServer::bind("127.0.0.1:0", token, 4096)
        .await
        .unwrap();
    let client =
        ControlClient::new(&server.local_addr().unwrap().to_string(), token, 4096).unwrap();
    let server = tokio::spawn(server.serve(backend.clone()));
    let NativeReply::State(before) = client.native(NativeRequest::State).await.unwrap() else {
        panic!("state reply")
    };
    let older = client.clone();
    let delayed =
        tokio::spawn(async move { older.native(preview(before.preview_generation)).await });
    tokio::time::timeout(Duration::from_secs(2), backend.received.notified())
        .await
        .unwrap();
    assert_eq!(
        client.native(NativeRequest::StopPreview).await.unwrap(),
        NativeReply::Accepted
    );
    let NativeReply::State(stopped) = client.native(NativeRequest::State).await.unwrap() else {
        panic!("state reply")
    };
    assert!(!stopped.preview_active);
    assert_eq!(stopped.preview_generation, before.preview_generation + 1);
    let unchanged = runtime.request(NativeRequest::State).await.unwrap();
    assert!(
        runtime
            .check_preview_generation(before.preview_generation)
            .is_err()
    );
    assert_eq!(
        runtime.request(NativeRequest::State).await.unwrap(),
        unchanged
    );
    backend.release.notify_one();
    let rejected = delayed.await.unwrap().unwrap_err();
    assert_eq!(rejected.kind, ai_ex_domain::ErrorKind::InvalidTransition);
    assert!(rejected.message.contains("试听状态已改变"));
    assert_eq!(generation(&runtime).await, stopped.preview_generation);
    runtime.shutdown().await.unwrap();
    server.abort();
    let _stopped = server.await;
}
