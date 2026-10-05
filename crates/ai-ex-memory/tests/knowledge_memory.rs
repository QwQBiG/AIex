use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ai_ex_core::{EmbeddingPort, MemoryPort};
use ai_ex_domain::{
    AppError, MemoryEntityGroup, MemoryEntityKind, MemoryEntityPage, MemoryEntry, MemoryKind,
    MemoryMedia, MemoryModality, MemoryPage, MemoryRequest, MemoryResponse, MemorySource, TurnId,
};
use ai_ex_memory::{MemoryStore, observation_media};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use uuid::Uuid;

const PAGE_BYTES: usize = 48 * 1024;

struct TestFiles {
    memory: PathBuf,
    export: PathBuf,
}

impl TestFiles {
    fn new() -> Self {
        let name = format!("ai-ex-knowledge-test-{}", Uuid::new_v4());
        Self {
            memory: std::env::temp_dir().join(format!("{name}.jsonl")),
            export: std::env::temp_dir().join(format!("{name}-export.jsonl")),
        }
    }
}

impl Drop for TestFiles {
    fn drop(&mut self) {
        // These exact unique synthetic files belong to this test; never scan a directory.
        for path in [&self.memory, &self.export] {
            let _ = std::fs::remove_file(path);
            let mut lock_path = path.as_os_str().to_os_string();
            lock_path.push(".lock");
            let _ = std::fs::remove_file(PathBuf::from(lock_path));
        }
    }
}

async fn read_rows(path: &Path) -> Vec<Value> {
    tokio::fs::read_to_string(path)
        .await
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn write_rows(path: &Path, rows: &[Value]) {
    let text = rows
        .iter()
        .map(|row| format!("{}\n", serde_json::to_string(row).unwrap()))
        .collect::<String>();
    tokio::fs::write(path, text).await.unwrap();
}

async fn run_page(store: &mut MemoryStore, request: MemoryRequest) -> MemoryPage {
    let response = store.manage(request).await.unwrap();
    assert!(
        serde_json::to_vec(&response).unwrap().len() <= PAGE_BYTES,
        "the complete control response must fit its 48 KiB budget"
    );
    let MemoryResponse::Page(page) = response else {
        panic!("request must return a memory page");
    };
    page
}

async fn memories(store: &mut MemoryStore, profile: &str) -> MemoryPage {
    run_page(
        store,
        MemoryRequest::List {
            profile_id: profile.to_owned(),
            query: String::new(),
            kind: None,
            offset: 0,
            limit: 100,
        },
    )
    .await
}

async fn entities(store: &mut MemoryStore, profile: &str) -> MemoryEntityPage {
    let response = store
        .manage(MemoryRequest::Entities {
            profile_id: profile.to_owned(),
            query: String::new(),
            offset: 0,
            limit: 100,
        })
        .await
        .unwrap();
    let MemoryResponse::Entities(page) = response else {
        panic!("entity query must return an entity page");
    };
    page
}

async fn evidence(store: &mut MemoryStore, profile: &str, key: &str) -> MemoryPage {
    run_page(
        store,
        MemoryRequest::Evidence {
            profile_id: profile.to_owned(),
            key: key.to_owned(),
            offset: 0,
            limit: 100,
        },
    )
    .await
}

fn group<'a>(page: &'a MemoryEntityPage, key: &str) -> &'a MemoryEntityGroup {
    page.entities
        .iter()
        .find(|entity| entity.key == key)
        .unwrap_or_else(|| panic!("missing entity {key}"))
}

async fn add_turn(
    store: &mut MemoryStore,
    profile: &str,
    user: &str,
    assistant: &str,
) -> MemoryEntry {
    store
        .remember(TurnId::new(), user.to_owned(), assistant.to_owned())
        .await
        .unwrap();
    memories(store, profile).await.entries.remove(0)
}

async fn correct(store: &mut MemoryStore, entry: &MemoryEntry, text: &str) {
    assert_eq!(
        store
            .manage(MemoryRequest::Correct {
                profile_id: entry.profile_id.clone(),
                id: entry.id,
                expected_revision: entry.revision,
                text: text.to_owned(),
            })
            .await
            .unwrap(),
        MemoryResponse::Changed
    );
}

async fn forget(store: &mut MemoryStore, entry: &MemoryEntry) {
    assert_eq!(
        store
            .manage(MemoryRequest::Forget {
                profile_id: entry.profile_id.clone(),
                id: entry.id,
                expected_revision: entry.revision,
            })
            .await
            .unwrap(),
        MemoryResponse::Changed
    );
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn media(modality: MemoryModality, tags: &[&str], importance: u16) -> MemoryMedia {
    let value = MemoryMedia {
        modality,
        tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        audio_features: matches!(modality, MemoryModality::Audio | MemoryModality::Multimodal)
            .then(|| {
                object(json!({
                    "mfcc": {"mean": [0.125, -0.25], "shape": [2, 1]},
                    "speech": {"transcript": "synthetic audio", "language": "en"}
                }))
            }),
        visual_features: matches!(
            modality,
            MemoryModality::Visual | MemoryModality::Multimodal
        )
        .then(|| {
            object(json!({
                "objects": [{"label": "cup", "box": [1, 2, 3, 4]}],
                "scene": {"color": "blue", "confidence": 0.75}
            }))
        }),
        metadata: object(json!({
            "origin": {"fixture": true, "labels": ["one", "two"]},
            "caption": "Synthetic test data only"
        })),
        importance,
    };
    value.validate().unwrap();
    value
}

async fn remember_media(
    store: &mut MemoryStore,
    profile: &str,
    text: &str,
    media: MemoryMedia,
) -> MemoryEntry {
    store
        .manage(MemoryRequest::RememberMedia {
            profile_id: profile.to_owned(),
            text: text.to_owned(),
            media,
        })
        .await
        .unwrap();
    memories(store, profile).await.entries.remove(0)
}

async fn query_media(
    store: &mut MemoryStore,
    profile: &str,
    modality: Option<MemoryModality>,
    tags: &[&str],
    min_importance: u16,
    pagination: (usize, usize),
) -> MemoryPage {
    run_page(
        store,
        MemoryRequest::QueryMedia {
            profile_id: profile.to_owned(),
            query: String::new(),
            modality,
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            min_importance,
            offset: pagination.0,
            limit: pagination.1,
        },
    )
    .await
}

fn ids(page: &MemoryPage) -> Vec<Uuid> {
    page.entries.iter().map(|entry| entry.id).collect()
}

#[tokio::test]
async fn user_entity_candidates_have_stable_keys_evidence_and_persist_across_restart() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    let name = add_turn(
        &mut store,
        "default",
        "My name is Alex",
        "My name is AssistantOnly. I like chocolate. I live in Paris.",
    )
    .await;
    let preference = add_turn(&mut store, "default", "I like coffee", "Acknowledged").await;
    let location = add_turn(&mut store, "default", "I live in Boston", "Acknowledged").await;
    let before = entities(&mut store, "default").await;
    assert_eq!(before.total, 3);
    assert_eq!(group(&before, "user.name").kind, MemoryEntityKind::Name);
    assert_eq!(group(&before, "user.name").current_value, "Alex");
    assert_eq!(
        group(&before, "preference:coffee").current_value,
        "like: coffee"
    );
    assert_eq!(group(&before, "fact:location").current_value, "Boston");
    assert!(!format!("{before:?}").contains("AssistantOnly"));
    assert!(!format!("{before:?}").contains("Paris"));
    assert!(!format!("{before:?}").contains("chocolate"));

    for (key, entry) in [
        ("user.name", &name),
        ("preference:coffee", &preference),
        ("fact:location", &location),
    ] {
        let entity = group(&before, key);
        assert_eq!(entity.total_mentions, 1);
        assert_eq!(entity.evidence.len(), 1);
        assert_eq!(entity.evidence[0].memory_id, entry.id);
        assert_eq!(entity.evidence[0].source, MemorySource::Automatic);
        assert!((1..=1000).contains(&entity.evidence[0].confidence));
        let supporting = evidence(&mut store, "default", key).await;
        assert_eq!(supporting.total, 1);
        assert_eq!(ids(&supporting), vec![entry.id]);
        assert!(
            supporting.knowledge[&entry.id]
                .entities
                .iter()
                .any(|e| e.key == key)
        );
    }

    let saved = read_rows(&files.memory).await;
    assert!(
        saved
            .iter()
            .all(|row| !row["knowledge"]["entities"].as_array().unwrap().is_empty())
    );
    let bytes = tokio::fs::read(&files.memory).await.unwrap();
    drop(store);
    let mut reopened = MemoryStore::open(&files.memory).await.unwrap();
    assert_eq!(entities(&mut reopened, "default").await, before);
    assert_eq!(tokio::fs::read(&files.memory).await.unwrap(), bytes);
}

#[tokio::test]
async fn conflicting_preferences_keep_evidence_and_corrections_and_forgetting_update_relationships()
{
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    let liked = add_turn(&mut store, "default", "I like coffee", "").await;
    let disliked = add_turn(&mut store, "default", "I dislike coffee", "").await;
    let first = entities(&mut store, "default").await;
    assert_eq!(first.total, 1);
    let coffee = group(&first, "preference:coffee");
    assert_eq!(coffee.current_value, "dislike: coffee");
    assert_eq!(coffee.total_mentions, 2);
    assert_eq!(
        coffee
            .evidence
            .iter()
            .map(|e| e.memory_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([liked.id, disliked.id])
    );

    correct(&mut store, &liked, "I like coffee").await;
    let newer_automatic = add_turn(&mut store, "default", "I dislike coffee", "").await;
    let corrected = entities(&mut store, "default").await;
    let coffee = group(&corrected, "preference:coffee");
    assert_eq!(
        coffee.current_value, "like: coffee",
        "an explicit correction takes precedence over a later automatic candidate"
    );
    assert_eq!(coffee.total_mentions, 3);
    let confirmed = coffee
        .evidence
        .iter()
        .find(|e| e.memory_id == liked.id)
        .unwrap();
    assert_eq!(confirmed.revision, 2);
    assert_eq!(confirmed.source, MemorySource::UserCorrection);

    let liked = memories(&mut store, "default")
        .await
        .entries
        .into_iter()
        .find(|entry| entry.id == liked.id)
        .unwrap();
    correct(&mut store, &liked, "I like tea").await;
    let moved = entities(&mut store, "default").await;
    assert_eq!(group(&moved, "preference:coffee").total_mentions, 2);
    assert_eq!(
        group(&moved, "preference:coffee").current_value,
        "dislike: coffee"
    );
    assert_eq!(
        group(&moved, "preference:tea").evidence[0].memory_id,
        liked.id
    );
    assert!(!ids(&evidence(&mut store, "default", "preference:coffee").await).contains(&liked.id));

    forget(&mut store, &disliked).await;
    let retained = entities(&mut store, "default").await;
    assert_eq!(group(&retained, "preference:coffee").total_mentions, 1);
    assert_eq!(
        group(&retained, "preference:coffee").evidence[0].memory_id,
        newer_automatic.id
    );
    forget(&mut store, &newer_automatic).await;
    let final_page = entities(&mut store, "default").await;
    assert_eq!(final_page.total, 1);
    assert_eq!(final_page.entities[0].key, "preference:tea");
    assert!(
        evidence(&mut store, "default", "preference:coffee")
            .await
            .entries
            .is_empty()
    );
    drop(store);
    let mut reopened = MemoryStore::open(&files.memory).await.unwrap();
    assert_eq!(entities(&mut reopened, "default").await, final_page);
}

#[tokio::test]
async fn entities_evidence_and_media_are_isolated_by_the_active_character() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    store.select_profile("alice").await.unwrap();
    let alice = add_turn(&mut store, "alice", "My name is Alex", "").await;
    let alice_media = remember_media(
        &mut store,
        "alice",
        "Alice picture",
        media(MemoryModality::Visual, &["shared"], 600),
    )
    .await;
    store.select_profile("bob").await.unwrap();
    let bob = add_turn(&mut store, "bob", "我叫小明", "").await;
    add_turn(&mut store, "bob", "我喜欢咖啡", "").await;
    let bob_media = remember_media(
        &mut store,
        "bob",
        "Bob picture",
        media(MemoryModality::Visual, &["shared"], 900),
    )
    .await;
    let bob_entities = entities(&mut store, "bob").await;
    assert_eq!(group(&bob_entities, "user.name").current_value, "小明");
    assert_eq!(
        group(&bob_entities, "preference:咖啡").current_value,
        "like: 咖啡"
    );
    assert_eq!(
        ids(&evidence(&mut store, "bob", "user.name").await),
        vec![bob.id]
    );
    assert_eq!(
        ids(&query_media(&mut store, "bob", None, &["shared"], 0, (0, 100)).await),
        vec![bob_media.id]
    );
    assert!(
        store
            .manage(MemoryRequest::Evidence {
                profile_id: "alice".to_owned(),
                key: "user.name".to_owned(),
                offset: 0,
                limit: 10
            })
            .await
            .is_err()
    );

    store.select_profile("alice").await.unwrap();
    let alice_entities = entities(&mut store, "alice").await;
    assert_eq!(alice_entities.total, 1);
    assert_eq!(group(&alice_entities, "user.name").current_value, "Alex");
    assert_eq!(
        ids(&evidence(&mut store, "alice", "user.name").await),
        vec![alice.id]
    );
    assert_eq!(
        ids(&query_media(&mut store, "alice", None, &["shared"], 0, (0, 100)).await),
        vec![alice_media.id]
    );
    drop(store);
    let mut reopened = MemoryStore::open(&files.memory).await.unwrap();
    reopened.select_profile("bob").await.unwrap();
    assert_eq!(entities(&mut reopened, "bob").await, bob_entities);
}

#[tokio::test]
async fn nested_multimodal_features_metadata_and_tags_survive_restart_and_export() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    let expected = media(MemoryModality::Multimodal, &["coffee", "recording"], 850);
    let entry = remember_media(&mut store, "default", "I like coffee", expected.clone()).await;
    let before = memories(&mut store, "default").await;
    assert_eq!(before.knowledge[&entry.id].media.as_ref(), Some(&expected));
    assert_eq!(
        before.knowledge[&entry.id].entities[0].key,
        "preference:coffee"
    );
    assert_eq!(store.export_kind(None, &files.export).await.unwrap(), 1);
    let source = read_rows(&files.memory).await;
    assert_eq!(read_rows(&files.export).await, source);
    drop(store);

    let mut reopened = MemoryStore::open(&files.memory).await.unwrap();
    let mut exported = MemoryStore::open(&files.export).await.unwrap();
    assert_eq!(memories(&mut reopened, "default").await, before);
    assert_eq!(memories(&mut exported, "default").await, before);
    let queried = query_media(
        &mut reopened,
        "default",
        Some(MemoryModality::Multimodal),
        &["recording"],
        850,
        (0, 10),
    )
    .await;
    assert_eq!(ids(&queried), vec![entry.id]);
    assert_eq!(queried.knowledge[&entry.id].media.as_ref(), Some(&expected));
}

#[tokio::test]
async fn media_filters_or_tags_modality_importance_and_pagination_preserve_order() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    let first_visual = remember_media(
        &mut store,
        "default",
        "First travel picture",
        media(MemoryModality::Visual, &["shared"], 900),
    )
    .await;
    let audio = remember_media(
        &mut store,
        "default",
        "Travel sound",
        media(MemoryModality::Audio, &["travel"], 600),
    )
    .await;
    remember_media(
        &mut store,
        "default",
        "Unrelated image",
        media(MemoryModality::Visual, &["other"], 500),
    )
    .await;
    remember_media(
        &mut store,
        "default",
        "Quiet sound",
        media(MemoryModality::Audio, &["shared"], 400),
    )
    .await;
    let latest_visual = remember_media(
        &mut store,
        "default",
        "Latest travel picture",
        media(MemoryModality::Visual, &["travel"], 900),
    )
    .await;
    let tagged_text = remember_media(
        &mut store,
        "default",
        "Travel note",
        media(MemoryModality::Text, &["shared"], 800),
    )
    .await;
    store
        .manage(MemoryRequest::Remember {
            profile_id: "default".to_owned(),
            text: "A regular note without media".to_owned(),
        })
        .await
        .unwrap();

    let all = query_media(&mut store, "default", None, &[], 0, (0, 100)).await;
    assert_eq!(all.total, 7, "ordinary notes participate as text memories");
    let visual = query_media(
        &mut store,
        "default",
        Some(MemoryModality::Visual),
        &["shared", "travel"],
        600,
        (0, 100),
    )
    .await;
    assert_eq!(visual.total, 2);
    assert_eq!(ids(&visual), vec![latest_visual.id, first_visual.id]);
    let filtered = query_media(
        &mut store,
        "default",
        None,
        &["shared", "travel"],
        450,
        (0, 100),
    )
    .await;
    assert_eq!(
        ids(&filtered),
        vec![latest_visual.id, first_visual.id, tagged_text.id, audio.id]
    );
    let second_page = query_media(
        &mut store,
        "default",
        None,
        &["shared", "travel"],
        450,
        (1, 2),
    )
    .await;
    assert_eq!(second_page.total, 4);
    assert_eq!(second_page.offset, 1);
    assert_eq!(ids(&second_page), vec![first_visual.id, tagged_text.id]);
    let disjunction = query_media(
        &mut store,
        "default",
        None,
        &["missing", "travel"],
        0,
        (0, 100),
    )
    .await;
    assert_eq!(ids(&disjunction), vec![latest_visual.id, audio.id]);
    let text_match = run_page(
        &mut store,
        MemoryRequest::QueryMedia {
            profile_id: "default".to_owned(),
            query: "latest".to_owned(),
            modality: None,
            tags: vec![],
            min_importance: 0,
            offset: 0,
            limit: 100,
        },
    )
    .await;
    assert_eq!(ids(&text_match), vec![latest_visual.id]);
    let past_end = query_media(&mut store, "default", None, &[], 0, (20, 10)).await;
    assert_eq!(past_end.total, 7);
    assert!(past_end.entries.is_empty());
    assert!(past_end.knowledge.is_empty());
}

#[tokio::test]
async fn large_text_and_media_pages_stay_bounded_without_truncating_persisted_data() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    let mut large_media = media(MemoryModality::Visual, &["large"], 700);
    large_media
        .visual_features
        .as_mut()
        .unwrap()
        .insert("fixture_blob".to_owned(), json!("v".repeat(7000)));
    large_media.validate().unwrap();
    let huge = remember_media(
        &mut store,
        "default",
        "Synthetic large imported record",
        large_media.clone(),
    )
    .await;
    let smaller = remember_media(
        &mut store,
        "default",
        "Another media record",
        media(MemoryModality::Audio, &["large"], 600),
    )
    .await;
    drop(store);

    // Historical imports can exceed the manual 4096-character editor limit.
    let mut rows = read_rows(&files.memory).await;
    let row = rows
        .iter_mut()
        .find(|row| row["id"] == json!(huge.id))
        .unwrap();
    row["user_text"] = json!("字".repeat(18_000));
    row["assistant_text"] = json!("文".repeat(18_000));
    write_rows(&files.memory, &rows).await;
    let persisted = tokio::fs::read(&files.memory).await.unwrap();
    let mut reopened = MemoryStore::open(&files.memory).await.unwrap();
    let preview = query_media(&mut reopened, "default", None, &["large"], 0, (0, 100)).await;
    assert_eq!(preview.total, 2);
    assert!(preview.truncated_ids.contains(&huge.id));
    assert_eq!(preview.entries[0].id, huge.id);
    assert!(preview.entries[0].user_text.chars().count() < 18_000);
    let next = query_media(
        &mut reopened,
        "default",
        None,
        &["large"],
        0,
        (preview.entries.len(), 100),
    )
    .await;
    let mut seen = ids(&preview);
    seen.extend(ids(&next));
    assert_eq!(
        seen,
        vec![huge.id, smaller.id],
        "bounded pages must not skip records"
    );
    let listed = memories(&mut reopened, "default").await;
    assert_eq!(listed.total, 2);
    assert_eq!(tokio::fs::read(&files.memory).await.unwrap(), persisted);
    assert_eq!(reopened.export_kind(None, &files.export).await.unwrap(), 2);
    assert_eq!(
        read_rows(&files.export).await,
        rows,
        "preview truncation must not alter export payloads"
    );
}

struct TopicEmbedder;

#[async_trait]
impl EmbeddingPort for TopicEmbedder {
    fn space(&self) -> &str {
        "synthetic-knowledge-topics-v1"
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
        // Deterministic classes exercise vector consistency, not real-model accuracy.
        Ok(if text.contains("coffee") || text.contains("咖啡查询") {
            vec![3.0, 0.0, 0.0]
        } else if text.contains("tea") || text.contains("茶叶查询") {
            vec![0.0, 3.0, 0.0]
        } else {
            vec![0.0, 0.0, 3.0]
        })
    }
}

#[tokio::test]
async fn media_correction_updates_text_vectors_and_tags_and_plain_correction_clears_media() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(Arc::new(TopicEmbedder));
    let entry = remember_media(
        &mut store,
        "default",
        "I like coffee",
        media(MemoryModality::Audio, &["old"], 400),
    )
    .await;
    assert_eq!(store.recall("咖啡查询", 10).await.unwrap().len(), 1);
    let changed_media = media(MemoryModality::Visual, &["new"], 900);
    store
        .manage(MemoryRequest::CorrectMedia {
            profile_id: "default".to_owned(),
            id: entry.id,
            expected_revision: entry.revision,
            text: "I like tea".to_owned(),
            media: changed_media.clone(),
        })
        .await
        .unwrap();
    let current = memories(&mut store, "default").await;
    assert_eq!(current.entries[0].revision, 2);
    assert_eq!(current.entries[0].user_text, "I like tea");
    assert_eq!(current.entries[0].source, MemorySource::UserCorrection);
    assert_eq!(
        current.knowledge[&entry.id].media.as_ref(),
        Some(&changed_media)
    );
    assert_eq!(
        current.knowledge[&entry.id].entities[0].key,
        "preference:tea"
    );
    assert_eq!(
        query_media(&mut store, "default", None, &["old"], 0, (0, 10))
            .await
            .total,
        0
    );
    assert_eq!(
        ids(&query_media(
            &mut store,
            "default",
            Some(MemoryModality::Visual),
            &["new"],
            900,
            (0, 10)
        )
        .await),
        vec![entry.id]
    );
    assert!(store.recall("咖啡查询", 10).await.unwrap().is_empty());
    assert_eq!(store.recall("茶叶查询", 10).await.unwrap().len(), 1);
    assert_eq!(
        read_rows(&files.memory).await[0]["embedding"]["values"],
        json!([0.0, 3.0, 0.0])
    );

    drop(store);
    let mut reopened = MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_embedder(Arc::new(TopicEmbedder));
    assert_eq!(memories(&mut reopened, "default").await, current);
    correct(&mut reopened, &current.entries[0], "My name is Alex").await;
    let plain = memories(&mut reopened, "default").await;
    assert_eq!(plain.entries[0].revision, 3);
    assert!(plain.knowledge[&entry.id].media.is_none());
    assert_eq!(plain.knowledge[&entry.id].entities[0].key, "user.name");
    assert_eq!(
        query_media(
            &mut reopened,
            "default",
            Some(MemoryModality::Visual),
            &[],
            0,
            (0, 10)
        )
        .await
        .total,
        0
    );
    assert!(reopened.recall("茶叶查询", 10).await.unwrap().is_empty());
    assert!(
        evidence(&mut reopened, "default", "preference:tea")
            .await
            .entries
            .is_empty()
    );
}

#[tokio::test]
async fn old_jsonl_and_legacy_metadata_are_read_without_rewriting_or_mining_assistant_claims() {
    let files = TestFiles::new();
    let name_id = Uuid::new_v4();
    let location_id = Uuid::new_v4();
    let old_rows = vec![
        json!({
            "id": name_id, "turn_id": TurnId::new(), "created_ms": 1,
            "user_text": "My name is Alex", "assistant_text": "My name is AssistantOnly"
        }),
        json!({
            "id": location_id, "turn_id": TurnId::new(), "created_ms": 2,
            "user_text": "My name is CompositeAssistant", "assistant_text": "I like chocolate",
            "legacy_metadata": {
                "user_input": "I live in Boston", "ai_response": "My name is MetadataAssistant",
                "custom": {"nested": [1, true, "retained"]}
            }
        }),
        json!({
            "id": Uuid::new_v4(), "turn_id": TurnId::new(), "created_ms": 3,
            "user_text": "I like chocolate", "assistant_text": "My name is SummaryAssistant",
            "legacy_metadata": {"memory_type": "SUMMARY", "summary": "I like chocolate"}
        }),
    ];
    write_rows(&files.memory, &old_rows).await;
    let original = tokio::fs::read(&files.memory).await.unwrap();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    let page = entities(&mut store, "default").await;
    assert_eq!(page.total, 2);
    assert_eq!(group(&page, "user.name").current_value, "Alex");
    assert_eq!(group(&page, "user.name").total_mentions, 1);
    assert_eq!(group(&page, "fact:location").current_value, "Boston");
    assert_eq!(
        ids(&evidence(&mut store, "default", "user.name").await),
        vec![name_id]
    );
    assert_eq!(
        ids(&evidence(&mut store, "default", "fact:location").await),
        vec![location_id]
    );
    assert!(
        memories(&mut store, "default")
            .await
            .entries
            .iter()
            .all(|entry| entry.source == MemorySource::Automatic)
    );
    assert_eq!(tokio::fs::read(&files.memory).await.unwrap(), original);
    assert_eq!(store.export_kind(None, &files.export).await.unwrap(), 3);
    let exported = read_rows(&files.export).await;
    assert_eq!(
        exported[1]["legacy_metadata"],
        old_rows[1]["legacy_metadata"]
    );
    assert_eq!(
        exported[2]["legacy_metadata"],
        old_rows[2]["legacy_metadata"]
    );
    assert_eq!(tokio::fs::read(&files.memory).await.unwrap(), original);
}
#[tokio::test]
async fn automatic_observation_importance_stats_and_modality_clearing_are_profile_scoped() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    let observed = observation_media(
        &"x".repeat(500),
        Some(object(json!({"entities": ["speaker", "music"]}))),
        Some(object(json!({"objects": ["cup", "table", "chair"]}))),
        vec!["observation".to_owned()],
        object(json!({"synthetic": true})),
    )
    .unwrap();
    assert_eq!(observed.modality, MemoryModality::Multimodal);
    assert_eq!(
        observed.importance, 640,
        "legacy heuristic: 300 text + 100 entities + 240 modality"
    );
    let text_observation =
        observation_media(&"x".repeat(500), None, None, vec![], Map::new()).unwrap();
    assert_eq!(text_observation.modality, MemoryModality::Text);
    assert_eq!(text_observation.importance, 500);

    store.select_profile("alice").await.unwrap();
    add_turn(&mut store, "alice", "I live in Boston", "").await;
    remember_media(&mut store, "alice", "Tagged text", text_observation).await;
    remember_media(
        &mut store,
        "alice",
        "Picture",
        media(MemoryModality::Visual, &["picture"], 500),
    )
    .await;
    store
        .remember_media(
            MemoryKind::Conversation,
            TurnId::new(),
            "Automatic audio".to_owned(),
            media(MemoryModality::Audio, &["audio"], 400),
        )
        .await
        .unwrap();
    store
        .remember_media(
            MemoryKind::Conversation,
            TurnId::new(),
            "Automatic observation".to_owned(),
            observed.clone(),
        )
        .await
        .unwrap();
    let alice_page = memories(&mut store, "alice").await;
    let automatic = alice_page
        .entries
        .iter()
        .find(|entry| entry.user_text == "Automatic observation")
        .unwrap();
    assert_eq!(automatic.source, MemorySource::Automatic);
    assert_eq!(
        alice_page.knowledge[&automatic.id].media.as_ref(),
        Some(&observed)
    );
    let stats = store.media_stats().await;
    assert_eq!(
        (
            stats.total,
            stats.text,
            stats.audio,
            stats.visual,
            stats.multimodal
        ),
        (5, 2, 1, 1, 1)
    );

    store.select_profile("bob").await.unwrap();
    remember_media(
        &mut store,
        "bob",
        "Bob picture",
        media(MemoryModality::Visual, &[], 500),
    )
    .await;
    remember_media(
        &mut store,
        "bob",
        "Bob audio",
        media(MemoryModality::Audio, &[], 500),
    )
    .await;
    let bob_stats = store.media_stats().await;
    assert_eq!(
        (bob_stats.total, bob_stats.visual, bob_stats.audio),
        (2, 1, 1)
    );
    store.select_profile("alice").await.unwrap();
    assert_eq!(store.clear_media(MemoryModality::Visual).await.unwrap(), 1);
    assert_eq!(store.clear_media(MemoryModality::Visual).await.unwrap(), 0);
    assert_eq!(store.clear_media(MemoryModality::Text).await.unwrap(), 2);
    let remaining = store.media_stats().await;
    assert_eq!(
        (
            remaining.total,
            remaining.text,
            remaining.audio,
            remaining.visual,
            remaining.multimodal
        ),
        (2, 0, 1, 0, 1)
    );
    assert!(entities(&mut store, "alice").await.entities.is_empty());
    drop(store);

    let mut reopened = MemoryStore::open(&files.memory).await.unwrap();
    reopened.select_profile("alice").await.unwrap();
    assert_eq!(reopened.media_stats().await, remaining);
    reopened.select_profile("bob").await.unwrap();
    assert_eq!(reopened.media_stats().await, bob_stats);
    assert_eq!(
        query_media(
            &mut reopened,
            "bob",
            Some(MemoryModality::Visual),
            &[],
            0,
            (0, 10)
        )
        .await
        .total,
        1
    );
}

#[tokio::test]
async fn observed_media_is_not_personal_evidence_but_explicit_notes_are() {
    let files = TestFiles::new();
    let mut store = MemoryStore::open(&files.memory).await.unwrap();
    store.select_profile("alice").await.unwrap();
    store
        .remember_media(
            MemoryKind::Conversation,
            TurnId::new(),
            "My name is Caption".to_owned(),
            media(MemoryModality::Audio, &[], 400),
        )
        .await
        .unwrap();
    assert!(entities(&mut store, "alice").await.entities.is_empty());
    remember_media(
        &mut store,
        "alice",
        "My name is Alice",
        media(MemoryModality::Audio, &[], 400),
    )
    .await;
    let page = entities(&mut store, "alice").await;
    let name = group(&page, "user.name");
    assert_eq!(name.current_value, "Alice");
    assert_eq!(name.total_mentions, 1);
    assert_eq!(name.evidence[0].source, MemorySource::UserNote);
    drop(store);
    let mut reopened = MemoryStore::open(&files.memory).await.unwrap();
    reopened.select_profile("alice").await.unwrap();
    assert_eq!(entities(&mut reopened, "alice").await, page);
}
