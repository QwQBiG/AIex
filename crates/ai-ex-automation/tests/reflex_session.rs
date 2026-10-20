use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ai_ex_automation::{
    ActionResult, AuditRecord, AuditSink, AuditStage, AutomationAction, AutomationCoordinator,
    AutomationPort, DesktopBounds, FrameGeometry, MatchOptions, PixelRect, ReflexControlMode,
    ReflexController, ReflexExecutionPhase, ReflexFailure, ReflexFailureStage,
    ReflexInactiveReason, ReflexMode, ReflexOptions, ReflexOutcome, ReflexSession,
    ReflexSessionOutcome, ReflexSessionReport, ReflexState, ReflexStepReport, ScreenFrame,
    TemplateMatcher,
};
use ai_ex_domain::AppError;
use ai_ex_safety::{Capability, Permit, SafetyGate, SafetyPolicy};
use async_trait::async_trait;
use tokio::sync::oneshot;

const TARGET: &str = "synthetic-target";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Capture,
    Move,
    Click,
}

impl Kind {
    fn action(action: &AutomationAction) -> Self {
        match action {
            AutomationAction::CaptureScreen => Self::Capture,
            AutomationAction::MovePointer { .. } => Self::Move,
            AutomationAction::Click { .. } => Self::Click,
            _ => panic!("unexpected action in synthetic reflex workflow"),
        }
    }

    fn label(label: &str) -> Self {
        if label == "capture_screen" {
            Self::Capture
        } else if label.starts_with("move_pointer(") {
            Self::Move
        } else if label.starts_with("click(") {
            Self::Click
        } else {
            panic!("unexpected audit action in synthetic reflex workflow")
        }
    }

    fn stage(self) -> ReflexFailureStage {
        match self {
            Self::Capture => ReflexFailureStage::Capture,
            Self::Move => ReflexFailureStage::Move,
            Self::Click => ReflexFailureStage::Click,
        }
    }

    fn preceding_actions(self) -> u64 {
        match self {
            Self::Capture | Self::Move => 0,
            Self::Click => 1,
        }
    }

    fn preceding_receipts(self) -> usize {
        match self {
            Self::Capture => 0,
            Self::Move => 1,
            Self::Click => 2,
        }
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

impl AwaitGate {
    async fn wait(self) {
        self.entered.send(()).expect("test observer remains alive");
        self.release
            .await
            .expect("test releases the suspended action");
    }
}

struct PortHook {
    kind: Kind,
    occurrence: usize,
    gate: AwaitGate,
}

struct AuditHook {
    kind: Kind,
    stage: AuditStage,
    occurrence: usize,
    gate: AwaitGate,
}

#[derive(Clone, Default)]
struct Ledger {
    actions: Vec<AutomationAction>,
    records: Vec<AuditRecord>,
    attempts: Vec<AuditRecord>,
}

#[derive(Clone, Default)]
struct Monitor(Arc<Mutex<Ledger>>);

impl Monitor {
    fn view(&self) -> Ledger {
        self.0.lock().unwrap().clone()
    }

    fn kinds(&self) -> Vec<Kind> {
        self.view().actions.iter().map(Kind::action).collect()
    }
}

struct Plan {
    frames: VecDeque<ScreenFrame>,
    port_hooks: Vec<PortHook>,
    audit_hooks: Vec<AuditHook>,
    port_failure: Option<Kind>,
    audit_failure: Option<(Kind, AuditStage)>,
}

impl Plan {
    fn new(frames: impl IntoIterator<Item = ScreenFrame>) -> Self {
        Self {
            frames: frames.into_iter().collect(),
            port_hooks: Vec::new(),
            audit_hooks: Vec::new(),
            port_failure: None,
            audit_failure: None,
        }
    }

    fn at_port(&mut self, kind: Kind) -> Checkpoint {
        let (gate, checkpoint) = rendezvous();
        self.port_hooks.push(PortHook {
            kind,
            occurrence: 1,
            gate,
        });
        checkpoint
    }

    fn at_audit(&mut self, kind: Kind, stage: AuditStage) -> Checkpoint {
        let (gate, checkpoint) = rendezvous();
        self.audit_hooks.push(AuditHook {
            kind,
            stage,
            occurrence: 1,
            gate,
        });
        checkpoint
    }
}

struct TestPort {
    frames: VecDeque<ScreenFrame>,
    hooks: Vec<PortHook>,
    failure: Option<Kind>,
    monitor: Monitor,
}

#[async_trait]
impl AutomationPort for TestPort {
    async fn execute(
        &mut self,
        permit: &Permit,
        action: &AutomationAction,
    ) -> Result<ActionResult, AppError> {
        permit.ensure_active()?;
        action.validate()?;
        if permit.capability() != action.required_capability() {
            return Err(AppError::safety("synthetic port capability mismatch"));
        }
        let kind = Kind::action(action);
        let occurrence = {
            let mut ledger = self.monitor.0.lock().unwrap();
            ledger.actions.push(action.clone());
            ledger
                .actions
                .iter()
                .filter(|action| Kind::action(action) == kind)
                .count()
        };
        if let Some(index) = self
            .hooks
            .iter()
            .position(|hook| hook.kind == kind && hook.occurrence == occurrence)
        {
            self.hooks.remove(index).gate.wait().await;
        }
        if self.failure == Some(kind) {
            return Err(AppError::unavailable("synthetic adapter failure"));
        }
        match action {
            AutomationAction::CaptureScreen => self
                .frames
                .pop_front()
                .map(ActionResult::ScreenCaptured)
                .ok_or_else(|| AppError::unavailable("synthetic frame queue exhausted")),
            _ => Ok(ActionResult::Completed),
        }
    }
}

struct TestAudit {
    hooks: Vec<AuditHook>,
    failure: Option<(Kind, AuditStage)>,
    monitor: Monitor,
}

#[async_trait]
impl AuditSink for TestAudit {
    async fn record(&mut self, record: AuditRecord) -> Result<(), AppError> {
        let kind = Kind::label(&record.action);
        let stage = record.stage;
        let occurrence = {
            let mut ledger = self.monitor.0.lock().unwrap();
            ledger.attempts.push(record.clone());
            ledger
                .attempts
                .iter()
                .filter(|attempt| Kind::label(&attempt.action) == kind && attempt.stage == stage)
                .count()
        };
        if let Some(index) = self.hooks.iter().position(|hook| {
            hook.kind == kind && hook.stage == stage && hook.occurrence == occurrence
        }) {
            self.hooks.remove(index).gate.wait().await;
        }
        if self.failure == Some((kind, stage)) {
            return Err(AppError::unavailable("synthetic audit persistence failure"));
        }
        self.monitor.0.lock().unwrap().records.push(record);
        Ok(())
    }
}

type Coordinator = AutomationCoordinator<TestPort, TestAudit>;

struct Harness {
    coordinator: Coordinator,
    monitor: Monitor,
}

impl Harness {
    fn new(plan: Plan) -> Self {
        let monitor = Monitor::default();
        let safety = Arc::new(SafetyGate::new(SafetyPolicy {
            automation_enabled: true,
            allowed_capabilities: BTreeSet::from([Capability::ScreenRead, Capability::MouseInput]),
            allowed_targets: vec![TARGET.to_owned()],
        }));
        let port = TestPort {
            frames: plan.frames,
            hooks: plan.port_hooks,
            failure: plan.port_failure,
            monitor: monitor.clone(),
        };
        let audit = TestAudit {
            hooks: plan.audit_hooks,
            failure: plan.audit_failure,
            monitor: monitor.clone(),
        };
        Self {
            coordinator: AutomationCoordinator::new(safety, port, audit),
            monitor,
        }
    }
}

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

fn black(width: u32) -> ScreenFrame {
    ScreenFrame::new(width, 20, vec![0; width as usize * 20 * 4]).unwrap()
}

fn placed(width: u32, x: u32) -> ScreenFrame {
    let template = texture();
    let mut frame = black(width);
    for row in 0..20_usize {
        let source = row * 20 * 4;
        let target = (row * width as usize + x as usize) * 4;
        frame.rgba[target..target + 80].copy_from_slice(&template.rgba[source..source + 80]);
    }
    frame
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

fn bounded_session(mode: ReflexMode, max_frames: u64, max_actions: u64) -> ReflexSession {
    let matcher = TemplateMatcher::new(&texture(), MatchOptions::default()).unwrap();
    let controller = ReflexController::new(
        TARGET.to_owned(),
        matcher,
        ReflexOptions {
            mode,
            interval_ms: 50,
            max_frames,
            max_actions,
            search_region: None,
        },
    )
    .unwrap();
    ReflexSession::new(controller)
}

fn session(mode: ReflexMode) -> ReflexSession {
    bounded_session(mode, 32, 64)
}

#[derive(Clone, Copy)]
enum Command {
    Pause,
    PauseResume,
    Stop,
    InvalidateTarget,
}

#[allow(
    clippy::result_large_err,
    reason = "test retains partial execution evidence"
)]
async fn at_hook(
    session: &mut ReflexSession,
    harness: &mut Harness,
    checkpoint: Checkpoint,
    command: Command,
    timestamp_ms: u64,
) -> Result<ReflexSessionReport, ReflexFailure> {
    let control = session.control();
    let commands = async move {
        checkpoint
            .entered
            .await
            .expect("runner reaches the scheduled await");
        match command {
            Command::Pause => {
                control.pause_for_chat().unwrap();
            }
            Command::PauseResume => {
                control.pause_for_chat().unwrap();
                control.resume().unwrap();
            }
            Command::Stop => {
                control.stop().unwrap();
            }
            Command::InvalidateTarget => {
                control.invalidate_target().unwrap();
            }
        }
        checkpoint
            .release
            .send(())
            .expect("runner waits for control changes");
    };
    let geometry = geometry();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            session.step(&mut harness.coordinator, &geometry, timestamp_ms),
            commands,
        )
    })
    .await
    .expect("synthetic await hook completes without sleeps");
    result
}

fn interrupted(report: &ReflexSessionReport, reason: ReflexInactiveReason) {
    assert_eq!(report.outcome, ReflexSessionOutcome::Interrupted);
    assert_eq!(report.step.outcome, ReflexOutcome::Inactive);
    assert_eq!(
        report.interruption.as_ref().unwrap().inactive.reason,
        reason
    );
}

fn receipt_kinds(report: &ReflexStepReport) -> Vec<Kind> {
    report
        .receipts
        .iter()
        .map(|receipt| Kind::action(&receipt.action))
        .collect()
}

fn audited(report: &ReflexStepReport, monitor: &Monitor) {
    let ledger = monitor.view();
    for receipt in &report.receipts {
        assert!(receipt.completion_audit_persisted);
        let records = ledger
            .records
            .iter()
            .filter(|record| record.action_id == receipt.action_id)
            .collect::<Vec<_>>();
        assert_eq!(
            records
                .iter()
                .map(|record| record.stage)
                .collect::<Vec<_>>(),
            vec![
                AuditStage::Requested,
                AuditStage::Authorized,
                AuditStage::Completed
            ]
        );
        assert!(records.iter().all(|record| record.target == TARGET));
    }
}

#[tokio::test]
async fn initially_paused_or_stopped_sessions_issue_no_action_or_audit() {
    for stopped in [false, true] {
        let mut session = session(ReflexMode::ClickOnce);
        let control = session.control();
        let original_id = control.snapshot().unwrap().session_id;
        if stopped {
            control.stop().unwrap();
        } else {
            control.pause_for_chat().unwrap();
        }
        let mut harness = Harness::new(Plan::new([texture()]));
        let report = session
            .step(&mut harness.coordinator, &geometry(), 0)
            .await
            .unwrap();
        interrupted(
            &report,
            if stopped {
                ReflexInactiveReason::Stopped
            } else {
                ReflexInactiveReason::PausedForChat
            },
        );
        assert_eq!((report.step.frames, report.step.actions), (0, 0));
        assert!(report.step.receipts.is_empty());
        let interruption = report.interruption.unwrap();
        assert_eq!(interruption.action_id, None);
        assert_eq!(interruption.rejection_audit_persisted, None);
        assert!(harness.monitor.kinds().is_empty());
        assert!(harness.monitor.view().attempts.is_empty());
        if stopped {
            assert!(control.resume().is_err());
            let mut fresh = bounded_session(ReflexMode::ClickOnce, 32, 64);
            assert_ne!(fresh.control().snapshot().unwrap().session_id, original_id);
            assert_eq!(
                fresh
                    .step(&mut harness.coordinator, &geometry(), 1)
                    .await
                    .unwrap()
                    .step
                    .state,
                ReflexState::Completed
            );
        } else {
            control.resume().unwrap();
            assert_eq!(
                session
                    .step(&mut harness.coordinator, &geometry(), 1)
                    .await
                    .unwrap()
                    .step
                    .state,
                ReflexState::Completed
            );
        }
    }
}

#[tokio::test]
async fn capture_pause_resume_keeps_its_receipt_and_interval_but_requires_a_fresh_frame() {
    let mut session = session(ReflexMode::ClickOnce);
    let mut plan = Plan::new([placed(40, 0), placed(40, 20)]);
    let hook = plan.at_port(Kind::Capture);
    let mut harness = Harness::new(plan);
    let report = at_hook(&mut session, &mut harness, hook, Command::PauseResume, 0)
        .await
        .unwrap();
    interrupted(&report, ReflexInactiveReason::StaleGeneration);
    assert_eq!(
        report
            .interruption
            .as_ref()
            .unwrap()
            .inactive
            .snapshot
            .unwrap()
            .mode,
        ReflexControlMode::Running
    );
    assert_eq!((report.step.frames, report.step.actions), (1, 0));
    assert_eq!(receipt_kinds(&report.step), vec![Kind::Capture]);
    audited(&report.step, &harness.monitor);
    let waiting = session
        .step(&mut harness.coordinator, &geometry(), 1)
        .await
        .unwrap();
    assert_eq!(waiting.outcome, ReflexSessionOutcome::Advanced);
    assert_eq!(waiting.step.outcome, ReflexOutcome::Waiting);
    assert_eq!(harness.monitor.kinds(), vec![Kind::Capture]);
    let fresh = session
        .step(&mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!(fresh.step.matched_bounds.unwrap().x, 20);
    assert_eq!((fresh.step.frames, fresh.step.actions), (2, 2));
    assert_eq!(fresh.step.state, ReflexState::Completed);
    assert_eq!(
        harness.monitor.kinds(),
        vec![Kind::Capture, Kind::Capture, Kind::Move, Kind::Click]
    );
    audited(&fresh.step, &harness.monitor);
}

#[tokio::test]
async fn pause_during_move_prevents_click_and_resume_captures_before_moving_again() {
    let mut session = session(ReflexMode::ClickOnce);
    let control = session.control();
    let mut plan = Plan::new([placed(40, 0), placed(40, 20)]);
    let hook = plan.at_port(Kind::Move);
    let mut harness = Harness::new(plan);
    let report = at_hook(&mut session, &mut harness, hook, Command::Pause, 0)
        .await
        .unwrap();
    interrupted(&report, ReflexInactiveReason::PausedForChat);
    assert_eq!(report.step.state, ReflexState::Searching);
    assert_eq!((report.step.frames, report.step.actions), (1, 1));
    assert_eq!(receipt_kinds(&report.step), vec![Kind::Capture, Kind::Move]);
    audited(&report.step, &harness.monitor);
    let paused = session
        .step(&mut harness.coordinator, &geometry(), 25)
        .await
        .unwrap();
    interrupted(&paused, ReflexInactiveReason::PausedForChat);
    assert_eq!(harness.monitor.kinds(), vec![Kind::Capture, Kind::Move]);
    control.resume().unwrap();
    let fresh = session
        .step(&mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!(fresh.step.matched_bounds.unwrap().x, 20);
    assert_eq!((fresh.step.frames, fresh.step.actions), (2, 3));
    assert_eq!(fresh.step.state, ReflexState::Completed);
    assert_eq!(
        harness.monitor.kinds(),
        vec![
            Kind::Capture,
            Kind::Move,
            Kind::Capture,
            Kind::Move,
            Kind::Click
        ]
    );
}

#[tokio::test]
async fn requested_and_authorized_audit_awaits_cannot_resume_a_stale_capture_dispatch() {
    for stage in [AuditStage::Requested, AuditStage::Authorized] {
        let mut session = session(ReflexMode::ClickOnce);
        let mut plan = Plan::new([texture()]);
        let hook = plan.at_audit(Kind::Capture, stage);
        let mut harness = Harness::new(plan);
        let report = at_hook(&mut session, &mut harness, hook, Command::PauseResume, 100)
            .await
            .unwrap();
        interrupted(&report, ReflexInactiveReason::StaleGeneration);
        assert_eq!((report.step.frames, report.step.actions), (0, 0));
        assert!(report.step.receipts.is_empty());
        assert!(harness.monitor.kinds().is_empty());
        let rejection = report.interruption.unwrap();
        assert_eq!(rejection.rejection_audit_persisted, Some(true));
        let ledger = harness.monitor.view();
        assert_eq!(
            ledger.records.iter().map(|r| r.stage).collect::<Vec<_>>(),
            vec![
                AuditStage::Requested,
                AuditStage::Authorized,
                AuditStage::Rejected
            ]
        );
        assert_eq!(
            ledger.records.last().unwrap().action_id,
            rejection.action_id.unwrap()
        );
        // No adapter dispatch means no capture budget or interval was consumed.
        let fresh = session
            .step(&mut harness.coordinator, &geometry(), 101)
            .await
            .unwrap();
        assert_eq!(fresh.step.state, ReflexState::Completed);
        assert_eq!((fresh.step.frames, fresh.step.actions), (1, 2));
        assert_eq!(
            harness.monitor.kinds(),
            vec![Kind::Capture, Kind::Move, Kind::Click]
        );
    }
}

#[tokio::test]
async fn final_dispatch_guard_rejects_stale_moves_and_clicks_before_the_adapter() {
    for kind in [Kind::Move, Kind::Click] {
        let mut session = session(ReflexMode::ClickOnce);
        let mut plan = Plan::new([texture(), texture()]);
        let hook = plan.at_audit(kind, AuditStage::Authorized);
        let mut harness = Harness::new(plan);
        let report = at_hook(&mut session, &mut harness, hook, Command::PauseResume, 0)
            .await
            .unwrap();
        interrupted(&report, ReflexInactiveReason::StaleGeneration);
        assert_eq!(
            (report.step.frames, report.step.actions),
            (1, kind.preceding_actions())
        );
        assert_eq!(report.step.receipts.len(), kind.preceding_receipts());
        assert!(!harness.monitor.kinds().contains(&Kind::Click));
        assert_eq!(
            report
                .interruption
                .as_ref()
                .unwrap()
                .rejection_audit_persisted,
            Some(true)
        );
        audited(&report.step, &harness.monitor);
        let fresh = session
            .step(&mut harness.coordinator, &geometry(), 50)
            .await
            .unwrap();
        assert_eq!(fresh.step.state, ReflexState::Completed);
        assert_eq!(
            (fresh.step.frames, fresh.step.actions),
            (2, kind.preceding_actions() + 2)
        );
    }
}
#[tokio::test]
async fn stopping_during_capture_is_terminal_for_that_control_allocation() {
    let mut session = session(ReflexMode::ClickOnce);
    let old_control = session.control();
    let mut plan = Plan::new([texture(), texture()]);
    let hook = plan.at_port(Kind::Capture);
    let mut harness = Harness::new(plan);
    let report = at_hook(&mut session, &mut harness, hook, Command::Stop, 0)
        .await
        .unwrap();
    interrupted(&report, ReflexInactiveReason::Stopped);
    assert_eq!((report.step.frames, report.step.actions), (1, 0));
    assert_eq!(receipt_kinds(&report.step), vec![Kind::Capture]);
    assert!(old_control.resume().is_err());
    let still_stopped = session
        .step(&mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    interrupted(&still_stopped, ReflexInactiveReason::Stopped);
    assert_eq!(harness.monitor.kinds(), vec![Kind::Capture]);
    let mut fresh = bounded_session(ReflexMode::ClickOnce, 32, 64);
    assert_ne!(
        fresh.control().snapshot().unwrap().session_id,
        old_control.snapshot().unwrap().session_id
    );
    let completed = fresh
        .step(&mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!(completed.step.state, ReflexState::Completed);
    assert_eq!((completed.step.frames, completed.step.actions), (1, 2));
    assert_eq!(
        old_control.snapshot().unwrap().mode,
        ReflexControlMode::Stopped
    );
}

#[tokio::test]
async fn target_generation_invalidation_discards_inflight_work_without_retargeting() {
    let mut session = session(ReflexMode::ClickOnce);
    let control = session.control();
    let mut plan = Plan::new([placed(40, 0), placed(40, 20)]);
    let hook = plan.at_port(Kind::Capture);
    let mut harness = Harness::new(plan);
    let report = at_hook(
        &mut session,
        &mut harness,
        hook,
        Command::InvalidateTarget,
        0,
    )
    .await
    .unwrap();
    interrupted(&report, ReflexInactiveReason::StaleGeneration);
    assert_eq!(control.snapshot().unwrap().generation, 1);
    assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Running);
    assert_eq!(harness.monitor.kinds(), vec![Kind::Capture]);
    assert_eq!((report.step.frames, report.step.actions), (1, 0));
    let fresh = session
        .step(&mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!(fresh.step.matched_bounds.unwrap().x, 20);
    assert_eq!(fresh.step.state, ReflexState::Completed);
    assert_eq!((fresh.step.frames, fresh.step.actions), (2, 2));
    assert!(
        harness
            .monitor
            .view()
            .records
            .iter()
            .all(|record| record.target == TARGET)
    );
}

#[tokio::test]
async fn adapter_failures_outrank_a_competing_pause_and_charge_possible_execution() {
    for kind in [Kind::Capture, Kind::Move, Kind::Click] {
        let mut session = session(ReflexMode::ClickOnce);
        let mut plan = Plan::new([texture()]);
        plan.port_failure = Some(kind);
        let hook = plan.at_port(kind);
        let mut harness = Harness::new(plan);
        let failure = at_hook(&mut session, &mut harness, hook, Command::Pause, 0)
            .await
            .unwrap_err();
        assert_eq!(failure.stage, kind.stage());
        assert_eq!(
            failure.execution_phase,
            Some(ReflexExecutionPhase::DuringExecution)
        );
        assert_eq!(failure.report.state, ReflexState::Failed);
        assert_eq!(failure.report.frames, 1);
        assert_eq!(
            failure.report.actions,
            kind.preceding_actions() + u64::from(kind != Kind::Capture)
        );
        assert_eq!(failure.report.receipts.len(), kind.preceding_receipts());
        assert_eq!(
            harness.monitor.view().records.last().unwrap().stage,
            AuditStage::Failed
        );
        let dispatched = harness.monitor.kinds();
        let terminal = session
            .step(&mut harness.coordinator, &geometry(), 50)
            .await
            .unwrap();
        assert_eq!(terminal.outcome, ReflexSessionOutcome::Advanced);
        assert_eq!(terminal.step.state, ReflexState::Failed);
        assert_eq!(terminal.step.outcome, ReflexOutcome::Inactive);
        assert!(terminal.interruption.is_none());
        assert!(terminal.step.receipts.is_empty());
        assert_eq!(harness.monitor.kinds(), dispatched);
    }
}

#[tokio::test]
async fn predispatch_and_completion_audit_failures_outrank_competing_control_changes() {
    for kind in [Kind::Capture, Kind::Move, Kind::Click] {
        for stage in [
            AuditStage::Requested,
            AuditStage::Authorized,
            AuditStage::Completed,
        ] {
            let mut session = session(ReflexMode::ClickOnce);
            let mut plan = Plan::new([texture()]);
            plan.audit_failure = Some((kind, stage));
            let hook = plan.at_audit(kind, stage);
            let mut harness = Harness::new(plan);
            let failure = at_hook(&mut session, &mut harness, hook, Command::PauseResume, 0)
                .await
                .unwrap_err();
            let completed = stage == AuditStage::Completed;
            assert_eq!(
                failure.stage,
                if completed {
                    ReflexFailureStage::CompletionAudit
                } else {
                    kind.stage()
                }
            );
            assert_eq!(
                failure.execution_phase,
                Some(if completed {
                    ReflexExecutionPhase::DuringExecution
                } else {
                    ReflexExecutionPhase::BeforeExecution
                })
            );
            assert_eq!(failure.report.state, ReflexState::Failed);
            assert_eq!(
                failure.report.frames,
                u64::from(kind != Kind::Capture || completed)
            );
            assert_eq!(
                failure.report.actions,
                kind.preceding_actions() + u64::from(completed && kind != Kind::Capture)
            );
            assert_eq!(
                failure.report.receipts.len(),
                kind.preceding_receipts() + usize::from(completed)
            );
            if completed {
                assert!(
                    !failure
                        .report
                        .receipts
                        .last()
                        .unwrap()
                        .completion_audit_persisted
                );
            }
            assert_eq!(harness.monitor.view().attempts.last().unwrap().stage, stage);
            let before = harness.monitor.kinds();
            let terminal = session
                .step(&mut harness.coordinator, &geometry(), 50)
                .await
                .unwrap();
            assert_eq!(terminal.outcome, ReflexSessionOutcome::Advanced);
            assert_eq!(terminal.step.state, ReflexState::Failed);
            assert!(terminal.interruption.is_none());
            assert_eq!(harness.monitor.kinds(), before);
        }
    }
}

#[tokio::test]
async fn guard_rejection_audit_failure_is_a_preexecution_failure_not_an_interruption() {
    let mut session = session(ReflexMode::ClickOnce);
    let mut plan = Plan::new([texture()]);
    let hook = plan.at_audit(Kind::Capture, AuditStage::Authorized);
    plan.audit_failure = Some((Kind::Capture, AuditStage::Rejected));
    let mut harness = Harness::new(plan);
    let failure = at_hook(&mut session, &mut harness, hook, Command::PauseResume, 0)
        .await
        .unwrap_err();
    assert_eq!(failure.stage, ReflexFailureStage::Capture);
    assert_eq!(
        failure.execution_phase,
        Some(ReflexExecutionPhase::BeforeExecution)
    );
    assert_eq!(failure.report.state, ReflexState::Failed);
    assert_eq!((failure.report.frames, failure.report.actions), (0, 0));
    assert!(failure.report.receipts.is_empty());
    assert!(harness.monitor.kinds().is_empty());
    let ledger = harness.monitor.view();
    assert_eq!(
        ledger
            .records
            .iter()
            .map(|record| record.stage)
            .collect::<Vec<_>>(),
        vec![AuditStage::Requested, AuditStage::Authorized]
    );
    assert_eq!(ledger.attempts.last().unwrap().stage, AuditStage::Rejected);
    assert_eq!(
        failure.action_id,
        Some(ledger.attempts.last().unwrap().action_id)
    );
    let terminal = session
        .step(&mut harness.coordinator, &geometry(), 1)
        .await
        .unwrap();
    assert_eq!(terminal.step.state, ReflexState::Failed);
    assert_eq!(terminal.outcome, ReflexSessionOutcome::Advanced);
    assert!(harness.monitor.kinds().is_empty());
}

#[tokio::test]
async fn confirmed_and_audited_click_once_completion_wins_late_pause_without_duplicate_clicks() {
    for command in [Command::Pause, Command::PauseResume] {
        let mut session = session(ReflexMode::ClickOnce);
        let control = session.control();
        let mut plan = Plan::new([texture(), texture()]);
        let hook = plan.at_audit(Kind::Click, AuditStage::Completed);
        let mut harness = Harness::new(plan);
        let completed = at_hook(&mut session, &mut harness, hook, command, 0)
            .await
            .unwrap();
        assert_eq!(completed.outcome, ReflexSessionOutcome::Advanced);
        assert_eq!(completed.step.state, ReflexState::Completed);
        assert_eq!(completed.step.outcome, ReflexOutcome::Completed);
        assert!(completed.interruption.is_none());
        assert_eq!((completed.step.frames, completed.step.actions), (1, 2));
        assert_eq!(
            receipt_kinds(&completed.step),
            vec![Kind::Capture, Kind::Move, Kind::Click]
        );
        audited(&completed.step, &harness.monitor);
        let terminal = session
            .step(&mut harness.coordinator, &geometry(), 1)
            .await
            .unwrap();
        assert_eq!(terminal.outcome, ReflexSessionOutcome::Advanced);
        assert_eq!(terminal.step.state, ReflexState::Completed);
        assert_eq!(terminal.step.outcome, ReflexOutcome::Inactive);
        assert!(terminal.step.receipts.is_empty());
        control.resume().unwrap();
        let resumed = session
            .step(&mut harness.coordinator, &geometry(), 50)
            .await
            .unwrap();
        assert_eq!(resumed.step.state, ReflexState::Completed);
        assert!(resumed.step.receipts.is_empty());
        assert_eq!(
            harness.monitor.kinds(),
            vec![Kind::Capture, Kind::Move, Kind::Click]
        );
        assert_eq!(harness.monitor.view().records.len(), 9);
    }
}

#[tokio::test]
async fn repeat_late_pause_retains_the_confirmed_cycle_and_resumes_from_a_fresh_capture() {
    let mut session = session(ReflexMode::ClickRepeat);
    let control = session.control();
    let mut plan = Plan::new([placed(40, 0), placed(40, 20)]);
    let hook = plan.at_audit(Kind::Click, AuditStage::Completed);
    let mut harness = Harness::new(plan);
    let report = at_hook(&mut session, &mut harness, hook, Command::Pause, 0)
        .await
        .unwrap();
    interrupted(&report, ReflexInactiveReason::PausedForChat);
    assert_eq!(report.step.state, ReflexState::Searching);
    assert_eq!((report.step.frames, report.step.actions), (1, 2));
    assert_eq!(
        receipt_kinds(&report.step),
        vec![Kind::Capture, Kind::Move, Kind::Click]
    );
    audited(&report.step, &harness.monitor);
    control.resume().unwrap();
    let resumed = session
        .step(&mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!(resumed.outcome, ReflexSessionOutcome::Advanced);
    assert_eq!(resumed.step.matched_bounds.unwrap().x, 20);
    assert_eq!((resumed.step.frames, resumed.step.actions), (2, 4));
    assert_eq!(
        receipt_kinds(&resumed.step),
        vec![Kind::Capture, Kind::Move, Kind::Click]
    );
    assert_eq!(harness.monitor.view().records.len(), 18);
}

#[tokio::test]
async fn pause_preserves_budgets_and_misses_but_discards_tracking_before_resume() {
    let mut session = bounded_session(ReflexMode::Hover, 4, 10);
    let control = session.control();
    let mut harness = Harness::new(Plan::new([
        placed(260, 220),
        black(260),
        black(260),
        placed(260, 0),
    ]));
    let first = session
        .step(&mut harness.coordinator, &geometry(), 0)
        .await
        .unwrap();
    assert_eq!(first.step.state, ReflexState::Tracking);
    assert_eq!((first.step.frames, first.step.actions), (1, 1));
    let miss = session
        .step(&mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!(miss.step.misses, 1);
    assert_eq!(miss.step.search_region.unwrap().x, 130);
    control.pause_for_chat().unwrap();
    let paused = session
        .step(&mut harness.coordinator, &geometry(), 100)
        .await
        .unwrap();
    interrupted(&paused, ReflexInactiveReason::PausedForChat);
    assert_eq!(
        (paused.step.frames, paused.step.actions, paused.step.misses),
        (2, 1, 1)
    );
    control.resume().unwrap();
    let resumed_miss = session
        .step(&mut harness.coordinator, &geometry(), 100)
        .await
        .unwrap();
    assert_eq!(
        resumed_miss.step.search_region,
        Some(PixelRect {
            x: 0,
            y: 0,
            width: 260,
            height: 20
        })
    );
    assert_eq!(
        (
            resumed_miss.step.frames,
            resumed_miss.step.actions,
            resumed_miss.step.misses
        ),
        (3, 1, 2)
    );
    let found = session
        .step(&mut harness.coordinator, &geometry(), 150)
        .await
        .unwrap();
    assert_eq!(found.step.matched_bounds.unwrap().x, 0);
    assert_eq!(
        (found.step.frames, found.step.actions, found.step.misses),
        (4, 2, 0)
    );
    assert_eq!(found.step.state, ReflexState::BudgetExhausted);
    control.pause_for_chat().unwrap();
    let terminal = session
        .step(&mut harness.coordinator, &geometry(), 200)
        .await
        .unwrap();
    assert_eq!(terminal.outcome, ReflexSessionOutcome::Advanced);
    assert_eq!(terminal.step.state, ReflexState::BudgetExhausted);
    assert!(terminal.step.receipts.is_empty());
    assert_eq!((terminal.step.frames, terminal.step.actions), (4, 2));
    assert_eq!(
        harness
            .monitor
            .kinds()
            .iter()
            .filter(|kind| **kind == Kind::Capture)
            .count(),
        4
    );
}

#[tokio::test]
async fn lost_progress_survives_pause_and_resumption_can_reacquire_the_target() {
    let mut session = session(ReflexMode::Hover);
    let control = session.control();
    let frames = (0..10).map(|_| black(20)).chain([texture()]);
    let mut harness = Harness::new(Plan::new(frames));
    for index in 0..10 {
        let report = session
            .step(&mut harness.coordinator, &geometry(), index * 50)
            .await
            .unwrap();
        assert_eq!(report.step.misses, index + 1);
    }
    assert_eq!(session.state(), ReflexState::Lost);
    control.pause_for_chat().unwrap();
    let paused = session
        .step(&mut harness.coordinator, &geometry(), 500)
        .await
        .unwrap();
    interrupted(&paused, ReflexInactiveReason::PausedForChat);
    assert_eq!(paused.step.state, ReflexState::Lost);
    assert_eq!(
        (paused.step.frames, paused.step.actions, paused.step.misses),
        (10, 0, 10)
    );
    control.resume().unwrap();
    let found = session
        .step(&mut harness.coordinator, &geometry(), 500)
        .await
        .unwrap();
    assert_eq!(found.step.state, ReflexState::Tracking);
    assert_eq!(
        (found.step.frames, found.step.actions, found.step.misses),
        (11, 1, 0)
    );
    assert_eq!(receipt_kinds(&found.step), vec![Kind::Capture, Kind::Move]);
}

#[tokio::test]
async fn between_step_generation_changes_immediately_discard_the_previous_tracking_roi() {
    for mode in [ReflexMode::Hover, ReflexMode::ClickRepeat] {
        for invalidate_target in [false, true] {
            let region = PixelRect {
                x: 20,
                y: 0,
                width: 280,
                height: 20,
            };
            let matcher = TemplateMatcher::new(&texture(), MatchOptions::default()).unwrap();
            let controller = ReflexController::new(
                TARGET.to_owned(),
                matcher,
                ReflexOptions {
                    mode,
                    interval_ms: 50,
                    max_frames: 10,
                    max_actions: 20,
                    search_region: Some(region),
                },
            )
            .unwrap();
            let mut session = ReflexSession::new(controller);
            let control = session.control();
            let mut harness = Harness::new(Plan::new([placed(300, 250), placed(300, 20)]));
            let first = session
                .step(&mut harness.coordinator, &geometry(), 0)
                .await
                .unwrap();
            assert_eq!(first.step.matched_bounds.unwrap().x, 250);
            assert_eq!(first.step.state, ReflexState::Tracking);
            if invalidate_target {
                control.invalidate_target().unwrap();
            } else {
                control.pause_for_chat().unwrap();
                control.resume().unwrap();
            }
            // No step ran while paused or stale: the next fresh lease itself must
            // detect the generation boundary and discard the old tracking ROI.
            let next = session
                .step(&mut harness.coordinator, &geometry(), 50)
                .await
                .unwrap();
            assert_eq!(next.outcome, ReflexSessionOutcome::Advanced);
            assert!(next.interruption.is_none());
            assert_eq!(next.step.search_region, Some(region));
            assert_eq!(next.step.matched_bounds.unwrap().x, 20);
            assert_eq!(next.step.misses, 0);
            assert_eq!(next.step.frames, 2);
            assert_eq!(
                next.step.actions,
                if mode == ReflexMode::Hover { 2 } else { 4 }
            );
            audited(&next.step, &harness.monitor);
        }
    }
}

async fn drop_at_checkpoint<F: std::future::Future>(future: F, checkpoint: Checkpoint) {
    let Checkpoint {
        mut entered,
        release,
    } = checkpoint;
    let mut future = Box::pin(future);
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = &mut future => panic!("step finished before its cancellation checkpoint"),
            reached = &mut entered => reached.expect("step reaches cancellation checkpoint"),
        }
    })
    .await
    .expect("synthetic cancellation checkpoint is bounded");
    // Cancel while the adapter/audit receiver is still pending. Closing release
    // first would wake the fake and test an error return instead of future Drop.
    drop(future);
    drop(release);
}

fn assert_cancelled_progress(report: &ReflexStepReport, frames: u64, actions: u64) {
    assert_eq!(report.state, ReflexState::Failed);
    assert_eq!(report.outcome, ReflexOutcome::Inactive);
    assert_eq!((report.frames, report.actions), (frames, actions));
    assert!(
        report.receipts.is_empty(),
        "terminal calls fabricate no pending receipt"
    );
}

async fn assert_session_cannot_retry_after_drop(
    session: &mut ReflexSession,
    harness: &mut Harness,
    frames: u64,
    actions: u64,
) {
    assert_eq!(session.state(), ReflexState::Failed);
    let before = harness.monitor.view();
    let report = session
        .step(&mut harness.coordinator, &geometry(), 50)
        .await
        .unwrap();
    assert_eq!(report.outcome, ReflexSessionOutcome::Advanced);
    assert!(report.interruption.is_none());
    assert_cancelled_progress(&report.step, frames, actions);
    let after = harness.monitor.view();
    assert_eq!(after.actions, before.actions);
    assert_eq!(after.records, before.records);
    assert_eq!(after.attempts, before.attempts);
}

#[tokio::test]
async fn dropping_predispatch_audit_awaits_fails_without_charging_unissued_actions() {
    for kind in [Kind::Capture, Kind::Move, Kind::Click] {
        for stage in [AuditStage::Requested, AuditStage::Authorized] {
            let mut session = session(ReflexMode::ClickOnce);
            let mut plan = Plan::new([texture(), texture()]);
            let hook = plan.at_audit(kind, stage);
            let mut harness = Harness::new(plan);
            let geometry = geometry();
            drop_at_checkpoint(session.step(&mut harness.coordinator, &geometry, 0), hook).await;
            let ledger = harness.monitor.view();
            assert_eq!(ledger.actions.len(), kind.preceding_receipts());
            assert_eq!(ledger.attempts.last().unwrap().stage, stage);
            assert_eq!(Kind::label(&ledger.attempts.last().unwrap().action), kind);
            assert!(!ledger.records.iter().any(|record| {
                Kind::label(&record.action) == kind && record.stage == AuditStage::Completed
            }));
            assert_session_cannot_retry_after_drop(
                &mut session,
                &mut harness,
                u64::from(kind != Kind::Capture),
                kind.preceding_actions(),
            )
            .await;
        }
    }
    for kind in [Kind::Capture, Kind::Move, Kind::Click] {
        let mut session = session(ReflexMode::ClickOnce);
        let mut plan = Plan::new([texture(), texture()]);
        let authorized = plan.at_audit(kind, AuditStage::Authorized);
        let rejected = plan.at_audit(kind, AuditStage::Rejected);
        let mut harness = Harness::new(plan);
        // The inner hook invalidates the lease and returns to the final guard;
        // cancel only after its Rejected audit itself has become pending.
        drop_at_checkpoint(
            at_hook(
                &mut session,
                &mut harness,
                authorized,
                Command::PauseResume,
                0,
            ),
            rejected,
        )
        .await;
        let ledger = harness.monitor.view();
        assert_eq!(ledger.actions.len(), kind.preceding_receipts());
        assert_eq!(ledger.attempts.last().unwrap().stage, AuditStage::Rejected);
        assert!(
            !ledger
                .records
                .iter()
                .any(|record| record.stage == AuditStage::Rejected)
        );
        assert_session_cannot_retry_after_drop(
            &mut session,
            &mut harness,
            u64::from(kind != Kind::Capture),
            kind.preceding_actions(),
        )
        .await;
    }
}

#[tokio::test]
async fn dropping_adapter_awaits_charges_exactly_one_possibly_issued_action() {
    for kind in [Kind::Capture, Kind::Move, Kind::Click] {
        let mut session = session(ReflexMode::ClickOnce);
        let mut plan = Plan::new([texture(), texture()]);
        let hook = plan.at_port(kind);
        let mut harness = Harness::new(plan);
        let geometry = geometry();
        drop_at_checkpoint(session.step(&mut harness.coordinator, &geometry, 0), hook).await;
        let ledger = harness.monitor.view();
        assert_eq!(ledger.actions.len(), kind.preceding_receipts() + 1);
        assert_eq!(Kind::action(ledger.actions.last().unwrap()), kind);
        assert_eq!(
            ledger
                .records
                .iter()
                .filter(|record| Kind::label(&record.action) == kind)
                .map(|record| record.stage)
                .collect::<Vec<_>>(),
            vec![AuditStage::Requested, AuditStage::Authorized]
        );
        assert_session_cannot_retry_after_drop(
            &mut session,
            &mut harness,
            1,
            kind.preceding_actions() + u64::from(kind != Kind::Capture),
        )
        .await;
    }
    for kind in [Kind::Capture, Kind::Move, Kind::Click] {
        let mut session = session(ReflexMode::ClickOnce);
        let mut plan = Plan::new([texture(), texture()]);
        plan.port_failure = Some(kind);
        let failed_audit = plan.at_audit(kind, AuditStage::Failed);
        let mut harness = Harness::new(plan);
        let geometry = geometry();
        drop_at_checkpoint(
            session.step(&mut harness.coordinator, &geometry, 0),
            failed_audit,
        )
        .await;
        let ledger = harness.monitor.view();
        assert_eq!(ledger.actions.len(), kind.preceding_receipts() + 1);
        assert_eq!(ledger.attempts.last().unwrap().stage, AuditStage::Failed);
        assert!(
            !ledger
                .records
                .iter()
                .any(|record| record.stage == AuditStage::Failed)
        );
        assert_session_cannot_retry_after_drop(
            &mut session,
            &mut harness,
            1,
            kind.preceding_actions() + u64::from(kind != Kind::Capture),
        )
        .await;
    }
}

#[tokio::test]
async fn dropping_completion_audit_awaits_preserves_possible_execution_without_retry() {
    for kind in [Kind::Capture, Kind::Move, Kind::Click] {
        let mut session = session(ReflexMode::ClickOnce);
        let mut plan = Plan::new([texture(), texture()]);
        let hook = plan.at_audit(kind, AuditStage::Completed);
        let mut harness = Harness::new(plan);
        let geometry = geometry();
        drop_at_checkpoint(session.step(&mut harness.coordinator, &geometry, 0), hook).await;
        let ledger = harness.monitor.view();
        assert_eq!(ledger.actions.len(), kind.preceding_receipts() + 1);
        assert_eq!(Kind::action(ledger.actions.last().unwrap()), kind);
        assert_eq!(ledger.attempts.last().unwrap().stage, AuditStage::Completed);
        assert!(!ledger.records.iter().any(|record| {
            Kind::label(&record.action) == kind && record.stage == AuditStage::Completed
        }));
        assert_session_cannot_retry_after_drop(
            &mut session,
            &mut harness,
            1,
            kind.preceding_actions() + u64::from(kind != Kind::Capture),
        )
        .await;
    }
}

#[tokio::test]
async fn cancelled_click_once_with_three_action_budget_never_issues_a_second_click() {
    let mut session = bounded_session(ReflexMode::ClickOnce, 4, 3);
    let mut plan = Plan::new([texture(), texture()]);
    let hook = plan.at_audit(Kind::Click, AuditStage::Completed);
    let mut harness = Harness::new(plan);
    let geometry = geometry();
    let unpolled = session.step(&mut harness.coordinator, &geometry, 0);
    drop(unpolled);
    assert_eq!(session.state(), ReflexState::Searching);
    assert!(harness.monitor.kinds().is_empty());
    assert!(harness.monitor.view().attempts.is_empty());
    drop_at_checkpoint(session.step(&mut harness.coordinator, &geometry, 0), hook).await;
    assert_session_cannot_retry_after_drop(&mut session, &mut harness, 1, 2).await;
    assert_eq!(
        harness.monitor.kinds(),
        vec![Kind::Capture, Kind::Move, Kind::Click]
    );
    assert_eq!(harness.monitor.view().records.len(), 8);
    assert_eq!(harness.monitor.view().attempts.len(), 9);

    // The legacy raw runner uses the same cancellation accounting and must not
    // become a retry bypass around the owned session wrapper.
    let matcher = TemplateMatcher::new(&texture(), MatchOptions::default()).unwrap();
    let mut controller = ReflexController::new(
        TARGET.to_owned(),
        matcher,
        ReflexOptions {
            mode: ReflexMode::ClickOnce,
            interval_ms: 50,
            max_frames: 4,
            max_actions: 3,
            search_region: None,
        },
    )
    .unwrap();
    let mut raw_plan = Plan::new([texture(), texture()]);
    let raw_hook = raw_plan.at_audit(Kind::Click, AuditStage::Completed);
    let mut raw = Harness::new(raw_plan);
    drop_at_checkpoint(
        ai_ex_automation::run_reflex_step(&mut controller, &mut raw.coordinator, &geometry, 0),
        raw_hook,
    )
    .await;
    assert_eq!(controller.state(), ReflexState::Failed);
    let before = raw.monitor.view();
    let terminal =
        ai_ex_automation::run_reflex_step(&mut controller, &mut raw.coordinator, &geometry, 50)
            .await
            .unwrap();
    assert_cancelled_progress(&terminal, 1, 2);
    let after = raw.monitor.view();
    assert_eq!(after.actions, before.actions);
    assert_eq!(after.records, before.records);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(
        raw.monitor.kinds(),
        vec![Kind::Capture, Kind::Move, Kind::Click]
    );
}
