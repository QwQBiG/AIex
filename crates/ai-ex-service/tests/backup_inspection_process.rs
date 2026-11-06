#[path = "../src/backup_inspection_coordinator.rs"]
mod backup_inspection_coordinator;
#[allow(dead_code)]
#[path = "../src/backup_inspection_worker.rs"]
mod backup_inspection_worker;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use backup_inspection_coordinator::BackupInspector;
use backup_inspection_worker::Request;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn backup_inspection_real_service_worker_uses_only_selected_synthetic_file() {
    let directory = std::env::temp_dir().join(format!("backup-worker-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let path: PathBuf = directory.join("private-path-marker.jsonl");
    let original = format!(
        "{}\n",
        json!({
            "schema_version":2,"record_type":"memory","id":Uuid::new_v4(),
            "profile_id":"synthetic","turn_id":Uuid::new_v4(),"created_ms":1,
            "user_text":"private-text-marker","assistant_text":"private-answer-marker"
        })
    );
    fs::write(&path, &original).unwrap();
    let inspector =
        BackupInspector::new(PathBuf::from(env!("CARGO_BIN_EXE_ai-ex-service"))).unwrap();
    let request = |profile: &str| Request {
        path: path.to_string_lossy().into_owned(),
        profile: profile.into(),
        comparison_space: None,
    };
    let summary = inspector
        .inspect(request("synthetic"), Duration::from_secs(12))
        .await
        .unwrap();
    assert_eq!(summary.memory_records, 1);
    assert_eq!(summary.matching_embeddings, None);
    let text = serde_json::to_string(&summary).unwrap();
    assert!(!text.contains("private-path-marker"));
    assert!(!text.contains("private-text-marker"));
    let error = inspector
        .inspect(request("other"), Duration::from_secs(12))
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("private-path-marker"));
    assert!(!error.to_string().contains("private-text-marker"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    inspector.shutdown().await.unwrap();
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}
