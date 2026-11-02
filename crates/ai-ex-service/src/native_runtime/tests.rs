use std::fs;
use std::time::Duration;

use ai_ex_core::{LanguageModelPort, ModelRequest};
use ai_ex_domain::{Message, NativeOperationPhase, NativeReply, NativeRequest, Role, TurnId};

use super::*;

mod preview;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("native service 中文 {}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("native-v1.toml")
    }
    async fn start(&self) -> Arc<NativeRuntime> {
        let runtime = NativeRuntime::start(self.path(), "first-profile".into());
        runtime.wait_ready(Duration::from_secs(5)).await.unwrap();
        runtime
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn setup_and_missing_models_keep_control_usable_without_devices() {
    let fixture = Fixture::new();
    let runtime = fixture.start().await;
    assert!(
        matches!(runtime.request(NativeRequest::State).await.unwrap(), NativeReply::State(state) if !state.configured)
    );
    let NativeReply::Settings { revision, document } = runtime
        .request(NativeRequest::CreateDefaults)
        .await
        .unwrap()
    else {
        panic!("settings reply")
    };
    assert_eq!(revision, 1);
    let config = NativeConfig::parse(&document).unwrap();
    assert!(!config.devices.microphone_enabled && !config.devices.playback_enabled);
    let mut model = runtime.language_model();
    assert!(
        model
            .stream(ModelRequest {
                turn_id: TurnId::new(),
                messages: vec![Message::new(Role::User, "你好")]
            })
            .await
            .is_err()
    );
    assert!(
        matches!(runtime.request(NativeRequest::Packages).await.unwrap(), NativeReply::Packages(packages) if packages.len() == 6)
    );
    assert!(!runtime.health().ready);
    let expected_generation = runtime.lock().unwrap().preview_generation;
    assert!(
        runtime
            .request(NativeRequest::PreviewVoice {
                voice_id: "builtin-zh".into(),
                text: "你好".into(),
                expected_generation,
            })
            .await
            .is_err()
    );
    let pool = runtime.worker_pool().unwrap();
    let selection = config.embedding.as_ref().unwrap();
    let unavailable = pool.warmup(selection).unwrap_err();
    let NativeReply::Operation(removal) = runtime
        .request(NativeRequest::Remove {
            package_id: "native-cpu".into(),
        })
        .await
        .unwrap()
    else {
        panic!("removal reply")
    };
    runtime.manager().unwrap().wait(removal.id).await.unwrap();
    // Removing the runtime unloads workers without permanently closing their
    // shared pool; another preparation attempt still reports missing packages.
    assert_eq!(
        pool.warmup(selection).unwrap_err().message,
        unavailable.message
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn private_voice_import_is_normalized_bound_and_persisted_with_cas() {
    let fixture = Fixture::new();
    let runtime = fixture.start().await;
    runtime
        .request(NativeRequest::CreateDefaults)
        .await
        .unwrap();
    let reference = fixture.0.join("reference 参考.wav");
    let mut writer = hound::WavWriter::create(
        &reference,
        hound::WavSpec {
            channels: 2,
            sample_rate: 24000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for sample in [0.25_f32, 0.75, -0.25, -0.75, 0.0, 0.0] {
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
    let original = fs::read(&reference).unwrap();
    let reply = runtime
        .request(NativeRequest::ImportVoice {
            name: "我的参考声音".into(),
            path: reference.to_string_lossy().into(),
            text: "参考文本。".into(),
            language: "zh".into(),
            expected_revision: 1,
        })
        .await
        .unwrap();
    let NativeReply::Operation(operation) = reply else {
        panic!("operation reply")
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        runtime.operations.wait(operation.id),
    )
    .await
    .unwrap()
    .unwrap();
    let operation = runtime.operations.get(operation.id).unwrap().unwrap();
    assert_eq!(
        operation.phase,
        NativeOperationPhase::Completed,
        "{:?}",
        operation.error
    );
    let config = runtime.config().unwrap();
    assert_eq!(config.revision, 2);
    assert_eq!(config.voices.len(), 2);
    let voice = config.voices.last().unwrap();
    let audio_id = voice.private_reference_id().unwrap();
    let storage = runtime.store().unwrap().resolve_storage(&config).unwrap();
    let mut normalized =
        hound::WavReader::open(storage.voices.join(format!("{audio_id}.wav"))).unwrap();
    assert_eq!(normalized.spec().channels, 1);
    assert_eq!(normalized.spec().bits_per_sample, 16);
    assert_eq!(normalized.samples::<i16>().count(), 3);
    assert_eq!(fs::read(&reference).unwrap(), original);
    let profile = "🙂".repeat(128);
    runtime
        .request(NativeRequest::BindVoice {
            profile_id: profile.clone(),
            voice_id: voice.id.clone(),
            expected_revision: 2,
        })
        .await
        .unwrap();
    runtime.select_profile(&profile).unwrap();
    assert_eq!(runtime.current_voice().unwrap().unwrap().id, voice.id);
    assert!(
        runtime
            .request(NativeRequest::BindVoice {
                profile_id: profile.clone(),
                voice_id: "builtin-zh".into(),
                expected_revision: 2
            })
            .await
            .is_err()
    );
    runtime.shutdown().await.unwrap();
    let reopened = fixture.start().await;
    reopened.select_profile(&profile).unwrap();
    assert_eq!(reopened.current_voice().unwrap().unwrap().id, voice.id);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_native_file_and_unknown_selection_do_not_overwrite_configuration() {
    let fixture = Fixture::new();
    fs::write(fixture.path(), b"invalid = [").unwrap();
    let runtime = NativeRuntime::start(fixture.path(), "profile".into());
    assert!(runtime.wait_ready(Duration::from_secs(5)).await.is_err());
    assert!(runtime.request(NativeRequest::State).await.is_ok());
    assert!(runtime.request(NativeRequest::Settings).await.is_err());
    assert!(
        runtime
            .request(NativeRequest::CreateDefaults)
            .await
            .is_err()
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), b"invalid = [");
    runtime.shutdown().await.unwrap();
    fs::remove_file(fixture.path()).unwrap();
    let runtime = fixture.start().await;
    runtime
        .request(NativeRequest::CreateDefaults)
        .await
        .unwrap();
    let previous = fs::read(fixture.path()).unwrap();
    let mut next = runtime.config().unwrap();
    next.dialogue.as_mut().unwrap().model_id = "unknown-model".into();
    assert!(
        runtime
            .request(NativeRequest::SaveSettings {
                expected_revision: 1,
                document: next.to_toml().unwrap()
            })
            .await
            .is_err()
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), previous);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn overlapping_storage_is_rejected_before_save_and_closure_drops_preview_sender() {
    let fixture = Fixture::new();
    let runtime = fixture.start().await;
    runtime
        .request(NativeRequest::CreateDefaults)
        .await
        .unwrap();
    let mut next = runtime.config().unwrap();
    next.storage.jobs = next.storage.packages.join("nested");
    assert!(
        runtime
            .request(NativeRequest::SaveSettings {
                expected_revision: 1,
                document: next.to_toml().unwrap()
            })
            .await
            .is_err()
    );
    let (queue, mut receiver) = ai_ex_audio::SpeechQueue::new(2).unwrap();
    let player = receiver.player();
    runtime.attach_preview(
        queue.clone(),
        player,
        ai_ex_observability::EventHub::new(8).unwrap(),
    );
    drop(queue);
    runtime.shutdown().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), receiver.receive())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn failed_storage_can_be_corrected_without_restarting_the_service() {
    let fixture = Fixture::new();
    let blocked = fixture.0.join("blocked-storage");
    fs::write(&blocked, b"personal file").unwrap();
    let mut config = NativeConfig::default();
    config.storage.packages = blocked.clone();
    NativeConfigStore::new(fixture.path())
        .unwrap()
        .create_new(&config)
        .unwrap();
    let runtime = NativeRuntime::start(fixture.path(), "profile".into());
    assert!(runtime.wait_ready(Duration::from_secs(5)).await.is_err());
    assert!(runtime.request(NativeRequest::Settings).await.is_ok());
    config.storage.packages = fixture.0.join("recovered-packages");
    runtime
        .request(NativeRequest::SaveSettings {
            expected_revision: 1,
            document: config.to_toml().unwrap(),
        })
        .await
        .unwrap();
    runtime.wait_ready(Duration::from_secs(5)).await.unwrap();
    assert_eq!(runtime.config().unwrap().revision, 2);
    assert_eq!(fs::read(blocked).unwrap(), b"personal file");
    assert!(runtime.request(NativeRequest::Packages).await.is_ok());
    runtime.shutdown().await.unwrap();
}
