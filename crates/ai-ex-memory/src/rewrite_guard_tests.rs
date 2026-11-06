use std::sync::atomic::{AtomicUsize, Ordering};

use ai_ex_domain::MemoryRequest;
use serde_json::{Value, json};

use super::*;

fn row(profile: &str, legacy: bool, entities: Option<Value>) -> Value {
    let mut value = json!({
        "id": Uuid::new_v4(), "profile_id": profile, "turn_id": TurnId::new(),
        "created_ms": 1, "user_text": "synthetic neutral text", "assistant_text": ""
    });
    if !legacy {
        value["schema_version"] = json!(2);
        value["record_type"] = json!("memory");
    }
    if let Some(entities) = entities {
        value["knowledge"] = json!({"entities": entities});
    }
    value
}

fn saved_entity() -> Value {
    json!([{ "kind": "fact", "key": "synthetic-key", "name": "synthetic-name",
        "value": "synthetic-value", "confidence": 800 }])
}

fn path() -> PathBuf {
    std::env::temp_dir().join(format!("memory-rewrite-{}.jsonl", Uuid::new_v4()))
}

struct CountingEmbedder(AtomicUsize);

#[async_trait]
impl ai_ex_core::EmbeddingPort for CountingEmbedder {
    fn space(&self) -> &str {
        "synthetic"
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>, AppError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(vec![1.0])
    }
}

#[tokio::test]
async fn refreshes_under_lease_and_rejects_cross_profile_rewrite_before_side_effects() {
    let path = path();
    let archive = path.with_extension("archive.jsonl");
    let embedder = Arc::new(CountingEmbedder(AtomicUsize::new(0)));
    let mut stale = MemoryStore::open(&path)
        .await
        .unwrap()
        .with_embedder(embedder.clone());
    let original = format!(
        "{}\n{}\n",
        row("default", false, None),
        row("other", false, Some(saved_entity()))
    );
    tokio::fs::write(&path, &original).await.unwrap();

    let request = MemoryRequest::Remember {
        profile_id: "default".into(),
        text: "new note".into(),
    };
    assert!(stale.manage(request).await.is_err());
    assert!(stale.reindex_embeddings().await.is_err());
    assert!(
        stale
            .maintain_media(1, 10, true, Some(&archive))
            .await
            .is_err()
    );
    assert_eq!(embedder.0.load(Ordering::Relaxed), 0);
    assert!(!archive.exists());
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), original);

    // A rejected first rewrite must not keep the writer lease.
    let mut independent = MemoryStore::open(&path).await.unwrap();
    assert_eq!(independent.len().await, 1);
    independent
        .remember_kind(
            MemoryKind::Conversation,
            TurnId::new(),
            "append".into(),
            "".into(),
        )
        .await
        .unwrap();
    assert!(
        tokio::fs::read_to_string(&path)
            .await
            .unwrap()
            .starts_with(&original)
    );
    drop(independent);
    drop(stale);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn prevents_forgetting_the_risky_record_and_legacy_append_rewrite() {
    for legacy in [false, true] {
        let path = path();
        let record = row("default", legacy, Some(saved_entity()));
        let id = Uuid::parse_str(record["id"].as_str().unwrap()).unwrap();
        let original = format!("{record}\n");
        tokio::fs::write(&path, &original).await.unwrap();
        let mut store = MemoryStore::open(&path).await.unwrap();
        assert_eq!(store.len().await, 1);
        assert!(
            store
                .manage(MemoryRequest::Forget {
                    profile_id: "default".into(),
                    id,
                    expected_revision: 1,
                })
                .await
                .is_err()
        );
        assert!(store.clear_kind(MemoryKind::Conversation).await.is_err());
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), original);
        if legacy {
            assert!(
                store
                    .remember_kind(
                        MemoryKind::Conversation,
                        TurnId::new(),
                        "next".into(),
                        "".into()
                    )
                    .await
                    .is_err()
            );
            assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), original);
        }
        drop(store);
        super::tests::remove_test_files(&path).await;
    }
}

#[tokio::test]
async fn empty_missing_and_unchanged_entities_remain_writable() {
    let text = "My name is Alice";
    let matching = entities::extract_entities(text);
    assert!(!matching.is_empty());
    for entities in [None, Some(json!([])), Some(json!(matching))] {
        let path = path();
        let mut record = row("default", false, entities);
        record["user_text"] = json!(text);
        tokio::fs::write(&path, format!("{record}\n"))
            .await
            .unwrap();
        let mut store = MemoryStore::open(&path).await.unwrap();
        assert_eq!(store.clear_kind(MemoryKind::Conversation).await.unwrap(), 1);
        assert!(tokio::fs::read(&path).await.unwrap().is_empty());
        drop(store);
        super::tests::remove_test_files(&path).await;
    }
}

#[tokio::test]
async fn scoped_export_rejects_only_when_selected_records_have_changed_entities() {
    let path = path();
    let safe_output = path.with_extension("safe.jsonl");
    let risky_output = path.with_extension("risky.jsonl");
    let original = format!(
        "{}\n{}\n",
        row("default", false, None),
        row("other", false, Some(saved_entity()))
    );
    tokio::fs::write(&path, &original).await.unwrap();
    let store = MemoryStore::open(&path).await.unwrap();
    assert_eq!(store.export_kind(None, &safe_output).await.unwrap(), 1);
    store.select_profile("other").await.unwrap();
    assert!(store.export_kind(None, &risky_output).await.is_err());
    assert!(!risky_output.exists());
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), original);
    drop(store);
    tokio::fs::remove_file(safe_output).await.unwrap();
    super::tests::remove_test_files(&path).await;
}

#[test]
fn upgrade_preview_reports_changed_entities_but_upgrade_refuses_them() {
    let original = format!(
        "{}\n{}\n",
        row("default", true, None),
        row("other", true, Some(saved_entity()))
    );
    let report = inspect_memory_jsonl(&original).unwrap();
    assert_eq!(report.memory_records, 2);
    assert_eq!(report.changed_saved_entity_records, 1);
    assert!(migrate_jsonl(&original).is_err());
}
