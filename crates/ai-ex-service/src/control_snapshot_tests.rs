use ai_ex_control::{ControlPayload, ControlResponse, MemoryReply};
use ai_ex_domain::{
    ConversationState, Emotion, MemoryEntry, MemoryKind, MemoryPage, MemoryResponse, MemorySource,
    SpeechPlaybackSnapshot, TurnId,
};

use super::*;

#[test]
fn ordinary_control_snapshot_is_unchanged() {
    let snapshot = RuntimeSnapshot {
        last_fault: Some("a short diagnostic".to_owned()),
        playback: SpeechPlaybackSnapshot {
            text: "你好，今天过得怎么样？".to_owned(),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(bounded(snapshot.clone()), snapshot);
}

#[test]
fn long_unicode_and_escaped_text_preserve_identity_and_fit_complete_control_replies() {
    let id = "05348654-314d-4f5b-bb70-665984b9f06f".parse().unwrap();
    let original = RuntimeSnapshot {
        state: ConversationState::Speaking,
        active_turn: Some(TurnId::new()),
        instance_id: Some(id),
        last_sequence: u64::MAX,
        last_fault: Some("诊断𠮷\"\\\u{0001}\n".repeat(12_000)),
        playback: SpeechPlaybackSnapshot {
            turn_id: Some(TurnId::new()),
            sentence_id: Some(id),
            active: true,
            mouth_level: 712,
            position_ms: 812,
            duration_ms: 1821,
            emotion: Some(Emotion::Happy),
            text: "🙂播放\u{0000}\"\\\t".repeat(12_000),
        },
        ..Default::default()
    };
    let snapshot = bounded(original.clone());
    assert!(serde_json::to_vec(&snapshot).unwrap().len() <= SNAPSHOT_BYTES);
    assert!(snapshot.last_fault.as_ref().unwrap().ends_with('…'));
    assert!(snapshot.playback.text.ends_with('…'));
    let mut metadata = original;
    metadata.last_fault = snapshot.last_fault.clone();
    metadata.playback.text = snapshot.playback.text.clone();
    assert_eq!(snapshot, metadata);

    let status = ControlResponse::Success {
        request_id: id,
        payload: ControlPayload::Snapshot(snapshot.clone()),
    };
    let bytes = serde_json::to_vec(&status).unwrap();
    assert!(bytes.len() <= SNAPSHOT_BYTES + 256);
    assert_eq!(
        serde_json::from_slice::<ControlResponse>(&bytes).unwrap(),
        status
    );

    let page = MemoryPage {
        profile_id: "default".to_owned(),
        enabled: true,
        total: 1,
        offset: 0,
        entries: vec![MemoryEntry {
            id,
            profile_id: "default".to_owned(),
            turn_id: TurnId::new(),
            created_ms: 1,
            updated_ms: None,
            kind: MemoryKind::Conversation,
            user_text: "x".repeat(47_000),
            assistant_text: String::new(),
            source: MemorySource::Automatic,
            revision: 1,
        }],
        truncated_ids: Vec::new(),
        knowledge: Default::default(),
    };
    assert!(serde_json::to_vec(&page).unwrap().len() <= 48 * 1024);
    for response in [MemoryResponse::Page(page), MemoryResponse::Changed] {
        let reply = ControlResponse::Success {
            request_id: id,
            payload: ControlPayload::Memory(Box::new(MemoryReply {
                response,
                snapshot: snapshot.clone(),
            })),
        };
        let bytes = serde_json::to_vec(&reply).unwrap();
        assert!(bytes.len() + 1 < 64 * 1024);
        assert_eq!(
            serde_json::from_slice::<ControlResponse>(&bytes).unwrap(),
            reply
        );
    }
}

#[test]
fn string_budgets_use_serialized_bytes_at_utf8_and_escape_boundaries() {
    for text in [
        "你好🙂\"\\\u{0000}\n",
        "\u{0001}".repeat(100).as_str(),
        "ordinary text",
    ] {
        for budget in 0..64 {
            let limited = fit_text(text.to_owned(), budget);
            let bytes = serde_json::to_vec(&limited).unwrap();
            assert!(bytes.len() <= budget + 2, "budget {budget}: {bytes:?}");
            if let Some(prefix) = limited.strip_suffix('…') {
                assert!(text.starts_with(prefix));
            } else if !limited.is_empty() {
                assert_eq!(limited, text);
            }
        }
    }
}
