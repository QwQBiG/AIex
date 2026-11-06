use super::*;
use ai_ex_domain::{NativeReply, NativeRequest};

#[tokio::test]
async fn native_stop_uses_its_own_connection_while_a_setup_request_has_no_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        4096,
    )
    .unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (first, _) = listener.accept().await.unwrap();
        let mut first = BufReader::new(first);
        let mut line = String::new();
        first.read_line(&mut line).await.unwrap();
        let request: ControlRequest = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            request.command,
            ControlCommand::Native {
                request: NativeRequest::Download { .. }
            }
        ));
        entered.send(()).unwrap();
        // Keep this authenticated request unanswered while accepting StopPreview separately.
        let (second, _) = listener.accept().await.unwrap();
        let mut second = BufReader::new(second);
        line.clear();
        second.read_line(&mut line).await.unwrap();
        let request: ControlRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(
            request.command,
            ControlCommand::Native {
                request: NativeRequest::StopPreview
            }
        );
        let reply = ControlResponse::Success {
            request_id: request.request_id,
            payload: ControlPayload::Native(Box::new(NativeReply::Accepted)),
        };
        second
            .get_mut()
            .write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
            .await
            .unwrap();
    });
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (events, received) = mpsc::channel();
    let worker = tokio::spawn(async move {
        run_commands(&client, receiver, &events, &AtomicU64::new(0)).await;
    });
    commands
        .send(WorkerCommand::Native {
            request_id: 31,
            request: NativeRequest::Download {
                package_id: "qwen3-4b".into(),
            },
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .unwrap()
        .unwrap();
    commands
        .send(WorkerCommand::Native {
            request_id: 33,
            request: NativeRequest::PreviewVoice {
                voice_id: "builtin-zh".into(),
                text: "queued preview".into(),
                expected_generation: 0,
            },
        })
        .unwrap();
    commands
        .send(WorkerCommand::Native {
            request_id: 32,
            request: NativeRequest::StopPreview,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut stopped = false;
        let mut discarded = false;
        loop {
            while let Ok(event) = received.try_recv() {
                match event {
                    WorkerEvent::Native {
                        request_id: 32,
                        result: Ok(NativeReply::Accepted),
                    } => stopped = true,
                    WorkerEvent::Native {
                        request_id: 33,
                        result: Err(error),
                    } if error.kind == ai_ex_domain::ErrorKind::InvalidTransition
                        && error.message == "preview cancelled before it was delivered" =>
                    {
                        discarded = true
                    }
                    _ => {}
                }
            }
            if stopped && discarded {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("StopPreview must bypass setup and retire the older queued preview");
    server.await.unwrap();
    drop(commands);
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn device_refresh_does_not_block_native_settings_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        4096,
    )
    .unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (first, _) = listener.accept().await.unwrap();
        let mut first = BufReader::new(first);
        let mut line = String::new();
        first.read_line(&mut line).await.unwrap();
        let request: ControlRequest = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            request.command,
            ControlCommand::Native {
                request: NativeRequest::InputDevices
            }
        ));
        entered.send(()).unwrap();
        let (second, _) = listener.accept().await.unwrap();
        let mut second = BufReader::new(second);
        line.clear();
        second.read_line(&mut line).await.unwrap();
        let request: ControlRequest = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            request.command,
            ControlCommand::Native {
                request: NativeRequest::Settings
            }
        ));
        let reply = ControlResponse::Success {
            request_id: request.request_id,
            payload: ControlPayload::Native(Box::new(NativeReply::Accepted)),
        };
        second
            .get_mut()
            .write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
            .await
            .unwrap();
    });
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (events, received) = mpsc::channel();
    let worker = tokio::spawn(async move {
        run_commands(&client, receiver, &events, &AtomicU64::new(0)).await;
    });
    commands
        .send(WorkerCommand::Native {
            request_id: 1,
            request: NativeRequest::InputDevices,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .unwrap()
        .unwrap();
    commands
        .send(WorkerCommand::Native {
            request_id: 2,
            request: NativeRequest::Settings,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(
                received.try_recv(),
                Ok(WorkerEvent::Native {
                    request_id: 2,
                    result: Ok(NativeReply::Accepted)
                })
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("settings must not wait for device enumeration");
    server.await.unwrap();
    drop(commands);
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap();
}
