use super::*;

async fn populated_store() -> (PathBuf, MemoryStore) {
    let directory = std::env::temp_dir().join(format!("ai-ex-memory-test-{}", Uuid::new_v4()));
    let mut store = MemoryStore::open(directory.join("memory.jsonl"))
        .await
        .unwrap();
    for kind in [MemoryKind::Viewer, MemoryKind::Persona] {
        store
            .remember_kind(
                kind,
                TurnId::new(),
                "保留中文记忆".to_owned(),
                "已记录".to_owned(),
            )
            .await
            .unwrap();
    }
    (directory, store)
}

#[tokio::test]
async fn export_rejects_existing_backup_and_source_without_changing_them() {
    let (directory, store) = populated_store().await;
    let source = directory.join("memory.jsonl");
    let original = tokio::fs::read(&source).await.unwrap();
    assert!(
        store
            .export_kind(Some(MemoryKind::Viewer), &source)
            .await
            .is_err()
    );
    assert_eq!(tokio::fs::read(&source).await.unwrap(), original);

    let backup = directory.join("backup.jsonl");
    assert_eq!(store.export_kind(None, &backup).await.unwrap(), 2);
    assert!(
        store
            .export_kind(Some(MemoryKind::Viewer), &backup)
            .await
            .is_err()
    );
    assert_eq!(tokio::fs::read(&backup).await.unwrap(), original);
    assert_eq!(MemoryStore::open(&backup).await.unwrap().len().await, 2);
    drop(store);
    super::tests::remove_test_files(&source).await;
    tokio::fs::remove_file(backup).await.unwrap();
    tokio::fs::remove_dir(directory).await.unwrap();
}

#[tokio::test]
async fn clear_preserves_unrelated_temporary_file_and_can_clear_last_record() {
    let (directory, mut store) = populated_store().await;
    let source = directory.join("memory.jsonl");
    let unrelated = source.with_extension("jsonl.tmp");
    tokio::fs::write(&unrelated, b"existing recovery data")
        .await
        .unwrap();
    assert_eq!(store.clear_kind(MemoryKind::Viewer).await.unwrap(), 1);
    assert_eq!(
        tokio::fs::read(&unrelated).await.unwrap(),
        b"existing recovery data"
    );
    let reopened = MemoryStore::open(&source).await.unwrap();
    assert_eq!(reopened.count(Some(MemoryKind::Persona)).await, 1);
    assert_eq!(reopened.count(Some(MemoryKind::Viewer)).await, 0);
    assert_eq!(store.clear_kind(MemoryKind::Persona).await.unwrap(), 1);
    assert!(MemoryStore::open(&source).await.unwrap().is_empty().await);
    assert_eq!(store.clear_kind(MemoryKind::Persona).await.unwrap(), 0);
    drop(store);
    super::tests::remove_test_files(&source).await;
    tokio::fs::remove_file(unrelated).await.unwrap();
    // A nonempty directory would also expose leaked replacement files.
    tokio::fs::remove_dir(directory).await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn failed_replacement_preserves_disk_and_memory_and_allows_retry() {
    use std::os::windows::fs::OpenOptionsExt;

    let (directory, mut store) = populated_store().await;
    let source = directory.join("memory.jsonl");
    let original = tokio::fs::read(&source).await.unwrap();
    // Allow readers, but deny deletion/replacement while this handle is open.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&source)
        .unwrap();
    assert!(store.clear_kind(MemoryKind::Viewer).await.is_err());
    assert_eq!(tokio::fs::read(&source).await.unwrap(), original);
    assert_eq!(store.count(Some(MemoryKind::Viewer)).await, 1);
    assert_eq!(MemoryStore::open(&source).await.unwrap().len().await, 2);
    drop(held);
    assert_eq!(store.clear_kind(MemoryKind::Viewer).await.unwrap(), 1);
    assert_eq!(MemoryStore::open(&source).await.unwrap().len().await, 1);
    drop(store);
    super::tests::remove_test_files(&source).await;
    tokio::fs::remove_dir(directory).await.unwrap();
}
