use super::*;
use serde_json::{Value, json};

fn record(id: Uuid, profile: &str, space: &str, values: &[f32]) -> Value {
    json!({"schema_version":2,"record_type":"memory","id":id,"profile_id":profile,
        "turn_id":Uuid::new_v4(),"created_ms":1,"user_text":"private-text-marker",
        "assistant_text":"","source":"automatic","revision":1,
        "embedding":{"space":space,"values":values},
        "legacy_metadata":{"private-key":"private-value-marker"},
        "unknown_extension":{"original":[1,2,3]}})
}

fn entity(id: Uuid, profile: &str) -> Value {
    json!({"schema_version":2,"record_type":"entity_candidate","candidate":{
        "id":id,"profile_id":profile,"subject":{"type":"unknown"},"entity":null,
        "first_mentioned_ms":null,"last_updated_ms":null,"legacy_record":{"private":"marker"}}})
}

fn source(entity: Uuid, memory: Uuid, status: &str, revision: u64) -> Value {
    json!({"schema_version":2,"record_type":"entity_source","source":{
        "entity_id":entity,"profile_id":"a","legacy_memory_id":memory.to_string(),
        "memory_id":memory,"memory_revision":revision,"status":status}})
}

fn rows(values: &[Value]) -> Vec<u8> {
    values
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\r\n")
        .into_bytes()
}

#[test]
fn preserves_raw_lines_unknown_fields_and_reports_only_counts() {
    let first_id = Uuid::new_v4();
    let second_id = Uuid::new_v4();
    let entity_id = Uuid::new_v4();
    let base = rows(&[
        record(first_id, "a", "private-space-marker", &[1.0, 2.0]),
        entity(entity_id, "a"),
        source(entity_id, first_id, "active", 1),
    ]);
    let incoming = rows(&[record(second_id, "a", "another-space-marker", &[3.0, 4.0])]);
    let candidate = merge_profile_candidates(&base, &incoming, "a").unwrap();
    let mut expected = base.clone();
    expected.push(b'\n');
    expected.extend_from_slice(&incoming);
    assert_eq!(candidate.content, expected);
    assert_eq!(candidate.report.base_memory_records, 1);
    assert_eq!(candidate.report.incoming_memory_records, 1);
    assert_eq!(candidate.report.entity_candidates, 1);
    assert_eq!(candidate.report.source_associations, 1);
    assert_eq!(candidate.report.inactive_source_associations, 0);
    assert_eq!(candidate.report.distinct_embedding_spaces, 2);
    assert!(!candidate.report.base_empty && !candidate.report.incoming_empty);
    let report = serde_json::to_string(&candidate.report).unwrap();
    for marker in ["private", "another-space", &first_id.to_string()] {
        assert!(!report.contains(marker));
    }
}

#[test]
fn absent_and_stale_active_sources_do_not_revive_across_inputs() {
    let memory_id = Uuid::new_v4();
    let entity_id = Uuid::new_v4();
    let incoming = rows(&[record(memory_id, "a", "space", &[1.0, 2.0])]);
    for bad in [
        rows(&[
            entity(entity_id, "a"),
            source(entity_id, memory_id, "active", 1),
        ]),
        rows(&[
            record(memory_id, "a", "space", &[1.0, 2.0]),
            entity(entity_id, "a"),
            source(entity_id, memory_id, "active", 9),
        ]),
    ] {
        assert!(merge_profile_candidates(&bad, &incoming, "a").is_err());
    }
    for state in ["missing", "corrected", "forgotten", "unresolved"] {
        let base = rows(&[
            entity(entity_id, "a"),
            source(entity_id, memory_id, state, 1),
        ]);
        let result = merge_profile_candidates(&base, &incoming, "a").unwrap();
        assert_eq!(result.report.inactive_source_associations, 1);
        let parsed = document::parse(std::str::from_utf8(&result.content).unwrap()).unwrap();
        assert_ne!(
            parsed.provenance.sources[0].status,
            MemoryEntitySourceStatus::Active
        );
    }
}

#[test]
fn empty_inputs_are_reported_but_two_empty_inputs_are_rejected() {
    let incoming = rows(&[record(Uuid::new_v4(), "a", "space", &[1.0, 2.0])]);
    let result = merge_profile_candidates(b" \r\n", &incoming, "a").unwrap();
    assert!(result.report.base_empty);
    assert!(!result.report.incoming_empty);
    assert!(result.content.starts_with(b" \r\n"));
    assert!(merge_profile_candidates(b" \n", b"", "a").is_err());
    let result = merge_profile_candidates(&incoming, b"", "a").unwrap();
    assert!(result.report.incoming_empty);
    assert_eq!(result.content, incoming);
}

#[test]
fn rejects_overlapping_identities_profiles_and_vector_spaces() {
    let id = Uuid::new_v4();
    let base = rows(&[record(id, "a", "one", &[1.0, 2.0])]);
    let repeated = rows(&[record(id, "a", "another", &[1.0, 2.0])]);
    assert!(merge_profile_candidates(&base, &repeated, "a").is_err());
    let other_profile = rows(&[record(Uuid::new_v4(), "b", "another", &[1.0, 2.0])]);
    assert!(merge_profile_candidates(&base, &other_profile, "a").is_err());
    let same_space = rows(&[record(Uuid::new_v4(), "a", "one", &[3.0, 4.0])]);
    assert!(merge_profile_candidates(&base, &same_space, "a").is_err());
    let mismatch = rows(&[record(Uuid::new_v4(), "a", "one", &[3.0, 4.0, 5.0])]);
    assert!(merge_profile_candidates(&base, &mismatch, "a").is_err());
    let within_input = rows(&[
        record(Uuid::new_v4(), "a", "one", &[1.0, 2.0]),
        record(Uuid::new_v4(), "a", "one", &[1.0, 2.0, 3.0]),
    ]);
    assert!(merge_profile_candidates(&within_input, b"", "a").is_err());
    let entity_id = Uuid::new_v4();
    assert!(
        merge_profile_candidates(
            &rows(&[entity(entity_id, "a")]),
            &rows(&[entity(entity_id, "a")]),
            "a"
        )
        .is_err()
    );
}

#[test]
fn rejects_nested_duplicate_keys_and_invalid_json_without_leaking_content() {
    let id = Uuid::new_v4();
    let valid = rows(&[record(id, "a", "space", &[1.0, 2.0])]);
    let text = std::str::from_utf8(&valid).unwrap();
    let duplicate = text.replace(
        "\"private-key\":\"private-value-marker\"",
        "\"private-key\":\"private-value-marker\",\"private-key\":\"other\"",
    );
    assert_ne!(duplicate, text);
    for input in [
        duplicate.as_bytes(),
        b"{\"record_type\":\"private-marker\"}",
        &[0xff],
    ] {
        let error = match merge_profile_candidates(input, b"", "a") {
            Ok(_) => panic!("invalid input must fail"),
            Err(error) => error,
        };
        assert!(!error.to_string().contains("private"));
    }
    assert!(unique_json::validate(r#"{"x":1,"\u0078":2}"#).is_err());
    let nested = format!("{}0{}", "[".repeat(65), "]".repeat(65));
    assert!(unique_json::validate(&nested).is_err());
}

#[test]
fn enforces_size_and_profile_budgets_before_output() {
    let input = rows(&[record(Uuid::new_v4(), "a", "space", &[1.0, 2.0])]);
    assert!(merge_profile_candidates(&vec![b' '; MAX_BYTES + 1], &input, "a").is_err());
    assert!(merge_profile_candidates(&input, b"", " ").is_err());
    assert!(merge_profile_candidates(&input, b"", &"a".repeat(129)).is_err());
    let many_spaces: Vec<_> = (0..=MAX_SPACES)
        .map(|n| record(Uuid::new_v4(), "a", &format!("space-{n}"), &[1.0, 2.0]))
        .collect();
    assert!(merge_profile_candidates(&rows(&many_spaces), b"", "a").is_err());
    let line = b"{\"record_type\":\"memory\",\"schema_version\":2,\"profile_id\":\"a\"}\n";
    let mut too_many = Vec::with_capacity(line.len() * (MAX_ROWS + 1));
    for _ in 0..=MAX_ROWS {
        too_many.extend_from_slice(line);
    }
    assert!(merge_profile_candidates(&too_many, b"", "a").is_err());
}
