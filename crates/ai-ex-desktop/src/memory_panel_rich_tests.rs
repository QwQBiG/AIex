use super::*;
use ai_ex_domain::{
    MemoryEntity, MemoryEntityEvidence, MemoryEntityGroup, MemoryEntityKind, MemoryEntityPage,
    MemoryKnowledge, MemoryMedia, MemoryModality, MemorySource, TurnId,
};
use serde_json::{Map, Value, json};

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("fixture object").clone()
}

fn media() -> MemoryMedia {
    MemoryMedia {
        modality: MemoryModality::Multimodal,
        tags: vec!["研究".to_owned(), "现场".to_owned()],
        audio_features: Some(object(json!({
            "description": "听到雨声",
            "channels": ["left", "right"],
            "measurements": {"rms": 0.25, "samples": [1, 2, 3]}
        }))),
        visual_features: Some(object(json!({
            "description": "看见窗外的树",
            "objects": [{"label": "tree", "bounds": [1, 2, 30, 40]}],
            "palette": {"leaf": "green"}
        }))),
        metadata: object(json!({
            "source": {"name": "synthetic fixture", "version": 3},
            "opaque": [true, null, {"preserve": "原样保留"}]
        })),
        importance: 725,
    }
}

fn entry(profile_id: &str) -> MemoryEntry {
    MemoryEntry {
        id: uuid::Uuid::new_v4(),
        profile_id: profile_id.to_owned(),
        turn_id: TurnId::new(),
        created_ms: 1_700_000_000_000,
        updated_ms: None,
        kind: MemoryKind::Persona,
        user_text: "原来的媒体记忆".to_owned(),
        assistant_text: String::new(),
        source: MemorySource::UserNote,
        revision: 4,
    }
}

fn knowledge(media: MemoryMedia) -> MemoryKnowledge {
    MemoryKnowledge {
        entities: vec![MemoryEntity {
            kind: MemoryEntityKind::Preference,
            key: "preference:茶".to_owned(),
            name: "茶".to_owned(),
            value: "喜欢绿茶".to_owned(),
            confidence: 900,
        }],
        media: Some(media),
    }
}

fn records(profile_id: &str, offset: usize) -> MemoryPage {
    MemoryPage {
        profile_id: profile_id.to_owned(),
        enabled: true,
        total: 0,
        offset,
        entries: Vec::new(),
        truncated_ids: Vec::new(),
        knowledge: BTreeMap::new(),
    }
}

fn entities(profile_id: &str, offset: usize) -> MemoryEntityPage {
    MemoryEntityPage {
        profile_id: profile_id.to_owned(),
        enabled: true,
        total: 0,
        offset,
        entities: Vec::new(),
    }
}

fn begin(panel: &mut MemoryPanel, request: MemoryRequest) -> u64 {
    let crate::worker::WorkerCommand::Memory { request_id, .. } =
        panel.begin(request).expect("valid memory request")
    else {
        panic!("memory worker command");
    };
    request_id
}

#[test]
fn editing_media_preserves_nested_features_and_metadata() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    let entry = entry("first");
    let original = media();
    panel.start_edit(&entry, Some(&knowledge(original.clone())), false);
    panel.draft = "只修改准确的正文".to_owned();
    let MemoryRequest::CorrectMedia {
        profile_id,
        id,
        expected_revision,
        text,
        media,
    } = panel.editor_request().unwrap()
    else {
        panic!("existing media must remain a media correction");
    };
    assert_eq!(profile_id, "first");
    assert_eq!(id, entry.id);
    assert_eq!(expected_revision, 4);
    assert_eq!(text, "只修改准确的正文");
    assert_eq!(media, original);
}

#[test]
fn description_changes_only_the_named_feature_field() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    let mut expected = media();
    panel.start_edit(&entry("first"), Some(&knowledge(expected.clone())), false);
    panel.media_draft.audio_description = "听到更准确的雨声".to_owned();
    panel.media_draft.visual_description = "窗外有两棵树".to_owned();
    expected
        .audio_features
        .as_mut()
        .unwrap()
        .insert("description".to_owned(), json!("听到更准确的雨声"));
    expected
        .visual_features
        .as_mut()
        .unwrap()
        .insert("description".to_owned(), json!("窗外有两棵树"));
    let MemoryRequest::CorrectMedia { media, .. } = panel.editor_request().unwrap() else {
        panic!("media correction");
    };
    assert_eq!(media, expected);
}

#[test]
fn untouched_non_string_descriptions_and_truncated_record_metadata_are_preserved() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    let mut original = media();
    original.audio_features.as_mut().unwrap().insert(
        "description".to_owned(),
        json!({"localized": ["雨声", "rain"]}),
    );
    original
        .visual_features
        .as_mut()
        .unwrap()
        .insert("description".to_owned(), json!(["树", "窗"]));
    panel.start_edit(&entry("first"), Some(&knowledge(original.clone())), true);
    assert!(
        panel.draft.is_empty(),
        "a truncated preview is not the full editable text"
    );
    assert!(panel.media_draft.audio_description.is_empty());
    assert!(panel.media_draft.visual_description.is_empty());
    panel.draft = "重新写下完整的事实".to_owned();
    let MemoryRequest::CorrectMedia { media, .. } = panel.editor_request().unwrap() else {
        panic!("media correction");
    };
    assert_eq!(media, original);
}

#[test]
fn explicit_modality_change_retains_metadata_and_only_applicable_features() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    let original = media();
    panel.start_edit(&entry("first"), Some(&knowledge(original.clone())), false);
    panel.media_draft.modality = MemoryModality::Audio;
    let MemoryRequest::CorrectMedia { media, .. } = panel.editor_request().unwrap() else {
        panic!("media correction");
    };
    assert_eq!(media.modality, MemoryModality::Audio);
    assert_eq!(media.audio_features, original.audio_features);
    assert!(media.visual_features.is_none());
    assert_eq!(media.metadata, original.metadata);
    media.validate().unwrap();
}

#[test]
fn plain_notes_stay_plain_and_new_audio_can_have_empty_features() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.draft = "普通笔记".to_owned();
    assert!(matches!(
        panel.editor_request().unwrap(),
        MemoryRequest::Remember { .. }
    ));
    panel.start_edit(&entry("first"), None, false);
    assert!(matches!(
        panel.editor_request().unwrap(),
        MemoryRequest::Correct { .. }
    ));
    panel.editing = None;
    panel.media_draft.modality = MemoryModality::Audio;
    let MemoryRequest::RememberMedia { media, .. } = panel.editor_request().unwrap() else {
        panic!("audio note");
    };
    assert_eq!(media.importance, 0);
    assert_eq!(media.audio_features, Some(Map::new()));
    assert!(media.visual_features.is_none());
    media.validate().unwrap();
}

#[test]
fn media_only_drafts_and_full_edit_state_are_isolated_per_profile() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.media_draft.tags = "仅标签草稿".to_owned();
    assert!(panel.has_unsaved());
    panel.set_profile("second");
    assert!(panel.draft.is_empty());
    assert!(panel.media_draft.tags.is_empty());
    let second = entry("second");
    let second_media = media();
    panel.start_edit(&second, Some(&knowledge(second_media.clone())), false);
    panel.draft = "第二个角色的正文".to_owned();
    panel.set_profile("first");
    assert!(panel.draft.is_empty());
    assert_eq!(panel.media_draft.tags, "仅标签草稿");
    assert!(panel.editing.is_none());
    panel.set_profile("second");
    let MemoryRequest::CorrectMedia {
        id,
        text,
        media,
        profile_id,
        ..
    } = panel.editor_request().unwrap()
    else {
        panic!("restored media correction");
    };
    assert_eq!(profile_id, "second");
    assert_eq!(id, second.id);
    assert_eq!(text, "第二个角色的正文");
    assert_eq!(media, second_media);
}

#[test]
fn read_responses_must_match_entity_or_record_type_and_requested_offset() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.view = MemoryView::Entities;
    let request = panel.list_request(3);
    let id = begin(&mut panel, request);
    assert!(!panel.receive(id, Ok(MemoryResponse::Page(records("first", 3)))));
    assert!(panel.page.is_none());
    assert!(panel.entity_page.is_none());
    assert!(panel.feedback.is_some());

    let request = panel.list_request(3);
    let id = begin(&mut panel, request);
    assert!(!panel.receive(id, Ok(MemoryResponse::Entities(entities("first", 4)))));
    assert!(panel.entity_page.is_none());

    panel.view = MemoryView::Records;
    let request = panel.list_request(5);
    let id = begin(&mut panel, request);
    assert!(!panel.receive(id, Ok(MemoryResponse::Entities(entities("first", 5)))));
    assert!(panel.page.is_none());
    let request = panel.list_request(5);
    let id = begin(&mut panel, request);
    assert!(!panel.receive(id, Ok(MemoryResponse::Page(records("first", 6)))));
    assert!(panel.page.is_none());
}

#[test]
fn old_profile_and_changed_media_filters_cannot_install_stale_results() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.view = MemoryView::Entities;
    let request = panel.list_request(0);
    let old = begin(&mut panel, request);
    panel.set_profile("second");
    panel.view = MemoryView::Entities;
    let request = panel.list_request(0);
    let current = begin(&mut panel, request);
    assert!(!panel.receive(old, Ok(MemoryResponse::Entities(entities("first", 0)))));
    assert!(panel.pending.is_some());
    panel.receive(current, Ok(MemoryResponse::Entities(entities("second", 0))));
    assert_eq!(panel.entity_page.as_ref().unwrap().profile_id, "second");

    panel.view = MemoryView::Media;
    panel.filter_tags = "旧条件".to_owned();
    let request = panel.list_request(0);
    let id = begin(&mut panel, request);
    panel.filter_tags = "新条件".to_owned();
    assert!(!panel.receive(id, Ok(MemoryResponse::Page(records("second", 0)))));
    assert!(panel.page.is_none());
}

#[test]
fn evidence_uses_exact_entity_key_and_media_queries_keep_all_filters() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.view = MemoryView::Evidence {
        key: "preference:绿茶/不加糖".to_owned(),
        name: "偏好显示名称".to_owned(),
    };
    panel.query = "这个旧搜索不能替换实体键".to_owned();
    let MemoryRequest::Evidence {
        profile_id,
        key,
        offset,
        limit,
    } = panel.list_request(7)
    else {
        panic!("entity evidence query");
    };
    assert_eq!(profile_id, "first");
    assert_eq!(key, "preference:绿茶/不加糖");
    assert_eq!(offset, 7);
    assert_eq!(limit, PAGE_SIZE);

    panel.view = MemoryView::Media;
    panel.query = "  窗外  ".to_owned();
    panel.filter_modality = Some(MemoryModality::Visual);
    panel.filter_tags = "研究, 现场".to_owned();
    panel.filter_importance = 650;
    let MemoryRequest::QueryMedia {
        profile_id,
        query,
        modality,
        tags,
        min_importance,
        offset,
        limit,
    } = panel.list_request(9)
    else {
        panic!("media query");
    };
    assert_eq!(profile_id, "first");
    assert_eq!(query, "窗外");
    assert_eq!(modality, Some(MemoryModality::Visual));
    assert_eq!(tags, ["研究", "现场"]);
    assert_eq!(min_importance, 650);
    assert_eq!(offset, 9);
    assert_eq!(limit, PAGE_SIZE);
}

#[test]
fn only_confirmed_media_changes_clear_the_complete_editor_draft() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.draft = "保留媒体草稿".to_owned();
    panel.media_draft = MemoryMediaDraft::from_media(Some(&media()));
    let request = panel.editor_request().unwrap();
    let id = begin(&mut panel, request);
    assert!(!panel.receive(
        id,
        Err(AppError::unavailable("synthetic unavailable service"))
    ));
    assert_eq!(panel.draft, "保留媒体草稿");
    assert_eq!(panel.media_draft.importance, 725);
    assert!(!panel.media_draft.tags.is_empty());
    let request = panel.editor_request().unwrap();
    let id = begin(&mut panel, request);
    assert!(panel.receive(id, Ok(MemoryResponse::Changed)));
    assert!(panel.draft.is_empty());
    assert!(panel.media_draft.tags.is_empty());
    assert!(panel.media_draft.audio_description.is_empty());
    assert!(panel.media_draft.visual_description.is_empty());
    assert_eq!(panel.media_draft.modality, MemoryModality::Text);
    assert!(panel.editing.is_none());
    assert!(!panel.has_unsaved());
}

#[test]
fn forgetting_keeps_an_unrelated_media_draft() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.draft = "另一个尚未保存的媒体记忆".to_owned();
    let original = media();
    panel.media_draft = MemoryMediaDraft::from_media(Some(&original));
    let id = begin(
        &mut panel,
        MemoryRequest::Forget {
            profile_id: "first".to_owned(),
            id: uuid::Uuid::new_v4(),
            expected_revision: 1,
        },
    );
    assert!(panel.receive(id, Ok(MemoryResponse::Changed)));
    let MemoryRequest::RememberMedia { text, media, .. } = panel.editor_request().unwrap() else {
        panic!("unrelated media draft must survive forgetting");
    };
    assert_eq!(text, "另一个尚未保存的媒体记忆");
    assert_eq!(media, original);
}

#[test]
fn each_rich_view_renders_headlessly_with_records_and_entity_evidence() {
    for view in [
        MemoryView::Records,
        MemoryView::Entities,
        MemoryView::Media,
        MemoryView::Evidence {
            key: "preference:茶".to_owned(),
            name: "茶".to_owned(),
        },
    ] {
        let mut panel = MemoryPanel::default();
        panel.set_profile("first");
        panel.view = view;
        let entry = entry("first");
        let mut page = records("first", 0);
        page.total = 1;
        page.knowledge.insert(entry.id, knowledge(media()));
        page.entries.push(entry.clone());
        panel.page = Some(page);
        let mut entity_page = entities("first", 0);
        entity_page.total = 1;
        entity_page.entities.push(MemoryEntityGroup {
            key: "preference:茶".to_owned(),
            kind: MemoryEntityKind::Preference,
            name: "茶".to_owned(),
            current_value: "喜欢绿茶".to_owned(),
            first_mentioned_ms: 1_700_000_000_000,
            last_updated_ms: 1_700_000_100_000,
            evidence: vec![MemoryEntityEvidence {
                memory_id: entry.id,
                revision: entry.revision,
                source: MemorySource::UserNote,
                value: "喜欢绿茶".to_owned(),
                confidence: 900,
            }],
            total_mentions: 1,
        });
        panel.entity_page = Some(entity_page);
        panel.loaded = true;
        panel.start_edit(&entry, Some(&knowledge(media())), false);
        let context = eframe::egui::Context::default();
        let _output = context.run_ui(Default::default(), |ui| {
            let _request = panel.show(ui, "合成测试角色", true, true);
        });
    }
}

#[test]
fn editing_sparse_legacy_multimodal_records_preserves_absent_feature_maps() {
    for (has_audio, has_visual) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut original = media();
        if !has_audio {
            original.audio_features = None;
        }
        if !has_visual {
            original.visual_features = None;
        }
        let mut panel = MemoryPanel::default();
        panel.set_profile("first");
        panel.start_edit(&entry("first"), Some(&knowledge(original.clone())), false);
        panel.draft = "更正正文，保留旧声画数据的实际形态".to_owned();
        let MemoryRequest::CorrectMedia { media, text, .. } = panel.editor_request().unwrap()
        else {
            panic!("legacy multimodal record remains a media correction");
        };
        assert_eq!(text, "更正正文，保留旧声画数据的实际形态");
        assert_eq!(
            media, original,
            "feature presence {has_audio}/{has_visual} must survive a text correction"
        );
    }
}

#[test]
fn adding_one_legacy_multimodal_description_only_creates_that_feature_map() {
    for audio in [false, true] {
        let mut original = media();
        original.audio_features = None;
        original.visual_features = None;
        let mut panel = MemoryPanel::default();
        panel.set_profile("first");
        panel.start_edit(&entry("first"), Some(&knowledge(original.clone())), false);
        let expected = Some(object(json!({"description": "补充的实际描述"})));
        if audio {
            panel.media_draft.audio_description = "补充的实际描述".to_owned();
            original.audio_features = expected;
        } else {
            panel.media_draft.visual_description = "补充的实际描述".to_owned();
            original.visual_features = expected;
        }
        let MemoryRequest::CorrectMedia { media, .. } = panel.editor_request().unwrap() else {
            panic!("legacy multimodal correction");
        };
        assert_eq!(media, original);
    }
}
