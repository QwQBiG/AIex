use super::*;
use serde_json::json;

fn backup(records: Vec<Value>) -> Vec<u8> {
    let raw = RawValue::from_string(serde_json::to_string(&records).unwrap()).unwrap();
    let mut canonical = String::new();
    canonical_json(&raw, &mut canonical, 0).unwrap();
    let hash = digest(&SHA256, canonical.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    serde_json::to_vec(&json!({
        "schema_version": "1.0",
        "total_memories": records.len(),
        "backup_integrity_hash": hash,
        "memories": records,
    }))
    .unwrap()
}

fn record(id: u128, embedding: Value) -> Value {
    json!({
        "id": Uuid::from_u128(id),
        "content": "User: I like tea.\nAI: Remembered.",
        "metadata": {
            "timestamp": "2026-10-05T12:00:00Z",
            "memory_type": "INTERACTION",
            "user_input": "I like tea.",
            "ai_response": "Remembered.",
            "importance_score": 0.8,
            "access_count": 3,
            "entities": ["like_tea"],
            "arbitrary": {
                "nested": [null, false, {"emoji": "\u{1f375}", "quoted": "\\\"\n"}],
                "large_integer": 9007199254740993_u64,
            },
            "source": "user_correction",
            "validated": true,
        },
        "embedding": embedding,
    })
}

#[test]
fn preserves_embedding_space_values_and_complete_legacy_metadata() {
    let original = record(1, json!([0.25, -0.5, 1.0]));
    let input = backup(vec![original.clone()]);
    let output = convert_backup(&input, "a", None, Some("synthetic-three-v1")).unwrap();
    let result: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(result["embedding"]["space"], "synthetic-three-v1");
    assert_eq!(result["embedding"]["values"], json!([0.25, -0.5, 1.0]));
    assert_eq!(result["legacy_metadata"], original["metadata"]);
    assert_eq!(result["profile_id"], "a");
    assert_eq!(result["source"], "automatic");
    assert!(result.get("entry").is_none());
    let entry: MemoryEntry = serde_json::from_slice(&output).unwrap();
    assert_eq!(entry.source, MemorySource::Automatic);
    assert_eq!(entry.user_text, "I like tea.");
    assert_eq!(
        output,
        convert_backup(&input, "a", None, Some("synthetic-three-v1")).unwrap()
    );
}

#[test]
fn old_unembedded_backups_remain_compatible_without_a_space() {
    let mut missing = record(1, Value::Null);
    missing.as_object_mut().unwrap().remove("embedding");
    let input = backup(vec![missing, record(2, Value::Null), record(3, json!([]))]);
    let output = convert_backup(&input, "a", None, None).unwrap();
    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| line.get("embedding").is_none()));
    assert!(
        lines
            .iter()
            .all(|line| line["legacy_metadata"]["importance_score"] == 0.8)
    );
}

#[test]
fn requires_an_explicit_space_for_present_vectors() {
    let input = backup(vec![record(1, json!([1.0, 0.0]))]);
    let error = convert_backup(&input, "a", None, None).unwrap_err();
    assert!(error.to_string().contains("--embedding-space"));
    for invalid in ["", " ", "\n"] {
        assert!(convert_backup(&input, "a", None, Some(invalid)).is_err());
    }
    assert!(convert_backup(&input, "a", None, Some(&"x".repeat(257))).is_err());
    // The legacy model name is not guessed or hard-coded to 384 dimensions.
    assert!(convert_backup(&input, "a", None, Some("all-MiniLM-L6-v2")).is_ok());
}

#[test]
fn rejects_invalid_vectors_and_inconsistent_dimensions() {
    for embedding in [
        json!("not an array"),
        json!([true, 1]),
        json!([null, 1]),
        json!(["1", 1]),
        json!([0.0, -0.0]),
        json!([1e39, 1.0]),
        json!([1e-300]),
        json!(vec![1.0; MAX_EMBEDDING_DIMENSIONS + 1]),
    ] {
        assert!(
            convert_backup(&backup(vec![record(1, embedding)]), "a", None, Some("test")).is_err()
        );
    }
    let mixed = backup(vec![
        record(1, json!([0.0, 1.0])),
        record(2, Value::Null),
        record(3, json!([1.0, 0.0, 0.0])),
    ]);
    let error = convert_backup(&mixed, "a", None, Some("test")).unwrap_err();
    assert!(error.to_string().contains("dimensions differ"));
}

#[test]
fn accepts_the_dimension_boundary_and_keeps_unembedded_records() {
    let input = backup(vec![
        record(1, json!(vec![1.0; MAX_EMBEDDING_DIMENSIONS])),
        record(2, Value::Null),
    ]);
    let output = convert_backup(&input, "a", None, Some("test")).unwrap();
    let text = String::from_utf8(output).unwrap();
    let result: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(
        result["embedding"]["values"].as_array().unwrap().len(),
        MAX_EMBEDDING_DIMENSIONS
    );
    assert_eq!(text.lines().count(), 2);
}

#[test]
fn hash_verification_covers_embedding_and_metadata_before_conversion() {
    let input = backup(vec![record(1, json!([0.5, 1.0]))]);
    let original: Value = serde_json::from_slice(&input).unwrap();
    for tamper_embedding in [false, true] {
        let mut changed = original.clone();
        if tamper_embedding {
            changed["memories"][0]["embedding"][0] = json!(0.75);
        } else {
            changed["memories"][0]["metadata"]["arbitrary"]["extra"] = json!("changed");
        }
        let error = convert_backup(
            &serde_json::to_vec(&changed).unwrap(),
            "a",
            None,
            Some("test"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("integrity check failed"));
    }
}

#[test]
fn requires_legacy_metadata_to_be_an_object() {
    for metadata in [Value::Null, json!([]), json!("invalid")] {
        let mut invalid = record(1, Value::Null);
        invalid["metadata"] = metadata;
        assert!(convert_backup(&backup(vec![invalid]), "a", None, None).is_err());
    }
}
