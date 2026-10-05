use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ai_ex_core::{EmbeddingPort, MemoryPort};
use ai_ex_domain::{
    AppError, MemoryEntry, MemoryKind, MemoryPage, MemoryRequest, MemoryResponse, TurnId,
};
use ai_ex_memory::MemoryStore;
use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

const DESSERT: &str = "草莓蛋糕";
const DESSERT_QUERY: &str = "甜点推荐";
const WALK: &str = "山间徒步";
const WALK_QUERY: &str = "登高出游";

/// Deliberately deterministic semantic classes, not a real-model accuracy test.
struct FakeEmbedder {
    space: &'static str,
    dimensions: usize,
    failed: AtomicBool,
    calls: AtomicUsize,
    fail_on_call: AtomicUsize,
}

impl FakeEmbedder {
    fn new(space: &'static str, dimensions: usize) -> Self {
        Self {
            space,
            dimensions,
            failed: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            fail_on_call: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl EmbeddingPort for FakeEmbedder {
    fn space(&self) -> &str {
        self.space
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.failed.load(Ordering::SeqCst) || self.fail_on_call.load(Ordering::SeqCst) == call {
            return Err(AppError::unavailable("synthetic encoder failure"));
        }
        let category = if text.contains(DESSERT) || text.contains(DESSERT_QUERY) {
            0
        } else if text.contains(WALK) || text.contains(WALK_QUERY) {
            1
        } else {
            2
        };
        let mut vector = vec![0.0; self.dimensions];
        if let Some(value) = vector.get_mut(category) {
            *value = 3.0; // Non-unit input also exercises normalization.
        }
        Ok(vector)
    }
}

struct TestFiles {
    memory: PathBuf,
    export: PathBuf,
}

impl TestFiles {
    fn new() -> Self {
        let name = format!("ai-ex-semantic-test-{}", Uuid::new_v4());
        Self {
            memory: std::env::temp_dir().join(format!("{name}.jsonl")),
            export: std::env::temp_dir().join(format!("{name}-export.jsonl")),
        }
    }
}

impl Drop for TestFiles {
    fn drop(&mut self) {
        // Only these unique files created by this test; never enumerate a directory.
        let _ = std::fs::remove_file(&self.memory);
        let _ = std::fs::remove_file(&self.export);
        for path in [&self.memory, &self.export] {
            let mut lock_path = path.as_os_str().to_os_string();
            lock_path.push(".lock");
            let _ = std::fs::remove_file(PathBuf::from(lock_path));
        }
    }
}

async fn rows(path: &Path) -> Vec<Value> {
    tokio::fs::read_to_string(path)
        .await
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn page(store: &mut MemoryStore, profile: &str) -> MemoryPage {
    let result = store
        .manage(MemoryRequest::List {
            profile_id: profile.to_owned(),
            query: String::new(),
            kind: None,
            offset: 0,
            limit: 100,
        })
        .await
        .unwrap();
    let MemoryResponse::Page(page) = result else {
        panic!("list must return a page");
    };
    page
}

fn correction(entry: &MemoryEntry, text: &str) -> MemoryRequest {
    MemoryRequest::Correct {
        profile_id: entry.profile_id.clone(),
        id: entry.id,
        expected_revision: entry.revision,
        text: text.to_owned(),
    }
}

fn fixture(text: &str) -> Value {
    json!({
        "id": Uuid::new_v4(),
        "profile_id": "default",
        "turn_id": TurnId::new(),
        "created_ms": 1,
        "kind": "conversation",
        "user_text": text,
        "assistant_text": "",
        "source": "automatic",
        "revision": 1
    })
}

async fn write_fixture(path: &Path, entry: &Value) {
    tokio::fs::write(path, format!("{}\n", serde_json::to_string(entry).unwrap()))
        .await
        .unwrap();
}

#[tokio::test]
async fn semantic_recall_survives_restart_without_lexical_overlap() {
    assert!(
        !DESSERT
            .chars()
            .any(|character| DESSERT_QUERY.contains(character))
    );
    let files = TestFiles::new();
    let embedder = Arc::new(FakeEmbedder::new("fixture-v1", 3));
    let mut store = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder.clone());
    store
        .remember(TurnId::new(), DESSERT.to_owned(), String::new())
        .await
        .unwrap();
    store
        .remember(TurnId::new(), WALK.to_owned(), String::new())
        .await
        .unwrap();
    let saved = rows(&files.memory).await;
    assert_eq!(
        saved[0]["embedding"],
        json!({"space": "fixture-v1", "values": [3.0, 0.0, 0.0]})
    );

    drop(store);
    let lexical_only = MemoryStore::open(&files.memory).await.unwrap();
    assert!(
        lexical_only
            .recall(DESSERT_QUERY, 5)
            .await
            .unwrap()
            .is_empty()
    );
    let reopened = lexical_only.with_embedder(embedder);
    let recalled = reopened.recall_for_context(DESSERT_QUERY, 5).await.unwrap();
    assert_eq!(recalled.len(), 1);
    assert!(recalled[0].content.contains(DESSERT));
    assert!(!recalled[0].content.contains(WALK));
}

#[tokio::test]
async fn semantic_results_are_scoped_to_the_selected_profile_and_kind() {
    let files = TestFiles::new();
    let embedder = Arc::new(FakeEmbedder::new("fixture-v1", 3));
    let mut store = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder);
    store.select_profile("alice").await.unwrap();
    store
        .remember(TurnId::new(), format!("{DESSERT}甲"), String::new())
        .await
        .unwrap();
    store
        .remember_kind(
            MemoryKind::LiveEvent,
            TurnId::new(),
            format!("{DESSERT}事件"),
            String::new(),
        )
        .await
        .unwrap();
    store.select_profile("bob").await.unwrap();
    store
        .remember(TurnId::new(), format!("{DESSERT}乙"), String::new())
        .await
        .unwrap();
    let bob = store.recall_for_context(DESSERT_QUERY, 10).await.unwrap();
    assert_eq!(bob.len(), 1);
    assert!(bob[0].content.contains('乙'));
    store.select_profile("alice").await.unwrap();
    let alice = store.recall_for_context(DESSERT_QUERY, 10).await.unwrap();
    assert_eq!(alice.len(), 1);
    assert!(alice[0].content.contains('甲'));
    let events = store
        .recall_kind(Some(MemoryKind::LiveEvent), DESSERT_QUERY, 10)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].content.contains("事件"));
}

#[tokio::test]
async fn incompatible_model_spaces_and_dimensions_do_not_create_semantic_hits() {
    for (space, values) in [
        ("different-model", vec![3.0, 0.0, 0.0]),
        ("fixture-v1", vec![3.0, 0.0]),
    ] {
        let files = TestFiles::new();
        let mut entry = fixture(DESSERT);
        entry["embedding"] = json!({"space": space, "values": values});
        write_fixture(&files.memory, &entry).await;
        let store = MemoryStore::open(&files.memory)
            .await
            .unwrap()
            .with_embedder(Arc::new(FakeEmbedder::new("fixture-v1", 3)));
        assert!(store.recall(DESSERT_QUERY, 5).await.unwrap().is_empty());
        assert_eq!(
            store.recall(DESSERT, 5).await.unwrap().len(),
            1,
            "lexical retrieval remains available"
        );
    }
}

#[tokio::test]
async fn correction_replaces_semantic_and_lexical_indexes_and_survives_restart() {
    assert!(!WALK.chars().any(|character| WALK_QUERY.contains(character)));
    let files = TestFiles::new();
    let embedder = Arc::new(FakeEmbedder::new("fixture-v1", 3));
    let mut store = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder.clone());
    store
        .remember(TurnId::new(), DESSERT.to_owned(), String::new())
        .await
        .unwrap();
    let original = page(&mut store, "default").await.entries.remove(0);
    store.manage(correction(&original, WALK)).await.unwrap();
    let changed = page(&mut store, "default").await.entries.remove(0);
    assert_eq!(changed.revision, original.revision + 1);
    assert_eq!(changed.user_text, WALK);
    assert!(store.recall(DESSERT_QUERY, 5).await.unwrap().is_empty());
    assert!(store.recall(DESSERT, 5).await.unwrap().is_empty());
    drop(store);
    let reopened = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder);
    let recalled = reopened.recall(WALK_QUERY, 5).await.unwrap();
    assert_eq!(recalled.len(), 1);
    assert!(recalled[0].content.contains(WALK));
    assert!(!recalled[0].content.contains(DESSERT));
}

#[tokio::test]
async fn failed_correction_embedding_drops_the_previous_vector_and_metadata() {
    let files = TestFiles::new();
    let mut entry = fixture(DESSERT);
    entry["embedding"] = json!({"space": "fixture-v1", "values": [3.0, 0.0, 0.0]});
    entry["legacy_metadata"] = json!({"entities": {"food": [DESSERT]}});
    write_fixture(&files.memory, &entry).await;
    let embedder = Arc::new(FakeEmbedder::new("fixture-v1", 3));
    let mut store = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder.clone());
    let original = page(&mut store, "default").await.entries.remove(0);
    embedder.failed.store(true, Ordering::SeqCst);
    store.manage(correction(&original, WALK)).await.unwrap();
    assert!(store.health().await.detail.contains("fallback"));
    let saved = rows(&files.memory).await;
    assert!(saved[0].get("embedding").is_none());
    assert!(saved[0].get("legacy_metadata").is_none());
    assert_eq!(saved[0]["user_text"], WALK);
    embedder.failed.store(false, Ordering::SeqCst);
    assert!(store.recall(DESSERT_QUERY, 5).await.unwrap().is_empty());
    assert!(
        store.recall(WALK_QUERY, 5).await.unwrap().is_empty(),
        "failed enrichment is not reported as semantic success"
    );
    drop(store);
    let reopened = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder);
    assert_eq!(reopened.recall(WALK, 5).await.unwrap().len(), 1);
    assert!(reopened.recall(DESSERT_QUERY, 5).await.unwrap().is_empty());
}

#[tokio::test]
async fn forgetting_and_clearing_remove_vectors_from_memory_and_disk() {
    let files = TestFiles::new();
    let embedder = Arc::new(FakeEmbedder::new("fixture-v1", 3));
    let mut store = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder.clone());
    store
        .remember(TurnId::new(), DESSERT.to_owned(), String::new())
        .await
        .unwrap();
    store
        .remember_kind(
            MemoryKind::Viewer,
            TurnId::new(),
            WALK.to_owned(),
            String::new(),
        )
        .await
        .unwrap();
    let original = page(&mut store, "default")
        .await
        .entries
        .into_iter()
        .find(|entry| entry.user_text == DESSERT)
        .unwrap();
    store
        .manage(MemoryRequest::Forget {
            profile_id: original.profile_id,
            id: original.id,
            expected_revision: original.revision,
        })
        .await
        .unwrap();
    assert!(store.recall(DESSERT_QUERY, 5).await.unwrap().is_empty());
    assert_eq!(store.recall(WALK_QUERY, 5).await.unwrap().len(), 1);
    assert_eq!(store.clear_kind(MemoryKind::Viewer).await.unwrap(), 1);
    assert!(store.recall(WALK_QUERY, 5).await.unwrap().is_empty());
    drop(store);
    let reopened = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder);
    assert!(reopened.is_empty().await);
    assert!(reopened.recall(DESSERT_QUERY, 5).await.unwrap().is_empty());
    assert!(reopened.recall(WALK_QUERY, 5).await.unwrap().is_empty());
}

#[tokio::test]
async fn imported_enrichment_survives_export_but_is_excluded_from_control_pages() {
    let files = TestFiles::new();
    let mut entry = fixture(DESSERT);
    entry["embedding"] = json!({"space": "fixture-v1", "values": [3.0, 0.0, 0.0]});
    entry["legacy_metadata"] = json!({
        "entities": {"food": [DESSERT]},
        "importance": 0.9,
        "modalities": {"image": {"caption": "synthetic cake", "source": "fixture.png"}}
    });
    write_fixture(&files.memory, &entry).await;
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    let control_page = page(&mut store, "default").await;
    let wire = serde_json::to_value(&control_page).unwrap();
    assert_eq!(control_page.entries.len(), 1);
    assert!(wire["entries"][0].get("embedding").is_none());
    assert!(wire["entries"][0].get("legacy_metadata").is_none());
    assert!(wire["entries"][0].get("normalized").is_none());
    assert!(wire["entries"][0].get("terms").is_none());
    assert_eq!(store.export_kind(None, &files.export).await.unwrap(), 1);
    let exported = rows(&files.export).await;
    assert_eq!(exported[0], entry);
    let reopened = MemoryStore::open(&files.export)
        .await
        .unwrap()
        .with_embedder(Arc::new(FakeEmbedder::new("fixture-v1", 3)));
    assert_eq!(reopened.recall(DESSERT_QUERY, 5).await.unwrap().len(), 1);
}

#[tokio::test]
async fn explicit_reindex_only_updates_the_selected_profile() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    store
        .remember(TurnId::new(), DESSERT.to_owned(), String::new())
        .await
        .unwrap();
    store
        .remember(TurnId::new(), WALK.to_owned(), String::new())
        .await
        .unwrap();
    store.select_profile("other-character").await.unwrap();
    store
        .remember(TurnId::new(), DESSERT.to_owned(), String::new())
        .await
        .unwrap();
    let other_before = rows(&files.memory).await.remove(2);
    store.select_profile("default").await.unwrap();
    let embedder = Arc::new(FakeEmbedder::new("fixture-v1", 3));
    let mut store = store.with_embedder(embedder.clone());
    assert_eq!(store.reindex_embeddings().await.unwrap(), 2);
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 2);
    let saved = rows(&files.memory).await;
    assert!(saved[0].get("embedding").is_some());
    assert!(saved[1].get("embedding").is_some());
    assert_eq!(saved[2], other_before);
    assert_eq!(store.recall(DESSERT_QUERY, 5).await.unwrap().len(), 1);
    store.select_profile("other-character").await.unwrap();
    assert!(store.recall(DESSERT_QUERY, 5).await.unwrap().is_empty());
}

#[tokio::test]
async fn failed_reindex_is_atomic_even_after_one_successful_embedding() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    store
        .remember(TurnId::new(), DESSERT.to_owned(), String::new())
        .await
        .unwrap();
    store
        .remember(TurnId::new(), WALK.to_owned(), String::new())
        .await
        .unwrap();
    let before = tokio::fs::read(&files.memory).await.unwrap();
    let embedder = Arc::new(FakeEmbedder::new("fixture-v1", 3));
    embedder.fail_on_call.store(2, Ordering::SeqCst);
    let mut store = store.with_embedder(embedder.clone());
    assert!(store.reindex_embeddings().await.is_err());
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 2);
    assert_eq!(tokio::fs::read(&files.memory).await.unwrap(), before);
    embedder.fail_on_call.store(0, Ordering::SeqCst);
    assert!(
        store.recall(DESSERT_QUERY, 5).await.unwrap().is_empty(),
        "no partial in-memory index commit"
    );
    assert_eq!(store.reindex_embeddings().await.unwrap(), 2);
    drop(store);
    let reopened = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder);
    assert_eq!(reopened.recall(DESSERT_QUERY, 5).await.unwrap().len(), 1);
    assert_eq!(reopened.recall(WALK_QUERY, 5).await.unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_persisted_vectors_are_rejected_without_rewriting_the_file() {
    let invalid = [
        json!({"space": "fixture-v1", "values": []}),
        json!({"space": "fixture-v1", "values": [0.0, 0.0]}),
        json!({"space": "fixture-v1", "values": [1e40, 0.0]}),
        json!({"space": "fixture-v1", "values": [null, 1.0]}),
        json!({"space": "fixture-v1", "values": vec![1.0; 8193]}),
        json!({"space": " ", "values": [1.0]}),
        json!({"space": "x".repeat(257), "values": [1.0]}),
    ];
    for embedding in invalid {
        let files = TestFiles::new();
        let mut entry = fixture(DESSERT);
        entry["embedding"] = embedding;
        write_fixture(&files.memory, &entry).await;
        let before = tokio::fs::read(&files.memory).await.unwrap();
        assert!(MemoryStore::open(&files.memory).await.is_err());
        assert_eq!(tokio::fs::read(&files.memory).await.unwrap(), before);
    }
}

#[tokio::test]
async fn independent_writers_are_blocked_until_the_last_owner_drops_then_can_retry() {
    let files = TestFiles::new();
    let mut owner = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(Arc::new(FakeEmbedder::new("fixture-v1", 3)));
    owner
        .remember(TurnId::new(), DESSERT.to_owned(), String::new())
        .await
        .unwrap();
    let retained_owner = owner.clone();
    let before = tokio::fs::read(&files.memory).await.unwrap();
    let embedder = Arc::new(FakeEmbedder::new("fixture-v1", 3));
    let mut contender = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder.clone());
    assert!(
        contender
            .remember(TurnId::new(), WALK.to_owned(), String::new())
            .await
            .is_err()
    );
    assert!(
        contender
            .manage(MemoryRequest::Remember {
                profile_id: "default".to_owned(),
                text: WALK.to_owned(),
            })
            .await
            .is_err()
    );
    assert!(
        contender
            .clear_kind(MemoryKind::Conversation)
            .await
            .is_err()
    );
    assert!(contender.reindex_embeddings().await.is_err());
    assert_eq!(
        embedder.calls.load(Ordering::SeqCst),
        0,
        "competing writers must fail before invoking the encoder"
    );
    assert_eq!(contender.len().await, 1);
    assert_eq!(tokio::fs::read(&files.memory).await.unwrap(), before);
    drop(owner);
    assert!(
        contender.reindex_embeddings().await.is_err(),
        "a live clone retains the lease"
    );
    drop(retained_owner);
    assert_eq!(contender.reindex_embeddings().await.unwrap(), 1);
    contender
        .remember(TurnId::new(), WALK.to_owned(), String::new())
        .await
        .unwrap();
    drop(contender);
    let reopened = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder);
    assert_eq!(reopened.len().await, 2);
    assert_eq!(reopened.recall(DESSERT_QUERY, 5).await.unwrap().len(), 1);
    assert_eq!(reopened.recall(WALK_QUERY, 5).await.unwrap().len(), 1);
}

#[tokio::test]
async fn first_write_refreshes_a_read_only_snapshot_and_preserves_later_records() {
    let files = TestFiles::new();
    let embedder = Arc::new(FakeEmbedder::new("fixture-v1", 3));
    let mut snapshot = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder.clone());
    assert!(snapshot.is_empty().await);
    let mut owner = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder.clone());
    owner
        .remember(TurnId::new(), DESSERT.to_owned(), String::new())
        .await
        .unwrap();
    owner
        .remember(TurnId::new(), WALK.to_owned(), String::new())
        .await
        .unwrap();
    let written = rows(&files.memory).await;
    drop(owner);
    assert!(
        snapshot.is_empty().await,
        "reads retain their original snapshot before acquiring a writer lease"
    );
    snapshot
        .manage(MemoryRequest::Remember {
            profile_id: "default".to_owned(),
            text: "天气晴朗".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(snapshot.len().await, 3);
    let saved = rows(&files.memory).await;
    assert_eq!(
        &saved[..2],
        written.as_slice(),
        "the full-file replacement must preserve records and vectors written after open"
    );
    assert_eq!(saved[2]["user_text"], "天气晴朗");
    drop(snapshot);
    let reopened = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(embedder);
    assert_eq!(reopened.len().await, 3);
    assert_eq!(reopened.recall(DESSERT_QUERY, 5).await.unwrap().len(), 1);
    assert_eq!(reopened.recall(WALK_QUERY, 5).await.unwrap().len(), 1);
}

struct SlowProvider;

#[async_trait]
impl EmbeddingPort for SlowProvider {
    fn space(&self) -> &str {
        "synthetic-pending-v1"
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>, AppError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn unresponsive_automatic_encoder_does_not_block_durable_text_storage() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(Arc::new(SlowProvider));
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        store.remember(TurnId::new(), DESSERT.to_owned(), "合成回答".to_owned()),
    )
    .await
    .expect("automatic enrichment must finish within its two-second budget")
    .expect("text persistence succeeds despite an unresponsive encoder");
    assert!(store.health().await.detail.contains("fallback"));
    let saved = rows(&files.memory).await;
    assert_eq!(saved[0]["user_text"], DESSERT);
    assert_eq!(saved[0]["assistant_text"], "合成回答");
    assert!(saved[0].get("embedding").is_none());
    drop(store);
    let reopened = MemoryStore::open(&files.memory).await.unwrap();
    assert_eq!(reopened.len().await, 1);
    let recalled = reopened.recall(DESSERT, 5).await.unwrap();
    assert_eq!(recalled.len(), 1);
    assert!(recalled[0].content.contains("合成回答"));
}
