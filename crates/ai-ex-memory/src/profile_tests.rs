use super::*;

#[tokio::test]
async fn profiles_isolate_recall_export_clear_and_survive_reopening() {
    let directory = std::env::temp_dir().join(format!("ai-ex-profiles-{}", Uuid::new_v4()));
    let path = directory.join("memory.jsonl");
    let mut store = MemoryStore::open(&path).await.unwrap();
    let mut live_writer = store.clone();
    for (profile, fact) in [
        ("friend", "shared clue: tea"),
        ("host", "shared clue: coffee"),
    ] {
        store.select_profile(profile).await.unwrap();
        live_writer
            .remember_kind(
                MemoryKind::Persona,
                TurnId::new(),
                fact.to_owned(),
                "ok".to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(store.len().await, 1);
    }
    let recalled = store.recall_for_context("shared clue", 10).await.unwrap();
    assert_eq!(recalled.len(), 1);
    assert!(recalled[0].content.contains("coffee"));
    assert!(!recalled[0].content.contains("tea"));
    assert!(store.select_profile(" ").await.is_err());
    assert_eq!(store.count(Some(MemoryKind::Persona)).await, 1);

    let export = directory.join("host-export.jsonl");
    assert_eq!(store.export_kind(None, &export).await.unwrap(), 1);
    let exported = MemoryStore::open(&export).await.unwrap();
    assert!(exported.is_empty().await);
    exported.select_profile("host").await.unwrap();
    assert_eq!(exported.len().await, 1);
    assert_eq!(store.clear_kind(MemoryKind::Persona).await.unwrap(), 1);
    assert_eq!(store.clear_kind(MemoryKind::Persona).await.unwrap(), 0);

    let reopened = MemoryStore::open(&path).await.unwrap();
    reopened.select_profile("host").await.unwrap();
    assert!(reopened.is_empty().await);
    reopened.select_profile("friend").await.unwrap();
    assert_eq!(reopened.len().await, 1);
    assert!(
        reopened.recall_kind(None, "shared clue", 10).await.unwrap()[0]
            .content
            .contains("tea")
    );
    drop(store);
    drop(live_writer);
    for file in [path, export] {
        super::tests::remove_test_files(&file).await;
    }
    tokio::fs::remove_dir(directory).await.unwrap();
}

#[tokio::test]
async fn untagged_records_belong_only_to_default_profile() {
    let path = std::env::temp_dir().join(format!("ai-ex-legacy-profile-{}.jsonl", Uuid::new_v4()));
    let legacy = serde_json::json!({
        "id": Uuid::new_v4(), "turn_id": TurnId::new(), "created_ms": 1,
        "user_text": "legacy preference", "assistant_text": "remembered",
    });
    tokio::fs::write(&path, format!("{legacy}\n"))
        .await
        .unwrap();
    let mut store = MemoryStore::open(&path).await.unwrap();
    assert_eq!(store.len().await, 1);
    store.select_profile("new-character").await.unwrap();
    assert!(store.recall("legacy", 10).await.unwrap().is_empty());
    assert_eq!(store.clear_kind(MemoryKind::Conversation).await.unwrap(), 0);
    store.select_profile("default").await.unwrap();
    assert_eq!(store.len().await, 1);
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        format!("{legacy}\n")
    );
    drop(store);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn switching_waits_for_a_pending_write_to_finish_in_its_original_scope() {
    let path = std::env::temp_dir().join(format!("ai-ex-profile-race-{}.jsonl", Uuid::new_v4()));
    let store = MemoryStore::open(&path).await.unwrap();
    let write_guard = store.inner.write_lock.lock().await;
    let mut writer = store.clone();
    let pending = tokio::spawn(async move {
        writer
            .remember(TurnId::new(), "original scope".to_owned(), "ok".to_owned())
            .await
            .unwrap();
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while store.inner.profile_id.try_write().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let switcher = store.clone();
    let mut switching = tokio::spawn(async move {
        switcher.select_profile("other").await.unwrap();
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut switching)
            .await
            .is_err()
    );
    drop(write_guard);
    pending.await.unwrap();
    switching.await.unwrap();
    assert!(store.is_empty().await);
    let reopened = MemoryStore::open(&path).await.unwrap();
    assert_eq!(reopened.len().await, 1);
    assert!(
        reopened.recall("original", 10).await.unwrap()[0]
            .content
            .contains("original scope")
    );
    drop(store);
    super::tests::remove_test_files(&path).await;
}
