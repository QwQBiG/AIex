use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use ai_ex_domain::{MemoryModality, MemoryPage, MemoryRequest, MemoryResponse, MemorySource};
use ai_ex_memory::MemoryStore;
use serde_json::{Value, json};
use uuid::Uuid;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("aiex-multimodal-import-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn run(&self, extra: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"))
            .args(["--multimodal-memory", "--input"])
            .arg(self.0.join("snapshot.json"))
            .arg("--output")
            .arg(self.0.join("memory.jsonl"))
            .args(["--profile", "character-a"])
            .args(extra)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for name in ["snapshot.json", "memory.jsonl", "memory.jsonl.lock"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

async fn query(
    store: &mut MemoryStore,
    profile: &str,
    modality: Option<MemoryModality>,
    tags: Vec<String>,
) -> MemoryPage {
    let response = store
        .manage(MemoryRequest::QueryMedia {
            profile_id: profile.to_owned(),
            query: String::new(),
            modality,
            tags,
            min_importance: 0,
            offset: 0,
            limit: 20,
        })
        .await
        .unwrap();
    let MemoryResponse::Page(page) = response else {
        panic!("expected media page")
    };
    page
}

#[tokio::test]
async fn explicit_snapshot_import_survives_reopen_and_preserves_tags_features_and_profiles() {
    let fixture = Fixture::new();
    let visual_id = Uuid::new_v4();
    let snapshots = json!([
        {"memory_id":visual_id,"modality":"visual","text_content":"舞台蓝光","visual_features":{"description":"蓝光","objects":[{"label":"舞台","confidence":0.9}]},"timestamp":1.001,"importance_score":0.875,"access_count":4,"tags":["stage","blue"],"metadata":{"legacy_access_count":"keep-user-value"}},
        {"memory_id":"mm_0123456789abcdef_1700000000","modality":"audio","audio_features":{"description":"合成音频","rms":0.25},"timestamp":1.999999,"importance_score":0.7,"access_count":2,"tags":["voice"],"metadata":{"nested":{"keep":[1,2]}}},
        {"memory_id":"mm_0123456789abcdee_1700000000","modality":"multimodal","text_content":"多模态合成记录","audio_features":{},"visual_features":{},"timestamp":2,"importance_score":0.95,"access_count":1,"tags":["stage","voice"],"metadata":{}},
        {"memory_id":Uuid::new_v4(),"modality":"text","text_content":"纯文本记忆","timestamp":3,"importance_score":0.5,"tags":["note"],"metadata":{}}
    ]);
    let source = serde_json::to_vec_pretty(&snapshots).unwrap();
    fs::write(fixture.0.join("snapshot.json"), &source).unwrap();
    let output = fixture.run(&[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original_output = fs::read(fixture.0.join("memory.jsonl")).unwrap();
    let records: Vec<Value> = std::str::from_utf8(&original_output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 4);
    for (record, snapshot) in records.iter().zip(snapshots.as_array().unwrap()) {
        assert_eq!(&record["legacy_metadata"], snapshot);
        assert_eq!(
            record["knowledge"]["media"]["metadata"],
            snapshot["metadata"]
        );
    }
    assert_eq!(records[0]["id"], visual_id.to_string());
    assert_eq!(records[0]["created_ms"], 1001);
    assert_eq!(records[1]["created_ms"], 1999);
    assert_eq!(records[1]["user_text"], "Audio observation");
    let store = MemoryStore::open(fixture.0.join("memory.jsonl"))
        .await
        .unwrap();
    assert_eq!(store.len().await, 0);
    drop(store);
    let mut reopened = MemoryStore::open(fixture.0.join("memory.jsonl"))
        .await
        .unwrap();
    reopened.select_profile("character-a").await.unwrap();
    let page = query(
        &mut reopened,
        "character-a",
        None,
        vec!["voice".to_owned(), "blue".to_owned()],
    )
    .await;
    assert_eq!(page.total, 3, "tag filters use OR semantics");
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    assert!(
        page.entries
            .iter()
            .all(|entry| entry.source == MemorySource::Automatic)
    );
    let visual = query(
        &mut reopened,
        "character-a",
        Some(MemoryModality::Visual),
        Vec::new(),
    )
    .await;
    assert_eq!(visual.total, 1);
    assert_eq!(visual.entries[0].id, visual_id);
    let media = visual.knowledge[&visual_id].media.as_ref().unwrap();
    assert_eq!(
        media.visual_features.as_ref().unwrap(),
        snapshots[0]["visual_features"].as_object().unwrap()
    );
    assert_eq!(media.importance, 875);
    reopened.select_profile("other").await.unwrap();
    assert_eq!(
        query(&mut reopened, "other", None, Vec::new()).await.total,
        0
    );
    assert!(
        reopened
            .manage(MemoryRequest::QueryMedia {
                profile_id: "character-a".to_owned(),
                query: String::new(),
                modality: None,
                tags: Vec::new(),
                min_importance: 0,
                offset: 0,
                limit: 10
            })
            .await
            .is_err()
    );
    drop(reopened);
    assert!(!fixture.run(&[]).status.success());
    assert_eq!(
        fs::read(fixture.0.join("memory.jsonl")).unwrap(),
        original_output
    );
    assert_eq!(fs::read(fixture.0.join("snapshot.json")).unwrap(), source);
}

#[test]
fn invalid_snapshots_and_conflicting_modes_never_publish_output() {
    let fixture = Fixture::new();
    let base = json!({"memory_id":Uuid::new_v4(),"modality":"visual","visual_features":{},"timestamp":1,"metadata":{}});
    for source in [
        json!([base.clone(), base.clone()]),
        json!([{ "memory_id":Uuid::new_v4(),"modality":"visual","visual_features":{"payload":"x".repeat(8192)},"timestamp":1 }]),
    ] {
        let bytes = serde_json::to_vec(&source).unwrap();
        fs::write(fixture.0.join("snapshot.json"), &bytes).unwrap();
        assert!(!fixture.run(&[]).status.success());
        assert!(!fixture.0.join("memory.jsonl").exists());
        assert_eq!(fs::read(fixture.0.join("snapshot.json")).unwrap(), bytes);
    }
    fs::write(
        fixture.0.join("snapshot.json"),
        serde_json::to_vec(&json!([base])).unwrap(),
    )
    .unwrap();
    let conflict = fixture.run(&["--memory-backup"]);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("mutually exclusive"));
    assert!(
        !fixture
            .run(&["--embedding-space", "not-applicable"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["--utc-offset-minutes", "480"])
            .status
            .success()
    );
    assert!(!fixture.0.join("memory.jsonl").exists());
}
