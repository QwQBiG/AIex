use std::{fs, process::Command};

#[test]
fn preview_matches_saved_config_without_creating_or_overwriting_files() {
    let dir = std::env::temp_dir().join(format!("aiex-config-preview-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let input = dir.join("旧配置.json");
    let output = dir.join("new.toml");
    let source = br#"{"ollama_model":"example","unused":"private-value"}"#;
    fs::write(&input, source).unwrap();
    let executable = env!("CARGO_BIN_EXE_ai-ex-migrate");
    let preview = Command::new(executable)
        .current_dir(&dir)
        .args(["--input", input.to_str().unwrap(), "--preview"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    assert_eq!(fs::read(&input).unwrap(), source);
    let warnings = String::from_utf8(preview.stderr).unwrap();
    assert!(warnings.contains("unmapped legacy field \"unused\""));
    assert!(!warnings.contains("private-value"));

    let save = Command::new(executable)
        .current_dir(&dir)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(save.status.success());
    assert_eq!(fs::read(&output).unwrap(), preview.stdout);
    let conflict = Command::new(executable)
        .current_dir(&dir)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--preview",
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!conflict.status.success());
    assert_eq!(fs::read(&output).unwrap(), preview.stdout);
    fs::write(&input, br#"{"ollama_model":"one","ollama_model":"two"}"#).unwrap();
    let duplicate = Command::new(executable)
        .current_dir(&dir)
        .args(["--input", input.to_str().unwrap(), "--preview"])
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert_eq!(fs::read(&output).unwrap(), preview.stdout);
    fs::remove_file(input).unwrap();
    fs::remove_file(output).unwrap();
    fs::remove_dir(dir).unwrap();
}
