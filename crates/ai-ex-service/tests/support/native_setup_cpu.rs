use std::path::{Path, PathBuf};
use std::time::Duration;

use ai_ex_control::{ControlCommand, ControlPayload};
use ai_ex_domain::{
    MemoryRequest, MemoryResponse, NativeOperationPhase, NativePackageState, NativeReply,
    NativeRequest, SystemEvent,
};
use ai_ex_observability::SequencedEvent;

use super::support::NativeService;

fn source(name: &str, directory: bool) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("explicit {name} is required")),
    );
    assert!(path.is_absolute());
    assert!(
        if directory {
            path.is_dir()
        } else {
            path.is_file()
        },
        "missing verified input: {}",
        path.display()
    );
    path
}

async fn import(service: &NativeService, package_id: &str, path: &Path) {
    let NativeReply::Operation(operation) = service
        .request(NativeRequest::Import {
            package_id: package_id.into(),
            path: path.to_string_lossy().into_owned(),
        })
        .await
    else {
        panic!("offline import operation");
    };
    let operation = service
        .operation(operation.id, Duration::from_secs(180))
        .await;
    assert_eq!(
        operation.phase,
        NativeOperationPhase::Completed,
        "{operation:?}"
    );
    let package = service
        .packages()
        .await
        .into_iter()
        .find(|package| package.id == package_id)
        .unwrap();
    assert!(
        package.installed
            && matches!(
                package.state,
                NativePackageState::Ready | NativePackageState::InUse
            )
    );
}

async fn events(service: &NativeService, after: u64) -> Vec<SequencedEvent> {
    let ControlPayload::Events(events) = service
        .client
        .send(ControlCommand::Events { after, limit: 1000 })
        .await
        .unwrap()
    else {
        panic!("events reply");
    };
    events
}

async fn memory(service: &NativeService, request: MemoryRequest) -> MemoryResponse {
    let ControlPayload::Memory(reply) = service
        .client
        .send(ControlCommand::Memory { request })
        .await
        .unwrap()
    else {
        panic!("memory reply");
    };
    reply.response
}

fn memory_record(service: &NativeService, user_text: &str) -> serde_json::Value {
    let persisted = std::fs::read_to_string(service.directory.join("memory.jsonl")).unwrap();
    persisted
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|record| {
            record["user_text"]
                .as_str()
                .is_some_and(|text| text == user_text)
        })
        .expect("the requested synthetic memory is durably present")
}

fn assert_native_vector(service: &NativeService, user_text: &str) {
    let record = memory_record(service, user_text);
    assert_eq!(
        record["embedding"]["space"],
        ai_ex_native_models::BGE_SPACE,
        "explicitly prepared memory must contain the actual native vector and space"
    );
    assert_eq!(
        record["embedding"]["values"].as_array().map(Vec::len),
        Some(512)
    );
}

async fn finished_turn(service: &NativeService, text: &str) -> String {
    let mut after = events(service, 0)
        .await
        .last()
        .map_or(0, |event| event.sequence);
    assert!(matches!(
        service
            .client
            .send(ControlCommand::Submit { text: text.into() })
            .await
            .unwrap(),
        ControlPayload::Accepted
    ));
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            for item in events(service, after).await {
                after = item.sequence;
                match item.event {
                    SystemEvent::TurnFinished { full_text, .. } => return full_text,
                    SystemEvent::Fault { message } => panic!(
                        "native CPU turn failed: {message}; {}",
                        service.diagnostics()
                    ),
                    _ => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native CPU turn completes")
}

async fn interrupt_turn(service: &NativeService) -> u128 {
    let mut after = events(service, 0)
        .await
        .last()
        .map_or(0, |event| event.sequence);
    service
        .client
        .send(ControlCommand::Submit {
            text: "请用中文详细写一篇至少一千字的合成旅行故事。".into(),
        })
        .await
        .unwrap();
    let turn = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            for item in events(service, after).await {
                after = item.sequence;
                match item.event {
                    SystemEvent::ModelChunk { turn_id, .. } => return turn_id,
                    SystemEvent::Fault { message } => {
                        panic!("long turn failed before interrupt: {message}")
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("actual CPU generation starts");
    let interrupt_started = std::time::Instant::now();
    assert!(matches!(
        service
            .client
            .send(ControlCommand::Interrupt {
                reason: "synthetic native lifecycle check".into()
            })
            .await
            .unwrap(),
        ControlPayload::Accepted
    ));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            for item in events(service, after).await {
                after = item.sequence;
                if matches!(item.event, SystemEvent::TurnInterrupted { turn_id, .. } if turn_id == turn) { return; }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("interrupted turn is confirmed by the service");
    let confirmed_ms = interrupt_started.elapsed().as_millis();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!events(service, after).await.iter().any(|item|
        matches!(item.event, SystemEvent::ModelChunk { turn_id, .. } | SystemEvent::TurnFinished { turn_id, .. } if turn_id == turn)));
    let ControlPayload::Snapshot(snapshot) =
        service.client.send(ControlCommand::Status).await.unwrap()
    else {
        panic!("status snapshot");
    };
    assert!(snapshot.active_turn.is_none() && !snapshot.playback.active);
    confirmed_ms
}

#[tokio::test]
#[ignore = "requires explicitly supplied verified CPU runtime ZIP, dialogue package and BGE package; never opens audio devices"]
async fn explicit_cpu_native_control_text_memory_interrupt_and_reopen() {
    let runtime = source("AIEX_NATIVE_TEST_RUNTIME_ZIP", false);
    let dialogue = source("AIEX_NATIVE_TEST_DIALOGUE_SOURCE", true);
    let embedding = source("AIEX_NATIVE_TEST_BGE_SOURCE", true);
    let mut service = NativeService::new(true);
    service.ready(Duration::from_secs(20)).await;
    service.request(NativeRequest::CreateDefaults).await;
    for (id, path) in [
        ("native-cpu", runtime.as_path()),
        ("qwen3-4b", dialogue.as_path()),
        ("bge-small-zh-v1.5", embedding.as_path()),
    ] {
        import(&service, id, path).await;
    }
    service.settings().await;
    let note = "合成偏好：喜欢淡茶";
    let cold_note_started = std::time::Instant::now();
    memory(
        &service,
        MemoryRequest::Remember {
            profile_id: "synthetic-companion".into(),
            text: note.into(),
        },
    )
    .await;
    let cold_note_ms = cold_note_started.elapsed().as_millis();
    let cold_record = memory_record(&service, note);
    let cold_note_vector_ready = cold_record["embedding"]["space"]
        == ai_ex_native_models::BGE_SPACE
        && cold_record["embedding"]["values"]
            .as_array()
            .is_some_and(|values| values.len() == 512);
    println!("cold_note_ms={cold_note_ms}; cold_note_vector_ready={cold_note_vector_ready}");
    service.close().await;
    let (reindex_ms, reindex_stdout) = service.reindex().await;
    assert!(
        reindex_stdout.contains("Reindexed 1 memory record(s)"),
        "{reindex_stdout}"
    );
    assert_native_vector(&service, note);
    let rebuilt = memory_record(&service, note);
    for field in ["id", "profile_id", "user_text", "source", "revision"] {
        assert_eq!(
            rebuilt[field], cold_record[field],
            "reindex preserves {field}"
        );
    }
    service.start();
    service.ready(Duration::from_secs(120)).await;
    service.settings().await;
    let first_started = std::time::Instant::now();
    let first_answer = finished_turn(&service, "请只回答中文词语：青鹭。不要解释。").await;
    let first_turn_ms = first_started.elapsed().as_millis();
    assert!(
        first_answer.contains("青鹭")
            && first_answer.chars().count() <= 12
            && !first_answer.contains("<think>"),
        "unfiltered native answer: {first_answer:?}"
    );
    let MemoryResponse::Page(page) = memory(
        &service,
        MemoryRequest::List {
            profile_id: "synthetic-companion".into(),
            query: "".into(),
            kind: None,
            offset: 0,
            limit: 100,
        },
    )
    .await
    else {
        panic!("memory page");
    };
    assert!(
        page.entries
            .iter()
            .any(|entry| entry.user_text.contains(note))
    );
    assert!(
        page.entries
            .iter()
            .any(|entry| entry.assistant_text.contains("青鹭"))
    );
    assert_native_vector(&service, "请只回答中文词语：青鹭。不要解释。");
    let interrupt_confirmed_ms = interrupt_turn(&service).await;
    service.close().await;
    service.start();
    service.ready(Duration::from_secs(120)).await;
    service.settings().await;
    assert_native_vector(&service, note);
    let MemoryResponse::Page(page) = memory(
        &service,
        MemoryRequest::List {
            profile_id: "synthetic-companion".into(),
            query: "".into(),
            kind: None,
            offset: 0,
            limit: 100,
        },
    )
    .await
    else {
        panic!("restored memory page");
    };
    assert!(
        page.entries
            .iter()
            .any(|entry| entry.user_text.contains(note))
    );
    let reopened_started = std::time::Instant::now();
    let answer = finished_turn(&service, "请只回答中文词语：晚安。不要解释。").await;
    let reopened_turn_ms = reopened_started.elapsed().as_millis();
    assert!(answer.contains("晚安") && answer.chars().count() <= 12 && !answer.contains("<think>"));
    service.close().await;
    let report = serde_json::json!({
        "status":"passed", "devices_enabled":false, "network_downloads":false,
        "first_turn":{"input":"请只回答中文词语：青鹭。不要解释。","answer":first_answer,"elapsed_ms":first_turn_ms},
        "reopened_turn":{"input":"请只回答中文词语：晚安。不要解释。","answer":answer,"elapsed_ms":reopened_turn_ms},
        "memory":{"reopened_note":note,"cold_note_ms":cold_note_ms,"cold_note_vector_ready":cold_note_vector_ready,
            "explicit_reindex_ms":reindex_ms,"explicit_reindex_stdout":reindex_stdout,
            "verified_vector_space":ai_ex_native_models::BGE_SPACE,"verified_vector_dimensions":512},
        "interrupt":{"confirmed_ms":interrupt_confirmed_ms,"old_turn_chunks_after_confirmation":false},
        "owned_service_exited_and_locks_released":true,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Some(path) = std::env::var_os("AIEX_NATIVE_TEST_REPORT") {
        use std::io::Write;
        let path = PathBuf::from(path);
        assert!(path.is_absolute());
        let mut output = std::fs::File::create_new(path).unwrap();
        output
            .write_all(&serde_json::to_vec_pretty(&report).unwrap())
            .unwrap();
        output.sync_all().unwrap();
    }
}
