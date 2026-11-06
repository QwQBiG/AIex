use super::*;
use serde_json::{Value, json};
use uuid::Uuid;

fn record(profile: &str, id: Uuid, space: &str) -> Value {
    json!({"schema_version":2,"record_type":"memory","id":id,"profile_id":profile,
        "turn_id":Uuid::new_v4(),"created_ms":1,"user_text":"private-text-marker",
        "assistant_text":"private-reply-marker","source":"automatic",
        "embedding":{"space":space,"values":[1.0,2.0]},
        "legacy_metadata":{"private-key":"private-value-marker"}})
}

fn candidate(profile: &str, id: Uuid) -> Value {
    json!({"schema_version":2,"record_type":"entity_candidate","candidate":{
        "id":id,"profile_id":profile,"subject":{"type":"unknown"},
        "entity":{"kind":"preference","key":"preference:tea","name":"tea",
                  "value":"likes tea","confidence":912},
        "first_mentioned_ms":2,"last_updated_ms":3,"legacy_record":{"id":"old"}}})
}

fn source(profile: &str, entity: Uuid, memory: Uuid) -> Value {
    json!({"schema_version":2,"record_type":"entity_source","source":{
        "entity_id":entity,"profile_id":profile,"legacy_memory_id":memory.to_string(),
        "memory_id":memory,"memory_revision":1,"status":"active"}})
}

fn bytes(rows: &[Value]) -> Vec<u8> {
    rows.iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

#[test]
fn summarizes_profile_and_space_without_exposing_private_fields() {
    let memory = Uuid::new_v4();
    let entity = Uuid::new_v4();
    let other = Uuid::new_v4();
    let data = bytes(&[
        record("a", memory, "private-space-marker"),
        record("a", other, "different-space-marker"),
        candidate("a", entity),
        source("a", entity, memory),
        source("a", entity, Uuid::new_v4()),
    ]);
    let report = inspect_profile_backup(&data, "a", Some("private-space-marker")).unwrap();
    assert_eq!(report.memory_records, 2);
    assert_eq!(report.entity_candidates, 1);
    assert_eq!(report.source_associations, 2);
    assert_eq!(report.inactive_source_associations, 1);
    assert_eq!(report.embedded_records, 2);
    assert_eq!(report.distinct_embedding_spaces, 2);
    assert_eq!(report.matching_embeddings, Some(1));
    let display = format!("{report:?} {}", serde_json::to_string(&report).unwrap());
    for marker in [
        "private-text-marker",
        "private-reply-marker",
        "private-value-marker",
        "private-space-marker",
        "different-space-marker",
    ] {
        assert!(!display.contains(marker));
    }
    assert_eq!(
        inspect_profile_backup(&data, "a", None)
            .unwrap()
            .matching_embeddings,
        None
    );
}

#[test]
fn rejects_empty_mixed_missing_and_cross_profile_identity() {
    let id = Uuid::new_v4();
    assert!(inspect_profile_backup(b"\n ", "a", None).is_err());
    let a = record("a", id, "space");
    assert!(inspect_profile_backup(&bytes(std::slice::from_ref(&a)), "b", None).is_err());
    assert!(
        inspect_profile_backup(
            &bytes(&[a.clone(), record("b", Uuid::new_v4(), "space")]),
            "a",
            None
        )
        .is_err()
    );
    let mut missing = a.clone();
    missing.as_object_mut().unwrap().remove("profile_id");
    assert!(inspect_profile_backup(&bytes(&[missing]), "default", None).is_err());
    let entity = Uuid::new_v4();
    assert!(
        inspect_profile_backup(
            &bytes(&[a, candidate("a", entity), source("b", entity, id)]),
            "a",
            None
        )
        .is_err()
    );
}

#[test]
fn rejects_invalid_relationships_formats_and_budgets_without_echoing_input() {
    let id = Uuid::new_v4();
    let invalid = bytes(&[record("a", id, "space"), source("a", Uuid::new_v4(), id)]);
    assert!(inspect_profile_backup(&invalid, "a", None).is_err());
    let invalid = b"{\"record_type\":\"private-text-marker\"}";
    let error = inspect_profile_backup(invalid, "a", None).unwrap_err();
    assert!(!error.to_string().contains("private-text-marker"));
    assert!(inspect_profile_backup(&[0x1f, 0x8b], "a", None).is_err());
    assert!(inspect_profile_backup(&vec![b' '; MAX_BYTES + 1], "a", None).is_err());
    let spaces: Vec<_> = (0..=MAX_SPACES)
        .map(|n| record("a", Uuid::new_v4(), &format!("space-{n}")))
        .collect();
    assert!(inspect_profile_backup(&bytes(&spaces), "a", None).is_err());
}
