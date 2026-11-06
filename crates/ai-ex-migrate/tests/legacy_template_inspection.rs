use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!("template-check-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    path
}

fn inspect(path: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"))
        .args(["--inspect-legacy-templates", "--input"])
        .arg(path)
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn legacy_template_inspection_counts_without_disclosing_or_modifying_source() {
    let directory = directory();
    let path = directory.join("private-path-marker.json");
    let source = serde_json::json!({
        "timestamp": 1,
        "private-root-field": "private-value",
        "templates": [
            {"template_id":"private-id", "game_name":"private-game", "type":"ui",
             "data":{"private-action":"private-content"}, "confidence":0.8,
             "adaptive_offset":[-1.5,2], "private-entry-field":true},
            {"template_id":"private-id", "game_name":"another", "type":"action",
             "data":{}, "adaptive_offset":[0,0]},
            {"template_id":"three", "game_name":"another", "type":"coordinate",
             "data":{}, "adaptive_offset":[0]},
            {"template_id":"four", "game_name":"another", "type":"ui", "data":{}},
            {"template_id":3, "game_name":"another", "type":"ui", "data":{}},
            null
        ]
    });
    let bytes = serde_json::to_vec(&source).unwrap();
    fs::write(&path, &bytes).unwrap();
    let output = inspect(&path, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["templates"], 6);
    assert_eq!(report["invalid_entries"], 2);
    assert_eq!(report["duplicate_ids"], 1);
    assert_eq!(report["unknown_root_fields"], 1);
    assert_eq!(report["unknown_entry_fields"], 1);
    assert_eq!(report["opaque_data_objects"], 5);
    assert_eq!(report["offsets_nonzero"], 1);
    assert_eq!(report["offsets_invalid"], 1);
    assert_eq!(report["offsets_absent"], 2);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!text.contains("private-"));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[test]
fn legacy_template_inspection_rejects_bad_input_and_options_without_disclosure() {
    let directory = directory();
    let path = directory.join("private-path-marker.json");
    for invalid in [
        b"{\"private-data\":".to_vec(),
        br#"{"templates":{}}"#.to_vec(),
        vec![b' '; 4 * 1024 * 1024 + 1],
        serde_json::to_vec(
            &serde_json::json!({"templates": vec![serde_json::Value::Null; 10_001]}),
        )
        .unwrap(),
    ] {
        fs::write(&path, &invalid).unwrap();
        let output = inspect(&path, &[]);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-"));
        assert_eq!(fs::read(&path).unwrap(), invalid);
    }
    fs::write(&path, br#"{"templates":[]}"#).unwrap();
    for extra in [
        &["--output", "private-output"][..],
        &["--inspect-legacy-templates"][..],
    ] {
        let output = inspect(&path, extra);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-"));
    }
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}
