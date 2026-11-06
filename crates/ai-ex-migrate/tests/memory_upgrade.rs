use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};
use uuid::Uuid;

struct Files(PathBuf);
impl Files {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("memory-upgrade-cli-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"));
        command
            .current_dir(&self.0)
            .args(["--upgrade-memory", "--input", "old.jsonl"]);
        command
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        for name in ["old.jsonl", "new.jsonl"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        fs::remove_dir(&self.0).unwrap();
    }
}

#[test]
fn preview_is_read_only_and_new_output_rejects_overwrite() {
    let files = Files::new();
    let old = json!({"id":Uuid::new_v4(), "turn_id":Uuid::new_v4(), "profile_id":"角色一号",
        "created_ms":100, "kind":"conversation", "source":"automatic", "revision":1,
        "user_text":"原始内容", "assistant_text":"原始回答",
        "embedding":{"space":"old-model-v1","values":[0.125,0.25]}});
    let original = format!("{}\r\n", serde_json::to_string(&old).unwrap());
    fs::write(files.0.join("old.jsonl"), &original).unwrap();
    let preview = files.command().arg("--preview").output().unwrap();
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let report: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(report["output_schema_version"], 2);
    assert_eq!(report["contains_legacy_records"], true);
    assert_eq!(fs::read_dir(&files.0).unwrap().count(), 1);
    let output = files
        .command()
        .args(["--output", "new.jsonl"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let converted = fs::read(files.0.join("new.jsonl")).unwrap();
    let row: Value = serde_json::from_slice(&converted).unwrap();
    assert_eq!(row["record_type"], "memory");
    assert_eq!(row["id"], old["id"]);
    assert_eq!(row["profile_id"], old["profile_id"]);
    assert_eq!(row["embedding"], old["embedding"]);
    let rejected = files
        .command()
        .args(["--output", "new.jsonl"])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("refusing to overwrite"));
    assert_eq!(fs::read(files.0.join("new.jsonl")).unwrap(), converted);
    assert_eq!(
        fs::read(files.0.join("old.jsonl")).unwrap(),
        original.as_bytes()
    );
    for options in [
        vec![],
        vec!["--preview", "--output", "other.jsonl"],
        vec!["--preview", "--chroma-export"],
    ] {
        let rejected = files.command().args(options).output().unwrap();
        assert!(!rejected.status.success());
    }
    assert_eq!(fs::read_dir(&files.0).unwrap().count(), 2);
}

#[test]
fn preview_reports_saved_entities_that_cannot_be_safely_rewritten() {
    let files = Files::new();
    let old = json!({"id":Uuid::new_v4(), "turn_id":Uuid::new_v4(), "created_ms":100,
    "user_text":"neutral", "assistant_text":"", "knowledge":{"entities":[{
        "kind":"fact", "key":"synthetic-key", "name":"synthetic-name",
        "value":"synthetic-value", "confidence":800
    }]}});
    let original = format!("{old}\n");
    fs::write(files.0.join("old.jsonl"), &original).unwrap();
    let preview = files.command().arg("--preview").output().unwrap();
    assert!(preview.status.success());
    let report: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(report["changed_saved_entity_records"], 1);
    let output = files
        .command()
        .args(["--output", "new.jsonl"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!files.0.join("new.jsonl").exists());
    assert_eq!(
        fs::read_to_string(files.0.join("old.jsonl")).unwrap(),
        original
    );
}
