use ai_ex_domain::{
    MemoryEntityOrigin, MemoryEntitySourceStatus, MemoryEntitySubject, MemoryRequest,
    MemoryResponse,
};
use ai_ex_memory::MemoryStore;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("memory-v2-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn path(&self) -> PathBuf {
        self.0.join("memory.jsonl")
    }
    fn write(&self, rows: &[Value]) {
        let content = rows
            .iter()
            .map(|row| serde_json::to_string(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(self.path(), content).unwrap();
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn memory(id: Uuid, profile: &str) -> Value {
    json!({"id":id,"profile_id":profile,"turn_id":Uuid::new_v4(),"created_ms":1,
        "kind":"conversation","user_text":"原始来源","assistant_text":"","source":"automatic","revision":1})
}
fn candidate(id: Uuid, profile: &str, subject: Value, value: &str) -> Value {
    json!({"schema_version":2,"record_type":"entity_candidate","candidate":{
        "id":id,"profile_id":profile,"subject":subject,
        "entity":{"kind":"preference","key":"preference:茶","name":"茶","value":value,"confidence":912},
        "first_mentioned_ms":2,"last_updated_ms":3,
        "legacy_record":{"id":"old-entity","metadata":{"confidence":0.912345,"value":value},"uri":"original://source"}}})
}
fn source(entity: Uuid, memory: Uuid, profile: &str) -> Value {
    json!({"schema_version":2,"record_type":"entity_source","source":{
        "entity_id":entity,"profile_id":profile,"legacy_memory_id":memory.to_string(),
        "memory_id":memory,"memory_revision":1,"status":"active"}})
}
async fn entities(store: &mut MemoryStore, profile: &str) -> ai_ex_domain::MemoryEntityPage {
    store.select_profile(profile).await.unwrap();
    let MemoryResponse::Entities(page) = store
        .manage(MemoryRequest::Entities {
            profile_id: profile.into(),
            query: String::new(),
            offset: 0,
            limit: 100,
        })
        .await
        .unwrap()
    else {
        panic!("entity response")
    };
    page
}

#[tokio::test]
async fn legacy_upgrade_backs_up_original_bytes_only_after_an_admitted_mutation() {
    let files = Files::new();
    let id = Uuid::new_v4();
    let mut old = memory(id, "default");
    old["embedding"] = json!({"space":"original-space","values":[1.0,2.0,3.0]});
    let original = format!("{}\r\n\r\n", serde_json::to_string(&old).unwrap());
    fs::write(files.path(), original.as_bytes()).unwrap();
    let mut store = MemoryStore::open(files.path()).await.unwrap();
    assert_eq!(fs::read(files.path()).unwrap(), original.as_bytes());
    assert!(store.format_backup_path().await.is_none());
    assert!(
        store
            .manage(MemoryRequest::Correct {
                profile_id: "default".into(),
                id,
                expected_revision: 99,
                text: "rejected".into(),
            })
            .await
            .is_err()
    );
    assert_eq!(fs::read(files.path()).unwrap(), original.as_bytes());
    assert!(store.format_backup_path().await.is_none());
    store
        .manage(MemoryRequest::Remember {
            profile_id: "default".into(),
            text: "new note".into(),
        })
        .await
        .unwrap();
    let backup = store.format_backup_path().await.unwrap();
    assert_eq!(fs::read(&backup).unwrap(), original.as_bytes());
    let upgraded: Vec<Value> = fs::read_to_string(files.path())
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(upgraded.len(), 2);
    assert!(
        upgraded
            .iter()
            .all(|row| row["record_type"] == "memory" && row["schema_version"] == 2)
    );
    assert_eq!(upgraded[0]["embedding"], old["embedding"]);
    assert_eq!(upgraded[0]["id"], old["id"]);
    assert_eq!(MemoryStore::open(backup).await.unwrap().len().await, 1);
    assert_eq!(
        MemoryStore::open(files.path()).await.unwrap().len().await,
        2
    );
}

#[tokio::test]
async fn imported_candidates_keep_subjects_sources_and_raw_metadata_across_restart() {
    let files = Files::new();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let unknown = Uuid::new_v4();
    let third_party = Uuid::new_v4();
    let other_profile = Uuid::new_v4();
    files.write(&[
        memory(a, "default"),
        memory(b, "default"),
        candidate(unknown, "default", json!({"type":"unknown"}), "喜欢绿茶"),
        source(unknown, a, "default"),
        source(unknown, b, "default"),
        candidate(
            third_party,
            "default",
            json!({"type":"third_party","id":"friend-1","name":"朋友"}),
            "喜欢红茶",
        ),
        source(third_party, a, "default"),
        candidate(
            other_profile,
            "private",
            json!({"type":"self_user"}),
            "私人候选",
        ),
    ]);
    let mut store = MemoryStore::open(files.path()).await.unwrap();
    let before = entities(&mut store, "default").await;
    assert_eq!(before.entities.len(), 2);
    assert!(
        before
            .entities
            .iter()
            .all(|item| item.origin == MemoryEntityOrigin::Imported)
    );
    assert!(
        before
            .entities
            .iter()
            .any(|item| item.subject == MemoryEntitySubject::Unknown)
    );
    assert!(
        before
            .entities
            .iter()
            .any(|item| matches!(item.subject, MemoryEntitySubject::ThirdParty { .. }))
    );
    store
        .manage(MemoryRequest::Correct {
            profile_id: "default".into(),
            id: a,
            expected_revision: 1,
            text: "我喜欢白茶".into(),
        })
        .await
        .unwrap();
    let updated = entities(&mut store, "default").await;
    let group = updated
        .entities
        .iter()
        .find(|item| item.key == format!("imported:{unknown}"))
        .unwrap();
    assert_eq!(group.current_value, "喜欢绿茶");
    assert_eq!(
        group
            .evidence
            .iter()
            .filter(|e| e.source_status == MemoryEntitySourceStatus::Active)
            .count(),
        1
    );
    assert_eq!(
        group
            .evidence
            .iter()
            .filter(|e| e.source_status == MemoryEntitySourceStatus::Corrected)
            .count(),
        1
    );
    let third = updated
        .entities
        .iter()
        .find(|item| item.key == format!("imported:{third_party}"))
        .unwrap();
    assert!(third.current_value.is_empty());
    store
        .manage(MemoryRequest::Forget {
            profile_id: "default".into(),
            id: b,
            expected_revision: 1,
        })
        .await
        .unwrap();
    drop(store);
    let mut restarted = MemoryStore::open(files.path()).await.unwrap();
    let after = entities(&mut restarted, "default").await;
    let group = after
        .entities
        .iter()
        .find(|item| item.key == format!("imported:{unknown}"))
        .unwrap();
    assert!(group.current_value.is_empty());
    assert_eq!(group.subject, MemoryEntitySubject::Unknown);
    assert!(
        group
            .evidence
            .iter()
            .any(|e| e.source_status == MemoryEntitySourceStatus::Forgotten)
    );
    let rows: Vec<Value> = fs::read_to_string(files.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let original = rows
        .iter()
        .find(|row| row["candidate"]["id"] == unknown.to_string())
        .unwrap();
    assert_eq!(
        original["candidate"]["legacy_record"]["metadata"]["confidence"],
        json!(0.912345)
    );
    assert_eq!(
        original["candidate"]["legacy_record"]["uri"],
        "original://source"
    );
    let export = files.0.join("export.jsonl");
    restarted.export_kind(None, &export).await.unwrap();
    let exported = fs::read_to_string(export).unwrap();
    let (_, report) = ai_ex_memory::migrate_jsonl(&exported).unwrap();
    assert_eq!(report.imported_entity_candidates, 2);
    assert!(!exported.contains("私人候选"));
    assert_eq!(entities(&mut restarted, "private").await.entities.len(), 1);
}

#[test]
fn explicit_upgrade_preserves_unknown_candidates_and_rejects_invalid_versioned_rows() {
    let id = Uuid::new_v4();
    let raw = candidate(id, "default", json!({"type":"unknown"}), "原始候选");
    let content = format!("{}\n", serde_json::to_string(&raw).unwrap());
    let (upgraded, report) = ai_ex_memory::migrate_jsonl(&content).unwrap();
    assert_eq!(report.imported_entity_candidates, 1);
    assert_eq!(report.memory_records, 0);
    assert!(!report.contains_legacy_records);
    assert_eq!(serde_json::from_str::<Value>(upgraded.trim()).unwrap(), raw);
    for invalid in [
        "{\"schema_version\":99,\"record_type\":\"memory\"}",
        "{\"schema_version\":2}",
        "{\"schema_version\":2,\"record_type\":\"unsupported\"}",
        "{\"schema_version\":2,\"schema_version\":2,\"record_type\":\"memory\"}",
    ] {
        assert!(ai_ex_memory::migrate_jsonl(invalid).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn missing_and_unresolved_sources_never_become_active_by_startup_reextraction() {
    let files = Files::new();
    let absent = Uuid::new_v4();
    let imported = Uuid::new_v4();
    let mut missing = source(imported, absent, "default");
    missing["source"]["status"] = json!("missing");
    missing["source"]["memory_revision"] = Value::Null;
    let unresolved = json!({"schema_version":2,"record_type":"entity_source","source":{
        "entity_id":imported,"profile_id":"default","legacy_memory_id":"opaque-source-id",
        "memory_id":null,"memory_revision":null,"status":"unresolved"}});
    files.write(&[
        memory(absent, "default"),
        candidate(imported, "default", json!({"type":"unknown"}), "保留候选"),
        missing,
        unresolved,
    ]);
    let mut store = MemoryStore::open(files.path()).await.unwrap();
    let page = entities(&mut store, "default").await;
    let group = page
        .entities
        .iter()
        .find(|group| group.origin == MemoryEntityOrigin::Imported)
        .unwrap();
    assert_eq!(group.subject, MemoryEntitySubject::Unknown);
    assert!(
        group
            .evidence
            .iter()
            .all(|evidence| evidence.source_status != MemoryEntitySourceStatus::Active)
    );
    store
        .manage(MemoryRequest::Remember {
            profile_id: "default".into(),
            text: "我喜欢新的饮料".into(),
        })
        .await
        .unwrap();
    drop(store);
    let mut store = MemoryStore::open(files.path()).await.unwrap();
    let page = entities(&mut store, "default").await;
    let group = page
        .entities
        .iter()
        .find(|group| group.key == format!("imported:{imported}"))
        .unwrap();
    assert_eq!(group.current_value, "保留候选");
    assert_eq!(
        group
            .evidence
            .iter()
            .map(|e| e.source_status)
            .collect::<Vec<_>>(),
        vec![
            MemoryEntitySourceStatus::Missing,
            MemoryEntitySourceStatus::Unresolved
        ]
    );
    let MemoryResponse::Page(evidence) = store
        .manage(MemoryRequest::Evidence {
            profile_id: "default".into(),
            key: group.key.clone(),
            offset: 0,
            limit: 20,
        })
        .await
        .unwrap()
    else {
        panic!("source memory page")
    };
    assert!(evidence.entries.is_empty());
}
