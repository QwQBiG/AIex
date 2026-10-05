use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn validates_without_opening_models_memory_or_credentials() {
    let directory = std::env::temp_dir().join(format!(
        "aiex-offline-validation-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&directory).unwrap();
    let config = directory.join("config.toml");
    fs::write(&config, "[model]\nbackend = 'deepseek'\n[memory]\nenabled = true\npath = 'must-not-be-created.jsonl'\n[control]\nenabled = true\ntoken_path = 'missing-token'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ai-ex-service"))
        .current_dir(&directory)
        .args(["--config", "config.toml", "--validate-config"])
        .env_remove("DEEPSEEK_API_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("offline"));
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    fs::write(&config, "[model]\nbackend = 'synthetic-private-marker'\n").unwrap();
    let invalid = Command::new(env!("CARGO_BIN_EXE_ai-ex-service"))
        .current_dir(&directory)
        .args(["--config", "config.toml", "--validate-config"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("synthetic-private-marker"));
    fs::remove_file(config).unwrap();
    fs::remove_dir(directory).unwrap();
}
