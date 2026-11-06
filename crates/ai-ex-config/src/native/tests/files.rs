use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "native-config-{}-{}-{}-中文 路径",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> NativeConfigStore {
        NativeConfigStore::new(self.0.join(NATIVE_CONFIG_FILENAME)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for entry in fs::read_dir(&self.0).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(&self.0).unwrap();
    }
}

#[test]
fn native_creation_refuses_overwrite_and_portable_paths_follow_config() {
    let first = Fixture::new();
    let second = Fixture::new();
    let config = NativeConfig::default();
    first.store().create_new(&config).unwrap();
    let original = fs::read(first.store().path()).unwrap();
    assert!(first.store().create_new(&config).is_err());
    assert_eq!(fs::read(first.store().path()).unwrap(), original);
    fs::copy(first.store().path(), second.store().path()).unwrap();
    let moved = second.store().load().unwrap();
    assert_eq!(
        second.store().resolve_storage(&moved).unwrap().models,
        second.0.join("native/packages/models")
    );
    assert_eq!(
        first.store().resolve_storage(&config).unwrap().jobs,
        first.0.join("native/jobs")
    );
    assert!(!first.0.join("native").exists());
}

#[test]
fn revision_save_is_atomic_preserves_original_backup_and_refuses_stale_edit() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let mut config = NativeConfig::default();
    store.create_new(&config).unwrap();
    let original = fs::read(store.path()).unwrap();
    config.devices.playback_enabled = true;
    config.microphone_input = Some(NativeMicrophoneInput::Named {
        name: "合成麦克风 (USB)".into(),
    });
    let saved = store.update(1, &config).unwrap();
    assert_eq!(saved.config.revision, 2);
    assert_eq!(fs::read(saved.backup).unwrap(), original);
    assert_eq!(store.load().unwrap(), saved.config);
    assert_eq!(
        store.load().unwrap().microphone_input,
        config.microphone_input
    );
    assert!(store.update(1, &config).is_err());
    assert_eq!(store.load().unwrap().revision, 2);
    assert!(!fs::read_dir(&fixture.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn operating_system_lock_releases_and_existing_lock_file_is_reusable() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let config = NativeConfig::default();
    store.create_new(&config).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.0.join("native-v1.toml.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(store.update(1, &config).is_err());
    drop(lock);
    assert_eq!(store.update(1, &config).unwrap().config.revision, 2);
}

#[test]
fn migration_previews_reference_without_copying_it_or_changing_legacy_config() {
    let fixture = Fixture::new();
    let legacy_path = fixture.0.join("旧配置.toml");
    let mut legacy = crate::AppConfig::default();
    legacy.tts.enabled = true;
    legacy.tts.ref_audio_path = "旧参考音频.wav".into();
    legacy.tts.prompt_text = "这是私人的对应文本".into();
    legacy.duplex.enabled = true;
    let original = legacy.to_toml().unwrap();
    fs::write(&legacy_path, &original).unwrap();
    let preview = preview_legacy_file(&legacy_path).unwrap();
    assert_eq!(fs::read_to_string(&legacy_path).unwrap(), original);
    assert!(!preview.destination.exists());
    let candidate = preview.reference_candidate.as_ref().unwrap();
    assert_eq!(candidate.source, fixture.0.join("旧参考音频.wav"));
    assert_eq!(candidate.transcript, "这是私人的对应文本");
    assert!(
        preview
            .notices
            .contains(&NativeMigrationNotice::MicrophoneRequiresExplicitOptIn)
    );
    assert!(!preview.config.devices.microphone_enabled);
    let store = preview.save_new().unwrap();
    let native_text = fs::read_to_string(store.path()).unwrap();
    assert!(!native_text.contains("旧参考音频.wav"));
    assert!(!native_text.contains("这是私人的对应文本"));
    assert!(preview.save_new().is_err());
    assert_eq!(fs::read_to_string(&legacy_path).unwrap(), original);
    assert!(!fixture.0.join("native").exists());
}

#[test]
fn conflicting_backup_never_replaces_current_configuration() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let config = NativeConfig::default();
    store.create_new(&config).unwrap();
    let original = fs::read(store.path()).unwrap();
    fs::write(
        fixture.0.join("native-v1.toml.r1.backup"),
        b"other original",
    )
    .unwrap();
    assert!(store.update(1, &config).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), original);
}
