//! Device-free host/strategy races with explicit provider and monotonic-clock boundaries.
use super::*;

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ai_ex_audio::{SpeechQueue, SpeechReceiver};
use ai_ex_automation::{
    AuditStage, DesktopBounds, FrameGeometry, MatchOptions, PixelRect, ReflexController,
    ReflexMode, ReflexOptions, ScreenFrame, TemplateMatcher,
};
use ai_ex_vision::{VisionAnalyzerPort, VisionObservation, VisionRequest};
use tokio::sync::{mpsc, oneshot};

use crate::strategy_loop::{StrategyOptions, StrategyProviderFactory, StrategyStatus};

const FOCUS: &str = r#"{"type":"focus_region","region":{"x":36,"y":6,"width":20,"height":20}}"#;
const KEEP: &str = r#"{"type":"keep"}"#;
const PROMPT: &str = "Choose one bounded synthetic focus region.";

#[derive(Default)]
struct ManualClock(AtomicU64);

impl HostClock for ManualClock {
    fn now_ms(&self) -> Result<u64, AppError> {
        Ok(self.0.load(Ordering::Acquire))
    }
}

#[derive(Default)]
struct Probe {
    started: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
}

struct ActiveCall(Arc<Probe>);

impl ActiveCall {
    fn new(probe: Arc<Probe>) -> Self {
        probe.started.fetch_add(1, Ordering::AcqRel);
        let active = probe.active.fetch_add(1, Ordering::AcqRel) + 1;
        probe.peak.fetch_max(active, Ordering::AcqRel);
        Self(probe)
    }
}

impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct PendingCall {
    request: VisionRequest,
    reply: oneshot::Sender<Result<VisionObservation, AppError>>,
}

impl PendingCall {
    fn finish(self, text: &str) -> bool {
        self.reply
            .send(Ok(VisionObservation {
                model: "synthetic-controlled-provider".into(),
                text: text.into(),
            }))
            .is_ok()
    }

    fn source_marker(&self) -> u8 {
        assert!(self.request.prompt.contains(PROMPT));
        let image = image::load_from_memory(&self.request.frame.bytes)
            .expect("strategy request contains an encoded synthetic image")
            .to_rgba8();
        assert_eq!((image.width(), image.height()), (64, 32));
        image.get_pixel(0, 0)[0]
    }
}

struct ControlledVision {
    calls: mpsc::Sender<PendingCall>,
    probe: Arc<Probe>,
}

#[async_trait]
impl VisionAnalyzerPort for ControlledVision {
    async fn analyze(&mut self, request: VisionRequest) -> Result<VisionObservation, AppError> {
        let _active = ActiveCall::new(Arc::clone(&self.probe));
        let (reply, response) = oneshot::channel();
        self.calls
            .send(PendingCall { request, reply })
            .await
            .map_err(|_| AppError::unavailable("synthetic provider inbox closed"))?;
        response
            .await
            .map_err(|_| AppError::unavailable("synthetic provider response closed"))?
    }
}

struct Harness {
    host: TemplateHost,
    clock: Arc<ManualClock>,
    calls: mpsc::Receiver<PendingCall>,
    probe: Arc<Probe>,
    _speech: SpeechQueue,
    _receiver: SpeechReceiver,
}

impl Harness {
    fn new() -> Self {
        let (speech, receiver) = SpeechQueue::new(4).unwrap();
        let clock = Arc::new(ManualClock::default());
        let (calls, inbox) = mpsc::channel(8);
        let probe = Arc::new(Probe::default());
        let provider_probe = Arc::clone(&probe);
        let factory: StrategyProviderFactory = Arc::new(move || {
            Ok(Box::new(ControlledVision {
                calls: calls.clone(),
                probe: Arc::clone(&provider_probe),
            }))
        });
        let host = TemplateHost::with_clock(
            Uuid::new_v4(),
            speech.activity(),
            Arc::new(SafetyGate::new(SafetyPolicy {
                automation_enabled: false,
                allowed_capabilities: BTreeSet::new(),
                allowed_targets: Vec::new(),
            })),
            clock.clone(),
        )
        .unwrap()
        .with_strategy_factory(factory);
        Self {
            host,
            clock,
            calls: inbox,
            probe,
            _speech: speech,
            _receiver: receiver,
        }
    }

    fn start(&self, prepared: PreparedTemplate) -> (Uuid, Uuid) {
        let session_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        let mut state = self.host.lock().unwrap();
        assert!(
            state.current.is_none(),
            "each fixture starts one initial owner"
        );
        state.snapshot.session = Some(AutomationSessionSnapshot::loading(session_id, operation_id));
        state.current = Some(CurrentSession {
            manifest: "synthetic-in-memory-only".into(),
            manual_resume: false,
            control: None,
            policy: None,
        });
        let host = self.host.clone();
        state.tasks.push(tokio::spawn(async move {
            // Match the production owner's lifetime admission, without decoding any files.
            let _execution = host.execution.lock().await;
            {
                let state = host.lock().unwrap();
                if !is_current(&state, session_id, operation_id) || state.closed {
                    return;
                }
            }
            if let Err(error) = host.run_prepared(session_id, operation_id, prepared).await {
                host.fail_reservation(session_id, operation_id, &error);
            }
        }));
        (session_id, operation_id)
    }

    fn snapshot(&self) -> AutomationSessionSnapshot {
        self.host.snapshot().unwrap().session.unwrap()
    }

    fn status(&self) -> StrategyStatus {
        self.host
            .lock()
            .unwrap()
            .strategy
            .as_ref()
            .expect("strategy handle installed")
            .status()
    }

    fn tick(&self, now_ms: u64) {
        self.clock.0.store(now_ms, Ordering::Release);
        self.host.wake.notify_waiters();
    }

    async fn frames(&self, now_ms: u64, expected: u64) {
        self.tick(now_ms);
        until(|| self.snapshot().frames >= expected).await;
    }

    async fn call(&mut self) -> PendingCall {
        bounded(self.calls.recv()).await.expect("strategy request")
    }

    fn command(&self, mutation: Mutation) {
        let snapshot = self.host.snapshot().unwrap();
        self.host
            .mutate(
                snapshot.instance_id,
                snapshot.session.unwrap().session_id,
                snapshot.revision,
                mutation,
            )
            .unwrap();
    }

    async fn shutdown(&self) {
        bounded(self.host.shutdown()).await.unwrap();
        assert_eq!(self.probe.active.load(Ordering::Acquire), 0);
        assert!(self.probe.peak.load(Ordering::Acquire) <= 1);
    }
}

async fn bounded<F: std::future::Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("bounded synthetic strategy workflow completed")
}

async fn until(mut predicate: impl FnMut() -> bool) {
    bounded(async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

fn options(max_requests: u16) -> StrategyOptions {
    StrategyOptions {
        prompt: PROMPT.into(),
        interval_ms: 100,
        timeout_ms: 30_000,
        max_age_ms: 60_000,
        max_requests,
    }
}

fn focus_region() -> PixelRect {
    PixelRect {
        x: 36,
        y: 6,
        width: 20,
        height: 20,
    }
}

fn geometry(origin_x: i32) -> FrameGeometry {
    FrameGeometry::new(
        origin_x,
        0,
        1.0,
        1.0,
        DesktopBounds::new(0, 0, 128, 64).unwrap(),
    )
    .unwrap()
}

fn prepared(count: usize, strategy: StrategyOptions) -> PreparedTemplate {
    let mut template = Vec::new();
    for y in 0..20u32 {
        for x in 0..20u32 {
            let value = ((x * 17 + y * 31 + x * y * 3) % 251) as u8;
            template.extend_from_slice(&[value, value, value, 255]);
        }
    }
    let template = ScreenFrame::new(20, 20, template).unwrap();
    let frames = (0..count)
        .map(|index| {
            let marker = u8::try_from(index).unwrap();
            let mut rgba = [marker, 0, 0, 255].repeat(64 * 32);
            for y in 0..20usize {
                for x in 0..20usize {
                    let source = (y * 20 + x) * 4;
                    let target = ((y + 6) * 64 + x + 36) * 4;
                    rgba[target..target + 4].copy_from_slice(&template.rgba[source..source + 4]);
                }
            }
            ScreenFrame::new(64, 32, rgba).unwrap()
        })
        .collect();
    PreparedTemplate {
        controller: ReflexController::new(
            "primary_target".into(),
            TemplateMatcher::new(
                &template,
                MatchOptions {
                    threshold: 0.99,
                    ..MatchOptions::default()
                },
            )
            .unwrap(),
            ReflexOptions {
                mode: ReflexMode::ClickRepeat,
                interval_ms: 100,
                max_frames: count as u64 + 1,
                max_actions: count as u64 * 2 + 2,
                search_region: None,
            },
        )
        .unwrap(),
        frames,
        geometries: (0..count).map(|_| geometry(0)).collect(),
        timestamps_ms: (0..count).map(|index| index as u64 * 100).collect(),
        profile: "synthetic-strategy-profile".into(),
        target: "primary_target".into(),
        mode: "click_repeat".into(),
        input_frames: count,
        report_path: None,
        action_capacity: count * 3 + 1,
        strategy: Some(strategy),
    }
}

fn archived(host: &TemplateHost, session_id: Uuid) -> Vec<ReflexSessionReport> {
    host.lock()
        .unwrap()
        .archives
        .iter()
        .find(|archive| archive._session_id == session_id)
        .expect("drained owner archived its reports")
        ._reports
        .clone()
}

fn assert_complete_audits(host: &TemplateHost, session_id: Uuid, frames: u64) {
    let state = host.lock().unwrap();
    let archive = state
        .archives
        .iter()
        .find(|archive| archive._session_id == session_id)
        .expect("archive exists after drain");
    assert!(
        archive._failure.is_none(),
        "provider completion must not cancel a step"
    );
    let receipts: Vec<_> = archive
        ._reports
        .iter()
        .flat_map(|report| report.step.receipts.iter())
        .collect();
    assert_eq!(receipts.len() as u64, frames * 3);
    let records = archive._audit.lock().unwrap();
    assert_eq!(records.len(), receipts.len() * 3);
    for receipt in receipts {
        assert!(receipt.completion_audit_persisted);
        let stages: Vec<_> = records
            .iter()
            .filter(|record| record.action_id == receipt.action_id)
            .map(|record| record.stage)
            .collect();
        assert_eq!(
            stages,
            vec![
                AuditStage::Requested,
                AuditStage::Authorized,
                AuditStage::Completed
            ]
        );
    }
}

#[tokio::test]
async fn slow_provider_keeps_fast_frames_running_with_one_latest_observation_and_request_budget() {
    let mut harness = Harness::new();
    let (session_id, _) = harness.start(prepared(8, options(2)));
    until(|| harness.snapshot().frames == 1).await;
    let first = harness.call().await;
    assert_eq!(first.source_marker(), 0);

    harness.frames(100, 2).await;
    harness.frames(200, 3).await;
    harness.frames(300, 4).await;
    until(|| harness.status().queued).await;
    assert_eq!(harness.status().requests_started, 1);
    assert_eq!(harness.probe.active.load(Ordering::Acquire), 1);
    assert_eq!(harness.probe.peak.load(Ordering::Acquire), 1);
    assert!(first.finish(KEEP));
    let second = harness.call().await;
    assert_eq!(
        second.source_marker(),
        3,
        "queued observations replace older frames"
    );
    assert!(second.finish(KEEP));
    until(|| harness.status().requests_finished == 2).await;
    harness.frames(400, 5).await;
    harness.frames(500, 6).await;
    assert_eq!(harness.status().requests_started, 2);
    assert_eq!(harness.probe.started.load(Ordering::Acquire), 2);
    harness.shutdown().await;
    assert_complete_audits(&harness.host, session_id, 6);
}

#[tokio::test]
async fn focus_is_committed_at_the_next_owned_frame_boundary_with_complete_audits() {
    let mut harness = Harness::new();
    let (session_id, _) = harness.start(prepared(4, options(1)));
    until(|| harness.snapshot().frames == 1).await;
    let call = harness.call().await;
    assert!(call.finish(FOCUS));
    until(|| harness.status().proposal_pending).await;
    assert_eq!(harness.snapshot().frames, 1);
    assert_eq!(harness.status().proposals_applied, 0);
    harness.frames(100, 2).await;
    until(|| harness.status().proposals_applied == 1).await;
    harness.shutdown().await;
    let reports = archived(&harness.host, session_id);
    assert_eq!(
        reports[0].step.search_region,
        Some(PixelRect {
            x: 0,
            y: 0,
            width: 64,
            height: 32
        })
    );
    assert_eq!(reports[1].step.search_region, Some(focus_region()));
    assert_eq!(reports[1].step.matched_bounds, Some(focus_region()));
    assert_complete_audits(&harness.host, session_id, 2);
}

#[tokio::test]
async fn chat_and_manual_pause_resume_permanently_reject_the_original_strategy_ticket() {
    for via_chat in [false, true] {
        let mut harness = Harness::new();
        let (session_id, _) = harness.start(prepared(4, options(1)));
        until(|| harness.snapshot().frames == 1).await;
        let call = harness.call().await;
        let original = harness.snapshot().control_generation;
        if via_chat {
            let guard = harness.host.begin_chat().unwrap();
            drop(guard);
        } else {
            harness.command(Mutation::Pause);
        }
        harness.command(Mutation::Resume);
        assert_eq!(
            harness.snapshot().control_mode,
            AutomationControlMode::Running
        );
        assert!(harness.snapshot().control_generation > original);
        let _delivered = call.finish(FOCUS);
        harness.frames(100, 2).await;
        until(|| harness.status().requests_finished == 1).await;
        harness.frames(200, 3).await;
        assert_eq!(harness.status().proposals_applied, 0);
        harness.shutdown().await;
        assert!(
            archived(&harness.host, session_id)
                .iter()
                .all(|report| report.step.search_region != Some(focus_region()))
        );
        assert_complete_audits(&harness.host, session_id, 3);
    }
}

#[tokio::test]
async fn stop_replacement_emergency_and_eof_close_late_provider_results_without_dispatch() {
    for terminal in ["stop", "replace", "emergency", "eof"] {
        let mut harness = Harness::new();
        let (session_id, _) = harness.start(prepared(2, options(1)));
        until(|| harness.snapshot().frames == 1).await;
        let call = harness.call().await;
        let handle = harness.host.lock().unwrap().strategy.clone().unwrap();
        let frames = if terminal == "eof" {
            harness.frames(100, 2).await;
            harness.tick(200);
            2
        } else {
            match terminal {
                "stop" => harness.command(Mutation::Stop),
                "emergency" => harness.host.emergency_stop().unwrap(),
                "replace" => {
                    let snapshot = harness.host.snapshot().unwrap();
                    let absent = std::env::temp_dir()
                        .join(format!("ai-ex-absent-strategy-{}.json", Uuid::new_v4()));
                    assert!(!absent.exists());
                    let replacement = harness
                        .host
                        .start(
                            snapshot.instance_id,
                            snapshot.revision,
                            Uuid::new_v4(),
                            absent.to_string_lossy().into_owned(),
                        )
                        .unwrap();
                    assert_ne!(replacement.session.unwrap().session_id, session_id);
                }
                _ => unreachable!(),
            }
            1
        };
        until(|| handle.status().closed && harness.probe.active.load(Ordering::Acquire) == 0).await;
        assert!(
            !call.finish(FOCUS),
            "terminated provider no longer receives replies"
        );
        harness.shutdown().await;
        assert_eq!(handle.status().proposals_applied, 0);
        assert!(
            archived(&harness.host, session_id)
                .iter()
                .all(|report| report.step.search_region != Some(focus_region()))
        );
        assert_complete_audits(&harness.host, session_id, frames);
    }
}

#[tokio::test]
async fn provider_timeout_releases_the_single_slot_and_preserves_fast_execution() {
    let mut harness = Harness::new();
    let mut strategy = options(1);
    strategy.timeout_ms = 25;
    let (session_id, _) = harness.start(prepared(4, strategy));
    until(|| harness.snapshot().frames == 1).await;
    let call = harness.call().await;
    until(|| {
        harness.status().requests_finished == 1 && harness.probe.active.load(Ordering::Acquire) == 0
    })
    .await;
    assert!(!call.finish(FOCUS));
    harness.frames(100, 2).await;
    assert_eq!(harness.status().requests_started, 1);
    assert_eq!(harness.status().proposals_applied, 0);
    harness.shutdown().await;
    assert_complete_audits(&harness.host, session_id, 2);
}

#[tokio::test]
async fn shutdown_cancels_a_pending_provider_without_waiting_for_its_response() {
    let mut harness = Harness::new();
    let (session_id, _) = harness.start(prepared(4, options(4)));
    until(|| harness.snapshot().frames == 1).await;
    let call = harness.call().await;
    assert_eq!(harness.probe.active.load(Ordering::Acquire), 1);
    harness.shutdown().await;
    assert!(!call.finish(FOCUS));
    assert!(harness.status().closed);
    assert_eq!(harness.status().proposals_applied, 0);
    assert_complete_audits(&harness.host, session_id, 1);
}

#[tokio::test]
async fn focus_from_a_different_source_geometry_cannot_reinterpret_coordinates() {
    let mut harness = Harness::new();
    let mut input = prepared(4, options(1));
    input.geometries[1] = geometry(20);
    let (session_id, _) = harness.start(input);
    until(|| harness.snapshot().frames == 1).await;
    let call = harness.call().await;
    assert!(call.finish(FOCUS));
    until(|| harness.status().proposal_pending).await;
    harness.frames(100, 2).await;
    until(|| harness.status().proposals_rejected > 0).await;
    assert_eq!(harness.status().proposals_applied, 0);
    harness.shutdown().await;
    let reports = archived(&harness.host, session_id);
    assert_ne!(reports[1].step.search_region, Some(focus_region()));
    assert_eq!(reports[1].step.absolute, Some((66.0, 16.0)));
    assert_complete_audits(&harness.host, session_id, 2);
}
