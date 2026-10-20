use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use ai_ex_domain::TurnId;
use ai_ex_memory::MemoryStore;
use serde_json::{Value, json};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("aiex-maintenance-cli-{}", TurnId::new().0));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    fn run(&self, options: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ai-ex-service"))
            .current_dir(&self.0)
            .args(["--config", "config.toml", "--maintain-media"])
            .args(options)
            .env_remove("DEEPSEEK_API_KEY")
            .output()
            .unwrap()
    }
    fn config(&self, enabled: bool) {
        fs::write(self.0.join("config.toml"), format!(
            "[model]\nbackend='deepseek'\n[memory]\nenabled={enabled}\npath='memories.jsonl'\n[memory.embedding]\nbase_url='http://127.0.0.1:1'\nmodel='must-not-request'\nspace='synthetic'\ntimeout_seconds=1\n[control]\nenabled=true\ntoken_path='must-not-read-token'\n"
        )).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Only these exact files and the unique empty directory belong to this test.
        for name in [
            "config.toml",
            "memories.jsonl",
            "memories.jsonl.lock",
            "archive.jsonl",
        ] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}
fn media(source: &str) -> Value {
    json!({"id":TurnId::new().0,"turn_id":TurnId::new(),"profile_id":"default","created_ms":1,"kind":"conversation","user_text":"synthetic old observation","assistant_text":"","source":source,"revision":1,"knowledge":{"media":{"modality":"text","importance":800,"tags":["fixture"],"metadata":{}}},"legacy_metadata":{"access_count":0,"original":"synthetic"}})
}
#[tokio::test]
async fn preview_then_apply_runs_offline_and_archives_before_removal() {
    let f = Fixture::new();
    f.config(true);
    let automatic = media("automatic");
    let note = media("user_note");
    let original = format!("{automatic}\n{note}\n");
    fs::write(f.0.join("memories.jsonl"), &original).unwrap();
    let preview = f.run(&[]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let report: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(report["archived"], 1);
    assert_eq!(report["protected_confirmed"], 1);
    assert_eq!(report["applied"], false);
    assert_eq!(
        fs::read_to_string(f.0.join("memories.jsonl")).unwrap(),
        original
    );
    assert!(!f.0.join("memories.jsonl.lock").exists());
    let missing = f.run(&["--apply-media-maintenance"]);
    assert!(!missing.status.success());
    assert_eq!(
        fs::read_to_string(f.0.join("memories.jsonl")).unwrap(),
        original
    );
    let applied = f.run(&[
        "--apply-media-maintenance",
        "--archive-memory",
        "archive.jsonl",
    ]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let report: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["applied"], true);
    let backup: Value =
        serde_json::from_str(&fs::read_to_string(f.0.join("archive.jsonl")).unwrap()).unwrap();
    assert_eq!(backup["id"], automatic["id"]);
    assert_eq!(
        backup["knowledge"]["media"],
        automatic["knowledge"]["media"]
    );
    assert_eq!(backup["legacy_metadata"], automatic["legacy_metadata"]);
    let remaining: Value =
        serde_json::from_str(&fs::read_to_string(f.0.join("memories.jsonl")).unwrap()).unwrap();
    assert_eq!(remaining["id"], note["id"]);
    assert_eq!(remaining["knowledge"]["media"], note["knowledge"]["media"]);
    drop(MemoryStore::open(f.0.join("memories.jsonl")).await.unwrap());
    drop(MemoryStore::open(f.0.join("archive.jsonl")).await.unwrap());
    assert!(!f.0.join("must-not-read-token").exists());
}
#[test]
fn disabled_memory_and_existing_archive_leave_original_data_unchanged() {
    let f = Fixture::new();
    f.config(false);
    let original = format!("{}\n", media("automatic"));
    fs::write(f.0.join("memories.jsonl"), &original).unwrap();
    assert!(!f.run(&[]).status.success());
    assert!(!f.0.join("memories.jsonl.lock").exists());
    f.config(true);
    fs::write(f.0.join("archive.jsonl"), "sentinel").unwrap();
    let result = f.run(&[
        "--apply-media-maintenance",
        "--archive-memory",
        "archive.jsonl",
    ]);
    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(f.0.join("memories.jsonl")).unwrap(),
        original
    );
    assert_eq!(
        fs::read_to_string(f.0.join("archive.jsonl")).unwrap(),
        "sentinel"
    );
}
