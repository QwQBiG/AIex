use std::collections::{BTreeSet, VecDeque};
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use ai_ex_automation::{
    ActionResult, AuditRecord, AuditSink, AuditStage, AutomationAction, AutomationCoordinator,
    AutomationPort, DesktopBounds, FrameGeometry, MatchOptions, PixelRect, ReflexController,
    ReflexFailureStage, ReflexInactiveReason, ReflexMode, ReflexOptions, ReflexSession,
    ReflexSessionOutcome, ReflexState, ScreenFrame, TemplateMatcher,
    TemplateSearchIncompleteReason, TemplateSearchProgress, TemplateSearchStep, run_reflex_step,
};
use ai_ex_domain::AppError;
use ai_ex_safety::{Capability, Permit, SafetyGate, SafetyPolicy};
use async_trait::async_trait;
use tokio::sync::oneshot;

const TARGET: &str = "synthetic-search";
const QUANTUM: u32 = 16_384;
const WORK: u64 = 250_000_000;

#[derive(Default)]
struct Ledger {
    actions: Vec<AutomationAction>,
    records: Vec<AuditRecord>,
}

struct Port {
    frames: VecDeque<ScreenFrame>,
    ledger: Arc<Mutex<Ledger>>,
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
        if permit.target() != TARGET || permit.capability() != action.required_capability() {
            return Err(AppError::safety("synthetic search permit mismatch"));
        }
        self.ledger.lock().unwrap().actions.push(action.clone());
        match action {
            AutomationAction::CaptureScreen => self
                .frames
                .pop_front()
                .map(ActionResult::ScreenCaptured)
                .ok_or_else(|| AppError::unavailable("synthetic search frame exhausted")),
            AutomationAction::MovePointer { .. } | AutomationAction::Click { .. } => {
                Ok(ActionResult::Completed)
            }
            _ => Err(AppError::configuration(
                "unexpected synthetic search action",
            )),
        }
    }
}

struct Audit {
    ledger: Arc<Mutex<Ledger>>,
    captured: Option<oneshot::Sender<()>>,
    fail_capture_completion: bool,
}

#[async_trait]
impl AuditSink for Audit {
    async fn record(&mut self, record: AuditRecord) -> Result<(), AppError> {
        let captured = record.action == "capture_screen" && record.stage == AuditStage::Completed;
        self.ledger.lock().unwrap().records.push(record);
        if captured {
            if self.fail_capture_completion {
                return Err(AppError::unavailable(
                    "synthetic capture completion audit failed",
                ));
            }
            if let Some(notice) = self.captured.take() {
                let _observer = notice.send(());
            }
        }
        Ok(())
    }
}

struct Harness {
    coordinator: AutomationCoordinator<Port, Audit>,
    ledger: Arc<Mutex<Ledger>>,
    captured: oneshot::Receiver<()>,
}

impl Harness {
    fn new(frames: impl IntoIterator<Item = ScreenFrame>) -> Self {
        Self::with_audit(frames, false)
    }

    fn with_audit(frames: impl IntoIterator<Item = ScreenFrame>, fail: bool) -> Self {
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        let (notice, captured) = oneshot::channel();
        Self {
            coordinator: AutomationCoordinator::new(
                Arc::new(SafetyGate::new(SafetyPolicy {
                    automation_enabled: true,
                    allowed_capabilities: BTreeSet::from([
                        Capability::ScreenRead,
                        Capability::MouseInput,
                    ]),
                    allowed_targets: vec![TARGET.into()],
                })),
                Port {
                    frames: frames.into_iter().collect(),
                    ledger: Arc::clone(&ledger),
                },
                Audit {
                    ledger: Arc::clone(&ledger),
                    captured: Some(notice),
                    fail_capture_completion: fail,
                },
            ),
            ledger,
            captured,
        }
    }
}

fn template() -> ScreenFrame {
    let mut pixels = Vec::new();
    for y in 0..20u32 {
        for x in 0..20u32 {
            let value = ((x * 37 + y * 61 + x * y * 13) % 251 + 1) as u8;
            pixels.extend_from_slice(&[value, value, value, 255]);
        }
    }
    ScreenFrame::new(20, 20, pixels).unwrap()
}

fn place(frame: &mut ScreenFrame, left: u32, top: u32, approximate: bool) {
    let tile = template();
    for y in 0..20usize {
        for x in 0..20usize {
            let source = (y * 20 + x) * 4;
            let target = ((top as usize + y) * frame.width as usize + left as usize + x) * 4;
            frame.rgba[target..target + 4].copy_from_slice(&tile.rgba[source..source + 4]);
        }
    }
    if approximate {
        let target = ((top * frame.width + left) * 4) as usize;
        for channel in 0..3 {
            frame.rgba[target + channel] += 1;
        }
    }
}

fn frame(width: u32, height: u32, constant: bool) -> ScreenFrame {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let value = if constant {
                17
            } else {
                ((x * 19 + y * 47 + x * y * 7) % 251) as u8
            };
            pixels.extend_from_slice(&[value, value, value, 255]);
        }
    }
    ScreenFrame::new(width, height, pixels).unwrap()
}

fn matcher(work: u64) -> TemplateMatcher {
    TemplateMatcher::new(
        &template(),
        MatchOptions {
            threshold: 0.7,
            max_work: work,
        },
    )
    .unwrap()
}

fn controller(work: u64) -> ReflexController {
    ReflexController::new(
        TARGET.into(),
        matcher(work),
        ReflexOptions {
            mode: ReflexMode::ClickOnce,
            interval_ms: 50,
            max_frames: 4,
            max_actions: 8,
            search_region: None,
        },
    )
    .unwrap()
}

fn geometry() -> FrameGeometry {
    FrameGeometry::new(
        0,
        0,
        1.0,
        1.0,
        DesktopBounds::new(0, 0, 4096, 4096).unwrap(),
    )
    .unwrap()
}

async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("bounded synthetic search workflow completed")
}

fn send_future<F: Future + Send>(future: F) -> F {
    future
}

async fn poll_pending<F: Future + ?Sized>(mut future: Pin<&mut F>) {
    poll_fn(|context| {
        assert!(future.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
}

fn only_capture(ledger: &Arc<Mutex<Ledger>>) {
    let ledger = ledger.lock().unwrap();
    assert_eq!(ledger.actions, vec![AutomationAction::CaptureScreen]);
    assert_eq!(ledger.records.len(), 3);
    assert_eq!(ledger.records[0].stage, AuditStage::Requested);
    assert_eq!(ledger.records[1].stage, AuditStage::Authorized);
    assert_eq!(ledger.records[2].stage, AuditStage::Completed);
}

fn scanning_checkpoint(
    frame: &ScreenFrame,
    work: u64,
    constant: bool,
) -> (usize, TemplateSearchProgress) {
    let matcher = matcher(work);
    let mut job = matcher.begin_search(frame, None).unwrap();
    for batches in 1..=1024 {
        let next = job.advance(QUANTUM).unwrap();
        let progress = job.progress();
        let scanning = if constant {
            progress.verified > 0
        } else {
            progress.bound_queries > 0
        };
        match next {
            TemplateSearchStep::Pending if scanning => return (batches, progress),
            TemplateSearchStep::Pending => {}
            terminal => panic!(
                "synthetic search must reach a pending scanning checkpoint (constant={constant}), got {terminal:?}"
            ),
        }
    }
    panic!("synthetic scanning checkpoint exceeded its bounded batch count");
}

#[tokio::test(flavor = "current_thread")]
async fn preprocessing_yield_allows_same_thread_pause_and_stop_before_pointer_actions() {
    for stop in [false, true] {
        let mut harness = Harness::new([frame(512, 256, false)]);
        let mut session = ReflexSession::new(controller(WORK));
        let control = session.control();
        let geometry = geometry();
        let ledger = Arc::clone(&harness.ledger);
        let captured = harness.captured;
        let step = send_future(session.step(&mut harness.coordinator, &geometry, 0));
        let command = async move {
            captured.await.expect("capture completion handshake");
            only_capture(&ledger);
            if stop {
                control.stop().unwrap();
            } else {
                control.pause_for_chat().unwrap();
            }
        };
        let (report, ()) = bounded(async { tokio::join!(biased; step, command) }).await;
        let report = report.unwrap();
        assert_eq!(report.outcome, ReflexSessionOutcome::Interrupted);
        assert_eq!(
            report.interruption.unwrap().inactive.reason,
            if stop {
                ReflexInactiveReason::Stopped
            } else {
                ReflexInactiveReason::PausedForChat
            }
        );
        assert_eq!(report.step.frames, 1);
        assert_eq!(report.step.actions, 0);
        assert_eq!(report.step.receipts.len(), 1);
        assert!(report.step.receipts[0].completion_audit_persisted);
        assert_eq!(report.step.search.unwrap().verified, 0);
        assert!(report.step.search_proof.is_none());
        assert!(report.step.matched_bounds.is_none());
        only_capture(&harness.ledger);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn scanning_and_constant_windows_keep_cooperative_cancellation_checkpoints() {
    for constant in [false, true] {
        let input = frame(256, 128, constant);
        // This frame needs only 10,333,200 pairs for exhaustive matching. The
        // ordinary 250m budget selects direct fallback, so use a lower budget
        // to exercise an actual projection scan rather than preprocessing alone.
        let work = if constant { 1 } else { 2_000_000 };
        let candidates = u64::from(input.width - 20 + 1) * u64::from(input.height - 20 + 1);
        assert!(candidates > 4_096);
        assert!(work < candidates * 400);
        let (batches, expected) = scanning_checkpoint(&input, work, constant);
        assert!(expected.verified < expected.candidates);
        let mut harness = Harness::new([input]);
        let mut session = ReflexSession::new(controller(work));
        let control = session.control();
        let geometry = geometry();
        let mut step = Box::pin(send_future(session.step(
            &mut harness.coordinator,
            &geometry,
            0,
        )));
        for _ in 0..batches {
            poll_pending(step.as_mut()).await;
        }
        control.stop().unwrap();
        let report = bounded(step).await.unwrap();
        assert_eq!(report.outcome, ReflexSessionOutcome::Interrupted);
        assert_eq!(report.step.search, Some(expected));
        assert_eq!(report.step.frames, 1);
        assert_eq!(report.step.actions, 0);
        if constant {
            assert_eq!(expected.exact_pixel_pairs, 0);
            assert_eq!(expected.work, 0);
        }
        only_capture(&harness.ledger);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn pause_resume_during_search_never_refreshes_the_original_lease_or_reuses_its_frame() {
    let mut old = frame(256, 128, false);
    place(&mut old, 220, 90, false);
    let mut fresh = frame(256, 128, false);
    place(&mut fresh, 7, 9, false);
    let mut harness = Harness::new([old, fresh]);
    let mut session = ReflexSession::new(controller(WORK));
    let control = session.control();
    let geometry = geometry();
    let mut step = Box::pin(send_future(session.step(
        &mut harness.coordinator,
        &geometry,
        0,
    )));
    poll_pending(step.as_mut()).await;
    control.pause_for_chat().unwrap();
    control.resume().unwrap();
    let interrupted = bounded(step).await.unwrap();
    assert_eq!(interrupted.outcome, ReflexSessionOutcome::Interrupted);
    assert_eq!(
        interrupted.interruption.unwrap().inactive.reason,
        ReflexInactiveReason::StaleGeneration
    );
    assert_eq!(interrupted.step.frames, 1);
    assert_eq!(interrupted.step.actions, 0);
    assert!(interrupted.step.matched_bounds.is_none());
    only_capture(&harness.ledger);

    let completed = bounded(send_future(session.step(
        &mut harness.coordinator,
        &geometry,
        50,
    )))
    .await
    .unwrap();
    assert_eq!(completed.step.state, ReflexState::Completed);
    assert_eq!(completed.step.frames, 2);
    assert_eq!(completed.step.actions, 2);
    assert_eq!(
        completed.step.matched_bounds,
        Some(PixelRect {
            x: 7,
            y: 9,
            width: 20,
            height: 20,
        })
    );
    assert!(completed.step.search_proof.is_some());
    assert!(completed.step.search_incomplete.is_none());
    let ledger = harness.ledger.lock().unwrap();
    assert_eq!(ledger.actions.len(), 4);
    assert_eq!(ledger.records.len(), 12);
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_a_search_yield_fails_the_session_without_charging_capture_twice() {
    let mut harness = Harness::new([frame(512, 256, false)]);
    let mut session = ReflexSession::new(controller(WORK));
    let geometry = geometry();
    let mut step = Box::pin(send_future(session.step(
        &mut harness.coordinator,
        &geometry,
        0,
    )));
    poll_pending(step.as_mut()).await;
    only_capture(&harness.ledger);
    drop(step);
    assert_eq!(session.state(), ReflexState::Failed);
    let retry = bounded(session.step(&mut harness.coordinator, &geometry, 50))
        .await
        .unwrap();
    assert_eq!(retry.step.state, ReflexState::Failed);
    assert_eq!(retry.step.frames, 1);
    assert_eq!(retry.step.actions, 0);
    assert!(retry.step.receipts.is_empty());
    assert!(retry.step.search.is_none());
    let wire = serde_json::to_value(&retry.step).unwrap();
    assert!(wire.get("search").is_none());
    assert!(wire.get("search_incomplete").is_none());
    assert!(wire.get("search_proof").is_none());
    only_capture(&harness.ledger);
}

#[tokio::test(flavor = "current_thread")]
async fn uncontrolled_search_drop_uses_the_same_cancellation_guard() {
    let mut harness = Harness::new([frame(512, 256, false)]);
    let mut controller = controller(WORK);
    let geometry = geometry();
    let mut step = Box::pin(send_future(run_reflex_step(
        &mut controller,
        &mut harness.coordinator,
        &geometry,
        0,
    )));
    poll_pending(step.as_mut()).await;
    drop(step);
    assert_eq!(controller.state(), ReflexState::Failed);
    let retry = bounded(run_reflex_step(
        &mut controller,
        &mut harness.coordinator,
        &geometry,
        50,
    ))
    .await
    .unwrap();
    assert_eq!(retry.frames, 1);
    assert_eq!(retry.actions, 0);
    only_capture(&harness.ledger);
}

#[tokio::test(flavor = "current_thread")]
async fn an_uncertified_provisional_hit_exhausts_work_without_counting_a_miss_or_moving() {
    let mut input = frame(60, 20, false);
    place(&mut input, 0, 0, true);
    place(&mut input, 40, 0, false);
    let mut harness = Harness::new([input]);
    let mut session = ReflexSession::new(controller(400));
    let error = bounded(session.step(&mut harness.coordinator, &geometry(), 0))
        .await
        .unwrap_err();
    assert_eq!(error.stage, ReflexFailureStage::Match);
    assert_eq!(error.report.state, ReflexState::Failed);
    assert_eq!(error.report.frames, 1);
    assert_eq!(error.report.actions, 0);
    assert_eq!(error.report.misses, 0);
    assert_eq!(
        error.report.search_incomplete,
        Some(TemplateSearchIncompleteReason::WorkBudgetExhausted)
    );
    let progress = error.report.search.unwrap();
    assert_eq!(progress.work, 400);
    assert_eq!(progress.max_work, 400);
    assert_eq!(progress.exact_evaluations, 1);
    assert_eq!(progress.exact_pixel_pairs, 400);
    assert!(error.report.score.is_none());
    assert!(error.report.matched_bounds.is_none());
    assert!(error.report.search_proof.is_none());
    only_capture(&harness.ledger);
    let retry = bounded(session.step(&mut harness.coordinator, &geometry(), 50))
        .await
        .unwrap();
    assert_eq!(retry.step.state, ReflexState::Failed);
    assert_eq!(retry.step.frames, 1);
    only_capture(&harness.ledger);
}

#[tokio::test(flavor = "current_thread")]
async fn a_failed_capture_completion_audit_never_enters_the_incremental_search() {
    let mut harness = Harness::with_audit([frame(512, 256, false)], true);
    let mut session = ReflexSession::new(controller(WORK));
    let error = bounded(session.step(&mut harness.coordinator, &geometry(), 0))
        .await
        .unwrap_err();
    assert_eq!(error.stage, ReflexFailureStage::CompletionAudit);
    assert_eq!(error.report.frames, 1);
    assert_eq!(error.report.actions, 0);
    assert_eq!(error.report.receipts.len(), 1);
    assert!(!error.report.receipts[0].completion_audit_persisted);
    assert!(error.report.search.is_none());
    assert!(error.report.search_incomplete.is_none());
    assert!(error.report.search_proof.is_none());
    only_capture(&harness.ledger);
}
