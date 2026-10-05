use ai_ex_domain::{MemoryEntry, MemoryPage, MemoryRequest, MemoryResponse};

use super::*;

async fn store() -> (PathBuf, MemoryStore) {
    let path = std::env::temp_dir().join(format!("ai-ex-managed-memory-{}.jsonl", Uuid::new_v4()));
    let store = MemoryStore::open(&path).await.unwrap();
    (path, store)
}

fn list(profile: &str, query: &str, offset: usize, limit: usize) -> MemoryRequest {
    MemoryRequest::List {
        profile_id: profile.to_owned(),
        query: query.to_owned(),
        kind: None,
        offset,
        limit,
    }
}

fn note(profile: &str, text: &str) -> MemoryRequest {
    MemoryRequest::Remember {
        profile_id: profile.to_owned(),
        text: text.to_owned(),
    }
}

fn correct(entry: &MemoryEntry, text: &str) -> MemoryRequest {
    MemoryRequest::Correct {
        profile_id: entry.profile_id.clone(),
        id: entry.id,
        expected_revision: entry.revision,
        text: text.to_owned(),
    }
}

fn forget(entry: &MemoryEntry) -> MemoryRequest {
    MemoryRequest::Forget {
        profile_id: entry.profile_id.clone(),
        id: entry.id,
        expected_revision: entry.revision,
    }
}

async fn page(store: &mut MemoryStore, request: MemoryRequest) -> MemoryPage {
    let MemoryResponse::Page(page) = store.manage(request).await.unwrap() else {
        panic!("memory page expected");
    };
    page
}

#[tokio::test]
async fn confirmed_notes_survive_restart_without_a_matching_query_and_respect_budget() {
    let (path, mut store) = store().await;
    store
        .remember(
            TurnId::new(),
            "alpha beta gamma".to_owned(),
            "automatic old".to_owned(),
        )
        .await
        .unwrap();
    store.manage(note("default", "喜欢桂花茶")).await.unwrap();
    store.manage(note("default", "请叫我小林")).await.unwrap();
    let reopened = MemoryStore::open(&path).await.unwrap();
    let recalled = reopened
        .recall_for_context("alpha beta gamma", 2)
        .await
        .unwrap();
    assert_eq!(recalled.len(), 2);
    assert!(recalled[0].content.contains("小林"));
    assert!(recalled[1].content.contains("桂花茶"));
    assert!(
        reopened
            .recall_for_context("alpha", 0)
            .await
            .unwrap()
            .is_empty()
    );
    let recalled = reopened
        .recall_for_context("alpha beta gamma", 3)
        .await
        .unwrap();
    assert!(recalled[2].content.contains("automatic old"));
    drop(store);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn correction_replaces_automatic_content_and_forgetting_survives_reopening() {
    let (path, mut store) = store().await;
    store
        .remember(
            TurnId::new(),
            "旧偏好咖啡".to_owned(),
            "旧错误回答".to_owned(),
        )
        .await
        .unwrap();
    let original = page(&mut store, list("default", "", 0, 10))
        .await
        .entries
        .remove(0);
    store
        .manage(correct(&original, "  新偏好红茶  "))
        .await
        .unwrap();
    let changed = page(&mut store, list("default", "", 0, 10))
        .await
        .entries
        .remove(0);
    assert_eq!(changed.id, original.id);
    assert_eq!(changed.turn_id, original.turn_id);
    assert_eq!(changed.source, MemorySource::UserCorrection);
    assert_eq!(changed.kind, MemoryKind::Persona);
    assert_eq!(changed.revision, 2);
    assert_eq!(changed.user_text, "新偏好红茶");
    assert!(changed.assistant_text.is_empty());
    assert!(changed.updated_ms.unwrap() >= changed.created_ms);
    assert!(
        store
            .manage(correct(&original, "stale edit"))
            .await
            .is_err()
    );
    assert!(store.manage(forget(&original)).await.is_err());
    let disk = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(!disk.contains("旧偏好") && !disk.contains("旧错误"));
    drop(store);
    let mut reopened = MemoryStore::open(&path).await.unwrap();
    let recalled = reopened.recall_for_context("unrelated", 10).await.unwrap();
    assert_eq!(recalled.len(), 1);
    assert!(recalled[0].content.contains("新偏好红茶"));
    reopened.manage(forget(&changed)).await.unwrap();
    assert!(MemoryStore::open(&path).await.unwrap().is_empty().await);
    assert!(reopened.manage(forget(&changed)).await.is_err());
    drop(reopened);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn pages_filter_actual_text_and_never_cross_profile_boundaries() {
    let (path, mut store) = store().await;
    for text in ["Blue tea", "Green tea", "蓝天"] {
        store.manage(note("default", text)).await.unwrap();
    }
    let first = page(&mut store, list("default", "TEA", 0, 1)).await;
    assert_eq!(first.total, 2);
    assert_eq!(first.entries[0].user_text, "Green tea");
    let second = page(&mut store, list("default", "tea", 1, 1)).await;
    assert_eq!(second.offset, 1);
    assert_eq!(second.entries[0].user_text, "Blue tea");
    assert_eq!(
        page(&mut store, list("default", "tea", 10, 1)).await.total,
        2
    );
    let owned = first.entries[0].clone();
    store.select_profile("other").await.unwrap();
    assert!(store.manage(list("default", "", 0, 10)).await.is_err());
    assert!(
        store
            .manage(note("default", "wrong profile"))
            .await
            .is_err()
    );
    assert!(store.manage(forget(&owned)).await.is_err());
    assert!(
        store
            .manage(MemoryRequest::Forget {
                profile_id: "other".to_owned(),
                id: owned.id,
                expected_revision: owned.revision
            })
            .await
            .is_err()
    );
    store.manage(note("other", "独立记录")).await.unwrap();
    assert_eq!(page(&mut store, list("other", "", 0, 10)).await.total, 1);
    assert!(
        !store.recall_for_context("tea", 10).await.unwrap()[0]
            .content
            .contains("tea")
    );
    store.select_profile("default").await.unwrap();
    store.manage(forget(&owned)).await.unwrap();
    assert_eq!(page(&mut store, list("default", "", 0, 10)).await.total, 2);
    let mut reopened = MemoryStore::open(&path).await.unwrap();
    reopened.select_profile("other").await.unwrap();
    assert_eq!(page(&mut reopened, list("other", "", 0, 10)).await.total, 1);
    drop(store);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn concurrent_edits_check_revision_after_obtaining_the_write_lock() {
    let (path, mut store) = store().await;
    store.manage(note("default", "original")).await.unwrap();
    let entry = page(&mut store, list("default", "", 0, 1))
        .await
        .entries
        .remove(0);
    let mut other = store.clone();
    let (one, two) = tokio::join!(
        store.manage(correct(&entry, "one")),
        other.manage(correct(&entry, "two"))
    );
    assert_ne!(one.is_ok(), two.is_ok());
    let entries = page(&mut store, list("default", "", 0, 10)).await.entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].revision, 2);
    let mut reopened = MemoryStore::open(&path).await.unwrap();
    assert_eq!(
        page(&mut reopened, list("default", "", 0, 10))
            .await
            .entries,
        entries
    );
    drop(other);
    drop(store);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn validation_and_disabled_storage_never_claim_a_write_succeeded() {
    let mut store = MemoryStore::disabled();
    assert!(!page(&mut store, list("default", "", 0, 1)).await.enabled);
    for request in [
        note("default", "valid"),
        note("default", "  "),
        note("default", &"x".repeat(4097)),
        list("default", "", 0, 0),
        list("default", "", 0, 101),
        list("default", &"x".repeat(513), 0, 1),
        list("default", "", usize::MAX, 1),
        list(" ", "", 0, 1),
    ] {
        assert!(store.manage(request).await.is_err());
    }
}

#[tokio::test]
async fn old_records_expose_default_provenance_without_rewriting_the_file() {
    let (path, _) = store().await;
    let original = serde_json::json!({ "id": Uuid::new_v4(), "turn_id": TurnId::new(),
        "created_ms": 1, "user_text": "legacy", "assistant_text": "answer" })
    .to_string();
    tokio::fs::write(&path, &original).await.unwrap();
    let mut store = MemoryStore::open(&path).await.unwrap();
    let entry = page(&mut store, list("default", "", 0, 10))
        .await
        .entries
        .remove(0);
    assert_eq!(entry.source, MemorySource::Automatic);
    assert_eq!(entry.revision, 1);
    assert_eq!(entry.updated_ms, None);
    assert_eq!(entry.kind, MemoryKind::Conversation);
    assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), original);
    drop(store);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn transport_budget_preserves_notes_and_marks_only_oversized_record_previews() {
    let (path, mut store) = store().await;
    for _ in 0..4 {
        store
            .manage(note("default", &"\u{0001}".repeat(4096)))
            .await
            .unwrap();
    }
    let first = page(&mut store, list("default", "", 0, 12)).await;
    assert!(serde_json::to_vec(&first).unwrap().len() <= 48 * 1024);
    assert!(!first.entries.is_empty() && first.entries.len() < 4);
    assert!(first.truncated_ids.is_empty());
    assert_eq!(first.entries[0].user_text.chars().count(), 4096);
    let second = page(&mut store, list("default", "", first.entries.len(), 12)).await;
    assert!(
        second
            .entries
            .iter()
            .all(|entry| first.entries.iter().all(|seen| seen.id != entry.id))
    );
    store
        .remember(TurnId::new(), "huge record".to_owned(), "x".repeat(100_000))
        .await
        .unwrap();
    let large = page(&mut store, list("default", "huge record", 0, 12)).await;
    assert_eq!(large.total, 1);
    assert_eq!(large.entries.len(), 1);
    assert_eq!(large.truncated_ids, vec![large.entries[0].id]);
    assert_eq!(
        large.entries[0].user_text.chars().count()
            + large.entries[0].assistant_text.chars().count(),
        4000
    );
    assert!(serde_json::to_vec(&large).unwrap().len() <= 48 * 1024);
    let raw = tokio::fs::read_to_string(&path).await.unwrap();
    assert!(raw.contains(&"x".repeat(100_000)));
    drop(store);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn automatic_recall_breaks_equal_relevance_ties_by_recency() {
    let (path, mut store) = store().await;
    for answer in ["old", "new"] {
        store
            .remember(TurnId::new(), "shared clue".to_owned(), answer.to_owned())
            .await
            .unwrap();
    }
    let recalled = store.recall_for_context("shared clue", 1).await.unwrap();
    assert!(recalled[0].content.ends_with("new"));
    drop(store);
    super::tests::remove_test_files(&path).await;
}

#[cfg(windows)]
#[tokio::test]
async fn failed_management_replacement_preserves_revision_and_allows_retry() {
    use std::os::windows::fs::OpenOptionsExt;
    let (path, mut store) = store().await;
    store.manage(note("default", "original")).await.unwrap();
    let entry = page(&mut store, list("default", "", 0, 1))
        .await
        .entries
        .remove(0);
    let original = tokio::fs::read(&path).await.unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    for request in [
        note("default", "new"),
        correct(&entry, "changed"),
        forget(&entry),
    ] {
        assert!(store.manage(request).await.is_err());
        assert_eq!(
            page(&mut store, list("default", "", 0, 10)).await.entries,
            vec![entry.clone()]
        );
        assert_eq!(tokio::fs::read(&path).await.unwrap(), original);
    }
    drop(held);
    store.manage(correct(&entry, "changed")).await.unwrap();
    let mut reopened = MemoryStore::open(&path).await.unwrap();
    assert_eq!(
        page(&mut reopened, list("default", "", 0, 10))
            .await
            .entries[0]
            .revision,
        2
    );
    drop(store);
    super::tests::remove_test_files(&path).await;
}
