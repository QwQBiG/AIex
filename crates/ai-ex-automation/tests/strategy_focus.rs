use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use ai_ex_automation::{
    AuditRecord, AuditSink, AutomationCoordinator, DesktopBounds, DryRunAutomationPort,
    FrameGeometry, MatchOptions, PixelRect, ReflexControlMode, ReflexControlSnapshot,
    ReflexController, ReflexFailureStage, ReflexMode, ReflexOptions, ReflexOutcome,
    ReflexPauseClaim, ReflexSession, ReflexState, ScreenFrame, TemplateMatcher,
};
use ai_ex_domain::AppError;
use ai_ex_safety::{Capability, SafetyGate, SafetyPolicy};
use async_trait::async_trait;

const FIXED: PixelRect = PixelRect {
    x: 100,
    y: 10,
    width: 400,
    height: 30,
};
const LEFT: PixelRect = PixelRect {
    x: 110,
    y: 10,
    width: 60,
    height: 30,
};
const RIGHT: PixelRect = PixelRect {
    x: 430,
    y: 10,
    width: 70,
    height: 30,
};

fn template() -> ScreenFrame {
    let mut rgba = Vec::new();
    for y in 0..20u32 {
        for x in 0..20u32 {
            let value = ((x * 17 + y * 31 + x * y * 3) % 251) as u8;
            rgba.extend_from_slice(&[value, value, value, 255]);
        }
    }
    ScreenFrame::new(20, 20, rgba).unwrap()
}

fn frame(target: Option<(u32, u32)>) -> ScreenFrame {
    let mut rgba = vec![0; 600 * 50 * 4];
    if let Some((left, top)) = target {
        let template = template();
        for y in 0..20usize {
            for x in 0..20usize {
                let source = (y * 20 + x) * 4;
                let destination = ((top as usize + y) * 600 + left as usize + x) * 4;
                rgba[destination..destination + 4]
                    .copy_from_slice(&template.rgba[source..source + 4]);
            }
        }
    }
    ScreenFrame::new(600, 50, rgba).unwrap()
}

fn make_session(
    mode: ReflexMode,
    fixed: Option<PixelRect>,
    max_frames: u64,
    max_actions: u64,
) -> ReflexSession {
    let matcher = TemplateMatcher::new(
        &template(),
        MatchOptions {
            threshold: 0.99,
            ..MatchOptions::default()
        },
    )
    .unwrap();
    let options = ReflexOptions {
        mode,
        interval_ms: 50,
        max_frames,
        max_actions,
        search_region: fixed,
    };
    ReflexSession::new(ReflexController::new("synthetic-target".into(), matcher, options).unwrap())
}

fn geometry() -> FrameGeometry {
    FrameGeometry::new(
        0,
        0,
        1.0,
        1.0,
        DesktopBounds {
            left: 0,
            top: 0,
            width: 2000,
            height: 1000,
        },
    )
    .unwrap()
}

struct TestAudit(Arc<Mutex<Vec<AuditRecord>>>);

#[async_trait]
impl AuditSink for TestAudit {
    async fn record(&mut self, record: AuditRecord) -> Result<(), AppError> {
        self.0.lock().unwrap().push(record);
        Ok(())
    }
}

type Coordinator = AutomationCoordinator<DryRunAutomationPort, TestAudit>;

fn coordinator(frames: Vec<ScreenFrame>) -> (Coordinator, Arc<Mutex<Vec<AuditRecord>>>) {
    let safety = Arc::new(SafetyGate::new(SafetyPolicy {
        automation_enabled: true,
        allowed_capabilities: BTreeSet::from([Capability::ScreenRead, Capability::MouseInput]),
        allowed_targets: vec!["synthetic-target".into()],
    }));
    let records = Arc::new(Mutex::new(Vec::new()));
    (
        AutomationCoordinator::new(
            safety,
            DryRunAutomationPort::with_frames(100, frames).unwrap(),
            TestAudit(Arc::clone(&records)),
        ),
        records,
    )
}

fn ticket(session: &ReflexSession) -> ReflexControlSnapshot {
    session.control().snapshot().unwrap()
}

#[test]
fn focus_consumes_exact_ticket_and_none_restores_an_originally_unrestricted_frame() {
    let mut session = make_session(ReflexMode::Hover, None, 20, 40);
    let control = session.control();
    let lease = control.begin_step().unwrap();
    let first = ticket(&session);
    let focused = session
        .refocus_if_current(first, Some(LEFT))
        .unwrap()
        .unwrap();
    assert_eq!(focused.generation, first.generation + 1);
    assert_eq!(focused.session_id, first.session_id);
    assert_eq!(focused.mode, ReflexControlMode::Running);
    assert_eq!(session.options().search_region, Some(LEFT));
    assert!(lease.ensure_current().is_err());
    assert_eq!(
        session.refocus_if_current(first, Some(RIGHT)).unwrap(),
        None
    );
    assert_eq!(session.options().search_region, Some(LEFT));
    let restored = session.refocus_if_current(focused, None).unwrap().unwrap();
    assert_eq!(restored.generation, focused.generation + 1);
    assert_eq!(session.options().search_region, None);
    assert_eq!(session.refocus_if_current(focused, None).unwrap(), None);
}

#[test]
fn focus_is_clipped_to_immutable_manifest_scope_and_can_move_between_disjoint_hints() {
    let mut session = make_session(ReflexMode::Hover, Some(FIXED), 20, 40);
    let first = ticket(&session);
    let focused = session
        .refocus_if_current(first, Some(LEFT))
        .unwrap()
        .unwrap();
    let moved = session
        .refocus_if_current(focused, Some(RIGHT))
        .unwrap()
        .unwrap();
    assert_eq!(
        session.options().search_region,
        Some(RIGHT),
        "a second hint is bounded by the original scope, not the previous hint"
    );
    let clipped = session
        .refocus_if_current(
            moved,
            Some(PixelRect {
                x: 450,
                y: 0,
                width: 150,
                height: 50,
            }),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        session.options().search_region,
        Some(PixelRect {
            x: 450,
            y: 10,
            width: 50,
            height: 30
        })
    );
    let restored = session.refocus_if_current(clipped, None).unwrap().unwrap();
    assert_eq!(session.options().search_region, Some(FIXED));
    session
        .refocus_if_current(
            restored,
            Some(PixelRect {
                x: 0,
                y: 0,
                width: 600,
                height: 50,
            }),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        session.options().search_region,
        Some(FIXED),
        "even an oversized hint cannot expand the manifest"
    );
}

#[test]
fn invalid_or_disjoint_regions_fail_before_control_or_controller_mutation() {
    let mut session = make_session(ReflexMode::Hover, Some(FIXED), 20, 40);
    let before = ticket(&session);
    let options = *session.options();
    for invalid in [
        PixelRect {
            x: 110,
            y: 10,
            width: 0,
            height: 20,
        },
        PixelRect {
            x: 110,
            y: 10,
            width: 20,
            height: 0,
        },
        PixelRect {
            x: u32::MAX,
            y: 10,
            width: 1,
            height: 20,
        },
        PixelRect {
            x: 110,
            y: u32::MAX,
            width: 20,
            height: 1,
        },
        PixelRect {
            x: 500,
            y: 10,
            width: 20,
            height: 20,
        },
        PixelRect {
            x: 100,
            y: 40,
            width: 20,
            height: 20,
        },
    ] {
        assert!(session.refocus_if_current(before, Some(invalid)).is_err());
        assert_eq!(ticket(&session), before);
        assert_eq!(*session.options(), options);
        assert_eq!(session.state(), ReflexState::Searching);
    }
    session.control().pause_for_chat().unwrap();
    let paused = ticket(&session);
    assert!(
        session
            .refocus_if_current(
                before,
                Some(PixelRect {
                    x: 500,
                    y: 10,
                    width: 20,
                    height: 20
                })
            )
            .is_err()
    );
    assert_eq!(
        ticket(&session),
        paused,
        "invalid ROI is rejected even before stale ticket handling"
    );
}

#[test]
fn cross_session_forged_mode_pause_resume_and_stop_tickets_never_change_focus() {
    let mut session = make_session(ReflexMode::Hover, Some(FIXED), 20, 40);
    let other = make_session(ReflexMode::Hover, Some(FIXED), 20, 40);
    let first = ticket(&session);
    assert_eq!(
        session
            .refocus_if_current(ticket(&other), Some(LEFT))
            .unwrap(),
        None
    );
    let forged = ReflexControlSnapshot {
        mode: ReflexControlMode::PausedForChat,
        ..first
    };
    assert_eq!(
        session.refocus_if_current(forged, Some(LEFT)).unwrap(),
        None
    );
    let control = session.control();
    let claim = match control.try_pause_owned().unwrap() {
        ReflexPauseClaim::Claimed(claim) => claim,
        ReflexPauseClaim::Unowned(_) => panic!("running control must issue its own pause"),
    };
    let paused = ticket(&session);
    assert_eq!(session.refocus_if_current(first, Some(LEFT)).unwrap(), None);
    assert_eq!(
        session.refocus_if_current(paused, Some(LEFT)).unwrap(),
        None
    );
    assert!(
        claim.is_current().unwrap(),
        "failed refocus cannot steal chat pause ownership"
    );
    claim.resume_if_current().unwrap();
    let resumed = ticket(&session);
    assert_eq!(session.refocus_if_current(first, Some(LEFT)).unwrap(), None);
    session
        .refocus_if_current(resumed, Some(LEFT))
        .unwrap()
        .unwrap();
    let focused = ticket(&session);
    control.stop().unwrap();
    let stopped = ticket(&session);
    assert_eq!(session.refocus_if_current(focused, None).unwrap(), None);
    assert_eq!(session.refocus_if_current(stopped, None).unwrap(), None);
    assert_eq!(ticket(&session), stopped);
    assert_eq!(session.options().search_region, Some(LEFT));
}

#[tokio::test]
async fn refocus_discards_tracking_but_preserves_frame_action_budgets_and_capture_cadence() {
    let mut session = make_session(ReflexMode::Hover, Some(FIXED), 20, 40);
    let original = *session.options();
    let (mut coordinator, records) = coordinator(vec![
        frame(Some((120, 15))),
        frame(Some((450, 15))),
        frame(Some((120, 15))),
    ]);
    let first = session
        .step(&mut coordinator, &geometry(), 100)
        .await
        .unwrap()
        .step;
    assert_eq!(first.state, ReflexState::Tracking);
    assert_eq!((first.frames, first.actions), (1, 1));
    assert_eq!(first.absolute, Some((130.0, 25.0)));
    let audit_before = records.lock().unwrap().len();
    session
        .refocus_if_current(ticket(&session), Some(RIGHT))
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), ReflexState::Searching);
    assert_eq!(
        records.lock().unwrap().len(),
        audit_before,
        "a focus change cannot dispatch an action"
    );
    let waiting = session
        .step(&mut coordinator, &geometry(), 149)
        .await
        .unwrap()
        .step;
    assert_eq!(waiting.outcome, ReflexOutcome::Waiting);
    assert_eq!((waiting.frames, waiting.actions), (1, 1));
    assert!(waiting.receipts.is_empty());
    let second = session
        .step(&mut coordinator, &geometry(), 150)
        .await
        .unwrap()
        .step;
    assert_eq!(second.outcome, ReflexOutcome::Matched);
    assert_eq!(second.search_region, Some(RIGHT));
    assert_eq!(second.absolute, Some((460.0, 25.0)));
    assert_eq!((second.frames, second.actions), (2, 2));
    session
        .refocus_if_current(ticket(&session), None)
        .unwrap()
        .unwrap();
    let third = session
        .step(&mut coordinator, &geometry(), 200)
        .await
        .unwrap()
        .step;
    assert_eq!(third.outcome, ReflexOutcome::Matched);
    assert_eq!(third.search_region, Some(FIXED));
    assert_eq!(third.absolute, Some((130.0, 25.0)));
    assert_eq!((third.frames, third.actions), (3, 3));
    assert_eq!(*session.options(), original);
}

#[tokio::test]
async fn focus_and_restore_keep_misses_and_monotonic_timestamp_history() {
    let mut session = make_session(ReflexMode::Hover, Some(FIXED), 20, 40);
    let (mut coordinator, _records) = coordinator(vec![frame(None), frame(None)]);
    let first = session
        .step(&mut coordinator, &geometry(), 100)
        .await
        .unwrap()
        .step;
    assert_eq!((first.frames, first.actions, first.misses), (1, 0, 1));
    session
        .refocus_if_current(ticket(&session), Some(LEFT))
        .unwrap()
        .unwrap();
    let waiting = session
        .step(&mut coordinator, &geometry(), 149)
        .await
        .unwrap()
        .step;
    assert_eq!((waiting.frames, waiting.actions, waiting.misses), (1, 0, 1));
    assert_eq!(waiting.outcome, ReflexOutcome::Waiting);
    let second = session
        .step(&mut coordinator, &geometry(), 150)
        .await
        .unwrap()
        .step;
    assert_eq!((second.frames, second.actions, second.misses), (2, 0, 2));
    session
        .refocus_if_current(ticket(&session), None)
        .unwrap()
        .unwrap();
    let failure = session
        .step(&mut coordinator, &geometry(), 149)
        .await
        .unwrap_err();
    assert_eq!(failure.stage, ReflexFailureStage::Timestamp);
    assert_eq!(
        (
            failure.report.frames,
            failure.report.actions,
            failure.report.misses
        ),
        (2, 0, 2)
    );
}

#[tokio::test]
async fn refocus_keeps_lost_state_and_existing_miss_budget() {
    let mut session = make_session(ReflexMode::Hover, Some(FIXED), 20, 40);
    let (mut coordinator, _records) = coordinator((0..11).map(|_| frame(None)).collect());
    for index in 0..10u64 {
        let report = session
            .step(&mut coordinator, &geometry(), index * 50)
            .await
            .unwrap()
            .step;
        assert_eq!(report.misses, index + 1);
    }
    assert_eq!(session.state(), ReflexState::Lost);
    session
        .refocus_if_current(ticket(&session), Some(LEFT))
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), ReflexState::Lost);
    let report = session
        .step(&mut coordinator, &geometry(), 500)
        .await
        .unwrap()
        .step;
    assert_eq!((report.frames, report.actions, report.misses), (11, 0, 11));
    assert_eq!(report.state, ReflexState::Lost);
}

#[tokio::test]
async fn completed_budget_exhausted_and_failed_controllers_cannot_be_refocused() {
    for terminal in [
        ReflexState::Completed,
        ReflexState::BudgetExhausted,
        ReflexState::Failed,
    ] {
        let mode = if terminal == ReflexState::Completed {
            ReflexMode::ClickOnce
        } else {
            ReflexMode::Hover
        };
        let mut session = make_session(mode, Some(FIXED), 1, 4);
        let frames = match terminal {
            ReflexState::Completed => vec![frame(Some((120, 15)))],
            ReflexState::BudgetExhausted => vec![frame(None)],
            ReflexState::Failed => Vec::new(),
            _ => unreachable!(),
        };
        let (mut coordinator, records) = coordinator(frames);
        let first = session.step(&mut coordinator, &geometry(), 0).await;
        if terminal == ReflexState::Failed {
            assert!(first.is_err());
        } else {
            first.unwrap();
        }
        if terminal == ReflexState::BudgetExhausted {
            session
                .step(&mut coordinator, &geometry(), 50)
                .await
                .unwrap();
        }
        assert_eq!(session.state(), terminal);
        let before = ticket(&session);
        let options = *session.options();
        let audit_count = records.lock().unwrap().len();
        assert_eq!(
            session.refocus_if_current(before, Some(LEFT)).unwrap(),
            None
        );
        assert_eq!(session.refocus_if_current(before, None).unwrap(), None);
        assert_eq!(ticket(&session), before);
        assert_eq!(*session.options(), options);
        assert_eq!(session.state(), terminal);
        assert_eq!(records.lock().unwrap().len(), audit_count);
    }
}
