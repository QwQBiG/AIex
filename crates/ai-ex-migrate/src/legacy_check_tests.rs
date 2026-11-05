use super::*;

#[test]
fn checks_flat_and_nested_fields_without_reporting_values() {
    let secret = "private-audio-secret-123";
    let flat = format!(
        r#"{{"log_level":"INFO","default_emotion":"happy","expression_timeout":0.5,"emotion_hotkey_map":{{"happy":"{secret}"}},"sovits_ref_audio_path":"{secret}"}}"#
    );
    let report = inspect(flat.as_bytes()).unwrap();
    assert_eq!(report["status"], "selected_fields_ok");
    assert_eq!(report["format"], "flat");
    assert!(!report.to_string().contains(secret));

    let nested = br#"{"ollama":{},"logging":{"level":"WARNING"},"log_level":"INVALID","emotion_hotkey_map":null}"#;
    let report = inspect(nested).unwrap();
    assert_eq!(report["status"], "selected_fields_ok");
    assert_eq!(report["format"], "nested");
}

#[test]
fn reports_invalid_selected_fields_without_echoing_untrusted_keys() {
    let secret = "private-key-never-display";
    let input = format!(
        r#"{{"log_level":"{secret}","default_emotion":"{secret}","expression_timeout":0,"emotion_hotkey_map":{{"{secret}":"private"}}}}"#
    );
    let report = inspect(input.as_bytes()).unwrap();
    assert_eq!(report["status"], "selected_fields_invalid");
    assert_eq!(report["issues"].as_array().unwrap().len(), 4);
    assert!(!report.to_string().contains(secret));
    let nested = inspect(br#"{"ollama":{},"logging":"private"}"#).unwrap();
    assert_eq!(nested["issues"][0]["field"], "logging");
    assert!(!nested.to_string().contains("private"));
}

#[test]
fn rejects_invalid_json_oversized_input_and_unsafe_options() {
    for bytes in [&b"not-json private"[..], &b"[]"[..]] {
        assert!(inspect(bytes).is_err());
    }
    assert!(inspect(&vec![b' '; ai_ex_config::MAX_CONFIG_BYTES + 1]).is_err());
    assert!(parse_input(vec!["--input".into(), "x".into()]).is_ok());
    for args in [
        vec![],
        vec!["--input".into(), "--output".into()],
        vec!["--input".into(), "x".into(), "--output".into(), "y".into()],
        vec!["--input".into(), "x".into(), "--input".into(), "y".into()],
    ] {
        assert!(parse_input(args).is_err());
    }
}

#[tokio::test]
async fn file_check_is_read_only_and_errors_do_not_echo_path() {
    let input = std::env::temp_dir().join(format!("legacy-check-{}.json", uuid::Uuid::new_v4()));
    let contents = br#"{"log_level":"INFO"}"#;
    tokio::fs::write(&input, contents).await.unwrap();
    run(vec!["--input".into(), input.to_string_lossy().into_owned()])
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&input).await.unwrap(), contents);
    tokio::fs::remove_file(&input).await.unwrap();
    let error = run(vec!["--input".into(), input.to_string_lossy().into_owned()])
        .await
        .unwrap_err();
    assert!(!error.to_string().contains(input.to_string_lossy().as_ref()));
}
