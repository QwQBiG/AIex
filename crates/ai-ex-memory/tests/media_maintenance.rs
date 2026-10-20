use ai_ex_memory::{MemoryStore, observation_media};
use serde_json::{Value, json};
use std::path::PathBuf;
use uuid::Uuid;
const NOW: u128 = 1_800_000_000_000;
const DAY: u128 = 86_400_000;
struct Files {
    memory: PathBuf,
    archive: PathBuf,
}
impl Files {
    fn new() -> Self {
        let n = Uuid::new_v4();
        Self {
            memory: std::env::temp_dir().join(format!("aiex-maint-{n}.jsonl")),
            archive: std::env::temp_dir().join(format!("aiex-maint-{n}-archive.jsonl")),
        }
    }
    fn write(&self, rows: &[Value]) {
        std::fs::write(
            &self.memory,
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        for p in [&self.memory, &self.archive] {
            let _ = std::fs::remove_file(p);
            let mut l = p.as_os_str().to_os_string();
            l.push(".lock");
            let _ = std::fs::remove_file(PathBuf::from(l));
        }
    }
}
fn row(profile: &str, source: &str, age: u128, importance: u16, count: u64) -> Value {
    json!({"id":Uuid::new_v4(),"turn_id":Uuid::new_v4(),"profile_id":profile,"created_ms":NOW-age,"kind":"conversation","user_text":"synthetic caption","assistant_text":"","source":source,"revision":1,"knowledge":{"media":{"modality":"visual","tags":["fixture"],"visual_features":{"objects":["synthetic"],"nested":{"x":[1,2]}},"metadata":{"original":true},"importance":importance}},"legacy_metadata":{"access_count":count,"original_snapshot":{"private":"synthetic"}},"embedding":{"space":"synthetic","values":[1.,2.]}})
}
fn rows(p: &PathBuf) -> Vec<Value> {
    std::fs::read_to_string(p)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
#[tokio::test]
async fn fixed_clock_preview_apply_and_reopen_preserve_complete_originals() {
    let f = Files::new();
    let original = vec![
        row("default", "automatic", 31 * DAY, 800, 0),
        row("default", "automatic", 30 * DAY, 800, 0),
        row("default", "automatic", 0, 800, 2),
        row("default", "user_note", 60 * DAY, 100, 0),
        row("other", "automatic", 60 * DAY, 100, 0),
    ];
    f.write(&original);
    let bytes = std::fs::read(&f.memory).unwrap();
    let mut memory = MemoryStore::open(&f.memory).await.unwrap();
    let preview = memory.maintain_media(NOW, 1000, false, None).await.unwrap();
    assert_eq!(preview.considered, 3);
    assert_eq!(preview.protected_confirmed, 1);
    assert_eq!(preview.archived, 1);
    assert_eq!(preview.updated, 2);
    assert!(!preview.applied);
    assert_eq!(std::fs::read(&f.memory).unwrap(), bytes);
    assert!(!f.archive.exists());
    assert!(!PathBuf::from(format!("{}.lock", f.memory.display())).exists());
    assert!(memory.maintain_media(NOW, 1000, true, None).await.is_err());
    assert_eq!(std::fs::read(&f.memory).unwrap(), bytes);
    let report = memory
        .maintain_media(NOW, 1000, true, Some(&f.archive))
        .await
        .unwrap();
    assert!(report.applied);
    assert_eq!(report.entries[1].after, 294);
    assert_eq!(report.entries[2].after, 960);
    let backup = rows(&f.archive);
    assert_eq!(backup.len(), 3);
    for (i, r) in backup.iter().enumerate() {
        assert_eq!(r["knowledge"]["media"], original[i]["knowledge"]["media"]);
        assert_eq!(r["legacy_metadata"], original[i]["legacy_metadata"]);
        assert_eq!(r["embedding"], original[i]["embedding"]);
        assert_eq!(r["revision"], 1);
    }
    let current = rows(&f.memory);
    assert_eq!(current.len(), 4);
    assert_eq!(current[0]["revision"], 2);
    assert_eq!(current[0]["knowledge"]["media"]["importance"], 294);
    assert_eq!(current[2]["knowledge"]["media"]["importance"], 100);
    assert_eq!(current[3]["profile_id"], "other");
    drop(memory);
    drop(MemoryStore::open(&f.memory).await.unwrap());
    drop(MemoryStore::open(&f.archive).await.unwrap());
}
#[tokio::test]
async fn existing_archive_and_active_writer_never_replace_memory() {
    let f = Files::new();
    f.write(&[row("default", "automatic", 30 * DAY, 800, 0)]);
    std::fs::write(&f.archive, b"sentinel").unwrap();
    let bytes = std::fs::read(&f.memory).unwrap();
    let mut first = MemoryStore::open(&f.memory).await.unwrap();
    assert!(
        first
            .maintain_media(NOW, 1000, true, Some(&f.archive))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&f.memory).unwrap(), bytes);
    assert_eq!(std::fs::read(&f.archive).unwrap(), b"sentinel");
    let mut second = MemoryStore::open(&f.memory).await.unwrap();
    assert!(
        second
            .maintain_media(NOW, 1000, true, Some(&f.archive))
            .await
            .unwrap_err()
            .to_string()
            .contains("active writer")
    );
    assert_eq!(std::fs::read(&f.memory).unwrap(), bytes);
}
#[test]
fn observation_audio_and_image_importance_have_identical_weights() {
    let feature = json!({"entities":["a","b"],"objects":["a","b"]})
        .as_object()
        .unwrap()
        .clone();
    let audio = observation_media(
        &"界".repeat(250),
        Some(feature.clone()),
        None,
        vec![],
        Default::default(),
    )
    .unwrap();
    let visual = observation_media(
        &"界".repeat(250),
        None,
        Some(feature),
        vec![],
        Default::default(),
    )
    .unwrap();
    assert_eq!(audio.importance, 410);
    assert_eq!(visual.importance, 410);
}
