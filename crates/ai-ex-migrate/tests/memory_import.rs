use std::fs;
use std::process::Command;

use ai_ex_domain::{MemoryKind, MemorySource};
use ai_ex_memory::MemoryStore;

#[tokio::test]
async fn imports_a_frozen_legacy_backup_that_the_rust_store_can_reopen_and_recall() {
    let directory =
        std::env::temp_dir().join(format!("aiex-memory-import-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let input = directory.join("backup.json.gz");
    let output = directory.join("memory.jsonl");
    let fixture = include_bytes!("fixtures/legacy-memory.json");
    {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(
            fs::File::create(&input).unwrap(),
            flate2::Compression::default(),
        );
        encoder.write_all(fixture).unwrap();
        encoder.finish().unwrap();
    }
    let original = fs::read(&input).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"))
            .args(["--memory-backup", "--input"])
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .args([
                "--profile",
                "character-a",
                "--utc-offset-minutes",
                "480",
                "--embedding-space",
                "legacy-two-dimensional-test",
            ])
            .output()
            .unwrap()
    };
    let result = run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(&input).unwrap(), original);
    let imported = fs::read(&output).unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&imported).unwrap();
    let source: serde_json::Value = serde_json::from_slice(fixture).unwrap();
    assert_eq!(
        payload["legacy_metadata"],
        source["memories"][0]["metadata"]
    );
    assert_eq!(payload["embedding"]["space"], "legacy-two-dimensional-test");
    let vector: Vec<f32> = serde_json::from_value(payload["embedding"]["values"].clone()).unwrap();
    assert_eq!(vector, vec![0.0, 1e-5]);
    let entry: ai_ex_domain::MemoryEntry = serde_json::from_slice(&imported).unwrap();
    assert_eq!(entry.created_ms, 123);
    assert_eq!(entry.source, MemorySource::Automatic);
    let store = MemoryStore::open(&output).await.unwrap();
    assert_eq!(store.len().await, 0);
    store.select_profile("character-a").await.unwrap();
    assert_eq!(store.len().await, 1);
    let recalled = store
        .recall_kind(Some(MemoryKind::Conversation), "茶", 5)
        .await
        .unwrap();
    assert_eq!(recalled.len(), 1);
    assert!(recalled[0].content.contains("茶"));
    assert!(recalled[0].content.contains("好"));
    assert!(!run().status.success());
    assert_eq!(fs::read(&output).unwrap(), imported);
    assert_eq!(fs::read(&input).unwrap(), original);
    fs::remove_file(&input).unwrap();
    fs::remove_file(&output).unwrap();
    fs::remove_dir(&directory).unwrap();
}

#[test]
fn cli_preserves_advanced_metadata_and_requires_explicit_vector_space() {
    let directory =
        std::env::temp_dir().join(format!("aiex-advanced-import-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let input = directory.join("backup.json");
    let output = directory.join("memory.jsonl");
    let fixture = include_bytes!("fixtures/legacy-memory-advanced.json");
    fs::write(&input, fixture).unwrap();
    let mut missing_space = Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"));
    missing_space
        .args(["--memory-backup", "--input"])
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .args(["--profile", "character-a"]);
    let rejected = missing_space.output().unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("--embedding-space"));
    assert!(!output.exists());
    assert_eq!(fs::read(&input).unwrap(), fixture);

    let result = missing_space
        .args(["--embedding-space", "synthetic-three-dimensional-v1"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let source: serde_json::Value = serde_json::from_slice(fixture).unwrap();
    let imported: serde_json::Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(
        imported["embedding"],
        serde_json::json!({
            "space": "synthetic-three-dimensional-v1",
            "values": [0.25, -0.5, 1.0],
        })
    );
    assert_eq!(
        imported["legacy_metadata"],
        source["memories"][0]["metadata"]
    );
    assert_eq!(imported["source"], "automatic");
    assert_eq!(imported["created_ms"], 123);
    assert!(imported.get("entry").is_none());
    assert_eq!(fs::read(&input).unwrap(), fixture);
    fs::remove_file(&input).unwrap();
    fs::remove_file(&output).unwrap();
    fs::remove_dir(&directory).unwrap();
}

#[test]
fn cli_accepts_an_unembedded_backup_without_new_options() {
    let directory =
        std::env::temp_dir().join(format!("aiex-unembedded-import-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let input = directory.join("backup.json");
    let output = directory.join("memory.jsonl");
    let fixture = include_bytes!("fixtures/legacy-memory-unembedded.json");
    fs::write(&input, fixture).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"))
        .args(["--memory-backup", "--input"])
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .args(["--profile", "character-a"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let source: serde_json::Value = serde_json::from_slice(fixture).unwrap();
    let imported: serde_json::Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert!(imported.get("embedding").is_none());
    assert_eq!(
        imported["legacy_metadata"],
        source["memories"][0]["metadata"]
    );
    assert_eq!(imported["user_text"], "Tea");
    assert_eq!(imported["source"], "automatic");
    assert_eq!(fs::read(&input).unwrap(), fixture);
    fs::remove_file(&input).unwrap();
    fs::remove_file(&output).unwrap();
    fs::remove_dir(&directory).unwrap();
}
