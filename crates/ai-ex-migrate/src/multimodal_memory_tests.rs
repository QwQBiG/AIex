use super::*;
use serde_json::json;

fn snapshot() -> Value {
    json!({
        "memory_id":"mm_0123456789abcdef_1700000000", "modality":"audio",
        "text_content":null, "audio_features":{"rms":0.25,"speaker":{"name":"测试"}},
        "visual_features":null, "timestamp":1.001, "importance_score":0.875,
        "access_count":7, "tags":["voice"],
        "metadata":{"legacy_access_count":"user-owned value","legacy_metadata":{"keep":true}},
    })
}

#[test]
fn preserves_full_snapshot_features_and_deterministic_identity() {
    let source = snapshot();
    let input = serde_json::to_vec_pretty(&json!([source])).unwrap();
    let first = convert_snapshot(&input, "character-a").unwrap();
    assert_eq!(first, convert_snapshot(&input, "character-a").unwrap());
    assert_eq!(first.iter().filter(|byte| **byte == b'\n').count(), 1);
    let imported: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(imported["legacy_metadata"], source);
    assert_eq!(
        imported["knowledge"]["media"]["metadata"],
        source["metadata"]
    );
    assert_eq!(
        imported["knowledge"]["media"]["audio_features"],
        source["audio_features"]
    );
    assert_eq!(imported["knowledge"]["media"]["importance"], 875);
    assert_eq!(imported["knowledge"]["entities"], json!([]));
    let entry: MemoryEntry = serde_json::from_slice(&first).unwrap();
    assert_eq!(
        entry.id,
        import_id("mm_0123456789abcdef_1700000000").unwrap()
    );
    assert_eq!(entry.id.as_bytes()[6] >> 4, 8);
    assert_eq!(entry.created_ms, 1001);
    assert_eq!(entry.user_text, "Audio observation");
    assert_eq!(entry.source, MemorySource::Automatic);
    assert_eq!(entry.profile_id, "character-a");
    assert_eq!(entry.kind, MemoryKind::Conversation);
}

#[test]
fn existing_uuids_are_preserved_and_old_ids_have_stable_distinct_mappings() {
    let id = Uuid::new_v4();
    assert_eq!(import_id(&id.to_string()).unwrap(), id);
    assert_ne!(
        import_id("mm_0123456789abcdef_1700000000").unwrap(),
        import_id("mm_0123456789abcdef_1700000001").unwrap()
    );
    for invalid in [
        "",
        "mm_0123_1700000000",
        "mm_0123456789abcdef_not-a-time",
        "arbitrary-id",
    ] {
        assert!(import_id(invalid).is_err());
    }
}

#[test]
fn epoch_seconds_use_exact_decimal_milliseconds_without_rounding_up() {
    for (raw, expected) in [
        ("0", 0),
        ("0.000999", 0),
        ("1.001", 1001),
        ("1.999999", 1999),
        ("1e3", 1_000_000),
        ("1.001e1", 10010),
        ("1.001e-1", 100),
        ("1700000000.123456", 1_700_000_000_123),
        ("18446744073709551.615", u64::MAX),
    ] {
        let raw = RawValue::from_string(raw.to_owned()).unwrap();
        assert_eq!(epoch_millis(&raw).unwrap(), expected);
    }
    for raw in [
        "-1",
        "-0.001",
        "1e30",
        "18446744073709551.616",
        "\"1.001\"",
        "null",
        "true",
    ] {
        let raw = RawValue::from_string(raw.to_owned()).unwrap();
        assert!(epoch_millis(&raw).is_err());
    }
}

#[test]
fn rejects_invalid_records_instead_of_silently_dropping_fields() {
    for (key, invalid) in [
        ("timestamp", json!(-1)),
        ("timestamp", json!("unknown")),
        ("importance_score", json!(1.001)),
        ("importance_score", json!(-0.1)),
        ("access_count", json!(-1)),
        ("modality", json!("unknown")),
        ("audio_features", Value::Null),
        ("visual_features", json!({})),
        ("tags", json!(vec!["tag"; 17])),
        ("text_content", json!("文".repeat(4097))),
        ("metadata", json!({"payload":"x".repeat(8192)})),
    ] {
        let mut source = snapshot();
        source[key] = invalid;
        assert!(
            convert_snapshot(
                &serde_json::to_vec(&json!([source])).unwrap(),
                "character-a"
            )
            .is_err(),
            "field {key}"
        );
    }
    let mut text = snapshot();
    text["modality"] = json!("text");
    text["audio_features"] = Value::Null;
    assert!(convert_snapshot(&serde_json::to_vec(&json!([text])).unwrap(), "character-a").is_err());
    let mut missing = snapshot();
    missing.as_object_mut().unwrap().remove("timestamp");
    assert!(
        convert_snapshot(
            &serde_json::to_vec(&json!([missing])).unwrap(),
            "character-a"
        )
        .is_err()
    );
}

#[test]
fn rejects_duplicate_ids_keys_malformed_utf8_and_non_array_input() {
    let source = snapshot();
    assert!(
        convert_snapshot(
            &serde_json::to_vec(&json!([source, source])).unwrap(),
            "character-a"
        )
        .is_err()
    );
    for input in [
        b"[{\"timestamp\":1,\"timestamp\":2}]".as_slice(),
        b"[{\"metadata\":{\"x\":1,\"x\":2}}]".as_slice(),
        b"[{\"text_content\":\"\xff\"}]".as_slice(),
        b"{}".as_slice(),
        b"[".as_slice(),
    ] {
        assert!(convert_snapshot(input, "character-a").is_err());
    }
    assert!(convert_snapshot(b"[]", " ").is_err());
    let too_many = format!("[{}]", vec!["null"; MAX_RECORDS + 1].join(","));
    assert!(convert_snapshot(too_many.as_bytes(), "character-a").is_err());
}

#[test]
fn multimodal_snapshots_preserve_all_optional_feature_combinations() {
    for audio in [false, true] {
        for visual in [false, true] {
            let mut source = snapshot();
            source["modality"] = json!("multimodal");
            source["text_content"] = json!("旧版多模态文本");
            source["audio_features"] = if audio {
                json!({"rms":0.25,"nested":{"preserve":"audio"}})
            } else {
                Value::Null
            };
            source["visual_features"] = if visual {
                json!({"objects":["synthetic stage"],"nested":{"preserve":"visual"}})
            } else {
                Value::Null
            };
            for omit_absent_fields in [false, true] {
                let mut source = source.clone();
                if omit_absent_fields {
                    if !audio {
                        source.as_object_mut().unwrap().remove("audio_features");
                    }
                    if !visual {
                        source.as_object_mut().unwrap().remove("visual_features");
                    }
                }
                let input = serde_json::to_vec(&json!([source])).unwrap();
                let output = convert_snapshot(&input, "character-a").unwrap();
                let imported: Value = serde_json::from_slice(&output).unwrap();
                assert_eq!(imported["legacy_metadata"], source);
                assert_eq!(imported["user_text"], "旧版多模态文本");
                let media: MemoryMedia =
                    serde_json::from_value(imported["knowledge"]["media"].clone()).unwrap();
                assert_eq!(media.modality, MemoryModality::Multimodal);
                assert_eq!(media.audio_features.is_some(), audio);
                assert_eq!(media.visual_features.is_some(), visual);
                if audio {
                    assert_eq!(
                        media.audio_features.as_ref().unwrap(),
                        source["audio_features"].as_object().unwrap()
                    );
                }
                if visual {
                    assert_eq!(
                        media.visual_features.as_ref().unwrap(),
                        source["visual_features"].as_object().unwrap()
                    );
                }
                assert_eq!(
                    imported["knowledge"]["media"]
                        .get("audio_features")
                        .is_some(),
                    audio
                );
                assert_eq!(
                    imported["knowledge"]["media"]
                        .get("visual_features")
                        .is_some(),
                    visual
                );
            }
        }
    }
}
