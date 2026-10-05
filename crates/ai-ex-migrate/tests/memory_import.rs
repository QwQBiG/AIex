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
            .args(["--profile", "character-a", "--utc-offset-minutes", "480"])
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
