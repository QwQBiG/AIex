use super::*;
use ai_ex_vision::{VisionAnalyzerPort, VisionObservation, VisionRequest};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Clone)]
struct FakeVision {
    output: String,
    calls: Arc<AtomicUsize>,
    entered: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
    change_target: Option<(Arc<Mutex<FakeState>>, u8)>,
}

#[async_trait]
impl VisionAnalyzerPort for FakeVision {
    async fn analyze(&mut self, request: VisionRequest) -> Result<VisionObservation, AppError> {
        assert!(request.prompt.contains("synthetic goal"));
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = &self.entered {
            entered.notify_one();
        }
        if let Some(release) = &self.release {
            release.notified().await;
        }
        if let Some((target, change)) = &self.change_target {
            let mut state = target.lock().unwrap();
            let window = state.window.as_mut().unwrap();
            match change {
                0 => window.foreground = false,
                1 => window.client.left += 1,
                _ => window.identity.process_id += 1,
            }
        }
        Ok(VisionObservation {
            model: "synthetic".into(),
            text: self.output.clone(),
        })
    }
}

fn install_vision(fixture: &mut Fixture, vision: FakeVision) {
    fixture.host.vision = Some(Arc::new(move || {
        Ok(Box::new(vision.clone()) as Box<dyn VisionAnalyzerPort>)
    }));
}

fn fake_vision(output: &str) -> FakeVision {
    FakeVision {
        output: output.into(),
        calls: Arc::new(AtomicUsize::new(0)),
        entered: None,
        release: None,
        change_target: None,
    }
}

async fn ready(fixture: &Fixture) -> NativeDesktopSnapshot {
    let prepared = fixture.start().await.unwrap().snapshot;
    fixture
        .host
        .request(NativeDesktopRequest::SetGoal {
            instance_id: prepared.instance_id,
            expected_revision: prepared.revision,
            session_id: prepared.session.unwrap().session_id,
            goal: "synthetic goal".into(),
        })
        .await
        .unwrap();
    fixture.resume().await
}

fn request(snapshot: &NativeDesktopSnapshot, operation_id: Uuid) -> NativeDesktopRequest {
    NativeDesktopRequest::PreviewVision {
        instance_id: snapshot.instance_id,
        expected_revision: snapshot.revision,
        session_id: snapshot.session.as_ref().unwrap().session_id,
        operation_id,
        goal: "synthetic goal".into(),
    }
}

fn prepare(snapshot: &NativeDesktopSnapshot, proposal_id: Uuid) -> NativeDesktopRequest {
    NativeDesktopRequest::PrepareVisionProposal {
        instance_id: snapshot.instance_id,
        expected_revision: snapshot.revision,
        session_id: snapshot.session.as_ref().unwrap().session_id,
        operation_id: proposal_id,
        goal: "synthetic goal".into(),
    }
}

fn confirm(
    snapshot: &NativeDesktopSnapshot,
    proposal_id: Uuid,
    operation_id: Uuid,
) -> NativeDesktopRequest {
    NativeDesktopRequest::ConfirmVisionProposal {
        instance_id: snapshot.instance_id,
        expected_revision: snapshot.revision,
        session_id: snapshot.session.as_ref().unwrap().session_id,
        proposal_id,
        operation_id,
    }
}

fn operation(
    snapshot: &NativeDesktopSnapshot,
    proposal_id: Uuid,
    operation_id: Uuid,
) -> NativeDesktopRequest {
    NativeDesktopRequest::GetVisionOperation {
        instance_id: snapshot.instance_id,
        session_id: snapshot.session.as_ref().unwrap().session_id,
        proposal_id,
        operation_id,
    }
}

#[tokio::test]
async fn service_owned_proposal_can_be_queried_and_discarded_without_input() {
    use ai_ex_domain::NativeVisionPreparationState;
    let mut fixture = Fixture::new(true);
    install_vision(
        &mut fixture,
        fake_vision(r#"{"type":"click","x":0,"y":0,"button":"left"}"#),
    );
    let running = ready(&fixture).await;
    let proposal_id = Uuid::new_v4();
    let result = fixture
        .host
        .request(prepare(&running, proposal_id))
        .await
        .unwrap();
    assert_eq!(
        result.vision_preparation.unwrap().state,
        NativeVisionPreparationState::Ready
    );
    let suggestion = result.vision_proposal.unwrap();
    assert_eq!(suggestion.proposal_id, proposal_id);
    assert!(suggestion.remaining_ms <= 120_000 && suggestion.remaining_ms > 0);
    assert!(fixture.host.state.lock().await.frame.is_none());
    assert!(result.snapshot.session.as_ref().unwrap().frame_id.is_none());
    let query = NativeDesktopRequest::GetVisionProposal {
        instance_id: result.snapshot.instance_id,
        session_id: result.snapshot.session.as_ref().unwrap().session_id,
        proposal_id,
    };
    assert!(
        fixture
            .host
            .request(query.clone())
            .await
            .unwrap()
            .vision_proposal
            .is_some()
    );
    let discarded = fixture
        .host
        .request(NativeDesktopRequest::DiscardVisionProposal {
            instance_id: result.snapshot.instance_id,
            expected_revision: result.snapshot.revision,
            session_id: result.snapshot.session.as_ref().unwrap().session_id,
            proposal_id,
        })
        .await
        .unwrap();
    assert_eq!(
        discarded.vision_preparation.unwrap().state,
        NativeVisionPreparationState::Unavailable
    );
    let after = fixture.host.request(query).await.unwrap();
    assert!(after.vision_proposal.is_none());
    assert_eq!(
        after.vision_preparation.unwrap().state,
        NativeVisionPreparationState::Unavailable
    );
    assert!(
        fixture
            .host
            .request(confirm(&discarded.snapshot, proposal_id, Uuid::new_v4()))
            .await
            .is_err()
    );
    assert_eq!(fixture.system.lock().unwrap().captures, 1);
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn expired_proposal_is_unavailable_and_cannot_be_confirmed() {
    use ai_ex_domain::NativeVisionPreparationState;
    let mut fixture = Fixture::new(true);
    install_vision(
        &mut fixture,
        fake_vision(r#"{"type":"click","x":0,"y":0,"button":"left"}"#),
    );
    let running = ready(&fixture).await;
    let proposal_id = Uuid::new_v4();
    let prepared = fixture
        .host
        .request(prepare(&running, proposal_id))
        .await
        .unwrap()
        .snapshot;
    fixture.host.expire_vision_proposal_for_test(proposal_id);
    let query = NativeDesktopRequest::GetVisionProposal {
        instance_id: prepared.instance_id,
        session_id: prepared.session.as_ref().unwrap().session_id,
        proposal_id,
    };
    let result = fixture.host.request(query).await.unwrap();
    assert_eq!(
        result.vision_preparation.unwrap().state,
        NativeVisionPreparationState::Unavailable
    );
    assert!(result.vision_proposal.is_none());
    assert!(
        fixture
            .host
            .request(confirm(&prepared, proposal_id, Uuid::new_v4()))
            .await
            .is_err()
    );
    assert_eq!(fixture.system.lock().unwrap().captures, 1);
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn confirmation_rechecks_screen_permission_and_target() {
    use ai_ex_domain::NativeVisionOperationState;
    for change in 0..4 {
        let mut fixture = Fixture::new(true);
        install_vision(
            &mut fixture,
            fake_vision(r#"{"type":"click","x":0,"y":0,"button":"left"}"#),
        );
        let running = ready(&fixture).await;
        let proposal_id = Uuid::new_v4();
        let prepared = fixture
            .host
            .request(prepare(&running, proposal_id))
            .await
            .unwrap()
            .snapshot;
        if change == 3 {
            fixture.host.safety.trigger_emergency_stop();
        } else {
            let mut system = fixture.system.lock().unwrap();
            let window = system.window.as_mut().unwrap();
            match change {
                0 => window.foreground = false,
                1 => window.client.left += 1,
                _ => window.identity.process_id += 1,
            }
        }
        let result = fixture
            .host
            .request(confirm(&prepared, proposal_id, Uuid::new_v4()))
            .await
            .unwrap();
        assert_eq!(
            result.vision_operation.unwrap().state,
            NativeVisionOperationState::Rejected
        );
        assert!(fixture.system.lock().unwrap().calls.is_empty());
        assert!(fixture.host.state.lock().await.frame.is_none());
    }
}

#[tokio::test]
async fn preparation_status_is_available_during_model_analysis() {
    use ai_ex_domain::NativeVisionPreparationState;
    let mut fixture = Fixture::new(true);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut vision = fake_vision(r#"{"type":"noop"}"#);
    vision.entered = Some(entered.clone());
    vision.release = Some(release.clone());
    install_vision(&mut fixture, vision);
    let running = ready(&fixture).await;
    let proposal_id = Uuid::new_v4();
    let host = fixture.host.clone();
    let command = prepare(&running, proposal_id);
    let task = tokio::spawn(async move { host.request(command).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let query = NativeDesktopRequest::GetVisionProposal {
        instance_id: running.instance_id,
        session_id: running.session.as_ref().unwrap().session_id,
        proposal_id,
    };
    let current = fixture.host.request(query.clone()).await.unwrap();
    assert_eq!(
        current.vision_preparation.unwrap().state,
        NativeVisionPreparationState::Preparing
    );
    release.notify_one();
    task.await.unwrap().unwrap();
    assert_eq!(
        fixture
            .host
            .request(query)
            .await
            .unwrap()
            .vision_preparation
            .unwrap()
            .state,
        NativeVisionPreparationState::Ready
    );
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn confirmed_proposal_rechecks_frame_and_executes_at_most_once() {
    use ai_ex_domain::NativeVisionOperationState;
    let mut fixture = Fixture::new(true);
    install_vision(
        &mut fixture,
        fake_vision(r#"{"type":"click","x":0,"y":0,"button":"left"}"#),
    );
    let running = ready(&fixture).await;
    let proposal_id = Uuid::new_v4();
    let prepared = fixture
        .host
        .request(prepare(&running, proposal_id))
        .await
        .unwrap()
        .snapshot;
    let operation_id = Uuid::new_v4();
    let request = confirm(&prepared, proposal_id, operation_id);
    let completed = fixture.host.request(request.clone()).await.unwrap();
    assert_eq!(
        completed.vision_operation.unwrap().state,
        NativeVisionOperationState::Completed
    );
    assert_eq!(fixture.system.lock().unwrap().captures, 2);
    assert_eq!(fixture.system.lock().unwrap().calls.len(), 1);
    assert_eq!(completed.snapshot.session.as_ref().unwrap().actions, 1);
    assert!(
        completed
            .snapshot
            .session
            .as_ref()
            .unwrap()
            .frame_id
            .is_none()
    );
    let queried = fixture
        .host
        .request(operation(&completed.snapshot, proposal_id, operation_id))
        .await
        .unwrap();
    assert_eq!(
        queried.vision_operation.unwrap().state,
        NativeVisionOperationState::Completed
    );
    let again = fixture.host.request(request).await.unwrap();
    assert_eq!(
        again.vision_operation.unwrap().state,
        NativeVisionOperationState::Completed
    );
    assert_eq!(fixture.system.lock().unwrap().captures, 2);
    assert_eq!(fixture.system.lock().unwrap().calls.len(), 1);
}

#[tokio::test]
async fn changed_frame_rejects_confirmation_without_input() {
    use ai_ex_domain::NativeVisionOperationState;
    let mut fixture = Fixture::new(true);
    install_vision(
        &mut fixture,
        fake_vision(r#"{"type":"click","x":0,"y":0,"button":"left"}"#),
    );
    let running = ready(&fixture).await;
    let proposal_id = Uuid::new_v4();
    let prepared = fixture
        .host
        .request(prepare(&running, proposal_id))
        .await
        .unwrap()
        .snapshot;
    fixture.system.lock().unwrap().frame_byte = 42;
    let operation_id = Uuid::new_v4();
    let request = confirm(&prepared, proposal_id, operation_id);
    let result = fixture.host.request(request.clone()).await.unwrap();
    assert_eq!(
        result.vision_operation.unwrap().state,
        NativeVisionOperationState::Rejected
    );
    assert_eq!(result.snapshot.session.as_ref().unwrap().actions, 0);
    assert!(result.snapshot.session.as_ref().unwrap().frame_id.is_none());
    assert_eq!(fixture.system.lock().unwrap().captures, 2);
    assert!(fixture.system.lock().unwrap().calls.is_empty());
    assert_eq!(
        fixture
            .host
            .request(request)
            .await
            .unwrap()
            .vision_operation
            .unwrap()
            .state,
        NativeVisionOperationState::Rejected
    );
    assert_eq!(fixture.system.lock().unwrap().captures, 2);
}

#[tokio::test]
async fn partial_input_is_unknown_and_duplicate_confirmation_never_replays() {
    use ai_ex_domain::NativeVisionOperationState;
    let mut fixture = Fixture::new(true);
    install_vision(
        &mut fixture,
        fake_vision(r#"{"type":"click","x":0,"y":0,"button":"left"}"#),
    );
    let running = ready(&fixture).await;
    let proposal_id = Uuid::new_v4();
    let prepared = fixture
        .host
        .request(prepare(&running, proposal_id))
        .await
        .unwrap()
        .snapshot;
    fixture.system.lock().unwrap().partial = true;
    let operation_id = Uuid::new_v4();
    let request = confirm(&prepared, proposal_id, operation_id);
    let result = fixture.host.request(request.clone()).await.unwrap();
    assert_eq!(
        result.vision_operation.unwrap().state,
        NativeVisionOperationState::Unknown
    );
    let before = fixture.system.lock().unwrap().calls.clone();
    assert!(!before.is_empty());
    assert_eq!(
        fixture
            .host
            .request(request)
            .await
            .unwrap()
            .vision_operation
            .unwrap()
            .state,
        NativeVisionOperationState::Unknown
    );
    assert_eq!(fixture.system.lock().unwrap().calls, before);
    assert_eq!(
        fixture
            .host
            .request(operation(&result.snapshot, proposal_id, operation_id))
            .await
            .unwrap()
            .vision_operation
            .unwrap()
            .state,
        NativeVisionOperationState::Unknown
    );
}

#[tokio::test]
async fn completion_audit_failure_is_unknown_and_never_replays() {
    use ai_ex_domain::NativeVisionOperationState;
    let mut fixture = Fixture::new(true);
    install_vision(
        &mut fixture,
        fake_vision(r#"{"type":"click","x":0,"y":0,"button":"left"}"#),
    );
    let running = ready(&fixture).await;
    let proposal_id = Uuid::new_v4();
    let prepared = fixture
        .host
        .request(prepare(&running, proposal_id))
        .await
        .unwrap()
        .snapshot;
    let failures = Arc::new(AtomicUsize::new(0));
    {
        let mut state = fixture.host.state.lock().await;
        let ticket = state.ticket.unwrap();
        let port = NativeDesktopPort::new(
            state.desktop.clone(),
            ticket,
            fixture.host.shared.cancelled.clone(),
            fixture.host.shared.activity.clone(),
            fixture.host.shared.activity.load(Ordering::Acquire),
        );
        state.coordinator = Some(AutomationCoordinator::new(
            fixture.host.safety.clone(),
            port,
            TestAudit::FailMouseCompletion(failures.clone()),
        ));
    }
    let operation_id = Uuid::new_v4();
    let request = confirm(&prepared, proposal_id, operation_id);
    let result = fixture.host.request(request.clone()).await.unwrap();
    assert_eq!(
        result.vision_operation.unwrap().state,
        NativeVisionOperationState::Unknown
    );
    assert_eq!(failures.load(Ordering::SeqCst), 1);
    let calls = fixture.system.lock().unwrap().calls.clone();
    assert_eq!(calls.len(), 1);
    let repeated = fixture.host.request(request).await.unwrap();
    assert_eq!(
        repeated.vision_operation.unwrap().state,
        NativeVisionOperationState::Unknown
    );
    assert_eq!(fixture.system.lock().unwrap().calls, calls);
    assert_eq!(failures.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .host
            .request(operation(&result.snapshot, proposal_id, operation_id))
            .await
            .unwrap()
            .vision_operation
            .unwrap()
            .state,
        NativeVisionOperationState::Unknown
    );
}

#[tokio::test]
async fn pending_confirmation_is_queryable_and_pause_revokes_before_input() {
    use ai_ex_domain::NativeVisionOperationState;
    let mut fixture = Fixture::new(true);
    install_vision(
        &mut fixture,
        fake_vision(r#"{"type":"click","x":0,"y":0,"button":"left"}"#),
    );
    let running = ready(&fixture).await;
    let proposal_id = Uuid::new_v4();
    let prepared = fixture
        .host
        .request(prepare(&running, proposal_id))
        .await
        .unwrap()
        .snapshot;
    let entered = Arc::new(Notify::new());
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    fixture.system.lock().unwrap().capture_barrier = Some(CaptureBarrier {
        entered: entered.clone(),
        release: release.clone(),
    });
    let operation_id = Uuid::new_v4();
    let host = fixture.host.clone();
    let command = confirm(&prepared, proposal_id, operation_id);
    let in_flight = tokio::spawn(async move { host.request(command).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let current = fixture.host.snapshot().unwrap();
    let pending = fixture
        .host
        .request(operation(&current, proposal_id, operation_id))
        .await
        .unwrap();
    assert_eq!(
        pending.vision_operation.unwrap().state,
        NativeVisionOperationState::Pending
    );
    fixture
        .host
        .request(NativeDesktopRequest::Pause {
            instance_id: prepared.instance_id,
            expected_revision: prepared.revision,
            session_id: prepared.session.as_ref().unwrap().session_id,
        })
        .await
        .unwrap();
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    let settled = in_flight.await.unwrap().unwrap();
    assert_eq!(
        settled.vision_operation.unwrap().state,
        NativeVisionOperationState::Rejected
    );
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn preview_is_read_only_audited_and_never_replays_an_operation() {
    let mut fixture = Fixture::new(true);
    let vision = fake_vision(r#"{"type":"type_text","text":"private-preview"}"#);
    let calls = vision.calls.clone();
    install_vision(&mut fixture, vision);
    let running = ready(&fixture).await;
    let operation_id = Uuid::new_v4();
    let preview_request = request(&running, operation_id);
    let reply = fixture.host.request(preview_request.clone()).await.unwrap();
    let preview = reply.vision_preview.unwrap();
    assert_eq!(preview.operation_id, operation_id);
    assert_eq!(
        preview.action,
        NativeDesktopAction::TypeText {
            text: "private-preview".into()
        }
    );
    assert!(!format!("{preview:?}").contains("private-preview"));
    let session = reply.snapshot.session.unwrap();
    assert_eq!((session.frames, session.actions), (1, 0));
    assert!(session.frame_id.is_none());
    assert!(fixture.host.state.lock().await.frame.is_none());
    assert_eq!(fixture.system.lock().unwrap().captures, 1);
    assert!(fixture.system.lock().unwrap().calls.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(fixture.host.request(preview_request.clone()).await.is_err());
    let mut changed = preview_request;
    if let NativeDesktopRequest::PreviewVision { goal, .. } = &mut changed {
        *goal = "different goal".into();
    }
    assert!(fixture.host.request(changed).await.is_err());
    assert_eq!(fixture.system.lock().unwrap().captures, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let log = std::fs::read_to_string(fixture.directory.join("audit.jsonl")).unwrap();
    let records: Vec<ai_ex_automation::AuditRecord> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 3);
    assert!(
        records
            .iter()
            .all(|record| record.capability == Capability::ScreenRead)
    );
    assert!(!log.contains("private-preview"));
}

#[tokio::test]
async fn changed_target_or_invalid_model_output_discards_the_preview_frame() {
    for change_target in [Some(0), Some(1), Some(2), None] {
        let mut fixture = Fixture::new(true);
        let mut vision = fake_vision(if change_target.is_some() {
            r#"{"type":"click","x":0,"y":0,"button":"left"}"#
        } else {
            r#"{"type":"unsupported"}"#
        });
        if let Some(change) = change_target {
            vision.change_target = Some((fixture.system.clone(), change));
        }
        let calls = vision.calls.clone();
        install_vision(&mut fixture, vision);
        let running = ready(&fixture).await;
        let operation_id = Uuid::new_v4();
        assert!(
            fixture
                .host
                .request(request(&running, operation_id))
                .await
                .is_err()
        );
        let settled = fixture.host.snapshot().unwrap();
        let session = settled.session.as_ref().unwrap();
        assert_eq!((session.frames, session.actions), (1, 0));
        assert!(session.frame_id.is_none());
        assert!(fixture.host.state.lock().await.frame.is_none());
        assert!(fixture.system.lock().unwrap().calls.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            fixture
                .host
                .request(request(&settled, operation_id))
                .await
                .is_err()
        );
        assert_eq!(fixture.system.lock().unwrap().captures, 1);
    }
}

#[tokio::test]
async fn revoked_screen_permission_never_captures_or_calls_vision() {
    let mut fixture = Fixture::new(true);
    let vision = fake_vision(r#"{"type":"noop"}"#);
    let calls = vision.calls.clone();
    install_vision(&mut fixture, vision);
    let running = ready(&fixture).await;
    fixture.host.safety.trigger_emergency_stop();
    assert!(
        fixture
            .host
            .request(request(&running, Uuid::new_v4()))
            .await
            .is_err()
    );
    assert_eq!(fixture.system.lock().unwrap().captures, 0);
    assert!(fixture.system.lock().unwrap().calls.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let snapshot = fixture.host.snapshot().unwrap();
    assert!(snapshot.session.unwrap().frame_id.is_none());
    assert!(fixture.host.state.lock().await.frame.is_none());
}

#[tokio::test]
async fn pause_during_analysis_revokes_late_result_without_input() {
    let mut fixture = Fixture::new(true);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut vision = fake_vision(r#"{"type":"click","x":0,"y":0,"button":"left"}"#);
    vision.entered = Some(entered.clone());
    vision.release = Some(release.clone());
    install_vision(&mut fixture, vision);
    let running = ready(&fixture).await;
    let session_id = running.session.as_ref().unwrap().session_id;
    let host = fixture.host.clone();
    let preview = tokio::spawn({
        let request = request(&running, Uuid::new_v4());
        async move { host.request(request).await }
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let paused = fixture
        .host
        .request(NativeDesktopRequest::Pause {
            instance_id: running.instance_id,
            expected_revision: running.revision,
            session_id,
        })
        .await
        .unwrap()
        .snapshot;
    assert_eq!(paused.session.unwrap().phase, NativeDesktopPhase::Paused);
    release.notify_one();
    assert!(preview.await.unwrap().is_err());
    let settled = fixture.host.snapshot().unwrap();
    assert!(settled.session.unwrap().frame_id.is_none());
    assert!(fixture.host.state.lock().await.frame.is_none());
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}
