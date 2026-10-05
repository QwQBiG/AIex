use super::*;
use crate::{MemoryEntry, MemoryKind, MemoryPage, MemoryRequest, MemoryResponse, TurnId};
use serde_json::json;

fn media(modality: MemoryModality) -> MemoryMedia {
    MemoryMedia {
        modality,
        audio_features: matches!(modality, MemoryModality::Audio | MemoryModality::Multimodal)
            .then(Map::new),
        visual_features: matches!(
            modality,
            MemoryModality::Visual | MemoryModality::Multimodal
        )
        .then(Map::new),
        ..MemoryMedia::default()
    }
}

#[test]
fn old_memory_entry_and_page_remain_readable_without_knowledge() {
    let id = Uuid::new_v4();
    let legacy = json!({
        "id":id, "turn_id":TurnId::new(), "created_ms":1,
        "user_text":"原有记录", "assistant_text":"保留内容",
    });
    let entry: MemoryEntry = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(entry.profile_id, "default");
    assert_eq!(entry.kind, MemoryKind::Conversation);
    assert_eq!(entry.revision, 1);
    assert_eq!(entry.source, MemorySource::Automatic);
    let page: MemoryPage = serde_json::from_value(json!({
        "profile_id":"default", "enabled":true, "total":1, "offset":0,
        "entries":[legacy],
    }))
    .unwrap();
    assert!(page.knowledge.is_empty());
    assert!(page.truncated_ids.is_empty());
    assert!(
        serde_json::to_value(page)
            .unwrap()
            .get("knowledge")
            .is_none()
    );
    assert!(
        serde_json::to_value(entry)
            .unwrap()
            .get("knowledge")
            .is_none()
    );
}

#[test]
fn knowledge_sidecar_and_entity_evidence_round_trip_with_provenance() {
    let id = Uuid::new_v4();
    let entity = MemoryEntity {
        kind: MemoryEntityKind::Preference,
        key: "preference:tea".to_owned(),
        name: "茶".to_owned(),
        value: "喜欢茉莉花茶".to_owned(),
        confidence: 850,
    };
    let knowledge = MemoryKnowledge {
        entities: vec![entity.clone()],
        media: Some(media(MemoryModality::Audio)),
    };
    knowledge.validate().unwrap();
    let page = MemoryPage {
        profile_id: "friend".to_owned(),
        enabled: true,
        total: 1,
        offset: 0,
        entries: Vec::new(),
        truncated_ids: Vec::new(),
        knowledge: [(id, knowledge)].into_iter().collect(),
    };
    let json = serde_json::to_value(&page).unwrap();
    assert_eq!(
        json["knowledge"][id.to_string()]["entities"][0]["kind"],
        "preference"
    );
    assert_eq!(serde_json::from_value::<MemoryPage>(json).unwrap(), page);
    let response = MemoryResponse::Entities(MemoryEntityPage {
        profile_id: "friend".to_owned(),
        enabled: true,
        total: 1,
        offset: 0,
        entities: vec![MemoryEntityGroup {
            key: entity.key,
            kind: entity.kind,
            name: entity.name,
            current_value: entity.value.clone(),
            first_mentioned_ms: 1,
            last_updated_ms: 2,
            evidence: vec![MemoryEntityEvidence {
                memory_id: id,
                revision: 3,
                source: MemorySource::UserCorrection,
                value: entity.value,
                confidence: entity.confidence,
            }],
            total_mentions: 1,
        }],
    });
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(json["type"], "entities");
    assert_eq!(
        serde_json::from_value::<MemoryResponse>(json).unwrap(),
        response
    );
}

#[test]
fn requests_preserve_character_scope_mutation_classification_and_wire_names() {
    let profile_id = "friend".to_owned();
    let requests = [
        (
            MemoryRequest::List {
                profile_id: profile_id.clone(),
                query: String::new(),
                kind: None,
                offset: 0,
                limit: 10,
            },
            false,
            "list",
        ),
        (
            MemoryRequest::Entities {
                profile_id: profile_id.clone(),
                query: String::new(),
                offset: 0,
                limit: 10,
            },
            false,
            "entities",
        ),
        (
            MemoryRequest::Evidence {
                profile_id: profile_id.clone(),
                key: "name:user".to_owned(),
                offset: 0,
                limit: 10,
            },
            false,
            "evidence",
        ),
        (
            MemoryRequest::QueryMedia {
                profile_id: profile_id.clone(),
                query: String::new(),
                modality: None,
                tags: Vec::new(),
                min_importance: 0,
                offset: 0,
                limit: 10,
            },
            false,
            "query_media",
        ),
        (
            MemoryRequest::Remember {
                profile_id: profile_id.clone(),
                text: "text".to_owned(),
            },
            true,
            "remember",
        ),
        (
            MemoryRequest::Correct {
                profile_id: profile_id.clone(),
                id: Uuid::new_v4(),
                expected_revision: 1,
                text: "text".to_owned(),
            },
            true,
            "correct",
        ),
        (
            MemoryRequest::Forget {
                profile_id: profile_id.clone(),
                id: Uuid::new_v4(),
                expected_revision: 1,
            },
            true,
            "forget",
        ),
        (
            MemoryRequest::RememberMedia {
                profile_id: profile_id.clone(),
                text: "description".to_owned(),
                media: media(MemoryModality::Visual),
            },
            true,
            "remember_media",
        ),
        (
            MemoryRequest::CorrectMedia {
                profile_id: profile_id.clone(),
                id: Uuid::new_v4(),
                expected_revision: 1,
                text: "description".to_owned(),
                media: media(MemoryModality::Multimodal),
            },
            true,
            "correct_media",
        ),
    ];
    for (request, mutation, wire_name) in requests {
        request.validate().unwrap();
        assert_eq!(request.profile_id(), "friend");
        assert_eq!(request.is_mutation(), mutation);
        let mut encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["type"], wire_name);
        assert_eq!(
            serde_json::from_value::<MemoryRequest>(encoded.clone()).unwrap(),
            request
        );
        encoded["profile_id"] = json!(" ");
        assert!(
            serde_json::from_value::<MemoryRequest>(encoded)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

#[test]
fn modalities_preserve_legacy_optional_multimodal_features() {
    for modality in [
        MemoryModality::Text,
        MemoryModality::Audio,
        MemoryModality::Visual,
        MemoryModality::Multimodal,
    ] {
        for audio in [false, true] {
            for visual in [false, true] {
                let value = MemoryMedia {
                    modality,
                    audio_features: audio.then(Map::new),
                    visual_features: visual.then(Map::new),
                    ..MemoryMedia::default()
                };
                let expected = match modality {
                    MemoryModality::Text => !audio && !visual,
                    MemoryModality::Audio => audio && !visual,
                    MemoryModality::Visual => !audio && visual,
                    MemoryModality::Multimodal => true,
                };
                assert_eq!(value.validate().is_ok(), expected);
            }
        }
    }
}

#[test]
fn media_limits_count_unicode_characters_and_complete_json_bytes() {
    let mut value = MemoryMedia {
        tags: vec!["茶".repeat(64); 16],
        importance: 1000,
        ..MemoryMedia::default()
    };
    value.validate().unwrap();
    value.tags.push("extra".to_owned());
    assert!(value.validate().is_err());
    value.tags = vec!["茶".repeat(65)];
    assert!(value.validate().is_err());
    value.tags = vec![" ".to_owned()];
    assert!(value.validate().is_err());
    value.tags.clear();
    value.importance = 1001;
    assert!(value.validate().is_err());
    value.importance = 0;
    value.metadata.insert("payload".to_owned(), json!(""));
    let overhead = serde_json::to_vec(&value).unwrap().len();
    value
        .metadata
        .insert("payload".to_owned(), json!("x".repeat(8192 - overhead)));
    assert_eq!(serde_json::to_vec(&value).unwrap().len(), 8192);
    value.validate().unwrap();
    value
        .metadata
        .insert("payload".to_owned(), json!("x".repeat(8193 - overhead)));
    assert!(value.validate().is_err());
}

#[test]
fn paged_requests_bound_filters_key_and_offsets() {
    let templates = [
        json!({"type":"entities","profile_id":"friend","query":"","offset":0,"limit":100}),
        json!({"type":"evidence","profile_id":"friend","key":"name:user","offset":0,"limit":100}),
        json!({"type":"query_media","profile_id":"friend","query":"","modality":null,"tags":[],"min_importance":1000,"offset":0,"limit":100}),
    ];
    for template in templates {
        serde_json::from_value::<MemoryRequest>(template.clone())
            .unwrap()
            .validate()
            .unwrap();
        for (field, invalid) in [
            ("limit", json!(0)),
            ("limit", json!(101)),
            ("offset", json!(usize::MAX)),
        ] {
            let mut changed = template.clone();
            changed[field] = invalid;
            assert!(
                serde_json::from_value::<MemoryRequest>(changed)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        let mut changed = template;
        let field = if changed["type"] == "evidence" {
            "key"
        } else {
            "query"
        };
        changed[field] = json!("茶".repeat(513));
        assert!(
            serde_json::from_value::<MemoryRequest>(changed)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    for (field, value) in [
        ("tags", json!(["x".repeat(65)])),
        ("tags", json!(vec!["x"; 17])),
        ("min_importance", json!(1001)),
    ] {
        let mut query = json!({"type":"query_media","profile_id":"friend","query":"","tags":[],"min_importance":0,"offset":0,"limit":1});
        query[field] = value;
        assert!(
            serde_json::from_value::<MemoryRequest>(query)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    assert!(
        MemoryRequest::Evidence {
            profile_id: "friend".to_owned(),
            key: " ".to_owned(),
            offset: 0,
            limit: 1
        }
        .validate()
        .is_err()
    );
}

#[test]
fn media_edits_require_description_revision_and_valid_media() {
    let original = MemoryRequest::CorrectMedia {
        profile_id: "friend".to_owned(),
        id: Uuid::new_v4(),
        expected_revision: 1,
        text: "声线样本".to_owned(),
        media: media(MemoryModality::Audio),
    };
    original.validate().unwrap();
    let base = serde_json::to_value(original).unwrap();
    for (field, value) in [
        ("expected_revision", json!(0)),
        ("text", json!(" ")),
        ("text", json!("茶".repeat(4097))),
    ] {
        let mut changed = base.clone();
        changed[field] = value;
        assert!(
            serde_json::from_value::<MemoryRequest>(changed)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut changed = base;
    changed["media"]["visual_features"] = json!({});
    assert!(
        serde_json::from_value::<MemoryRequest>(changed)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn knowledge_defaults_and_confidence_bounds_are_explicit() {
    assert_eq!(
        serde_json::from_str::<MemoryKnowledge>("{}").unwrap(),
        MemoryKnowledge::default()
    );
    assert_eq!(
        serde_json::from_str::<MemoryMedia>("{}").unwrap(),
        MemoryMedia::default()
    );
    let mut entity = MemoryEntity {
        kind: MemoryEntityKind::Name,
        key: "name:user".to_owned(),
        name: "user".to_owned(),
        value: "Alice".to_owned(),
        confidence: 1000,
    };
    entity.validate().unwrap();
    entity.confidence = 1001;
    assert!(
        MemoryKnowledge {
            entities: vec![entity],
            media: None
        }
        .validate()
        .is_err()
    );
}

#[test]
fn all_legacy_multimodal_feature_combinations_round_trip_without_normalization() {
    for audio in [false, true] {
        for visual in [false, true] {
            let original = MemoryMedia {
                modality: MemoryModality::Multimodal,
                audio_features: audio
                    .then(|| serde_json::from_value(json!({"entities": ["speaker"]})).unwrap()),
                visual_features: visual
                    .then(|| serde_json::from_value(json!({"objects": ["cup"]})).unwrap()),
                tags: vec!["legacy".to_owned()],
                importance: 500,
                ..MemoryMedia::default()
            };
            original.validate().unwrap();
            let encoded = serde_json::to_value(&original).unwrap();
            assert_eq!(encoded["modality"], "multimodal");
            assert_eq!(encoded.get("audio_features").is_some(), audio);
            assert_eq!(encoded.get("visual_features").is_some(), visual);
            assert_eq!(
                serde_json::from_value::<MemoryMedia>(encoded).unwrap(),
                original
            );
            for request in [
                MemoryRequest::RememberMedia {
                    profile_id: "legacy-character".to_owned(),
                    text: "Synthetic observation".to_owned(),
                    media: original.clone(),
                },
                MemoryRequest::CorrectMedia {
                    profile_id: "legacy-character".to_owned(),
                    id: Uuid::new_v4(),
                    expected_revision: 1,
                    text: "Updated synthetic observation".to_owned(),
                    media: original.clone(),
                },
            ] {
                request.validate().unwrap();
            }
        }
    }
}
