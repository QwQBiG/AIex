use super::*;
use ai_ex_control::{ControlRequest, ControlResponse};
use ai_ex_domain::{
    DesktopRuntimeReply, DesktopRuntimeSnapshot, NativeDesktopReply, NativeDesktopSnapshot,
};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc};
use uuid::Uuid;

struct Backend {
    seen: mpsc::UnboundedSender<&'static str>,
    release: Semaphore,
}
impl Backend {
    async fn execute(&self, command: ControlCommand) -> Result<ControlPayload, AppError> {
        let name = match &command {
            ControlCommand::NativeDesktop {
                request: NativeDesktopRequest::Resume { .. },
            } => "resume",
            ControlCommand::NativeDesktop {
                request: NativeDesktopRequest::Stop { .. },
            } => "stop",
            ControlCommand::NativeDesktop {
                request: NativeDesktopRequest::Status,
            } => "status",
            ControlCommand::NativeDesktop {
                request: NativeDesktopRequest::Capture { .. },
            } => "revoked capture",
            ControlCommand::DesktopRuntime {
                request: DesktopRuntimeRequest::DiscoverVts,
            } => "discovery",
            ControlCommand::DesktopRuntime {
                request: DesktopRuntimeRequest::State,
            } => "state",
            _ => return Err(AppError::protocol("unexpected test command")),
        };
        self.seen.send(name).unwrap();
        if name == "resume" {
            self.release.acquire().await.unwrap().forget();
        }
        if name == "discovery" {
            return std::future::pending().await;
        }
        if name == "state" {
            return Ok(ControlPayload::DesktopRuntime(Box::new(
                DesktopRuntimeReply::State(DesktopRuntimeSnapshot {
                    instance_id: Uuid::nil(),
                    revision: 0,
                    settings: Default::default(),
                    hotkeys_registered: false,
                    hotkeys_error: false,
                    proactive_available: true,
                    vts_discovery_available: true,
                }),
            )));
        }
        Ok(ControlPayload::NativeDesktop(Box::new(
            NativeDesktopReply {
                snapshot: NativeDesktopSnapshot {
                    instance_id: Uuid::nil(),
                    revision: 0,
                    execution_mode: "native".into(),
                    available: true,
                    session: None,
                },
                windows: Vec::new(),
                vision_preview: None,
                vision_proposal: None,
                vision_operation: None,
                vision_preparation: None,
            },
        )))
    }
}

#[tokio::test]
async fn stalled_native_and_discovery_do_not_hold_stop_status_or_caption_confirmation() {
    let (seen_tx, mut seen) = mpsc::unbounded_channel();
    let backend = Arc::new(Backend {
        seen: seen_tx,
        release: Semaphore::new(0),
    });
    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &server.local_addr().unwrap().to_string(),
        "x".repeat(32),
        8192,
    )
    .unwrap();
    let server_backend = backend.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = server.accept().await.unwrap();
            let backend = server_backend.clone();
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let request: ControlRequest = serde_json::from_str(&line).unwrap();
                assert_eq!(request.token, "x".repeat(32));
                let payload = backend.execute(request.command).await.unwrap();
                let response = ControlResponse::Success {
                    request_id: request.request_id,
                    payload,
                };
                let mut bytes = serde_json::to_vec(&response).unwrap();
                bytes.push(b'\n');
                stream.get_mut().write_all(&bytes).await.unwrap();
            });
        }
    });
    let (commands, receiver) = mpsc::unbounded_channel();
    let (events, result) = std::sync::mpsc::channel();
    let worker = tokio::spawn(async move {
        crate::worker::run_commands(&client, receiver, &events, &AtomicU64::new(0)).await;
    });
    let native = |id, request| WorkerCommand::NativeDesktop {
        request_id: id,
        request,
    };
    let instance_id = Uuid::nil();
    let session_id = Uuid::new_v4();
    commands
        .send(native(
            1,
            NativeDesktopRequest::Resume {
                instance_id,
                expected_revision: 0,
                session_id,
            },
        ))
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), seen.recv())
            .await
            .unwrap(),
        Some("resume")
    );
    commands
        .send(native(
            2,
            NativeDesktopRequest::Capture {
                instance_id,
                expected_revision: 0,
                session_id,
            },
        ))
        .unwrap();
    commands
        .send(native(
            3,
            NativeDesktopRequest::Stop {
                instance_id,
                expected_revision: 0,
                session_id,
            },
        ))
        .unwrap();
    commands
        .send(native(4, NativeDesktopRequest::Status))
        .unwrap();
    commands
        .send(WorkerCommand::DesktopRuntime {
            request_id: 5,
            request: DesktopRuntimeRequest::DiscoverVts,
        })
        .unwrap();
    commands
        .send(WorkerCommand::DesktopRuntime {
            request_id: 6,
            request: DesktopRuntimeRequest::State,
        })
        .unwrap();
    let mut observed = Vec::new();
    for _ in 0..4 {
        observed.push(
            tokio::time::timeout(Duration::from_secs(3), seen.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    for expected in ["stop", "status", "discovery", "state"] {
        assert!(observed.contains(&expected));
    }
    backend.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            while let Ok(event) = result.try_recv() {
                if let WorkerEvent::NativeDesktop {
                    request_id: 2,
                    result: Err(error),
                } = event
                {
                    assert!(error.message.contains("revoked"));
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(seen.try_recv().is_err());
    drop(commands);
    tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn cancelled_or_expired_preview_is_rejected_before_network_send() {
    let client = ControlClient::new("127.0.0.1:1", "x".repeat(32), 8192).unwrap();
    let (commands, input) = mpsc::channel(2);
    let (events, replies) = std::sync::mpsc::channel();
    let guard = Arc::new(AtomicU64::new(1));
    for (request_id, generation, send_before) in [
        (1, 0, std::time::Instant::now() + Duration::from_secs(2)),
        (2, 1, std::time::Instant::now()),
    ] {
        commands
            .send(Queued {
                command: WorkerCommand::NativeDesktopGuarded {
                    request_id,
                    request: NativeDesktopRequest::PreviewVision {
                        instance_id: Uuid::new_v4(),
                        expected_revision: 1,
                        session_id: Uuid::new_v4(),
                        operation_id: Uuid::new_v4(),
                        goal: "合成目标".into(),
                    },
                    generation,
                    guard: Arc::clone(&guard),
                    send_before,
                },
                epoch: 0,
            })
            .await
            .unwrap();
    }
    drop(commands);
    execute(&client, input, &events, None).await;
    for id in [1, 2] {
        let WorkerEvent::NativeDesktop {
            request_id,
            result: Err(error),
        } = replies.recv().unwrap()
        else {
            panic!("preview must be rejected")
        };
        assert_eq!(request_id, id);
        assert!(error.message.contains("已取消或等待过久"));
    }
}

#[tokio::test]
async fn confirmation_requires_a_current_guard_before_network_send() {
    let client = ControlClient::new("127.0.0.1:1", "x".repeat(32), 8192).unwrap();
    let (commands, input) = mpsc::channel(3);
    let (events, replies) = std::sync::mpsc::channel();
    let guard = Arc::new(AtomicU64::new(1));
    let request = NativeDesktopRequest::ConfirmVisionProposal {
        instance_id: Uuid::new_v4(),
        expected_revision: 1,
        session_id: Uuid::new_v4(),
        proposal_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
    };
    for (request_id, generation, send_before) in [
        (1, 0, std::time::Instant::now() + Duration::from_secs(2)),
        (2, 1, std::time::Instant::now()),
    ] {
        commands
            .send(Queued {
                command: WorkerCommand::NativeDesktopGuarded {
                    request_id,
                    request: request.clone(),
                    generation,
                    guard: Arc::clone(&guard),
                    send_before,
                },
                epoch: 0,
            })
            .await
            .unwrap();
    }
    commands
        .send(Queued {
            command: WorkerCommand::NativeDesktop {
                request_id: 3,
                request,
            },
            epoch: 0,
        })
        .await
        .unwrap();
    drop(commands);
    execute(&client, input, &events, None).await;
    for id in 1..=3 {
        let WorkerEvent::NativeDesktop {
            request_id,
            result: Err(error),
        } = replies.recv().unwrap()
        else {
            panic!("confirmation must be rejected")
        };
        assert_eq!(request_id, id);
        assert!(error.message.contains(if id == 3 {
            "受控通道"
        } else {
            "已取消或等待过久"
        }));
    }
}
