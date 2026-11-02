use super::*;
use ai_ex_control::{ControlRequest, ControlResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[path = "worker_memory_tests.rs"]
mod memory_tests;
#[path = "worker_native_tests.rs"]
mod native_tests;

async fn scripted_client(
    exchanges: Vec<(ControlCommand, ControlPayload)>,
) -> (ControlClient, tokio::task::JoinHandle<()>) {
    scripted_responses(
        exchanges
            .into_iter()
            .map(|(command, payload)| (command, Ok(payload)))
            .collect(),
    )
    .await
}

async fn scripted_responses(
    exchanges: Vec<(ControlCommand, Result<ControlPayload, AppError>)>,
) -> (ControlClient, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        65_536,
    )
    .unwrap();
    let server = tokio::spawn(async move {
        for (expected, payload) in exchanges {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let request: ControlRequest = serde_json::from_str(&line).unwrap();
            assert_eq!(request.command, expected);
            let response = match payload {
                Ok(payload) => ControlResponse::Success {
                    request_id: request.request_id,
                    payload,
                },
                Err(error) => ControlResponse::Failure {
                    request_id: Some(request.request_id),
                    error,
                },
            };
            stream
                .get_mut()
                .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
                .await
                .unwrap();
        }
    });
    (client, server)
}

async fn wait_event(receiver: &Receiver<WorkerEvent>, matches: impl Fn(&WorkerEvent) -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            while let Ok(event) = receiver.try_recv() {
                if matches(&event) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("expected worker event");
}

fn reduce_received(receiver: &Receiver<WorkerEvent>, state: &mut ai_ex_ui_model::UiState) {
    for event in receiver.try_iter() {
        match event {
            WorkerEvent::Snapshot(snapshot) => state.apply_snapshot(snapshot),
            WorkerEvent::HistoryGap(snapshot) => state.recover_snapshot(snapshot),
            WorkerEvent::Events(events) => {
                for event in events {
                    assert_eq!(
                        state.apply_event(event),
                        ai_ex_ui_model::ApplyOutcome::Applied
                    );
                }
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn bounded_regular_queue_returns_rejected_text_and_keeps_both_urgent_intents() {
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (regular, mut waiting) = tokio::sync::mpsc::channel(1);
    let (urgent, priority) = tokio::sync::watch::channel(UrgentCommands::default());
    let (events, received) = mpsc::channel();
    commands
        .send(WorkerCommand::Submit("accepted".to_owned()))
        .unwrap();
    commands
        .send(WorkerCommand::Submit("return this".to_owned()))
        .unwrap();
    commands
        .send(WorkerCommand::SetPersona(PersonaSnapshot::default()))
        .unwrap();
    for _ in 0..1000 {
        commands.send(WorkerCommand::Interrupt).unwrap();
        commands.send(WorkerCommand::EmergencyStop).unwrap();
    }
    drop(commands);
    let epoch = AtomicU64::new(0);
    route_commands(receiver, regular, urgent, &events, &epoch).await;
    assert!(waiting.try_recv().is_ok());
    assert!(waiting.try_recv().is_err());
    assert_eq!(priority.borrow().interrupt, 1000);
    assert_eq!(priority.borrow().stop, 1000);
    assert_eq!(epoch.load(Ordering::Acquire), 2000);
    let received = received.try_iter().collect::<Vec<_>>();
    assert!(received.iter().any(|event| matches!(event,
        WorkerEvent::SubmitRejected { text, .. } if text == "return this")));
    assert!(
        received
            .iter()
            .any(|event| matches!(event, WorkerEvent::PersonaApplyFailed(_)))
    );
}

#[tokio::test]
async fn snapshot_refresh_delivers_text_before_advancing_its_sequence() {
    use ai_ex_domain::{ConversationState, SystemEvent, TurnId};
    for extra_event in [false, true] {
        let turn_id = TurnId::new();
        let mut items = vec![
            SequencedEvent {
                sequence: 1,
                event: SystemEvent::TurnStarted {
                    turn_id,
                    user_text: "hello".to_owned(),
                },
            },
            SequencedEvent {
                sequence: 2,
                event: SystemEvent::ModelChunk {
                    turn_id,
                    text: "visible reply".to_owned(),
                },
            },
        ];
        if extra_event {
            items.push(SequencedEvent {
                sequence: 3,
                event: SystemEvent::StateChanged {
                    from: ConversationState::Thinking,
                    to: ConversationState::Speaking,
                },
            });
        }
        let (client, server) = scripted_client(vec![(
            ControlCommand::Events {
                after: 0,
                limit: 256,
            },
            ControlPayload::Events(items),
        )])
        .await;
        let (events, receiver) = mpsc::channel();
        let mut cursor = 0;
        let snapshot = RuntimeSnapshot {
            last_sequence: 2,
            turns_started: 1,
            active_turn: Some(turn_id),
            state: ConversationState::Thinking,
            ..Default::default()
        };
        assert!(
            synchronize_snapshot(&client, &events, &mut cursor, snapshot, &mut None)
                .await
                .unwrap()
        );
        let mut state = ai_ex_ui_model::UiState::new(10).unwrap();
        reduce_received(&receiver, &mut state);
        assert_eq!(state.turns[0].user_text, "hello");
        assert_eq!(state.turns[0].assistant_text, "visible reply");
        assert_eq!(state.runtime.turns_started, 1);
        assert_eq!(state.runtime.last_sequence, if extra_event { 3 } else { 2 });
        assert_eq!(
            state.runtime.state,
            if extra_event {
                ConversationState::Speaking
            } else {
                ConversationState::Thinking
            }
        );
        assert!(!state.needs_resync);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn reconnect_replays_from_its_existing_cursor_across_multiple_pages() {
    use ai_ex_domain::{SystemEvent, TurnId};
    let turn_id = TurnId::new();
    let mut state = ai_ex_ui_model::UiState::new(10).unwrap();
    state.apply_event(SequencedEvent {
        sequence: 1,
        event: SystemEvent::TurnStarted {
            turn_id,
            user_text: "before disconnection".to_owned(),
        },
    });
    state.apply_event(SequencedEvent {
        sequence: 2,
        event: SystemEvent::ModelChunk {
            turn_id,
            text: "first ".to_owned(),
        },
    });
    let (client, server) = scripted_client(vec![
        (
            ControlCommand::Events {
                after: 2,
                limit: 256,
            },
            ControlPayload::Events(vec![SequencedEvent {
                sequence: 3,
                event: SystemEvent::ModelChunk {
                    turn_id,
                    text: "second".to_owned(),
                },
            }]),
        ),
        (
            ControlCommand::Events {
                after: 3,
                limit: 256,
            },
            ControlPayload::Events(vec![SequencedEvent {
                sequence: 4,
                event: SystemEvent::TurnFinished {
                    turn_id,
                    full_text: "first second".to_owned(),
                },
            }]),
        ),
    ])
    .await;
    let (events, receiver) = mpsc::channel();
    let mut cursor = 2;
    let snapshot = RuntimeSnapshot {
        last_sequence: 4,
        turns_started: 1,
        turns_completed: 1,
        ..Default::default()
    };
    assert!(
        synchronize_snapshot(&client, &events, &mut cursor, snapshot.clone(), &mut None)
            .await
            .unwrap()
    );
    reduce_received(&receiver, &mut state);
    assert_eq!(state.runtime.last_sequence, 3);
    assert_eq!(state.runtime.turns_completed, 0);
    assert_eq!(state.turns[0].assistant_text, "first second");
    assert!(
        synchronize_snapshot(&client, &events, &mut cursor, snapshot, &mut None)
            .await
            .unwrap()
    );
    reduce_received(&receiver, &mut state);
    assert_eq!(state.runtime.last_sequence, 4);
    assert_eq!(state.runtime.turns_started, 1);
    assert_eq!(state.runtime.turns_completed, 1);
    assert_eq!(state.turns[0].status, ai_ex_ui_model::TurnStatus::Completed);
    server.await.unwrap();
}

#[tokio::test]
async fn persona_refresh_discards_responses_that_overlap_an_apply() {
    for changed in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = ControlClient::new(
            &listener.local_addr().unwrap().to_string(),
            "x".repeat(32),
            4096,
        )
        .unwrap();
        let epoch = std::sync::Arc::new(AtomicU64::new(0));
        let server_epoch = epoch.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let request: ControlRequest = serde_json::from_str(&line).unwrap();
            if changed {
                server_epoch.fetch_add(2, Ordering::AcqRel);
            }
            let response = ControlResponse::Success {
                request_id: request.request_id,
                payload: ControlPayload::Persona(PersonaSnapshot::default()),
            };
            stream
                .get_mut()
                .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
                .await
                .unwrap();
        });
        let result = fetch_persona_current(&client, &epoch).await.unwrap();
        assert_eq!(result.is_none(), changed);
        server.await.unwrap();
        epoch.store(3, Ordering::Release);
        assert!(
            fetch_persona_current(&client, &epoch)
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn stalled_poll_does_not_block_interrupt_stop_or_worker_exit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        4096,
    )
    .unwrap();
    let (poll_started, started) = tokio::sync::oneshot::channel();
    let (release_server, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stalled = BufReader::new(stream);
        let mut line = String::new();
        stalled.read_line(&mut line).await.unwrap();
        let request: ControlRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(request.command, ControlCommand::Status);
        poll_started.send(()).unwrap();
        // Hold the status response while accepting independent commands.
        for expected in ["emergency_stop", "interrupt"] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            let request: ControlRequest = serde_json::from_str(&line).unwrap();
            match (expected, request.command) {
                ("interrupt", ControlCommand::Interrupt { .. }) => {}
                ("emergency_stop", ControlCommand::EmergencyStop) => {}
                _ => panic!("unexpected command while status is stalled"),
            }
            let response = ControlResponse::Success {
                request_id: request.request_id,
                payload: ControlPayload::Accepted,
            };
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            stream.get_mut().write_all(&bytes).await.unwrap();
        }
        released.await.unwrap();
        drop(stalled);
    });
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (events, received) = mpsc::channel();
    let worker = tokio::spawn(run_worker(client, receiver, events));
    tokio::time::timeout(Duration::from_secs(2), started)
        .await
        .unwrap()
        .unwrap();
    commands.send(WorkerCommand::Interrupt).unwrap();
    commands.send(WorkerCommand::EmergencyStop).unwrap();
    for _ in 0..2 {
        wait_event(&received, |event| {
            matches!(event,
            WorkerEvent::Log(message) if message == "control command sent")
        })
        .await;
    }
    drop(commands);
    // Complete both commands and exit before the five-second status timeout.
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .expect("polling must not hold up commands or shutdown")
        .unwrap();
    release_server.send(()).unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn stalled_submit_does_not_block_priority_and_old_queued_text_is_returned() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        4096,
    )
    .unwrap();
    let (started, wait_started) = tokio::sync::oneshot::channel();
    let (urgent_done, wait_urgent) = tokio::sync::oneshot::channel();
    let (release, wait_release) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stalled = BufReader::new(stream);
        let mut line = String::new();
        stalled.read_line(&mut line).await.unwrap();
        let original: ControlRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(
            original.command,
            ControlCommand::Submit {
                text: "first".to_owned()
            }
        );
        started.send(()).unwrap();
        for emergency in [true, false] {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            let request: ControlRequest = serde_json::from_str(&line).unwrap();
            assert!(match request.command {
                ControlCommand::EmergencyStop => emergency,
                ControlCommand::Interrupt { .. } => !emergency,
                _ => false,
            });
            let response = ControlResponse::Success {
                request_id: request.request_id,
                payload: ControlPayload::Accepted,
            };
            stream
                .get_mut()
                .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
                .await
                .unwrap();
        }
        urgent_done.send(()).unwrap();
        wait_release.await.unwrap();
        let response = ControlResponse::Success {
            request_id: original.request_id,
            payload: ControlPayload::Accepted,
        };
        stalled
            .get_mut()
            .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
            .await
            .unwrap();
    });
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (events, received) = mpsc::channel();
    let worker = tokio::spawn(async move {
        run_commands(&client, receiver, &events, &AtomicU64::new(0)).await;
    });
    commands
        .send(WorkerCommand::Submit("first".to_owned()))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), wait_started)
        .await
        .unwrap()
        .unwrap();
    commands
        .send(WorkerCommand::Submit("queued".to_owned()))
        .unwrap();
    commands.send(WorkerCommand::Interrupt).unwrap();
    commands.send(WorkerCommand::EmergencyStop).unwrap();
    tokio::time::timeout(Duration::from_secs(2), wait_urgent)
        .await
        .expect("urgent commands cannot wait for a submit response")
        .unwrap();
    release.send(()).unwrap();
    wait_event(&received, |event| {
        matches!(event,
        WorkerEvent::SubmitRejected { text, .. } if text == "queued")
    })
    .await;
    drop(commands);
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn unavailable_history_recovers_without_losing_visible_partial_text() {
    use ai_ex_domain::{SystemEvent, TurnId};
    let turn_id = TurnId::new();
    let mut state = ai_ex_ui_model::UiState::new(10).unwrap();
    state.apply_event(SequencedEvent {
        sequence: 1,
        event: SystemEvent::TurnStarted {
            turn_id,
            user_text: "keep me".to_owned(),
        },
    });
    state.apply_event(SequencedEvent {
        sequence: 2,
        event: SystemEvent::ModelChunk {
            turn_id,
            text: "partial text".to_owned(),
        },
    });
    let newest = RuntimeSnapshot {
        instance_id: Some(uuid::Uuid::new_v4()),
        last_sequence: 12,
        turns_started: 1,
        turns_completed: 1,
        ..Default::default()
    };
    let (client, server) = scripted_client(vec![
        (
            ControlCommand::Events {
                after: 2,
                limit: 256,
            },
            ControlPayload::Events(vec![SequencedEvent {
                sequence: 10,
                event: SystemEvent::TurnFinished {
                    turn_id,
                    full_text: "unavailable history".to_owned(),
                },
            }]),
        ),
        (
            ControlCommand::Status,
            ControlPayload::Snapshot(newest.clone()),
        ),
    ])
    .await;
    let (events, receiver) = mpsc::channel();
    let mut cursor = 2;
    let mut instance_id = Some(uuid::Uuid::new_v4());
    let stale = RuntimeSnapshot {
        instance_id,
        last_sequence: 12,
        ..Default::default()
    };
    assert!(
        synchronize_snapshot(&client, &events, &mut cursor, stale, &mut instance_id)
            .await
            .unwrap()
    );
    reduce_received(&receiver, &mut state);
    assert_eq!(cursor, 12);
    assert_eq!(state.runtime, newest);
    assert_eq!(state.turns[0].assistant_text, "partial text");
    assert_eq!(
        state.turns[0].status,
        ai_ex_ui_model::TurnStatus::Interrupted
    );
    assert!(!state.needs_resync);
    server.await.unwrap();
    assert!(
        synchronize_snapshot(
            &client,
            &events,
            &mut cursor,
            RuntimeSnapshot::default(),
            &mut instance_id
        )
        .await
        .unwrap()
    );
    reduce_received(&receiver, &mut state);
    assert_eq!(cursor, 0);
    assert_eq!(state.turns[0].user_text, "keep me");
}

#[tokio::test]
async fn a_restarted_service_is_detected_even_when_its_sequence_has_caught_up() {
    use ai_ex_domain::{SystemEvent, TurnId};
    for next_sequence in [2, 3] {
        let previous_id = uuid::Uuid::new_v4();
        let next_id = uuid::Uuid::new_v4();
        let turn_id = TurnId::new();
        let mut state = ai_ex_ui_model::UiState::new(10).unwrap();
        state.apply_event(SequencedEvent {
            sequence: 1,
            event: SystemEvent::TurnStarted {
                turn_id,
                user_text: "old service".to_owned(),
            },
        });
        state.apply_event(SequencedEvent {
            sequence: 2,
            event: SystemEvent::ModelChunk {
                turn_id,
                text: "keep this partial reply".to_owned(),
            },
        });
        state.apply_snapshot(RuntimeSnapshot {
            instance_id: Some(previous_id),
            last_sequence: 2,
            active_turn: Some(turn_id),
            ..Default::default()
        });
        let newest = RuntimeSnapshot {
            instance_id: Some(next_id),
            last_sequence: next_sequence,
            turns_started: 1,
            turns_completed: 1,
            ..Default::default()
        };
        let (client, server) = scripted_client(Vec::new()).await;
        let (events, receiver) = mpsc::channel();
        let mut cursor = 2;
        let mut instance_id = Some(previous_id);
        assert!(
            synchronize_snapshot(
                &client,
                &events,
                &mut cursor,
                newest.clone(),
                &mut instance_id
            )
            .await
            .unwrap()
        );
        reduce_received(&receiver, &mut state);
        assert_eq!(state.runtime, newest);
        assert_eq!(cursor, next_sequence);
        assert_eq!(instance_id, Some(next_id));
        assert_eq!(state.turns[0].assistant_text, "keep this partial reply");
        assert_eq!(
            state.turns[0].status,
            ai_ex_ui_model::TurnStatus::Interrupted
        );
        assert!(!state.needs_resync);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn closing_commands_cancels_a_stalled_write_and_discards_queued_work() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        4096,
    )
    .unwrap();
    let (started, wait_started) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        started.send(()).unwrap();
        line.clear();
        assert_eq!(
            stream.read_line(&mut line).await.unwrap(),
            0,
            "closing the desktop must close its in-flight connection"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "queued requests must not be sent after desktop shutdown"
        );
    });
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (events, _received) = mpsc::channel();
    let worker = tokio::spawn(async move {
        run_commands(&client, receiver, &events, &AtomicU64::new(0)).await;
    });
    commands
        .send(WorkerCommand::Submit("stalled".to_owned()))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), wait_started)
        .await
        .unwrap()
        .unwrap();
    commands
        .send(WorkerCommand::Submit("do not send".to_owned()))
        .unwrap();
    drop(commands);
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .expect("desktop shutdown must cancel pending network I/O")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn only_definite_submit_rejections_return_text_as_unsent() {
    for error in [
        AppError::unavailable("conversation queue is full"),
        AppError::connectivity("result unknown"),
        AppError::protocol("invalid response"),
    ] {
        let definite = error.kind == ai_ex_domain::ErrorKind::Unavailable;
        let (client, server) = scripted_responses(vec![(
            ControlCommand::Submit {
                text: "my draft".to_owned(),
            },
            Err(error),
        )])
        .await;
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (events, received) = mpsc::channel();
        let worker = tokio::spawn(async move {
            run_commands(&client, receiver, &events, &AtomicU64::new(0)).await;
        });
        commands
            .send(WorkerCommand::Submit("my draft".to_owned()))
            .unwrap();
        wait_event(&received, |event| match event {
            WorkerEvent::SubmitRejected { text, .. } => {
                assert!(definite);
                assert_eq!(text, "my draft");
                true
            }
            WorkerEvent::SubmitUncertain { text, .. } => {
                assert!(!definite);
                assert_eq!(text, "my draft");
                true
            }
            _ => false,
        })
        .await;
        drop(commands);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
    }
}

#[tokio::test]
async fn messages_after_an_interrupt_wait_for_its_acknowledgement() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        4096,
    )
    .unwrap();
    let (started, wait_started) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let request: ControlRequest = serde_json::from_str(&line).unwrap();
        assert!(matches!(request.command, ControlCommand::Interrupt { .. }));
        started.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "new messages must not overtake the preceding interrupt"
        );
        let response = ControlResponse::Success {
            request_id: request.request_id,
            payload: ControlPayload::Accepted,
        };
        stream
            .get_mut()
            .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        line.clear();
        stream.read_line(&mut line).await.unwrap();
        let request: ControlRequest = serde_json::from_str(&line).unwrap();
        assert_eq!(
            request.command,
            ControlCommand::Submit {
                text: "after stop".to_owned()
            }
        );
        let response = ControlResponse::Success {
            request_id: request.request_id,
            payload: ControlPayload::Accepted,
        };
        stream
            .get_mut()
            .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
            .await
            .unwrap();
    });
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (events, received) = mpsc::channel();
    let worker = tokio::spawn(async move {
        run_commands(&client, receiver, &events, &AtomicU64::new(0)).await;
    });
    commands.send(WorkerCommand::Interrupt).unwrap();
    tokio::time::timeout(Duration::from_secs(2), wait_started)
        .await
        .unwrap()
        .unwrap();
    commands
        .send(WorkerCommand::Submit("after stop".to_owned()))
        .unwrap();
    for _ in 0..2 {
        wait_event(&received, |event| {
            matches!(event,
            WorkerEvent::Log(message) if message == "control command sent")
        })
        .await;
    }
    drop(commands);
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap();
    server.await.unwrap();
}
