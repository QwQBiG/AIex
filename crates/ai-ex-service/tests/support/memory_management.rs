use super::*;
use ai_ex_domain::{
    ErrorKind, MemoryEntry, MemoryPage, MemoryRequest, MemoryResponse, MemorySource,
    PersonaSnapshot,
};
use std::sync::{Arc, atomic::AtomicBool};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Notify, mpsc};

struct ModelServer {
    url: String,
    captured: mpsc::Receiver<serde_json::Value>,
    hold_next: Arc<AtomicBool>,
    release: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl ModelServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sent, captured) = mpsc::channel(32);
        let hold_next = Arc::new(AtomicBool::new(false));
        let hold = hold_next.clone();
        let release = Arc::new(Notify::new());
        let gate = release.clone();
        let task = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    connection = listener.accept() => {
                        let (mut stream, _) = connection.unwrap();
                        let sent = sent.clone();
                        let hold = hold.clone();
                        let gate = gate.clone();
                        handlers.spawn(async move {
                            let (chat, request) = persona_switch::read_request(&mut stream).await;
                            let body = if chat {
                                sent.send(serde_json::from_slice(&request).unwrap()).await.unwrap();
                                if hold.swap(false, Ordering::AcqRel) { gate.notified().await; }
                                "{\"message\":{\"role\":\"assistant\",\"content\":\"acknowledged.\"},\"done\":true}\n"
                            } else { "{\"models\":[]}" };
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len(),
                            );
                            let _ignored = stream.write_all(response.as_bytes()).await;
                        });
                    }
                    result = handlers.join_next(), if !handlers.is_empty() => {
                        result.unwrap().unwrap();
                    }
                }
            }
        });
        Self {
            url,
            captured,
            hold_next,
            release,
            task,
        }
    }

    async fn next(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(3), self.captured.recv())
            .await
            .unwrap()
            .unwrap()
            .to_string()
    }

    async fn turn(&mut self, service: &ServiceProcess, text: &str) -> String {
        persona_switch::complete_turn(service, text).await;
        self.next().await
    }
}

impl Drop for ModelServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn manage(
    service: &ServiceProcess,
    request: MemoryRequest,
) -> Result<MemoryResponse, ai_ex_domain::AppError> {
    match service
        .client
        .send(ControlCommand::Memory { request })
        .await?
    {
        ControlPayload::Memory(reply) => {
            assert!(reply.snapshot.instance_id.is_some());
            Ok(reply.response)
        }
        response => panic!("unexpected memory response: {response:?}"),
    }
}

async fn list(service: &ServiceProcess, profile: &str) -> MemoryPage {
    let MemoryResponse::Page(page) = manage(
        service,
        MemoryRequest::List {
            profile_id: profile.to_owned(),
            query: String::new(),
            kind: None,
            offset: 0,
            limit: 100,
        },
    )
    .await
    .unwrap() else {
        panic!("memory page expected");
    };
    page
}

fn remember(text: &str) -> MemoryRequest {
    MemoryRequest::Remember {
        profile_id: "default".to_owned(),
        text: text.to_owned(),
    }
}

fn correct(entry: &MemoryEntry, text: &str) -> MemoryRequest {
    MemoryRequest::Correct {
        profile_id: entry.profile_id.clone(),
        id: entry.id,
        expected_revision: entry.revision,
        text: text.to_owned(),
    }
}

fn forget(entry: &MemoryEntry) -> MemoryRequest {
    MemoryRequest::Forget {
        profile_id: entry.profile_id.clone(),
        id: entry.id,
        expected_revision: entry.revision,
    }
}

async fn restart(service: &mut ServiceProcess) {
    drop(service.child.stdin.take());
    service.exited().await;
    service.child = Command::new(env!("CARGO_BIN_EXE_ai-ex-service"))
        .current_dir(&service.directory)
        .args(["--config", "service.toml", "--managed"])
        .stdin(Stdio::piped())
        .stdout(File::create(service.directory.join("stdout.log")).unwrap())
        .stderr(File::create(service.directory.join("stderr.log")).unwrap())
        .spawn()
        .unwrap();
    service.ready().await;
}

#[tokio::test]
async fn memory_edits_change_actual_model_context_and_survive_restart() {
    let mut model = ModelServer::start().await;
    let mut service = ServiceProcess::with_memory("--managed", true, &model.url, true);
    service.ready().await;
    manage(&service, remember("饮品偏好：茉莉茶"))
        .await
        .unwrap();
    let request = model.turn(&service, "早上好").await;
    assert!(
        request.contains("茉莉茶"),
        "explicit notes must survive unrelated wording"
    );
    let note = list(&service, "default")
        .await
        .entries
        .into_iter()
        .find(|entry| entry.source == MemorySource::UserNote)
        .unwrap();
    manage(&service, forget(&note)).await.unwrap();
    assert!(!model.turn(&service, "今日安排").await.contains("茉莉茶"));

    model.turn(&service, "My-home-OLD-731").await;
    let original = list(&service, "default")
        .await
        .entries
        .into_iter()
        .find(|entry| entry.user_text == "My-home-OLD-731")
        .unwrap();
    manage(&service, correct(&original, "My-home-NEW-842"))
        .await
        .unwrap();
    let request = model.turn(&service, "全新话题").await;
    assert!(request.contains("My-home-NEW-842"));
    assert!(
        !request.contains("My-home-OLD-731"),
        "old short-term history must be cleared"
    );
    assert_eq!(
        manage(&service, correct(&original, "stale edit"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidTransition
    );
    assert_eq!(
        manage(&service, forget(&original)).await.unwrap_err().kind,
        ErrorKind::InvalidTransition
    );
    restart(&mut service).await;
    let request = model.turn(&service, "喝水").await;
    assert!(request.contains("My-home-NEW-842"));
    assert!(!request.contains("My-home-OLD-731"));
    let corrected = list(&service, "default")
        .await
        .entries
        .into_iter()
        .find(|entry| entry.id == original.id)
        .unwrap();
    assert_eq!(corrected.revision, original.revision + 1);
    assert_eq!(corrected.source, MemorySource::UserCorrection);
    manage(&service, forget(&corrected)).await.unwrap();
    let request = model.turn(&service, "休息").await;
    assert!(!request.contains("My-home-OLD-731") && !request.contains("My-home-NEW-842"));
    restart(&mut service).await;
    let request = model.turn(&service, "星星").await;
    assert!(!request.contains("My-home-OLD-731") && !request.contains("My-home-NEW-842"));
    assert!(!request.contains("茉莉茶"));
    assert!(
        !list(&service, "default")
            .await
            .entries
            .iter()
            .any(|entry| entry.id == original.id)
    );
    model.turn(&service, "scratch-value-901").await;
    let scratch = list(&service, "default")
        .await
        .entries
        .into_iter()
        .find(|entry| entry.user_text == "scratch-value-901")
        .unwrap();
    manage(&service, forget(&scratch)).await.unwrap();
    assert!(
        !model
            .turn(&service, "最后一个话题")
            .await
            .contains("scratch-value-901")
    );
    drop(service.child.stdin.take());
    service.exited().await;
}

#[tokio::test]
async fn memory_profile_checks_prevent_cross_character_edits_and_recall() {
    let mut model = ModelServer::start().await;
    let mut service = ServiceProcess::with_memory("--managed", true, &model.url, true);
    service.ready().await;
    manage(&service, remember("default-private-582"))
        .await
        .unwrap();
    let original = list(&service, "default").await.entries.remove(0);
    service
        .client
        .send(ControlCommand::SetPersona {
            profile: PersonaSnapshot {
                profile_id: "host".to_owned(),
                name: "Host".to_owned(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    assert_eq!(
        manage(&service, remember("must not write old identity"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidTransition
    );
    assert_eq!(
        manage(&service, forget(&original)).await.unwrap_err().kind,
        ErrorKind::InvalidTransition
    );
    assert!(list(&service, "host").await.entries.is_empty());
    let request = model.turn(&service, "unrelated greeting").await;
    assert!(!request.contains("default-private-582"));
    service
        .client
        .send(ControlCommand::SetPersona {
            profile: PersonaSnapshot::default(),
        })
        .await
        .unwrap();
    assert!(
        model
            .turn(&service, "another greeting")
            .await
            .contains("default-private-582")
    );
    drop(service.child.stdin.take());
    service.exited().await;
}

#[tokio::test]
async fn generating_service_rejects_memory_edits_and_accepts_them_after_interrupt() {
    let mut model = ModelServer::start().await;
    let mut service = ServiceProcess::with_memory("--managed", true, &model.url, true);
    service.ready().await;
    model.hold_next.store(true, Ordering::Release);
    service
        .client
        .send(ControlCommand::Submit {
            text: "hold this reply".to_owned(),
        })
        .await
        .unwrap();
    model.next().await;
    assert_eq!(
        manage(&service, remember("not yet"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidTransition
    );
    service
        .client
        .send(ControlCommand::Interrupt {
            reason: "change memory".to_owned(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match manage(&service, remember("available after interrupt")).await {
                Ok(MemoryResponse::Changed) => break,
                Err(error) if error.kind == ErrorKind::InvalidTransition => {
                    tokio::task::yield_now().await
                }
                response => panic!("unexpected response: {response:?}"),
            }
        }
    })
    .await
    .unwrap();
    model.release.notify_one();
    assert!(
        model
            .turn(&service, "new message")
            .await
            .contains("available after interrupt")
    );
    assert!(
        !list(&service, "default")
            .await
            .entries
            .iter()
            .any(|entry| entry.user_text == "not yet")
    );
    drop(service.child.stdin.take());
    service.exited().await;
}

#[tokio::test]
async fn disabled_memory_reports_unavailable_without_creating_a_store() {
    let mut service = ServiceProcess::start("--managed", true);
    service.ready().await;
    let page = list(&service, "default").await;
    assert!(!page.enabled);
    assert!(page.entries.is_empty());
    assert_eq!(
        manage(&service, remember("cannot persist"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    assert!(!service.directory.join("memory.jsonl").exists());
    drop(service.child.stdin.take());
    service.exited().await;
}

#[tokio::test]
async fn knowledge_and_media_control_requests_survive_restart_and_remain_profile_scoped() {
    use ai_ex_domain::{MemoryEntityKind, MemoryMedia, MemoryModality};
    use serde_json::json;

    let mut model = ModelServer::start().await;
    let mut service = ServiceProcess::with_memory("--managed", true, &model.url, true);
    service.ready().await;
    assert_eq!(
        manage(&service, remember("My name is Quinn."))
            .await
            .unwrap(),
        MemoryResponse::Changed,
    );
    let entities = |profile: &str| MemoryRequest::Entities {
        profile_id: profile.to_owned(),
        query: String::new(),
        offset: 0,
        limit: 20,
    };
    let MemoryResponse::Entities(names) = manage(&service, entities("default")).await.unwrap()
    else {
        panic!("entity response expected over the control channel");
    };
    let name = names
        .entities
        .iter()
        .find(|entity| entity.kind == MemoryEntityKind::Name)
        .unwrap();
    assert_eq!(name.current_value, "Quinn");
    assert_eq!(name.total_mentions, 1);
    let evidence = |profile: &str| MemoryRequest::Evidence {
        profile_id: profile.to_owned(),
        key: name.key.clone(),
        offset: 0,
        limit: 20,
    };
    let MemoryResponse::Page(proof) = manage(&service, evidence("default")).await.unwrap() else {
        panic!("evidence record page expected");
    };
    assert_eq!(proof.total, 1);
    assert_eq!(proof.entries[0].id, name.evidence[0].memory_id);
    assert_eq!(proof.entries[0].revision, name.evidence[0].revision);
    assert_eq!(proof.entries[0].source, MemorySource::UserNote);
    assert_eq!(proof.entries[0].user_text, "My name is Quinn.");

    let original_media = MemoryMedia {
        modality: MemoryModality::Visual,
        tags: vec!["stage".to_owned(), "blue".to_owned()],
        visual_features: Some(
            json!({
                "description":"blue lights", "objects":[{"label":"synthetic stage","score":0.9}],
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
        metadata: json!({"source":"synthetic control test","nested":{"retain":[1,2,3]}})
            .as_object()
            .unwrap()
            .clone(),
        importance: 850,
        ..MemoryMedia::default()
    };
    assert_eq!(
        manage(
            &service,
            MemoryRequest::RememberMedia {
                profile_id: "default".to_owned(),
                text: "A synthetic stage with blue lights.".to_owned(),
                media: original_media.clone(),
            }
        )
        .await
        .unwrap(),
        MemoryResponse::Changed,
    );
    let media_query = |profile: &str, tag: &str| MemoryRequest::QueryMedia {
        profile_id: profile.to_owned(),
        query: String::new(),
        modality: Some(MemoryModality::Visual),
        tags: vec!["not-present".to_owned(), tag.to_owned()],
        min_importance: 800,
        offset: 0,
        limit: 20,
    };
    let MemoryResponse::Page(created) = manage(&service, media_query("default", "stage"))
        .await
        .unwrap()
    else {
        panic!("media page expected");
    };
    assert_eq!(
        created.total, 1,
        "control queries preserve tag OR semantics"
    );
    let original = &created.entries[0];
    assert_eq!(
        created.knowledge[&original.id].media.as_ref(),
        Some(&original_media)
    );
    let mut corrected_media = original_media.clone();
    corrected_media.tags = vec!["stage".to_owned(), "green".to_owned()];
    corrected_media.importance = 950;
    corrected_media
        .visual_features
        .as_mut()
        .unwrap()
        .insert("description".to_owned(), json!("green lights"));
    assert_eq!(
        manage(
            &service,
            MemoryRequest::CorrectMedia {
                profile_id: "default".to_owned(),
                id: original.id,
                expected_revision: original.revision,
                text: "A synthetic stage with green lights.".to_owned(),
                media: corrected_media.clone(),
            }
        )
        .await
        .unwrap(),
        MemoryResponse::Changed,
    );
    let MemoryResponse::Page(old_filter) = manage(&service, media_query("default", "blue"))
        .await
        .unwrap()
    else {
        panic!("media page expected");
    };
    assert_eq!(old_filter.total, 0);

    restart(&mut service).await;
    let MemoryResponse::Page(reopened) = manage(&service, media_query("default", "green"))
        .await
        .unwrap()
    else {
        panic!("persisted media page expected");
    };
    assert_eq!(reopened.total, 1);
    assert_eq!(reopened.entries[0].id, original.id);
    assert_eq!(reopened.entries[0].revision, original.revision + 1);
    assert_eq!(reopened.entries[0].source, MemorySource::UserCorrection);
    assert_eq!(
        reopened.entries[0].user_text,
        "A synthetic stage with green lights."
    );
    assert_eq!(
        reopened.knowledge[&original.id].media.as_ref(),
        Some(&corrected_media)
    );
    assert_eq!(
        manage(&service, entities("default")).await.unwrap(),
        MemoryResponse::Entities(names.clone())
    );
    assert_eq!(
        manage(&service, evidence("default")).await.unwrap(),
        MemoryResponse::Page(proof)
    );

    service
        .client
        .send(ControlCommand::SetPersona {
            profile: PersonaSnapshot {
                profile_id: "other".to_owned(),
                name: "Other".to_owned(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let MemoryResponse::Entities(other_names) = manage(&service, entities("other")).await.unwrap()
    else {
        panic!("other character entity response expected");
    };
    assert_eq!(other_names.total, 0);
    for request in [evidence("other"), media_query("other", "stage")] {
        let MemoryResponse::Page(other) = manage(&service, request).await.unwrap() else {
            panic!("other character page expected");
        };
        assert_eq!(other.total, 0);
        assert!(other.knowledge.is_empty());
    }
    assert_eq!(
        manage(&service, media_query("default", "stage"))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidTransition
    );
    assert!(
        matches!(
            model.captured.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ),
        "knowledge management must not invoke chat generation"
    );
    drop(service.child.stdin.take());
    service.exited().await;
}
