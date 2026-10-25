use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ai_ex_audio::{SpeechQueue, SpeechReceiver};
use ai_ex_automation::{
    DesktopBounds, FrameGeometry, MatchOptions, ReflexController, ReflexMode, ReflexOptions,
    ScreenFrame, TemplateMatcher,
};
use ai_ex_core::SpeechPort;
use ai_ex_domain::TurnId;
use ai_ex_safety::ActionRequest;

#[derive(Default)]
struct TestClock {
    value: AtomicU64,
    failed: AtomicBool,
}

impl HostClock for TestClock {
    fn now_ms(&self) -> Result<u64, AppError> {
        if self.failed.load(Ordering::Acquire) {
            Err(AppError::unavailable("injected monotonic clock failure"))
        } else {
            Ok(self.value.load(Ordering::Acquire))
        }
    }
}

fn fixture() -> (TemplateHost, Arc<TestClock>, SpeechQueue, SpeechReceiver) {
    let (speech, receiver) = SpeechQueue::new(4).unwrap();
    let clock = Arc::new(TestClock::default());
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
    .unwrap();
    (host, clock, speech, receiver)
}

fn reserve(host: &TemplateHost) -> (Uuid, Uuid) {
    let session_id = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let mut state = host.lock().unwrap();
    state.snapshot.session = Some(AutomationSessionSnapshot::loading(session_id, operation_id));
    state.current = Some(CurrentSession {
        manifest: "injected-only".into(),
        manual_resume: false,
        control: None,
        policy: None,
    });
    (session_id, operation_id)
}

fn attach(host: &TemplateHost) -> (Uuid, Uuid, ReflexControl) {
    let (session_id, operation_id) = reserve(host);
    let control = ReflexControl::new();
    let mut state = host.lock().unwrap();
    state.snapshot.session.as_mut().unwrap().phase = AutomationSessionPhase::Active;
    let current = state.current.as_mut().unwrap();
    current.control = Some(control.clone());
    current.policy =
        Some(ChatPausePolicy::new(control.clone(), ChatPauseOptions::default()).unwrap());
    (session_id, operation_id, control)
}

fn session_snapshot(host: &TemplateHost) -> AutomationSessionSnapshot {
    host.snapshot().unwrap().session.unwrap()
}

fn command(
    host: &TemplateHost,
    session_id: Uuid,
    mutation: Mutation,
) -> Result<AutomationSnapshot, AppError> {
    let current = host.snapshot()?;
    host.mutate(current.instance_id, session_id, current.revision, mutation)
}

fn advance(host: &TemplateHost, clock: &TestClock, now: u64) {
    clock.value.store(now, Ordering::Release);
    host.wake.notify_waiters();
}

async fn until(host: &TemplateHost, predicate: impl Fn(&AutomationSessionSnapshot) -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if predicate(&session_snapshot(host)) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("synthetic host transition completes");
}

#[test]
fn pending_admissions_and_quiet_deadline_share_one_authoritative_lock() {
    let (host, clock, _speech, _receiver) = fixture();
    let (session_id, operation_id, control) = attach(&host);
    let old_lease = control.begin_step().unwrap();
    advance(&host, &clock, 100);
    let first = host.begin_chat().unwrap();
    advance(&host, &clock, 200);
    let second = host.begin_chat().unwrap();
    let paused = session_snapshot(&host);
    assert_eq!(paused.pause_reason, Some(AutomationPauseReason::Chat));
    assert_eq!(paused.chat_deadline_ms, Some(30_200));
    advance(&host, &clock, 30_200);
    assert_eq!(
        host.poll_owner(session_id, operation_id)
            .unwrap()
            .unwrap()
            .1,
        ReflexControlMode::PausedForChat
    );
    drop(first);
    assert_eq!(
        session_snapshot(&host).resume_eligibility,
        AutomationResumeEligibility::Busy
    );
    assert!(command(&host, session_id, Mutation::Resume).is_err());
    drop(second);
    assert_eq!(
        host.poll_owner(session_id, operation_id)
            .unwrap()
            .unwrap()
            .1,
        ReflexControlMode::Running
    );
    assert_eq!(session_snapshot(&host).chat_deadline_ms, None);
    assert!(
        old_lease.ensure_current().is_err(),
        "resume cannot revive old work"
    );
}

#[tokio::test]
async fn queued_speech_and_in_worker_synthesis_hold_resume_after_turn_completion() {
    let (host, clock, mut speech, mut receiver) = fixture();
    let (session_id, operation_id, control) = attach(&host);
    let guard = host.begin_chat().unwrap();
    speech
        .enqueue(TurnId::new(), "queued output".into())
        .await
        .unwrap();
    drop(guard);
    advance(&host, &clock, 30_000);
    host.poll_owner(session_id, operation_id).unwrap();
    assert_eq!(
        session_snapshot(&host).resume_eligibility,
        AutomationResumeEligibility::Busy
    );
    let _job = receiver.receive().await.unwrap();
    host.poll_owner(session_id, operation_id).unwrap();
    assert_eq!(
        control.snapshot().unwrap().mode,
        ReflexControlMode::PausedForChat
    );
    let mut next = Box::pin(receiver.receive());
    std::future::poll_fn(|context| {
        assert!(next.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(next);
    host.poll_owner(session_id, operation_id).unwrap();
    assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Running);
}

#[test]
fn manual_pause_revokes_owned_deadline_and_only_explicit_idle_resume_restarts() {
    let (host, clock, _speech, _receiver) = fixture();
    let (session_id, operation_id, control) = attach(&host);
    let guard = host.begin_chat().unwrap();
    let manual = command(&host, session_id, Mutation::Pause).unwrap();
    assert_eq!(manual.session.unwrap().chat_deadline_ms, None);
    assert!(command(&host, session_id, Mutation::Resume).is_err());
    drop(guard);
    advance(&host, &clock, 60_000);
    host.poll_owner(session_id, operation_id).unwrap();
    let held = session_snapshot(&host);
    assert_eq!(held.pause_reason, Some(AutomationPauseReason::Manual));
    assert_eq!(held.chat_deadline_ms, None);
    assert_eq!(
        control.snapshot().unwrap().mode,
        ReflexControlMode::PausedForChat
    );
    command(&host, session_id, Mutation::Resume).unwrap();
    assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Running);
}

#[test]
fn unavailable_speech_never_counts_as_idle() {
    let (host, clock, _speech, receiver) = fixture();
    let (session_id, operation_id, control) = attach(&host);
    let guard = host.begin_chat().unwrap();
    drop(guard);
    drop(receiver);
    advance(&host, &clock, 30_000);
    host.poll_owner(session_id, operation_id).unwrap();
    assert_eq!(
        session_snapshot(&host).resume_eligibility,
        AutomationResumeEligibility::Unknown
    );
    assert!(command(&host, session_id, Mutation::Resume).is_err());
    assert_eq!(
        control.snapshot().unwrap().mode,
        ReflexControlMode::PausedForChat
    );
}

#[test]
fn clock_failure_or_regression_cannot_half_register_a_chat() {
    let (host, clock, _speech, _receiver) = fixture();
    let (_session_id, _operation_id, control) = attach(&host);
    clock.failed.store(true, Ordering::Release);
    let before = host.snapshot().unwrap();
    assert!(host.begin_chat().is_err());
    assert_eq!(host.snapshot().unwrap(), before);
    assert_eq!(host.lock().unwrap().pending_chat, 0);
    assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Running);
    clock.failed.store(false, Ordering::Release);
    advance(&host, &clock, 100);
    let first = host.begin_chat().unwrap();
    let before = host.snapshot().unwrap();
    advance(&host, &clock, 99);
    assert!(host.begin_chat().is_err());
    assert_eq!(host.snapshot().unwrap(), before);
    assert_eq!(host.lock().unwrap().pending_chat, 1);
    drop(first);
}

#[test]
fn exhausted_cursors_revoke_before_due_policy_can_resume() {
    for exhaust_projection in [false, true] {
        let (host, clock, _speech, _receiver) = fixture();
        let (session_id, operation_id, control) = attach(&host);
        let guard = host.begin_chat().unwrap();
        drop(guard);
        advance(&host, &clock, 30_000);
        {
            let mut state = host.lock().unwrap();
            if exhaust_projection {
                state.snapshot.projection_sequence = u64::MAX;
            } else {
                state.snapshot.revision = u64::MAX;
            }
        }
        assert!(host.poll_owner(session_id, operation_id).is_err());
        assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Stopped);
        assert!(!host.snapshot().unwrap().available);
        assert!(
            host.emergency_stop().is_ok()
                || control.snapshot().unwrap().mode == ReflexControlMode::Stopped
        );
    }
}

#[test]
fn same_session_stop_accepts_older_revision_but_stale_or_future_identity_cannot_stop() {
    let (host, _clock, _speech, _receiver) = fixture();
    let (session_id, _operation_id, control) = attach(&host);
    let before = host.snapshot().unwrap();
    command(&host, session_id, Mutation::Pause).unwrap();
    let stopped = host
        .mutate(
            before.instance_id,
            session_id,
            before.revision,
            Mutation::Stop,
        )
        .unwrap();
    assert_eq!(
        stopped.session.unwrap().phase,
        AutomationSessionPhase::Stopping
    );
    assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Stopped);
    let (fresh_id, _operation, fresh) = attach(&host);
    let current = host.snapshot().unwrap();
    assert!(
        host.mutate(current.instance_id, session_id, 0, Mutation::Stop)
            .is_err()
    );
    assert!(
        host.mutate(Uuid::new_v4(), fresh_id, 0, Mutation::Stop)
            .is_err()
    );
    assert!(
        host.mutate(
            current.instance_id,
            fresh_id,
            current.revision + 1,
            Mutation::Stop
        )
        .is_err()
    );
    assert_eq!(fresh.snapshot().unwrap().mode, ReflexControlMode::Running);
}

#[tokio::test]
async fn loading_dedup_stop_and_owner_backlog_remain_bounded_without_decoding() {
    let (host, _clock, _speech, _receiver) = fixture();
    let execution = host.execution.lock().await;
    let initial = host.snapshot().unwrap();
    let operation = Uuid::new_v4();
    let first = host
        .start(
            initial.instance_id,
            initial.revision,
            operation,
            "must-not-read.json".into(),
        )
        .unwrap();
    let session_id = first.session.as_ref().unwrap().session_id;
    let retry = host
        .start(
            initial.instance_id,
            initial.revision,
            operation,
            "must-not-read.json".into(),
        )
        .unwrap();
    assert_eq!(retry, first);
    assert!(
        host.start(
            initial.instance_id,
            first.revision,
            operation,
            "different.json".into()
        )
        .is_err()
    );
    assert_eq!(host.lock().unwrap().tasks.len(), 1);
    command(&host, session_id, Mutation::Pause).unwrap();
    let stopped = host
        .mutate(
            initial.instance_id,
            session_id,
            initial.revision,
            Mutation::Stop,
        )
        .unwrap();
    assert_eq!(
        stopped.session.unwrap().latest_outcome,
        Some(AutomationStepOutcome::Stopped)
    );
    let cursor = host.snapshot().unwrap();
    let second = host
        .start(
            cursor.instance_id,
            cursor.revision,
            Uuid::new_v4(),
            "also-not-read.json".into(),
        )
        .unwrap();
    let second_id = second.session.unwrap().session_id;
    command(&host, second_id, Mutation::Stop).unwrap();
    let cursor = host.snapshot().unwrap();
    assert!(
        host.start(
            cursor.instance_id,
            cursor.revision,
            Uuid::new_v4(),
            "third.json".into()
        )
        .is_err()
    );
    assert_eq!(host.lock().unwrap().tasks.len(), 2);
    drop(execution);
    host.shutdown().await.unwrap();
}

fn texture() -> ScreenFrame {
    let mut pixels = Vec::new();
    for y in 0..20u32 {
        for x in 0..20u32 {
            let value = ((x * 17 + y * 31 + x * y * 3) % 251) as u8;
            pixels.extend_from_slice(&[value, value, value, 255]);
        }
    }
    ScreenFrame::new(20, 20, pixels).unwrap()
}

fn prepared(mode: ReflexMode, count: usize) -> PreparedTemplate {
    let template = texture();
    let frames = (0..count)
        .map(|index| {
            let offset = index * 5;
            let mut rgba = vec![0; 30 * 30 * 4];
            for y in 0..20usize {
                for x in 0..20usize {
                    let target = ((y + offset) * 30 + x + offset) * 4;
                    let source = (y * 20 + x) * 4;
                    rgba[target..target + 4].copy_from_slice(&template.rgba[source..source + 4]);
                }
            }
            ScreenFrame::new(30, 30, rgba).unwrap()
        })
        .collect();
    let geometries = (0..count)
        .map(|index| {
            FrameGeometry::new(
                100 + index as i32 * 100,
                0,
                1.0,
                1.0,
                DesktopBounds {
                    left: 0,
                    top: 0,
                    width: 1000,
                    height: 1000,
                },
            )
            .unwrap()
        })
        .collect();
    let options = ReflexOptions {
        mode,
        interval_ms: 100,
        max_frames: 3,
        max_actions: 10,
        search_region: None,
    };
    let matcher = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: 0.99,
            ..MatchOptions::default()
        },
    )
    .unwrap();
    PreparedTemplate {
        controller: ReflexController::new("primary_target".into(), matcher, options).unwrap(),
        frames,
        geometries,
        timestamps_ms: (0..count).map(|index| index as u64 * 100).collect(),
        profile: "界".repeat(400),
        target: "primary_target".into(),
        mode: match mode {
            ReflexMode::ClickOnce => "click_once",
            ReflexMode::ClickRepeat => "click_repeat",
            ReflexMode::Hover => "hover",
        }
        .into(),
        input_frames: count,
        report_path: Some("must-not-create-report.json".into()),
        action_capacity: 13,
        strategy: None,
    }
}

#[tokio::test]
async fn loading_manual_pause_survives_install_and_source_geometry_follows_consumed_capture() {
    let (host, clock, _speech, _receiver) = fixture();
    let (session_id, operation_id) = reserve(&host);
    command(&host, session_id, Mutation::Pause).unwrap();
    let owner = host.run_prepared(
        session_id,
        operation_id,
        prepared(ReflexMode::ClickRepeat, 2),
    );
    let script = async {
        until(&host, |session| {
            session.phase == AutomationSessionPhase::Active
        })
        .await;
        let paused = session_snapshot(&host);
        assert_eq!(paused.frames, 0);
        assert_eq!(paused.pause_reason, Some(AutomationPauseReason::Manual));
        assert!(paused.profile_name.as_ref().unwrap().len() <= 512);
        host.snapshot().unwrap().validate().unwrap();
        command(&host, session_id, Mutation::Resume).unwrap();
        until(&host, |session| session.frames == 1).await;
        command(&host, session_id, Mutation::Pause).unwrap();
        advance(&host, &clock, 30_000);
        assert_eq!(
            host.poll_owner(session_id, operation_id)
                .unwrap()
                .unwrap()
                .1,
            ReflexControlMode::PausedForChat
        );
        assert_eq!(session_snapshot(&host).frames, 1);
        command(&host, session_id, Mutation::Resume).unwrap();
        until(&host, |session| session.frames == 2).await;
        advance(&host, &clock, 30_100);
        until(&host, |session| {
            session.phase == AutomationSessionPhase::Finished
        })
        .await;
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(4), async {
        tokio::join!(owner, script)
    })
    .await
    .unwrap();
    result.unwrap();
    let complete = session_snapshot(&host);
    assert_eq!(
        complete.latest_outcome,
        Some(AutomationStepOutcome::InputExhausted)
    );
    assert_eq!((complete.frames, complete.actions), (2, 4));
    let state = host.lock().unwrap();
    let archive = state.archives.back().unwrap();
    assert_eq!(archive._reports.len(), 2);
    assert_eq!(archive._reports[0].step.absolute, Some((110.0, 10.0)));
    assert_eq!(archive._reports[1].step.absolute, Some((215.0, 15.0)));
    assert_eq!(archive._audit.lock().unwrap().len(), 18);
}

#[tokio::test]
async fn installation_during_pending_chat_starts_owned_paused_then_finishes_click_once_once() {
    let (host, clock, _speech, _receiver) = fixture();
    let (session_id, operation_id) = reserve(&host);
    let guard = host.begin_chat().unwrap();
    let owner = host.run_prepared(session_id, operation_id, prepared(ReflexMode::ClickOnce, 1));
    let script = async {
        until(&host, |session| {
            session.phase == AutomationSessionPhase::Active
        })
        .await;
        let paused = session_snapshot(&host);
        assert_eq!(paused.control_mode, AutomationControlMode::PausedForChat);
        assert_eq!(paused.pause_reason, Some(AutomationPauseReason::Chat));
        assert_eq!(paused.frames, 0);
        assert!(command(&host, session_id, Mutation::Resume).is_err());
        drop(guard);
        advance(&host, &clock, 30_000);
        until(&host, |session| {
            session.phase == AutomationSessionPhase::Finished
        })
        .await;
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(4), async {
        tokio::join!(owner, script)
    })
    .await
    .unwrap();
    result.unwrap();
    let complete = session_snapshot(&host);
    assert_eq!(
        complete.latest_outcome,
        Some(AutomationStepOutcome::Completed)
    );
    assert_eq!((complete.frames, complete.actions), (1, 2));
    assert!(command(&host, session_id, Mutation::Resume).is_err());
    assert_eq!(
        host.lock()
            .unwrap()
            .archives
            .back()
            .unwrap()
            ._audit
            .lock()
            .unwrap()
            .len(),
        9
    );
}

#[tokio::test]
async fn private_dry_run_scope_works_with_disabled_real_automation_and_checks_emergency_each_dispatch()
 {
    let (host, _clock, _speech, _receiver) = fixture();
    let dry = SafetyGate::new(SafetyPolicy {
        automation_enabled: true,
        allowed_capabilities: BTreeSet::from([Capability::ScreenRead]),
        allowed_targets: vec!["primary_target".into()],
    });
    let permit = dry
        .authorize(ActionRequest {
            capability: Capability::ScreenRead,
            target: "primary_target".into(),
            rationale: "synthetic test".into(),
        })
        .unwrap();
    let mut port = HostPort {
        inner: DryRunAutomationPort::with_screen(3, texture()).unwrap(),
        global_safety: Arc::clone(&host.global_safety),
    };
    port.execute(&permit, &AutomationAction::CaptureScreen)
        .await
        .unwrap();
    host.global_safety.trigger_emergency_stop();
    assert!(
        port.execute(&permit, &AutomationAction::CaptureScreen)
            .await
            .is_err()
    );
    assert_eq!(port.inner.len(), 1);
    let cursor = host.snapshot().unwrap();
    assert!(
        host.start(
            cursor.instance_id,
            cursor.revision,
            Uuid::new_v4(),
            "not-opened.json".into()
        )
        .is_err()
    );
}

#[tokio::test]
async fn completed_chat_during_loading_preserves_its_original_quiet_deadline_unless_manually_resumed()
 {
    for explicit_resume in [false, true] {
        let (host, clock, _speech, _receiver) = fixture();
        let (session_id, operation_id) = reserve(&host);
        advance(&host, &clock, 100);
        drop(host.begin_chat().unwrap());
        assert_eq!(
            host.lock().unwrap().pending_chat,
            0,
            "the loading chat has already completed"
        );
        advance(&host, &clock, 200);
        if explicit_resume {
            command(&host, session_id, Mutation::Resume).unwrap();
            tokio::time::timeout(
                Duration::from_secs(3),
                host.run_prepared(session_id, operation_id, prepared(ReflexMode::ClickOnce, 1)),
            )
            .await
            .unwrap()
            .unwrap();
        } else {
            let owner =
                host.run_prepared(session_id, operation_id, prepared(ReflexMode::ClickOnce, 1));
            let script = async {
                until(&host, |session| {
                    session.phase == AutomationSessionPhase::Active
                })
                .await;
                let paused = session_snapshot(&host);
                assert_eq!(paused.frames, 0);
                assert_eq!(paused.control_mode, AutomationControlMode::PausedForChat);
                assert_eq!(paused.pause_reason, Some(AutomationPauseReason::Chat));
                assert_eq!(
                    paused.chat_deadline_ms,
                    Some(30_100),
                    "loading must not reset the original chat timestamp"
                );
                advance(&host, &clock, 30_099);
                assert_eq!(
                    host.poll_owner(session_id, operation_id)
                        .unwrap()
                        .unwrap()
                        .1,
                    ReflexControlMode::PausedForChat
                );
                assert_eq!(session_snapshot(&host).frames, 0);
                advance(&host, &clock, 30_100);
                until(&host, |session| {
                    session.phase == AutomationSessionPhase::Finished
                })
                .await;
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(4), async {
                tokio::join!(owner, script)
            })
            .await
            .unwrap();
            result.unwrap();
        }
        let completed = session_snapshot(&host);
        assert_eq!(
            completed.latest_outcome,
            Some(AutomationStepOutcome::Completed)
        );
        assert_eq!((completed.frames, completed.actions), (1, 2));
    }
}

#[tokio::test]
async fn shutdown_still_joins_owned_work_when_cursor_exhaustion_makes_stop_return_an_error() {
    let (host, _clock, _speech, _receiver) = fixture();
    let (_session_id, _operation_id, control) = attach(&host);
    let (entered, observed) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let completed = Arc::clone(&finished);
    let task = tokio::spawn(async move {
        let _entered = entered.send(());
        let _released = held.await;
        completed.store(true, Ordering::Release);
    });
    {
        let mut state = host.lock().unwrap();
        state.tasks.push(task);
        state.snapshot.revision = u64::MAX;
    }
    let mut shutdown = Box::pin(host.shutdown());
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! {
            biased;
            result = &mut shutdown => panic!("shutdown returned before its owned task drained: {result:?}"),
            result = observed => result.unwrap(),
        }
    }).await.unwrap();
    assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Stopped);
    assert!(!finished.load(Ordering::Acquire));
    release.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), shutdown)
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        finished.load(Ordering::Acquire),
        "stop error must not detach an owned task"
    );
    assert!(host.lock().unwrap().tasks.is_empty());
}
