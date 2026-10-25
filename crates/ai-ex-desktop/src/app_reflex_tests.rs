use super::*;
use crate::worker::{WorkerEvent, WorkerHandle};
use ai_ex_domain::{
    AutomationSearchIncompleteReason, AutomationSearchRegion, AutomationSearchSnapshot,
    AutomationSessionSnapshot, AutomationStepOutcome,
};

fn snapshot(instance: Uuid, revision: u64, sequence: u64) -> AutomationSnapshot {
    let mut snapshot = AutomationSnapshot::new(instance, true);
    snapshot.revision = revision;
    snapshot.projection_sequence = sequence;
    snapshot
}

fn active(instance: Uuid) -> AutomationSnapshot {
    let mut snapshot = snapshot(instance, 3, 8);
    let mut session = AutomationSessionSnapshot::loading(Uuid::new_v4(), Uuid::new_v4());
    session.phase = AutomationSessionPhase::Active;
    session.resume_eligibility = AutomationResumeEligibility::Idle;
    session.max_frames = 20;
    session.max_actions = 20;
    snapshot.session = Some(session);
    snapshot
}

fn panel(
    snapshot: AutomationSnapshot,
) -> (
    ReflexPanel,
    tokio::sync::mpsc::UnboundedReceiver<WorkerCommand>,
    tokio::sync::mpsc::UnboundedSender<WorkerCommand>,
) {
    let mut panel = ReflexPanel::default();
    panel.synchronize(Some(snapshot.instance_id), true);
    assert!(panel.accept(snapshot).unwrap());
    panel.manifest = "  fixtures/replay.json  ".to_owned();
    let (commands, sent) = tokio::sync::mpsc::unbounded_channel();
    (panel, sent, commands)
}

fn sent(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<WorkerCommand>,
) -> (u64, AutomationRequest) {
    match receiver.try_recv().unwrap() {
        WorkerCommand::Automation {
            request_id,
            request,
        } => (request_id, request),
        _ => panic!("expected automation command"),
    }
}

#[test]
fn repeated_start_is_correlated_and_timeout_never_replays_or_discards_draft() {
    let original = snapshot(Uuid::new_v4(), 2, 4);
    let (mut panel, mut received, commands) = panel(original.clone());
    panel.enqueue(Intent::Start, true, &commands);
    panel.enqueue(Intent::Start, true, &commands);
    let (id, request) = sent(&mut received);
    let AutomationRequest::StartTemplate {
        operation_id,
        manifest,
        ..
    } = request
    else {
        panic!("start")
    };
    assert!(!operation_id.is_nil());
    assert_eq!(manifest, "  fixtures/replay.json  ");
    assert!(received.try_recv().is_err());
    panel.receive(
        Some(id),
        Err(AppError::connectivity("outcome unknown after timeout")),
    );
    panel.receive(None, Ok(original.clone()));
    assert!(
        !panel.fresh,
        "unchanged Status cannot prove a timed-out CAS request will not arrive later"
    );
    assert!(panel.request(Intent::Start, true).is_none());
    assert_eq!(panel.manifest, "  fixtures/replay.json  ");
    assert!(received.try_recv().is_err());
    let mut confirmed = original;
    confirmed.revision += 1;
    confirmed.projection_sequence += 1;
    panel.receive(None, Ok(confirmed));
    assert!(panel.fresh);
    assert!(!panel.mutation_pending());
}

#[test]
fn priority_pause_and_stop_keep_same_session_cursor_and_do_not_change_confirmed_state() {
    let original = active(Uuid::new_v4());
    let (mut panel, mut received, commands) = panel(original.clone());
    panel.enqueue(Intent::Pause, true, &commands);
    panel.enqueue(Intent::Pause, true, &commands);
    panel.enqueue(Intent::Stop, true, &commands);
    let (_, pause) = sent(&mut received);
    let (_, stop) = sent(&mut received);
    assert!(matches!(
        pause,
        AutomationRequest::Pause {
            expected_revision: 3,
            ..
        }
    ));
    assert!(matches!(
        stop,
        AutomationRequest::Stop {
            expected_revision: 3,
            ..
        }
    ));
    assert!(received.try_recv().is_err());
    assert_eq!(panel.confirmed, Some(original));
}

#[test]
fn uncertain_stop_is_not_resolved_by_unrelated_revision_advance() {
    let original = active(Uuid::new_v4());
    let (mut panel, mut received, commands) = panel(original.clone());
    panel.enqueue(Intent::Stop, true, &commands);
    let (id, _) = sent(&mut received);
    panel.receive(Some(id), Err(AppError::connectivity("timeout")));
    let mut paused = original;
    paused.revision += 1;
    paused.projection_sequence += 1;
    paused.session.as_mut().unwrap().control_mode = AutomationControlMode::PausedForChat;
    panel.receive(None, Ok(paused.clone()));
    assert!(!panel.fresh);
    assert!(panel.mutation_pending());
    let session = paused.session.as_mut().unwrap();
    session.control_mode = AutomationControlMode::Stopped;
    session.phase = AutomationSessionPhase::Finished;
    session.latest_outcome = Some(AutomationStepOutcome::Stopped);
    paused.revision += 1;
    paused.projection_sequence += 1;
    panel.receive(None, Ok(paused));
    assert!(panel.fresh);
    assert!(!panel.mutation_pending());
    assert!(panel.request(Intent::Resume, true).is_none());
    assert!(panel.request(Intent::Start, true).is_some());
}

#[test]
fn projection_rejects_old_instance_old_progress_and_inconsistent_same_sequence() {
    let original = active(Uuid::new_v4());
    let (mut panel, _, _) = panel(original.clone());
    assert!(!panel.accept(snapshot(Uuid::new_v4(), 99, 99)).unwrap());
    assert!(!panel.accept(snapshot(original.instance_id, 2, 7)).unwrap());
    let mut progress = original.clone();
    progress.projection_sequence += 1;
    progress.session.as_mut().unwrap().frames = 1;
    assert!(panel.accept(progress.clone()).unwrap());
    assert!(!panel.accept(original).unwrap());
    let mut invalid = progress.clone();
    invalid.session.as_mut().unwrap().frames = 2;
    assert!(panel.accept(invalid).is_err());
    assert_eq!(panel.confirmed, Some(progress));
}

#[test]
fn reconnect_preserves_manifest_and_blocks_old_instance_replies() {
    let original = active(Uuid::new_v4());
    let (mut panel, mut received, commands) = panel(original.clone());
    panel.enqueue(Intent::Pause, true, &commands);
    let (id, _) = sent(&mut received);
    panel.disconnected();
    assert!(panel.request(Intent::Resume, false).is_none());
    panel.synchronize(Some(Uuid::new_v4()), true);
    panel.receive(Some(id), Ok(original));
    assert!(panel.confirmed.is_none());
    assert!(!panel.fresh);
    assert_eq!(panel.manifest, "  fixtures/replay.json  ");
    assert!(panel.request(Intent::Status, true).is_some());
}

#[test]
fn resume_requires_idle_active_confirmed_state_and_terminal_requires_new_start() {
    let mut original = active(Uuid::new_v4());
    original.session.as_mut().unwrap().control_mode = AutomationControlMode::PausedForChat;
    let (mut panel, _, _) = panel(original.clone());
    assert!(panel.request(Intent::Resume, true).is_some());
    for eligibility in [
        AutomationResumeEligibility::Busy,
        AutomationResumeEligibility::Unknown,
    ] {
        let mut busy = original.clone();
        busy.projection_sequence = panel.confirmed.as_ref().unwrap().projection_sequence + 1;
        busy.session.as_mut().unwrap().resume_eligibility = eligibility;
        panel.accept(busy).unwrap();
        assert!(panel.request(Intent::Resume, true).is_none());
    }
    let mut finished = original;
    finished.projection_sequence = panel.confirmed.as_ref().unwrap().projection_sequence + 1;
    let session = finished.session.as_mut().unwrap();
    session.phase = AutomationSessionPhase::Finished;
    session.control_mode = AutomationControlMode::Stopped;
    session.latest_outcome = Some(AutomationStepOutcome::InputExhausted);
    panel.accept(finished).unwrap();
    assert!(panel.request(Intent::Resume, true).is_none());
    assert!(panel.request(Intent::Stop, true).is_none());
    assert!(panel.request(Intent::Start, true).is_some());
}

#[test]
fn definite_rejection_and_uncorrelated_reply_preserve_draft() {
    let original = snapshot(Uuid::new_v4(), 2, 4);
    let (mut panel, mut received, commands) = panel(original.clone());
    panel.enqueue(Intent::Start, true, &commands);
    let (id, _) = sent(&mut received);
    panel.receive(Some(id + 1), Ok(original.clone()));
    assert!(panel.mutation_pending());
    panel.receive(
        Some(id),
        Err(AppError::invalid_transition("stale revision")),
    );
    assert!(!panel.mutation_pending());
    assert!(!panel.fresh);
    panel.receive(None, Ok(original));
    assert!(panel.fresh);
    assert_eq!(panel.manifest, "  fixtures/replay.json  ");
}

#[test]
fn headless_panel_is_visible_without_health_and_only_queries_status_once() {
    let (events, receiver) = std::sync::mpsc::channel();
    let (commands, mut sent) = tokio::sync::mpsc::unbounded_channel();
    let mut app = DesktopApp::with_storage(
        WorkerHandle {
            commands,
            events: receiver,
        },
        false,
        None,
    );
    app.state.connection = ConnectionState::Connected;
    app.state.runtime.instance_id = Some(Uuid::new_v4());
    app.state.needs_resync = false;
    app.reflex_runtime_ready = true;
    let context = egui::Context::default();
    for _ in 0..2 {
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            app.show_automation_panel(ui)
        });
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.job.text == "合成模板回放")));
    }
    assert!(matches!(
        sent.try_recv().unwrap(),
        WorkerCommand::Automation {
            request: AutomationRequest::Status,
            ..
        }
    ));
    assert!(sent.try_recv().is_err());
    events.send(WorkerEvent::Connection(false)).unwrap();
    app.drain_events();
    assert!(!app.reflex.fresh);
}

use ai_ex_control::{
    ControlClient, ControlCommand, ControlPayload, ControlRequest, ControlResponse,
};
use std::sync::{Arc, atomic::AtomicU64};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn event(
    receiver: &std::sync::mpsc::Receiver<WorkerEvent>,
    id: u64,
    timeout: Duration,
) -> Result<AutomationSnapshot, AppError> {
    tokio::time::timeout(timeout, async {
        loop {
            while let Ok(event) = receiver.try_recv() {
                if let WorkerEvent::Automation { request_id, result } = event
                    && request_id == id
                {
                    return result;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("correlated automation reply")
}

#[tokio::test]
async fn slow_regular_and_pause_do_not_block_stop_or_status_and_old_waiting_start_is_revoked() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        65_536,
    )
    .unwrap();
    let original = active(Uuid::new_v4());
    let session = original.session.as_ref().unwrap().session_id;
    let (observed, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let server_release = release.clone();
    let server_snapshot = original.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let observed = observed.clone();
            let release = server_release.clone();
            let snapshot = server_snapshot.clone();
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).await.unwrap();
                let request: ControlRequest = serde_json::from_str(&line).unwrap();
                observed.send(request.command.clone()).unwrap();
                let mut snapshot = snapshot;
                let payload = match &request.command {
                    ControlCommand::Submit { .. } => {
                        release.notified().await;
                        ControlPayload::Accepted
                    }
                    ControlCommand::Automation {
                        request: AutomationRequest::Resume { .. } | AutomationRequest::Pause { .. },
                    } => {
                        release.notified().await;
                        ControlPayload::Automation(Box::new(snapshot))
                    }
                    ControlCommand::Automation {
                        request: AutomationRequest::Stop { .. },
                    } => {
                        snapshot.revision += 1;
                        snapshot.projection_sequence += 1;
                        snapshot.session.as_mut().unwrap().control_mode =
                            AutomationControlMode::Stopped;
                        ControlPayload::Automation(Box::new(snapshot))
                    }
                    ControlCommand::Automation {
                        request: AutomationRequest::Status,
                    } => ControlPayload::Automation(Box::new(snapshot)),
                    other => panic!("unexpected request: {other:?}"),
                };
                let response = ControlResponse::Success {
                    request_id: request.request_id,
                    payload,
                };
                let _written = stream
                    .get_mut()
                    .write_all(
                        format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes(),
                    )
                    .await;
            });
        }
    });
    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, received) = std::sync::mpsc::channel();
    let worker = tokio::spawn(async move {
        crate::worker::run_commands(&client, command_rx, &events, &AtomicU64::new(0)).await;
    });
    commands
        .send(WorkerCommand::Submit("blocked conversation".to_owned()))
        .unwrap();
    commands
        .send(WorkerCommand::Automation {
            request_id: 1,
            request: AutomationRequest::Resume {
                instance_id: original.instance_id,
                session_id: session,
                expected_revision: 3,
            },
        })
        .unwrap();
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(2), seen.recv())
            .await
            .unwrap()
            .unwrap();
    }
    commands
        .send(WorkerCommand::Automation {
            request_id: 2,
            request: AutomationRequest::StartTemplate {
                instance_id: original.instance_id,
                expected_revision: 3,
                operation_id: Uuid::new_v4(),
                manifest: "synthetic.json".to_owned(),
            },
        })
        .unwrap();
    commands
        .send(WorkerCommand::Automation {
            request_id: 3,
            request: AutomationRequest::Pause {
                instance_id: original.instance_id,
                session_id: session,
                expected_revision: 3,
            },
        })
        .unwrap();
    commands
        .send(WorkerCommand::Automation {
            request_id: 4,
            request: AutomationRequest::Stop {
                instance_id: original.instance_id,
                session_id: session,
                expected_revision: 3,
            },
        })
        .unwrap();
    commands
        .send(WorkerCommand::Automation {
            request_id: 5,
            request: AutomationRequest::Status,
        })
        .unwrap();
    let mut observed_commands = Vec::new();
    for _ in 0..3 {
        observed_commands.push(
            tokio::time::timeout(Duration::from_secs(2), seen.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert!(observed_commands.iter().any(|command| matches!(
        command,
        ControlCommand::Automation {
            request: AutomationRequest::Pause {
                expected_revision: 3,
                ..
            }
        }
    )));
    assert!(observed_commands.iter().any(|command| matches!(
        command,
        ControlCommand::Automation {
            request: AutomationRequest::Stop {
                expected_revision: 3,
                ..
            }
        }
    )));
    assert!(observed_commands.iter().any(|command| matches!(
        command,
        ControlCommand::Automation {
            request: AutomationRequest::Status
        }
    )));
    release.notify_waiters();
    let error = event(&received, 2, Duration::from_secs(2))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert!(error.message.contains("revoked"));
    while let Ok(command) = seen.try_recv() {
        assert!(!matches!(
            command,
            ControlCommand::Automation {
                request: AutomationRequest::StartTemplate { .. }
            }
        ));
    }
    drop(commands);
    worker.await.unwrap();
    server.abort();
}

#[tokio::test]
async fn network_timeout_is_correlated_once_without_automatic_mutation_retry() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        65_536,
    )
    .unwrap();
    let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, received) = std::sync::mpsc::channel();
    let worker = tokio::spawn(async move {
        crate::worker::run_commands(&client, command_rx, &events, &AtomicU64::new(0)).await;
    });
    commands
        .send(WorkerCommand::Automation {
            request_id: 7,
            request: AutomationRequest::StartTemplate {
                instance_id: Uuid::new_v4(),
                expected_revision: 0,
                operation_id: Uuid::new_v4(),
                manifest: "synthetic.json".to_owned(),
            },
        })
        .unwrap();
    let (stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    stream.read_line(&mut line).await.unwrap();
    let request: ControlRequest = serde_json::from_str(&line).unwrap();
    assert!(matches!(
        request.command,
        ControlCommand::Automation {
            request: AutomationRequest::StartTemplate { .. }
        }
    ));
    let error = event(&received, 7, Duration::from_secs(6))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Connectivity);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
    assert!(received.try_recv().is_err());
    drop(commands);
    worker.await.unwrap();
}

#[test]
fn chat_pause_can_be_manually_owned_and_status_never_emits_automatic_resume() {
    let mut original = active(Uuid::new_v4());
    let session = original.session.as_mut().unwrap();
    session.control_mode = AutomationControlMode::PausedForChat;
    session.pause_reason = Some(AutomationPauseReason::Chat);
    session.chat_deadline_ms = Some(30_000);
    let (mut panel, mut received, commands) = panel(original.clone());
    panel.enqueue(Intent::Pause, true, &commands);
    panel.enqueue(Intent::Pause, true, &commands);
    let (id, request) = sent(&mut received);
    assert!(
        matches!(request, AutomationRequest::Pause { instance_id, session_id, expected_revision: 3 } if instance_id == original.instance_id && session_id == original.session.as_ref().unwrap().session_id)
    );
    let mut manual = original;
    manual.revision += 1;
    manual.projection_sequence += 1;
    manual.session.as_mut().unwrap().pause_reason = Some(AutomationPauseReason::Manual);
    manual.session.as_mut().unwrap().chat_deadline_ms = None;
    panel.receive(Some(id), Ok(manual.clone()));
    manual.projection_sequence += 1;
    panel.receive(None, Ok(manual));
    assert!(panel.fresh);
    assert_eq!(
        panel
            .confirmed
            .as_ref()
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .control_mode,
        AutomationControlMode::PausedForChat
    );
    assert!(
        received.try_recv().is_err(),
        "the desktop never automatically sends Resume"
    );
}

#[test]
fn successful_older_correlated_reply_settles_without_regressing_newer_projection() {
    let original = active(Uuid::new_v4());
    let (mut panel, mut received, commands) = panel(original.clone());
    panel.enqueue(Intent::Pause, true, &commands);
    let (id, _) = sent(&mut received);
    let mut reply = original;
    reply.revision += 1;
    reply.projection_sequence += 1;
    reply.session.as_mut().unwrap().control_mode = AutomationControlMode::PausedForChat;
    reply.session.as_mut().unwrap().pause_reason = Some(AutomationPauseReason::Manual);
    let mut newer = reply.clone();
    newer.projection_sequence += 1;
    newer.session.as_mut().unwrap().frames = 1;
    panel.receive(None, Ok(newer.clone()));
    assert!(panel.mutation_pending());
    panel.receive(Some(id), Ok(reply));
    assert!(!panel.mutation_pending());
    assert!(panel.fresh);
    assert_eq!(panel.confirmed, Some(newer));
}

#[test]
fn reconnect_and_resync_need_runtime_barrier_before_late_automation_replies_can_enable_controls() {
    let original = active(Uuid::new_v4());
    let runtime = ai_ex_observability::RuntimeSnapshot {
        instance_id: Some(original.instance_id),
        ..Default::default()
    };
    let (events, receiver) = std::sync::mpsc::channel();
    let (commands, _sent) = tokio::sync::mpsc::unbounded_channel();
    let mut app = DesktopApp::with_storage(
        WorkerHandle {
            commands,
            events: receiver,
        },
        false,
        None,
    );
    app.state.connection = ConnectionState::Connected;
    app.state.runtime = runtime.clone();
    app.reflex_runtime_ready = true;
    app.sync_reflex_identity();
    app.reflex.accept(original.clone()).unwrap();
    assert!(app.reflex.request(Intent::Pause, true).is_some());
    events.send(WorkerEvent::Connection(false)).unwrap();
    events
        .send(WorkerEvent::AutomationStatus(Ok(original.clone())))
        .unwrap();
    app.drain_events();
    assert!(!app.reflex.fresh);
    assert!(!app.reflex.ready);
    events.send(WorkerEvent::Connection(true)).unwrap();
    events
        .send(WorkerEvent::AutomationStatus(Ok(original.clone())))
        .unwrap();
    app.drain_events();
    assert!(!app.reflex.fresh);
    assert!(app.reflex.request(Intent::Pause, true).is_none());
    events.send(WorkerEvent::Snapshot(runtime.clone())).unwrap();
    app.drain_events();
    assert!(app.reflex.ready);
    assert!(!app.reflex.fresh);
    events
        .send(WorkerEvent::AutomationStatus(Ok(original.clone())))
        .unwrap();
    app.drain_events();
    assert!(app.reflex.fresh);
    assert!(app.reflex.request(Intent::Pause, true).is_some());
    app.state.needs_resync = true;
    events
        .send(WorkerEvent::AutomationStatus(Ok(original)))
        .unwrap();
    app.drain_events();
    assert!(!app.reflex.ready);
    assert!(!app.reflex.fresh);
    assert!(app.reflex.request(Intent::Pause, true).is_none());
    events.send(WorkerEvent::HistoryGap(runtime)).unwrap();
    app.drain_events();
    assert!(app.reflex.ready);
    assert!(
        !app.reflex.fresh,
        "runtime recovery alone is not an automation confirmation"
    );
}

#[test]
fn headless_chat_pause_shows_manual_takeover_and_localized_state() {
    let mut snapshot = active(Uuid::new_v4());
    let session = snapshot.session.as_mut().unwrap();
    session.control_mode = AutomationControlMode::PausedForChat;
    session.pause_reason = Some(AutomationPauseReason::Chat);
    session.resume_eligibility = AutomationResumeEligibility::Busy;
    let (_events, receiver) = std::sync::mpsc::channel();
    let (commands, _sent) = tokio::sync::mpsc::unbounded_channel();
    let mut app = DesktopApp::with_storage(
        WorkerHandle {
            commands,
            events: receiver,
        },
        false,
        None,
    );
    app.state.connection = ConnectionState::Connected;
    app.state.runtime.instance_id = Some(snapshot.instance_id);
    app.reflex_runtime_ready = true;
    app.sync_reflex_identity();
    app.reflex.accept(snapshot).unwrap();
    app.reflex.activated = true;
    let output =
        egui::Context::default().run_ui(egui::RawInput::default(), |ui| app.show_reflex_panel(ui));
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(labels.contains("保持暂停"));
    assert!(labels.contains("聊天暂停"));
    assert!(labels.contains("等待语音或生成结束"));
    assert!(!labels.contains("PausedForChat"));
    assert!(!labels.contains("Some(Chat)"));
}

fn incomplete_snapshot(instance: Uuid) -> AutomationSnapshot {
    let mut snapshot = active(instance);
    let session = snapshot.session.as_mut().unwrap();
    session.phase = AutomationSessionPhase::Finished;
    session.control_mode = AutomationControlMode::Stopped;
    session.latest_outcome = Some(AutomationStepOutcome::SearchIncomplete);
    session.frames = 3;
    session.actions = 7; // Earlier steps may have acted; this diagnostic describes only the latest step.
    session.search = Some(AutomationSearchSnapshot {
        region: AutomationSearchRegion {
            x: 7,
            y: 11,
            width: 100,
            height: 100,
        },
        candidates: 100,
        verified: 40,
        bound_queries: 80,
        exact_evaluations: 20,
        exact_pixel_pairs: 8_000,
        bound_work: 160,
        work: 8_160,
        max_work: 10_000,
        reason: AutomationSearchIncompleteReason::WorkBudgetExhausted,
    });
    snapshot
}

#[test]
fn headless_incomplete_search_shows_verified_costs_and_region_without_automatic_retries() {
    let snapshot = incomplete_snapshot(Uuid::new_v4());
    let (_events, receiver) = std::sync::mpsc::channel();
    let (commands, mut sent) = tokio::sync::mpsc::unbounded_channel();
    let mut app = DesktopApp::with_storage(
        WorkerHandle {
            commands,
            events: receiver,
        },
        false,
        None,
    );
    app.state.connection = ConnectionState::Connected;
    app.state.runtime.instance_id = Some(snapshot.instance_id);
    app.reflex_runtime_ready = true;
    app.sync_reflex_identity();
    app.reflex.accept(snapshot).unwrap();
    app.reflex.manifest = "synthetic.json".into();
    app.reflex.activated = true;
    let context = egui::Context::default();
    for _ in 0..2 {
        let output = context.run_ui(egui::RawInput::default(), |ui| app.show_reflex_panel(ui));
        let labels = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(labels.contains("结果：搜索未完成"));
        assert!(labels.contains("尚未认证目标；本步未移动或点击"));
        assert!(labels.contains("已核验：40/100 个位置"));
        assert!(labels.contains("计算：8160/10000"));
        assert!(labels.contains("精确计算：8000（20 个位置）"));
        assert!(labels.contains("上界计算：160（80 次）"));
        assert!(labels.contains("搜索区域（帧内像素）：x=7，y=11，宽=100，高=100"));
        assert!(!labels.contains("结果：未匹配"));
        assert!(!labels.contains("帧或动作预算已用尽"));
        assert!(!labels.contains("临时命中"));
        assert!(
            sent.try_recv().is_err(),
            "rendering never retries or increases a budget"
        );
    }
    assert!(app.reflex.request(Intent::Resume, true).is_none());
    assert!(app.reflex.request(Intent::Pause, true).is_none());
    assert!(
        app.reflex.request(Intent::Start, true).is_some(),
        "a new start remains an explicit user action"
    );
}

#[test]
fn incomplete_search_keeps_ambiguous_new_start_correlated_without_replaying() {
    let original = incomplete_snapshot(Uuid::new_v4());
    let (mut panel, mut received, commands) = panel(original.clone());
    panel.enqueue(Intent::Start, true, &commands);
    let (id, _) = sent(&mut received);
    panel.receive(
        Some(id),
        Err(AppError::connectivity("start outcome unknown")),
    );
    panel.receive(None, Ok(original));
    assert!(!panel.fresh);
    assert!(panel.mutation_pending());
    assert!(panel.request(Intent::Start, true).is_none());
    assert!(panel.request(Intent::Resume, true).is_none());
    assert_eq!(panel.manifest, "  fixtures/replay.json  ");
    assert!(received.try_recv().is_err());
}

#[test]
fn invalid_search_projection_preserves_confirmed_state_and_closes_mutation_admission() {
    let original = incomplete_snapshot(Uuid::new_v4());
    let (mut panel, mut received, _commands) = panel(original.clone());
    let mut invalid = original.clone();
    invalid.projection_sequence += 1;
    invalid
        .session
        .as_mut()
        .unwrap()
        .search
        .as_mut()
        .unwrap()
        .work += 1;
    panel.receive(None, Ok(invalid));
    assert_eq!(panel.confirmed, Some(original));
    assert!(!panel.fresh);
    assert!(panel.error.is_some());
    assert!(panel.request(Intent::Start, true).is_none());
    assert!(panel.request(Intent::Resume, true).is_none());
    assert!(received.try_recv().is_err());
}

#[test]
fn incomplete_search_label_distinguishes_no_match_and_frame_or_action_budget() {
    assert_eq!(
        outcome_label(Some(AutomationStepOutcome::SearchIncomplete)),
        "搜索未完成"
    );
    assert_eq!(outcome_label(Some(AutomationStepOutcome::Missed)), "未匹配");
    assert_eq!(
        outcome_label(Some(AutomationStepOutcome::BudgetExhausted)),
        "帧或动作预算已用尽"
    );
}

#[test]
fn contradictory_incomplete_search_cannot_enable_resume_or_replace_confirmed_terminal_state() {
    for (phase, control_mode) in [
        (
            AutomationSessionPhase::Active,
            AutomationControlMode::PausedForChat,
        ),
        (
            AutomationSessionPhase::Active,
            AutomationControlMode::Running,
        ),
        (
            AutomationSessionPhase::Finished,
            AutomationControlMode::PausedForChat,
        ),
        (
            AutomationSessionPhase::Stopping,
            AutomationControlMode::Stopped,
        ),
    ] {
        let original = incomplete_snapshot(Uuid::new_v4());
        let (mut panel, mut received, _commands) = panel(original.clone());
        let mut invalid = original.clone();
        invalid.projection_sequence += 1;
        let session = invalid.session.as_mut().unwrap();
        session.phase = phase;
        session.control_mode = control_mode;
        session.pause_reason = Some(AutomationPauseReason::Manual);
        session.resume_eligibility = AutomationResumeEligibility::Idle;
        panel.receive(None, Ok(invalid));
        assert_eq!(panel.confirmed, Some(original));
        assert!(!panel.fresh);
        assert!(panel.error.is_some());
        assert!(panel.request(Intent::Resume, true).is_none());
        assert!(panel.request(Intent::Start, true).is_none());
        assert!(
            received.try_recv().is_err(),
            "a contradictory search state never sends a command"
        );
    }
}
