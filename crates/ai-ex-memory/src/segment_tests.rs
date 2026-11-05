use super::*;
use ai_ex_domain::{MemoryRequest, MemoryResponse, MemoryTextField, MemoryTextSegment};

fn request(entry: &MemoryEntry, field: MemoryTextField, offset: usize) -> MemoryRequest {
    MemoryRequest::ReadSegment {
        profile_id: entry.profile_id.clone(),
        id: entry.id,
        expected_revision: entry.revision,
        field,
        offset,
    }
}

async fn segment(store: &mut MemoryStore, request: MemoryRequest) -> MemoryTextSegment {
    let MemoryResponse::Segment(segment) = store.manage(request).await.unwrap() else {
        panic!("memory segment expected");
    };
    segment
}

#[tokio::test]
async fn segments_reconstruct_utf8_and_escaped_text_after_reopening() {
    let path = std::env::temp_dir().join(format!("memory-segments-{}.jsonl", Uuid::new_v4()));
    let mut store = MemoryStore::open(&path).await.unwrap();
    let user = format!("开始{}末尾", "茶\n\u{0001}😀".repeat(5000));
    let assistant = "你好\"\\\u{0001}".repeat(3000);
    store
        .remember(TurnId::new(), user.clone(), assistant.clone())
        .await
        .unwrap();
    drop(store);
    let mut store = MemoryStore::open(&path).await.unwrap();
    let MemoryResponse::Page(page) = store
        .manage(MemoryRequest::List {
            profile_id: "default".into(),
            query: String::new(),
            kind: None,
            offset: 0,
            limit: 1,
        })
        .await
        .unwrap()
    else {
        panic!("memory page expected");
    };
    let entry = &page.entries[0];
    assert_eq!(page.truncated_ids, vec![entry.id]);
    for (field, expected) in [
        (MemoryTextField::User, &user),
        (MemoryTextField::Assistant, &assistant),
    ] {
        let mut offset = 0;
        let mut actual = String::new();
        loop {
            let part = segment(&mut store, request(entry, field, offset)).await;
            assert_eq!(
                (part.id, part.revision, part.field, part.offset),
                (entry.id, entry.revision, field, offset)
            );
            assert!(part.text.len() <= 4096);
            assert!(
                serde_json::to_vec(&MemoryResponse::Segment(part.clone()))
                    .unwrap()
                    .len()
                    < 48 * 1024
            );
            actual.push_str(&part.text);
            match part.next_offset {
                Some(next) => {
                    assert!(next > offset);
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(&actual, expected);
    }
    drop(store);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn segment_requires_current_profile_revision_and_utf8_offset() {
    let path = std::env::temp_dir().join(format!("memory-segment-access-{}.jsonl", Uuid::new_v4()));
    let mut store = MemoryStore::open(&path).await.unwrap();
    store
        .remember(TurnId::new(), "茶".into(), String::new())
        .await
        .unwrap();
    let MemoryResponse::Page(page) = store
        .manage(MemoryRequest::List {
            profile_id: "default".into(),
            query: String::new(),
            kind: None,
            offset: 0,
            limit: 1,
        })
        .await
        .unwrap()
    else {
        panic!("memory page expected")
    };
    let entry = &page.entries[0];
    assert_eq!(
        segment(&mut store, request(entry, MemoryTextField::Assistant, 0))
            .await
            .text,
        ""
    );
    assert!(
        store
            .manage(request(entry, MemoryTextField::User, 1))
            .await
            .is_err()
    );
    assert!(
        store
            .manage(request(entry, MemoryTextField::User, usize::MAX))
            .await
            .is_err()
    );
    store.select_profile("other").await.unwrap();
    assert!(
        store
            .manage(request(entry, MemoryTextField::User, 0))
            .await
            .is_err()
    );
    store.select_profile("default").await.unwrap();
    store
        .manage(MemoryRequest::Correct {
            profile_id: entry.profile_id.clone(),
            id: entry.id,
            expected_revision: entry.revision,
            text: "更新".into(),
        })
        .await
        .unwrap();
    assert!(
        store
            .manage(request(entry, MemoryTextField::User, 0))
            .await
            .is_err()
    );
    let mut current = entry.clone();
    current.revision += 1;
    store
        .manage(MemoryRequest::Forget {
            profile_id: current.profile_id.clone(),
            id: current.id,
            expected_revision: current.revision,
        })
        .await
        .unwrap();
    assert!(
        store
            .manage(request(&current, MemoryTextField::User, 0))
            .await
            .is_err()
    );
    drop(store);
    super::tests::remove_test_files(&path).await;
}

#[tokio::test]
async fn control_character_segment_stays_within_transport_budget() {
    let path = std::env::temp_dir().join(format!("memory-segment-escape-{}.jsonl", Uuid::new_v4()));
    let mut store = MemoryStore::open(&path).await.unwrap();
    let profile = "\u{0001}".repeat(128);
    store.select_profile(&profile).await.unwrap();
    store
        .remember(TurnId::new(), "\u{0001}".repeat(5000), String::new())
        .await
        .unwrap();
    let MemoryResponse::Page(page) = store
        .manage(MemoryRequest::List {
            profile_id: profile,
            query: String::new(),
            kind: None,
            offset: 0,
            limit: 1,
        })
        .await
        .unwrap()
    else {
        panic!("memory page expected")
    };
    let part = segment(
        &mut store,
        request(&page.entries[0], MemoryTextField::User, 0),
    )
    .await;
    assert_eq!(part.text.len(), 4096);
    assert_eq!(part.next_offset, Some(4096));
    assert!(
        serde_json::to_vec(&MemoryResponse::Segment(part))
            .unwrap()
            .len()
            < 48 * 1024
    );
    drop(store);
    super::tests::remove_test_files(&path).await;
}
