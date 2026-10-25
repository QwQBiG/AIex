use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ai_ex_automation::{
    ActionResult, AuditRecord, AuditSink, AuditStage, AutomationAction, AutomationCoordinator,
    AutomationPort, ChatPauseOptions, ChatPauseOutcome, ChatPausePolicy, DesktopBounds,
    FrameGeometry, MatchOptions, ReflexControlMode, ReflexController, ReflexInactiveReason,
    ReflexMode, ReflexOptions, ReflexOutcome, ReflexSession, ReflexSessionOutcome, ReflexState,
    ResumeEligibility, ScreenFrame, TemplateMatcher,
};
use ai_ex_domain::AppError;
use ai_ex_safety::{Capability, Permit, SafetyGate, SafetyPolicy};
use async_trait::async_trait;
use tokio::sync::oneshot;

const TARGET: &str = "synthetic-chat-target";
const START: u64 = 100;
const DEADLINE: u64 = START + 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Capture,
    Move,
    Click,
}

fn kind(action: &AutomationAction) -> Kind {
    match action {
        AutomationAction::CaptureScreen => Kind::Capture,
        AutomationAction::MovePointer { .. } => Kind::Move,
        AutomationAction::Click { .. } => Kind::Click,
        _ => panic!("unexpected action in chat pause fixture"),
    }
}

struct AwaitGate {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

struct Checkpoint {
    entered: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
}

fn rendezvous() -> (AwaitGate, Checkpoint) {
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    (
        AwaitGate {
            entered: entered_tx,
            release: release_rx,
        },
        Checkpoint {
            entered: entered_rx,
            release: release_tx,
        },
    )
}

struct Port {
    frames: VecDeque<ScreenFrame>,
    actions: Arc<Mutex<Vec<AutomationAction>>>,
    hook: Option<(Kind, AwaitGate)>,
}

#[async_trait]
impl AutomationPort for Port {
    async fn execute(
        &mut self,
        permit: &Permit,
        action: &AutomationAction,
    ) -> Result<ActionResult, AppError> {
        permit.ensure_active()?;
        action.validate()?;
        if permit.capability() != action.required_capability() {
            return Err(AppError::safety("chat pause fixture permit mismatch"));
        }
        self.actions.lock().unwrap().push(action.clone());
        if self
            .hook
            .as_ref()
            .is_some_and(|(selected, _)| *selected == kind(action))
        {
            let (_, hook) = self.hook.take().unwrap();
            hook.entered.send(()).expect("test observer remains alive");
            hook.release
                .await
                .expect("test releases the dispatched action");
        }
        match action {
            AutomationAction::CaptureScreen => self
                .frames
                .pop_front()
                .map(ActionResult::ScreenCaptured)
                .ok_or_else(|| AppError::unavailable("synthetic frame sequence exhausted")),
            _ => Ok(ActionResult::Completed),
        }
    }
}

struct Audit(Arc<Mutex<Vec<AuditRecord>>>);

#[async_trait]
impl AuditSink for Audit {
    async fn record(&mut self, record: AuditRecord) -> Result<(), AppError> {
        self.0.lock().unwrap().push(record);
        Ok(())
    }
}

struct Harness {
    coordinator: AutomationCoordinator<Port, Audit>,
    actions: Arc<Mutex<Vec<AutomationAction>>>,
    audit: Arc<Mutex<Vec<AuditRecord>>>,
}

fn harness(frames: Vec<ScreenFrame>, hook: Option<Kind>) -> (Harness, Option<Checkpoint>) {
    let (hook, checkpoint) = match hook {
        Some(kind) => {
            let (gate, checkpoint) = rendezvous();
            (Some((kind, gate)), Some(checkpoint))
        }
        None => (None, None),
    };
    let actions = Arc::new(Mutex::new(Vec::new()));
    let audit = Arc::new(Mutex::new(Vec::new()));
    let safety = Arc::new(SafetyGate::new(SafetyPolicy {
        automation_enabled: true,
        allowed_capabilities: BTreeSet::from([Capability::ScreenRead, Capability::MouseInput]),
        allowed_targets: vec![TARGET.to_owned()],
    }));
    (
        Harness {
            coordinator: AutomationCoordinator::new(
                safety,
                Port {
                    frames: frames.into(),
                    actions: Arc::clone(&actions),
                    hook,
                },
                Audit(Arc::clone(&audit)),
            ),
            actions,
            audit,
        },
        checkpoint,
    )
}

// The same seeded 20x20 grayscale texture used by the reflex session regressions.
fn texture() -> ScreenFrame {
    let mut state = 19_u32;
    let mut rgba = Vec::new();
    for _ in 0..400 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let pixel = (state >> 24) as u8;
        rgba.extend_from_slice(&[pixel, pixel, pixel, 255]);
    }
    ScreenFrame::new(20, 20, rgba).unwrap()
}

fn placed(x: u32) -> ScreenFrame {
    let template = texture();
    let mut frame = ScreenFrame::new(80, 20, vec![0; 80 * 20 * 4]).unwrap();
    for row in 0..20_usize {
        let source = row * 80;
        let destination = (row * 80 + x as usize) * 4;
        frame.rgba[destination..destination + 80]
            .copy_from_slice(&template.rgba[source..source + 80]);
    }
    frame
}

fn session(mode: ReflexMode) -> ReflexSession {
    ReflexSession::new(
        ReflexController::new(
            TARGET.to_owned(),
            TemplateMatcher::new(&texture(), MatchOptions::default()).unwrap(),
            ReflexOptions {
                mode,
                max_frames: 4,
                max_actions: 6,
                ..ReflexOptions::default()
            },
        )
        .unwrap(),
    )
}

fn geometry() -> FrameGeometry {
    FrameGeometry::new(
        0,
        0,
        1.0,
        1.0,
        DesktopBounds::new(0, 0, 1000, 1000).unwrap(),
    )
    .unwrap()
}

fn policy(session: &ReflexSession) -> ChatPausePolicy {
    ChatPausePolicy::new(
        session.control(),
        ChatPauseOptions {
            enabled: true,
            timeout_ms: 5_000,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn busy_and_unknown_hold_chat_pause_then_idle_resumes_without_reviving_the_dispatched_capture()
 {
    let mut session = session(ReflexMode::ClickOnce);
    let control = session.control();
    let original = control.begin_step().unwrap();
    let mut policy = policy(&session);
    let (mut harness, checkpoint) = harness(vec![placed(5), placed(45)], Some(Kind::Capture));
    let checkpoint = checkpoint.unwrap();
    let geometry = geometry();
    let commands = async {
        checkpoint
            .entered
            .await
            .expect("capture reaches the bounded checkpoint");
        let claimed = policy.on_chat(START).unwrap();
        assert_eq!(claimed.outcome, ChatPauseOutcome::Claimed);
        assert_eq!(claimed.deadline_ms, Some(DEADLINE));
        assert_eq!(
            policy
                .poll(DEADLINE - 1, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Waiting
        );
        assert_eq!(
            policy
                .poll(DEADLINE, ResumeEligibility::Busy)
                .unwrap()
                .outcome,
            ChatPauseOutcome::HeldBusy
        );
        assert_eq!(
            control.snapshot().unwrap().mode,
            ReflexControlMode::PausedForChat
        );
        assert_eq!(
            policy
                .poll(DEADLINE, ResumeEligibility::Unknown)
                .unwrap()
                .outcome,
            ChatPauseOutcome::HeldUnknown
        );
        assert_eq!(
            control.snapshot().unwrap().mode,
            ReflexControlMode::PausedForChat
        );
        assert_eq!(
            policy
                .poll(DEADLINE, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Resumed
        );
        assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Running);
        assert_eq!(
            original.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::StaleGeneration
        );
        checkpoint
            .release
            .send(())
            .expect("capture is still awaiting release");
    };
    let (interrupted, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            session.step(&mut harness.coordinator, &geometry, START),
            commands
        )
    })
    .await
    .expect("capture/chat handshake completes without sleeps");
    let interrupted = interrupted.unwrap();
    assert_eq!(interrupted.outcome, ReflexSessionOutcome::Interrupted);
    assert_eq!(
        interrupted.interruption.unwrap().inactive.reason,
        ReflexInactiveReason::StaleGeneration
    );
    assert_eq!((interrupted.step.frames, interrupted.step.actions), (1, 0));
    assert_eq!(interrupted.step.receipts.len(), 1);
    assert_eq!(
        harness.actions.lock().unwrap().as_slice(),
        &[AutomationAction::CaptureScreen]
    );

    let fresh = session
        .step(&mut harness.coordinator, &geometry, DEADLINE)
        .await
        .unwrap();
    assert_eq!(fresh.outcome, ReflexSessionOutcome::Advanced);
    assert_eq!(fresh.step.state, ReflexState::Completed);
    assert_eq!(fresh.step.outcome, ReflexOutcome::Completed);
    assert_eq!(
        fresh.step.matched_bounds.unwrap().x,
        45,
        "resume must capture the next frame"
    );
    assert_eq!((fresh.step.frames, fresh.step.actions), (2, 2));
    assert_eq!(fresh.step.receipts.len(), 3);
    let actions = harness.actions.lock().unwrap();
    assert_eq!(actions.len(), 4);
    assert_eq!(kind(&actions[0]), Kind::Capture);
    assert_eq!(kind(&actions[1]), Kind::Capture);
    assert_eq!(kind(&actions[2]), Kind::Move);
    assert_eq!(kind(&actions[3]), Kind::Click);
    let audit = harness.audit.lock().unwrap();
    assert_eq!(audit.len(), 12);
    assert_eq!(
        audit
            .iter()
            .filter(|record| record.stage == AuditStage::Completed)
            .count(),
        4
    );
}

#[tokio::test]
async fn manual_pause_after_chat_claim_keeps_the_dispatched_move_but_prevents_click_and_auto_resume()
 {
    let mut session = session(ReflexMode::ClickOnce);
    let control = session.control();
    let mut policy = policy(&session);
    let (mut harness, checkpoint) = harness(vec![placed(5)], Some(Kind::Move));
    let checkpoint = checkpoint.unwrap();
    let geometry = geometry();
    let commands = async {
        checkpoint
            .entered
            .await
            .expect("move reaches the bounded checkpoint");
        assert_eq!(
            policy.on_chat(START).unwrap().outcome,
            ChatPauseOutcome::Claimed
        );
        control.pause_for_chat().unwrap();
        assert_eq!(
            policy
                .poll(DEADLINE, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Superseded
        );
        assert_eq!(
            policy
                .poll(DEADLINE + 1, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Unowned
        );
        assert_eq!(
            control.snapshot().unwrap().mode,
            ReflexControlMode::PausedForChat
        );
        checkpoint
            .release
            .send(())
            .expect("move remains pending until released");
    };
    let (interrupted, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            session.step(&mut harness.coordinator, &geometry, START),
            commands
        )
    })
    .await
    .expect("move/chat handshake completes without sleeps");
    let interrupted = interrupted.unwrap();
    assert_eq!(interrupted.outcome, ReflexSessionOutcome::Interrupted);
    assert_eq!(
        interrupted.interruption.unwrap().inactive.reason,
        ReflexInactiveReason::PausedForChat
    );
    assert_eq!((interrupted.step.frames, interrupted.step.actions), (1, 1));
    assert_eq!(interrupted.step.receipts.len(), 2);
    assert_eq!(harness.actions.lock().unwrap().len(), 2);
    assert!(
        harness
            .actions
            .lock()
            .unwrap()
            .iter()
            .all(|action| kind(action) != Kind::Click)
    );
    let paused = session
        .step(&mut harness.coordinator, &geometry, DEADLINE + 1)
        .await
        .unwrap();
    assert_eq!(paused.outcome, ReflexSessionOutcome::Interrupted);
    assert_eq!(
        paused.interruption.unwrap().inactive.reason,
        ReflexInactiveReason::PausedForChat
    );
    assert!(paused.step.receipts.is_empty());
    assert_eq!((paused.step.frames, paused.step.actions), (1, 1));
    assert_eq!(harness.actions.lock().unwrap().len(), 2);
    assert_eq!(harness.audit.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn terminal_stop_supersedes_chat_ownership_and_never_dispatches_a_resumed_step() {
    let mut session = session(ReflexMode::Hover);
    let control = session.control();
    let mut policy = policy(&session);
    let (mut harness, _) = harness(vec![placed(5)], None);
    let geometry = geometry();
    let initial = session
        .step(&mut harness.coordinator, &geometry, START)
        .await
        .unwrap();
    assert_eq!(initial.step.state, ReflexState::Tracking);
    assert_eq!((initial.step.frames, initial.step.actions), (1, 1));
    assert_eq!(
        policy.on_chat(START).unwrap().outcome,
        ChatPauseOutcome::Claimed
    );
    let stopped = control.stop().unwrap();
    assert_eq!(
        policy
            .poll(DEADLINE, ResumeEligibility::Idle)
            .unwrap()
            .outcome,
        ChatPauseOutcome::Superseded
    );
    assert_eq!(
        policy
            .poll(DEADLINE + 1, ResumeEligibility::Idle)
            .unwrap()
            .outcome,
        ChatPauseOutcome::Unowned
    );
    assert_eq!(control.snapshot().unwrap(), stopped);
    assert_eq!(stopped.mode, ReflexControlMode::Stopped);
    let inactive = session
        .step(&mut harness.coordinator, &geometry, DEADLINE + 1)
        .await
        .unwrap();
    assert_eq!(inactive.outcome, ReflexSessionOutcome::Interrupted);
    assert_eq!(
        inactive.interruption.unwrap().inactive.reason,
        ReflexInactiveReason::Stopped
    );
    assert_eq!(inactive.step.state, ReflexState::Searching);
    assert_eq!((inactive.step.frames, inactive.step.actions), (1, 1));
    assert!(inactive.step.receipts.is_empty());
    assert_eq!(harness.actions.lock().unwrap().len(), 2);
    assert_eq!(harness.audit.lock().unwrap().len(), 6);
}
