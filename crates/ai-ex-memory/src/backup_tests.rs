use super::*;
use crate::{MemoryKind, TurnId};
use ai_ex_domain::{MemoryBackupState, MemoryRequest, MemoryResponse};
use serde_json::{Value, json};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("memory-backup-test-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn source(&self) -> PathBuf {
        self.0.join("memory.jsonl")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn backup_preserves_current_profile_provenance_and_embedding() {
    let fixture = Fixture::new();
    let memory_id = Uuid::new_v4();
    let entity_id = Uuid::new_v4();
    let rows = [
        json!({"schema_version":2,"record_type":"memory","id":memory_id,"profile_id":"a",
            "turn_id":Uuid::new_v4(),"created_ms":1,"user_text":"茶","assistant_text":"",
            "embedding":{"space":"test-space","values":[1.0,2.0]},
            "legacy_metadata":{"user_input":"茶","source":"original"}}),
        json!({"schema_version":2,"record_type":"memory","id":Uuid::new_v4(),"profile_id":"b",
            "turn_id":Uuid::new_v4(),"created_ms":1,"user_text":"private-other","assistant_text":""}),
        json!({"schema_version":2,"record_type":"entity_candidate","candidate":{
            "id":entity_id,"profile_id":"a","subject":{"type":"unknown"},
            "entity":{"kind":"preference","key":"preference:茶","name":"茶","value":"喜欢茶","confidence":912},
            "first_mentioned_ms":2,"last_updated_ms":3,"legacy_record":{"id":"old-entity"}}}),
        json!({"schema_version":2,"record_type":"entity_source","source":{
            "entity_id":entity_id,"profile_id":"a","legacy_memory_id":memory_id.to_string(),
            "memory_id":memory_id,"memory_revision":1,"status":"active"}}),
    ];
    let original = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(fixture.source(), original.as_bytes()).unwrap();
    let store = MemoryStore::open(fixture.source()).await.unwrap();
    store.select_profile("a").await.unwrap();
    let result = store.backup_snapshot("a", Uuid::new_v4()).await.unwrap();
    assert_eq!(result.memory_count, 1);
    assert!(!result.temporary_retained);
    assert!(result.path.is_absolute());
    let exported = fs::read_to_string(&result.path).unwrap();
    assert!(!exported.contains("private-other"));
    let output = exported
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(output.len(), 3);
    assert_eq!(output[0]["embedding"]["space"], "test-space");
    assert_eq!(output[0]["legacy_metadata"]["source"], "original");
    assert_eq!(output[1]["record_type"], "entity_candidate");
    assert_eq!(output[2]["record_type"], "entity_source");
    let reopened = MemoryStore::open(&result.path).await.unwrap();
    reopened.select_profile("a").await.unwrap();
    assert_eq!(reopened.len().await, 1);
    assert_eq!(fs::read_to_string(fixture.source()).unwrap(), original);
}

#[tokio::test]
async fn backup_refuses_other_writer_then_refreshes_stale_snapshot() {
    let fixture = Fixture::new();
    fs::write(fixture.source(), "").unwrap();
    let reader = MemoryStore::open(fixture.source()).await.unwrap();
    let mut writer = MemoryStore::open(fixture.source()).await.unwrap();
    writer
        .remember_kind(
            MemoryKind::Persona,
            TurnId::new(),
            "latest".into(),
            "".into(),
        )
        .await
        .unwrap();
    assert!(reader.is_empty().await);
    let id = Uuid::new_v4();
    assert!(reader.backup_snapshot("default", id).await.is_err());
    let (temporary, destination) = backup_paths(&fixture.source(), id).unwrap();
    assert!(!temporary.exists() && !destination.exists());
    drop(writer);
    let backup = reader.backup_snapshot("default", id).await.unwrap();
    assert_eq!(backup.memory_count, 1);
    assert!(fs::read_to_string(backup.path).unwrap().contains("latest"));
    assert!(reader.is_empty().await);
}

#[tokio::test]
async fn backup_rejects_invalid_scopes_missing_storage_and_existing_files() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let empty = MemoryStore::open(&source).await.unwrap();
    assert!(
        empty
            .backup_snapshot("default", Uuid::new_v4())
            .await
            .is_err()
    );
    assert!(!source.exists());
    assert!(
        MemoryStore::disabled()
            .backup_snapshot("default", Uuid::new_v4())
            .await
            .is_err()
    );
    fs::write(&source, "").unwrap();
    let store = MemoryStore::open(&source).await.unwrap();
    assert!(
        store
            .backup_snapshot("other", Uuid::new_v4())
            .await
            .is_err()
    );
    assert!(store.backup_snapshot("default", Uuid::nil()).await.is_err());
    let id = Uuid::new_v4();
    let (temporary, destination) = backup_paths(&source, id).unwrap();
    fs::write(&destination, "existing backup").unwrap();
    assert!(store.backup_snapshot("default", id).await.is_err());
    assert_eq!(fs::read_to_string(&destination).unwrap(), "existing backup");
    assert!(!temporary.exists());
    fs::write(&temporary, "existing partial").unwrap();
    assert!(store.backup_snapshot("default", id).await.is_err());
    assert_eq!(fs::read_to_string(&temporary).unwrap(), "existing partial");
    assert_eq!(fs::read_to_string(&destination).unwrap(), "existing backup");
}

#[tokio::test]
async fn managed_backup_can_be_queried_without_replaying_and_restart_is_unknown() {
    let fixture = Fixture::new();
    fs::write(fixture.source(), "").unwrap();
    let mut store = MemoryStore::open(fixture.source()).await.unwrap();
    store
        .remember_kind(
            MemoryKind::Persona,
            TurnId::new(),
            "initial".into(),
            "".into(),
        )
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let request = MemoryRequest::Backup {
        profile_id: "default".into(),
        backup_id: id,
    };
    assert!(!request.is_mutation());
    let first = store.manage(request.clone()).await.unwrap();
    let MemoryResponse::BackupStatus(status) = &first else {
        panic!("backup status expected")
    };
    let MemoryBackupState::Completed {
        path, memory_count, ..
    } = &status.state
    else {
        panic!("completed backup expected")
    };
    assert_eq!(*memory_count, 1);
    let original_backup = fs::read(path).unwrap();
    store
        .remember_kind(
            MemoryKind::Persona,
            TurnId::new(),
            "later".into(),
            "".into(),
        )
        .await
        .unwrap();
    assert_eq!(store.manage(request).await.unwrap(), first);
    assert_eq!(fs::read(path).unwrap(), original_backup);
    assert_eq!(
        store
            .manage(MemoryRequest::BackupStatus {
                profile_id: "default".into(),
                backup_id: id
            })
            .await
            .unwrap(),
        first
    );
    store.select_profile("other").await.unwrap();
    assert!(
        matches!(store.manage(MemoryRequest::BackupStatus { profile_id: "other".into(), backup_id: id }).await.unwrap(),
        MemoryResponse::BackupStatus(status) if matches!(status.state, MemoryBackupState::Unknown))
    );
    assert!(
        store
            .manage(MemoryRequest::Backup {
                profile_id: "other".into(),
                backup_id: id
            })
            .await
            .is_err()
    );
    drop(store);
    let mut restarted = MemoryStore::open(fixture.source()).await.unwrap();
    assert!(
        matches!(restarted.manage(MemoryRequest::BackupStatus { profile_id: "default".into(), backup_id: id }).await.unwrap(),
        MemoryResponse::BackupStatus(status) if matches!(status.state, MemoryBackupState::Unknown))
    );
    assert_eq!(fs::read(path).unwrap(), original_backup);
}

#[tokio::test]
async fn managed_backup_rejects_disabled_storage_and_nonrandom_id() {
    let mut disabled = MemoryStore::disabled();
    assert!(
        disabled
            .manage(MemoryRequest::Backup {
                profile_id: "default".into(),
                backup_id: Uuid::new_v4()
            })
            .await
            .is_err()
    );
    assert!(
        disabled
            .manage(MemoryRequest::Backup {
                profile_id: "default".into(),
                backup_id: Uuid::nil()
            })
            .await
            .is_err()
    );
}
