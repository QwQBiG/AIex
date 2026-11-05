use std::{fs, process::Command};

#[test]
fn flat_legacy_preview_matches_saved_config_without_creating_or_overwriting_files() {
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
    let config: toml::Value =
        toml::from_str(std::str::from_utf8(&preview.stdout).unwrap()).unwrap();
    assert_eq!(config["ollama"]["model"].as_str(), Some("example"));
    assert_eq!(
        config["ollama"]["base_url"].as_str(),
        Some("http://localhost:11434")
    );
    assert_eq!(config["vts"]["enabled"].as_bool(), Some(false));
    assert_eq!(config["audio"]["queue_capacity"].as_integer(), Some(10));
    assert_eq!(config["tts"]["timeout_seconds"].as_integer(), Some(10));
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

#[test]
fn nested_legacy_preview_keeps_old_precedence_without_writing_files() {
    let dir = std::env::temp_dir().join(format!("aiex-nested-preview-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let input = dir.join("旧配置.json");
    let source = br#"{"ollama":{"base_url":"http://127.0.0.1:11555","model":"nested"},"vts":{"port":9001},"ollama_model":"ignored","vts_port":"ignored-invalid-value"}"#;
    fs::write(&input, source).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"))
        .current_dir(&dir)
        .args(["--input", input.to_str().unwrap(), "--preview"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let config: toml::Value = toml::from_str(std::str::from_utf8(&result.stdout).unwrap()).unwrap();
    assert_eq!(config["ollama"]["model"].as_str(), Some("nested"));
    assert_eq!(config["vts"]["port"].as_integer(), Some(9001));
    let warnings = String::from_utf8(result.stderr).unwrap();
    assert!(warnings.contains("ollama_model"));
    assert!(warnings.contains("vts_port"));
    assert!(!warnings.contains("ignored-invalid-value"));
    assert_eq!(fs::read(&input).unwrap(), source);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    fs::remove_file(input).unwrap();
    fs::remove_dir(dir).unwrap();
}
