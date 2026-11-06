use std::fs;
use std::time::{Duration, Instant};

use ai_ex_config::native::{NativeConfig, VoiceSource};
use ai_ex_control::{ControlCommand, ControlPayload};
use ai_ex_domain::{
    NativeInputStatus, NativeOperationPhase, NativePackageState, NativeReply, NativeRequest,
};

#[path = "support/native_setup_cpu.rs"]
mod cpu;
#[path = "support/native_setup.rs"]
mod support;

use support::NativeService;

#[tokio::test]
async fn input_status_uses_service_instance_without_opening_devices() {
    let mut service = NativeService::new(false);
    service.ready(Duration::from_secs(12)).await;
    let ControlPayload::Snapshot(snapshot) =
        service.client.send(ControlCommand::Status).await.unwrap()
    else {
        panic!("snapshot reply")
    };
    let first_instance = snapshot.instance_id.unwrap();
    assert_eq!(
        service
            .request(NativeRequest::InputStatus {
                instance_id: first_instance
            })
            .await,
        NativeReply::InputStatus {
            instance_id: first_instance,
            status: NativeInputStatus::Unknown
        },
    );
    assert!(
        service
            .client
            .native(NativeRequest::InputStatus {
                instance_id: uuid::Uuid::new_v4()
            })
            .await
            .is_err()
    );
    service.request(NativeRequest::CreateDefaults).await;
    service.close().await;
    service.start();
    service.ready(Duration::from_secs(12)).await;
    let ControlPayload::Snapshot(snapshot) =
        service.client.send(ControlCommand::Status).await.unwrap()
    else {
        panic!("snapshot reply")
    };
    let second_instance = snapshot.instance_id.unwrap();
    assert_ne!(first_instance, second_instance);
    assert!(
        service
            .client
            .native(NativeRequest::InputStatus {
                instance_id: first_instance
            })
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if service
                .request(NativeRequest::InputStatus {
                    instance_id: second_instance,
                })
                .await
                == (NativeReply::InputStatus {
                    instance_id: second_instance,
                    status: NativeInputStatus::Disabled,
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("disabled input status becomes available");
    service.close().await;
}

async fn settings_reply(service: &NativeService, reply: NativeReply) -> NativeConfig {
    let NativeReply::Settings { revision, document } = reply else {
        panic!("settings reply");
    };
    let config = NativeConfig::parse(&document).unwrap();
    assert_eq!(revision, config.revision);
    assert!(!config.devices.microphone_enabled && !config.devices.playback_enabled);
    assert_eq!(config, service.settings().await);
    config
}

fn synthetic_wave(path: &std::path::Path) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for index in 0..48_000 {
        let sample = if (index / 48) % 2 == 0 {
            2048_i16
        } else {
            -2048_i16
        };
        writer.write_sample(sample).unwrap();
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
}

#[tokio::test]
async fn native_setup_keeps_bootstrap_and_source_audio_intact_across_cas_binding_and_restart() {
    let mut service = NativeService::new(false);
    service.ready(Duration::from_secs(12)).await;
    let NativeReply::State(state) = service.request(NativeRequest::State).await else {
        panic!("state reply");
    };
    assert!(!state.configured && !state.preview_active && !service.native_path.exists());
    assert!(
        service
            .wrong_token_client()
            .native(NativeRequest::State)
            .await
            .is_err()
    );
    let packages = service.packages().await;
    assert_eq!(packages.len(), 6);
    assert!(packages.iter().all(|package| !package.installed
        && !matches!(
            package.state,
            NativePackageState::Ready | NativePackageState::InUse
        )));
    let config = settings_reply(
        &service,
        service.request(NativeRequest::CreateDefaults).await,
    )
    .await;
    assert_eq!(config.revision, 1);
    service.bootstrap_unchanged();
    let mut edited = config.clone();
    edited.voices[0].label = "合成默认声音".into();
    let config = settings_reply(
        &service,
        service
            .request(NativeRequest::SaveSettings {
                expected_revision: config.revision,
                document: edited.to_toml().unwrap(),
            })
            .await,
    )
    .await;
    assert_eq!(config.revision, 2);
    assert!(
        service
            .client
            .native(NativeRequest::SaveSettings {
                expected_revision: 1,
                document: edited.to_toml().unwrap(),
            })
            .await
            .is_err()
    );
    assert_eq!(service.settings().await, config);
    let source = service.directory.join("参考 合成.wav");
    synthetic_wave(&source);
    let original = fs::read(&source).unwrap();
    let started = Instant::now();
    let NativeReply::Operation(import) = service
        .request(NativeRequest::ImportVoice {
            name: "合成测试音色".into(),
            path: source.to_string_lossy().into_owned(),
            text: "合成测试对应文字".into(),
            language: "zh".into(),
            expected_revision: config.revision,
        })
        .await
    else {
        panic!("reference audio import starts with an operation ID")
    };
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "voice import acknowledges before normalization completes"
    );
    let import = service.operation(import.id, Duration::from_secs(10)).await;
    assert_eq!(import.phase, NativeOperationPhase::Completed, "{import:?}");
    let config = service.settings().await;
    let voice = config
        .voices
        .iter()
        .find(|voice| voice.label == "合成测试音色")
        .unwrap();
    let VoiceSource::ReferenceAudio { audio_id, text } = &voice.source else {
        panic!("private reference source");
    };
    assert_eq!(text, "合成测试对应文字");
    let private_audio = config
        .storage
        .resolve(&service.native_path)
        .unwrap()
        .voices
        .join(format!("{audio_id}.wav"));
    assert_ne!(private_audio, source);
    let wave = hound::WavReader::open(&private_audio).unwrap();
    assert_eq!(wave.spec().channels, 1);
    assert_eq!(wave.spec().bits_per_sample, 16);
    assert_eq!(fs::read(&source).unwrap(), original);
    let voice_id = voice.id.clone();
    let config = settings_reply(
        &service,
        service
            .request(NativeRequest::BindVoice {
                profile_id: "synthetic-companion".into(),
                voice_id: voice_id.clone(),
                expected_revision: config.revision,
            })
            .await,
    )
    .await;
    assert_eq!(
        config.character_voices.get("synthetic-companion"),
        Some(&voice_id)
    );
    let unicode_profile = "🪷".repeat(128);
    let config = settings_reply(
        &service,
        service
            .request(NativeRequest::BindVoice {
                profile_id: unicode_profile.clone(),
                voice_id: voice_id.clone(),
                expected_revision: config.revision,
            })
            .await,
    )
    .await;
    assert_eq!(
        config.character_voices.get(&unicode_profile),
        Some(&voice_id)
    );
    let NativeReply::State(preview_state) = service.request(NativeRequest::State).await else {
        panic!("preview clock reply");
    };
    let error = service
        .client
        .native(NativeRequest::PreviewVoice {
            voice_id,
            text: "合成试听请求".into(),
            expected_generation: preview_state.preview_generation,
        })
        .await
        .unwrap_err();
    assert!(
        error.message.contains("声音")
            || error.message.contains("播放")
            || error.message.to_ascii_lowercase().contains("playback")
    );
    let NativeReply::State(state) = service.request(NativeRequest::State).await else {
        panic!("state reply");
    };
    assert!(!state.preview_active);
    service.close().await;
    service.start();
    service.ready(Duration::from_secs(12)).await;
    assert_eq!(service.settings().await, config);
    assert!(private_audio.is_file());
    assert_eq!(fs::read(source).unwrap(), original);
    service.close().await;
}

#[tokio::test]
async fn invalid_native_imports_fail_without_blocking_control_or_marking_a_model_ready() {
    let mut service = NativeService::new(false);
    service.ready(Duration::from_secs(12)).await;
    service.request(NativeRequest::CreateDefaults).await;
    let missing = service
        .client
        .native(NativeRequest::Operation {
            id: uuid::Uuid::new_v4(),
        })
        .await;
    assert!(
        missing.is_err(),
        "unknown operations must be explicit failures"
    );
    let empty = service.directory.join("空模型 目录");
    fs::create_dir(&empty).unwrap();
    let started = Instant::now();
    let unknown = service
        .client
        .native(NativeRequest::Import {
            package_id: "synthetic-unknown-package".into(),
            path: empty.to_string_lossy().into_owned(),
        })
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "unknown package lookup must not block control"
    );
    match unknown {
        Err(_) => {}
        Ok(NativeReply::Operation(operation)) => {
            let operation = service
                .operation(operation.id, Duration::from_secs(5))
                .await;
            assert_eq!(operation.phase, NativeOperationPhase::Failed);
        }
        other => panic!("unknown package cannot be accepted successfully: {other:?}"),
    }
    let started = Instant::now();
    let NativeReply::Operation(operation) = service
        .request(NativeRequest::Import {
            package_id: "qwen3-4b".into(),
            path: empty.to_string_lossy().into_owned(),
        })
        .await
    else {
        panic!("import starts with an operation ID");
    };
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "import must acknowledge before file verification completes"
    );
    let operation = service
        .operation(operation.id, Duration::from_secs(5))
        .await;
    assert_eq!(operation.phase, NativeOperationPhase::Failed);
    assert!(operation.error.is_some());
    let packages = service.packages().await;
    assert!(
        !packages
            .iter()
            .any(|package| package.id == "synthetic-unknown-package")
    );
    let model = packages
        .iter()
        .find(|package| package.id == "qwen3-4b")
        .unwrap();
    assert!(!model.installed);
    assert!(!matches!(
        model.state,
        NativePackageState::Ready | NativePackageState::InUse
    ));
    assert!(matches!(
        service.request(NativeRequest::State).await,
        NativeReply::State(_)
    ));
    service.bootstrap_unchanged();
    service.close().await;
}

#[tokio::test]
async fn native_reindex_rejects_a_missing_configuration_without_creating_it() {
    let mut service = NativeService::new(true);
    service.ready(Duration::from_secs(12)).await;
    service.request(NativeRequest::CreateDefaults).await;
    let original = fs::read(&service.native_path).unwrap();
    service.close().await;
    service.reindex_path("missing-native.toml", false).await;
    assert!(!service.directory.join("missing-native.toml").exists());
    assert_eq!(fs::read(&service.native_path).unwrap(), original);
    assert!(service.diagnostics().contains("native") || service.diagnostics().contains("原生"));
}
