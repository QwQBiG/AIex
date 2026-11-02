use super::*;
use serde_json::{Value, json};

fn specification() -> Value {
    let root = std::env::current_dir().unwrap();
    json!({
        "schema_version": 1, "executable": root.join("worker.exe"), "arguments": [],
        "job_directory": root, "ready_timeout_ms": 1000,
        "models": [{"manifest": root.join("model.json"), "directory": root}],
        "steps": [{"timeout_ms": 1000, "request": {
            "identity": {"session_id": "00000000-0000-0000-0000-000000000001",
                "request_id": "00000000-0000-0000-0000-000000000002", "generation": 1},
            "model_id": "bge-small-zh", "task": {"type": "embed", "text": "你好", "space": "fixed-space"}
        }}]
    })
}

fn validate(value: Value) -> Result<(), String> {
    serde_json::from_value::<ProbeSpec>(value)
        .map_err(|error| error.to_string())?
        .validate()
}

#[test]
fn accepts_complete_probe_and_rejects_relative_paths() {
    assert!(validate(specification()).is_ok());
    let mut value = specification();
    value["models"][0]["directory"] = json!("relative/model");
    assert!(validate(value).is_err());
}

#[test]
fn cancellation_must_end_sequence_and_fit_deadline() {
    let mut value = specification();
    value["steps"][0]["cancel_after_ms"] = json!(50);
    assert!(validate(value.clone()).is_ok());
    value["steps"][0]["cancel_after_ms"] = json!(1000);
    assert!(validate(value.clone()).is_err());
    value["steps"][0]["cancel_after_ms"] = json!(50);
    let mut second = value["steps"][0].clone();
    second["request"]["identity"]["request_id"] = json!("00000000-0000-0000-0000-000000000003");
    value["steps"].as_array_mut().unwrap().push(second);
    assert!(validate(value).is_err());
}

#[test]
fn rejects_duplicate_request_ids_unknown_fields_and_unbounded_waits() {
    let mut value = specification();
    let second = value["steps"][0].clone();
    value["steps"].as_array_mut().unwrap().push(second);
    assert!(validate(value).is_err());
    let mut value = specification();
    value["ready_timeout_ms"] = json!(600001);
    assert!(validate(value).is_err());
    let mut value = specification();
    value["environment_override_typo"] = json!({});
    assert!(validate(value).is_err());
}
