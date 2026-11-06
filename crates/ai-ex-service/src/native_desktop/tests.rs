use super::*;
use ai_ex_domain::{NativeDesktopAction, NativeDesktopPhase, NativePointerButton};
use ai_ex_platform_windows::desktop::{
    InputEvent, OwnedInput, PixelBounds, RgbaFrame, WindowIdentity, WindowSnapshot,
};
use ai_ex_safety::{Capability, SafetyPolicy};
use std::collections::BTreeSet;
use std::io;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[path = "preview_tests.rs"]
mod preview_tests;

pub(super) enum TestAudit {
    Real(JsonlAuditLog),
    FailMouseCompletion(Arc<std::sync::atomic::AtomicUsize>),
}

impl From<JsonlAuditLog> for TestAudit {
    fn from(audit: JsonlAuditLog) -> Self {
        Self::Real(audit)
    }
}

#[async_trait::async_trait]
impl ai_ex_automation::AuditSink for TestAudit {
    async fn record(&mut self, record: ai_ex_automation::AuditRecord) -> Result<(), AppError> {
        match self {
            Self::Real(audit) => ai_ex_automation::AuditSink::record(audit, record).await,
            Self::FailMouseCompletion(failures)
                if record.stage == ai_ex_automation::AuditStage::Completed
                    && record.capability == Capability::MouseInput =>
            {
                failures.fetch_add(1, Ordering::SeqCst);
                Err(AppError::unavailable("synthetic completion audit failure"))
            }
            Self::FailMouseCompletion(_) => Ok(()),
        }
    }
}

#[derive(Clone)]
struct CaptureBarrier {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<(Mutex<bool>, std::sync::Condvar)>,
}
struct FakeState {
    window: Option<WindowSnapshot>,
    listed: Option<Vec<WindowSnapshot>>,
    calls: Vec<Vec<InputEvent>>,
    captures: usize,
    frame_byte: u8,
    partial: bool,
    refuse_release: bool,
    shut_down: bool,
    capture_barrier: Option<CaptureBarrier>,
}
struct FakeSystem(Arc<Mutex<FakeState>>);
impl DesktopSystem for FakeSystem {
    fn windows(&mut self) -> io::Result<Vec<WindowSnapshot>> {
        let state = self.0.lock().unwrap();
        Ok(state
            .listed
            .clone()
            .unwrap_or_else(|| state.window.iter().cloned().collect()))
    }
    fn inspect(&mut self, _: u64) -> io::Result<Option<WindowSnapshot>> {
        Ok(self.0.lock().unwrap().window.clone())
    }
    fn capture(
        &mut self,
        _: &WindowSnapshot,
        _: Duration,
        _: &AtomicBool,
    ) -> io::Result<RgbaFrame> {
        let (barrier, frame_byte) = {
            let mut state = self.0.lock().unwrap();
            state.captures += 1;
            (state.capture_barrier.clone(), state.frame_byte)
        };
        if let Some(barrier) = barrier {
            barrier.entered.notify_one();
            let (lock, changed) = &*barrier.release;
            let (released, _) = changed
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |released| {
                    !*released
                })
                .unwrap();
            if !*released {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "synthetic capture barrier expired",
                ));
            }
        }
        Ok(RgbaFrame {
            width: 2,
            height: 2,
            rgba: vec![frame_byte; 16],
        })
    }
    fn send_input(&mut self, events: &[InputEvent]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(events.to_vec());
        if state.refuse_release
            && events.iter().all(|event| {
                matches!(
                    event,
                    InputEvent::Key { down: false, .. }
                        | InputEvent::Button { down: false, .. }
                        | InputEvent::Unicode { down: false, .. }
                )
            })
        {
            return Ok(0);
        }
        if state.partial {
            state.partial = false;
            return Ok(events.len().saturating_sub(1));
        }
        Ok(events.len())
    }
    fn is_pressed(&mut self, _: OwnedInput) -> io::Result<bool> {
        Ok(false)
    }
    fn cursor(&mut self) -> io::Result<(i32, i32)> {
        Ok((0, 0))
    }
    fn shutdown(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().shut_down = true;
        Ok(())
    }
}
struct Fixture {
    host: NativeHost<FakeSystem>,
    system: Arc<Mutex<FakeState>>,
    directory: PathBuf,
    _speech: ai_ex_audio::SpeechQueue,
    _receiver: ai_ex_audio::SpeechReceiver,
}
impl Fixture {
    async fn resume(&self) -> NativeDesktopSnapshot {
        self.host
            .start_resume_current()
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        self.host.snapshot().unwrap()
    }
    async fn capture(&self) -> NativeDesktopSnapshot {
        let snapshot = self.host.snapshot().unwrap();
        self.host
            .request(NativeDesktopRequest::Capture {
                instance_id: snapshot.instance_id,
                expected_revision: snapshot.revision,
                session_id: snapshot.session.unwrap().session_id,
            })
            .await
            .unwrap()
            .snapshot
    }
    fn execute(
        &self,
        snapshot: &NativeDesktopSnapshot,
        operation_id: Uuid,
    ) -> NativeDesktopRequest {
        let session = snapshot.session.as_ref().unwrap();
        NativeDesktopRequest::Execute {
            instance_id: snapshot.instance_id,
            expected_revision: snapshot.revision,
            session_id: session.session_id,
            operation_id,
            frame_id: session.frame_id.unwrap(),
            action: NativeDesktopAction::Click {
                x: 0,
                y: 0,
                button: NativePointerButton::Left,
            },
        }
    }
    fn new(enabled: bool) -> Self {
        let bounds = PixelBounds {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
        };
        let system = Arc::new(Mutex::new(FakeState {
            window: Some(WindowSnapshot {
                identity: WindowIdentity {
                    handle: 1,
                    process_id: 2,
                    process_started_ticks: 3,
                },
                title: "fixture".into(),
                executable: "C:\\fixture.exe".into(),
                client: bounds,
                desktop: bounds,
                dpi: 144,
                foreground: true,
            }),
            listed: None,
            calls: Vec::new(),
            captures: 0,
            frame_byte: 255,
            partial: false,
            refuse_release: false,
            shut_down: false,
            capture_barrier: None,
        }));
        let gate = Arc::new(SafetyGate::new(SafetyPolicy {
            automation_enabled: enabled,
            allowed_capabilities: BTreeSet::from([
                Capability::ScreenRead,
                Capability::KeyboardInput,
                Capability::MouseInput,
            ]),
            allowed_targets: vec!["C:\\fixture.exe".into()],
        }));
        let directory = std::env::temp_dir().join(format!("native-host-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let (speech, receiver) = ai_ex_audio::SpeechQueue::new(8).unwrap();
        let host = NativeHost::new(
            Uuid::new_v4(),
            FakeSystem(system.clone()),
            gate,
            speech.activity(),
            directory.join("audit.jsonl"),
            None,
        );
        Self {
            host,
            system,
            directory,
            _speech: speech,
            _receiver: receiver,
        }
    }
    async fn start(&self) -> Result<NativeDesktopReply, AppError> {
        let snapshot = self.host.snapshot().unwrap();
        let current = self.system.lock().unwrap().window.clone().unwrap();
        self.host
            .request(NativeDesktopRequest::StartNative {
                instance_id: snapshot.instance_id,
                expected_revision: snapshot.revision,
                operation_id: Uuid::new_v4(),
                window: ai_ex_domain::NativeWindow {
                    handle: current.identity.handle,
                    process_id: current.identity.process_id,
                    process_started_ticks: current.identity.process_started_ticks,
                    title: current.title,
                    scope: current.executable,
                    left: current.client.left,
                    top: current.client.top,
                    width: current.client.width,
                    height: current.client.height,
                    dpi: current.dpi,
                    foreground: current.foreground,
                },
                max_frames: 8,
                max_actions: 8,
            })
            .await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(
            self.directory.parent(),
            Some(std::env::temp_dir().as_path())
        );
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[tokio::test]
async fn start_uses_global_policy_and_prepared_sessions_cannot_capture_or_execute() {
    let denied = Fixture::new(false);
    assert!(denied.start().await.is_err());
    assert!(denied.host.snapshot().unwrap().session.is_none());
    assert!(denied.system.lock().unwrap().calls.is_empty());
    let fixture = Fixture::new(true);
    let prepared = fixture.start().await.unwrap().snapshot;
    let session = prepared.session.unwrap();
    assert_eq!(session.phase, NativeDesktopPhase::Prepared);
    assert!(
        fixture
            .host
            .request(NativeDesktopRequest::Capture {
                instance_id: prepared.instance_id,
                expected_revision: prepared.revision,
                session_id: session.session_id
            })
            .await
            .is_err()
    );
    assert!(
        fixture
            .host
            .request(NativeDesktopRequest::Execute {
                instance_id: prepared.instance_id,
                expected_revision: prepared.revision,
                session_id: session.session_id,
                operation_id: Uuid::new_v4(),
                frame_id: Uuid::new_v4(),
                action: NativeDesktopAction::Noop
            })
            .await
            .is_err()
    );
    assert_eq!(fixture.system.lock().unwrap().captures, 0);
}

#[tokio::test]
async fn resume_requires_foreground_and_a_second_confirmation_after_geometry_changes() {
    let fixture = Fixture::new(true);
    fixture.start().await.unwrap();
    fixture
        .system
        .lock()
        .unwrap()
        .window
        .as_mut()
        .unwrap()
        .foreground = false;
    assert_eq!(
        fixture.resume().await.session.unwrap().phase,
        NativeDesktopPhase::Prepared
    );
    {
        let mut system = fixture.system.lock().unwrap();
        let window = system.window.as_mut().unwrap();
        window.foreground = true;
        window.client.left = -1;
        window.desktop.left = -1;
    }
    let changed = fixture.resume().await.session.unwrap();
    assert_eq!(changed.phase, NativeDesktopPhase::Prepared);
    assert_eq!(changed.window.left, -1);
    assert_eq!(
        fixture.resume().await.session.unwrap().phase,
        NativeDesktopPhase::Running
    );
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn each_frame_is_consumed_once_and_operations_are_idempotent_with_real_durable_audit() {
    let fixture = Fixture::new(true);
    fixture.start().await.unwrap();
    fixture.resume().await;
    let captured = fixture.capture().await;
    let request = fixture.execute(&captured, Uuid::new_v4());
    let completed = fixture
        .host
        .request(request.clone())
        .await
        .unwrap()
        .snapshot;
    assert_eq!(completed.session.as_ref().unwrap().actions, 1);
    assert!(completed.session.as_ref().unwrap().frame_id.is_none());
    let calls = fixture.system.lock().unwrap().calls.clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        fixture
            .host
            .request(request.clone())
            .await
            .unwrap()
            .snapshot,
        completed
    );
    assert!(
        fixture
            .host
            .request(fixture.execute(&captured, Uuid::new_v4()))
            .await
            .is_err()
    );
    let mut conflicting = request;
    if let NativeDesktopRequest::Execute { action, .. } = &mut conflicting {
        *action = NativeDesktopAction::Noop;
    }
    assert!(fixture.host.request(conflicting).await.is_err());
    assert_eq!(fixture.system.lock().unwrap().calls, calls);
    let records: Vec<ai_ex_automation::AuditRecord> =
        std::fs::read_to_string(fixture.directory.join("audit.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    use ai_ex_automation::AuditStage::{Authorized, Completed, Requested};
    assert_eq!(
        records
            .iter()
            .map(|record| record.stage)
            .collect::<Vec<_>>(),
        vec![
            Requested, Authorized, Completed, Requested, Authorized, Completed
        ]
    );
    assert!(
        records
            .iter()
            .all(|record| record.target == "C:\\fixture.exe")
    );
}

#[tokio::test]
async fn malformed_durable_audit_refuses_preparation_before_any_system_action() {
    let fixture = Fixture::new(true);
    std::fs::write(fixture.directory.join("audit.jsonl"), b"broken audit\n").unwrap();
    assert!(fixture.start().await.is_err());
    assert!(fixture.host.snapshot().unwrap().session.is_none());
    assert!(fixture.system.lock().unwrap().calls.is_empty());
    assert_eq!(fixture.system.lock().unwrap().captures, 0);
}

#[tokio::test]
async fn start_operation_ids_identify_one_intent_after_stop() {
    let fixture = Fixture::new(true);
    let prepared = fixture.start().await.unwrap().snapshot;
    let start = fixture.host.state.lock().await.start.clone().unwrap();
    assert_eq!(
        fixture.host.request(start.clone()).await.unwrap().snapshot,
        prepared
    );
    let stopped = fixture
        .host
        .request(NativeDesktopRequest::Stop {
            instance_id: prepared.instance_id,
            expected_revision: prepared.revision,
            session_id: prepared.session.unwrap().session_id,
        })
        .await
        .unwrap()
        .snapshot;
    drop(fixture.host.admission.acquire().await.unwrap());
    assert_eq!(
        fixture.host.request(start.clone()).await.unwrap().snapshot,
        stopped
    );
    let mut changed = start;
    if let NativeDesktopRequest::StartNative {
        expected_revision,
        max_frames,
        ..
    } = &mut changed
    {
        *expected_revision = stopped.revision;
        *max_frames = 9;
    }
    assert!(
        fixture
            .host
            .request(changed)
            .await
            .unwrap_err()
            .message
            .contains("already used")
    );
    assert_eq!(fixture.host.snapshot().unwrap(), stopped);
    let paused = fixture
        .host
        .request(NativeDesktopRequest::Pause {
            instance_id: stopped.instance_id,
            expected_revision: stopped.revision,
            session_id: stopped.session.unwrap().session_id,
        })
        .await
        .unwrap()
        .snapshot;
    assert_eq!(paused.session.unwrap().phase, NativeDesktopPhase::Stopped);
    assert!(
        fixture
            .host
            .start_resume_current()
            .unwrap()
            .await
            .unwrap()
            .is_err()
    );
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn partial_input_is_never_replayed_and_shutdown_releases_only_owned_input() {
    let fixture = Fixture::new(true);
    fixture.start().await.unwrap();
    fixture.resume().await;
    let captured = fixture.capture().await;
    {
        let mut system = fixture.system.lock().unwrap();
        system.partial = true;
        system.refuse_release = true;
    }
    let request = fixture.execute(&captured, Uuid::new_v4());
    assert!(fixture.host.request(request.clone()).await.is_err());
    let paused = fixture.host.snapshot().unwrap();
    assert_eq!(
        paused.session.as_ref().unwrap().phase,
        NativeDesktopPhase::Paused
    );
    assert_eq!(paused.session.as_ref().unwrap().actions, 1);
    let before = fixture.system.lock().unwrap().calls.clone();
    assert_eq!(
        fixture.host.request(request).await.unwrap().snapshot,
        paused
    );
    assert_eq!(fixture.system.lock().unwrap().calls, before);
    assert_eq!(
        before
            .iter()
            .flatten()
            .filter(|event| matches!(event, InputEvent::Button { down: true, .. }))
            .count(),
        1
    );
    fixture.system.lock().unwrap().refuse_release = false;
    fixture.host.shutdown().await.unwrap();
    let system = fixture.system.lock().unwrap();
    assert!(system.shut_down);
    assert_eq!(
        system.calls[before.len()..]
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>(),
        vec![InputEvent::Button {
            button: ai_ex_platform_windows::desktop::PointerButton::Left,
            down: false
        }]
    );
    assert!(!fixture.host.snapshot().unwrap().available);
}

#[tokio::test]
async fn chat_and_stop_revoke_the_generation_and_late_capture_cannot_publish_a_frame() {
    let fixture = Fixture::new(true);
    fixture.start().await.unwrap();
    let running = fixture.resume().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    fixture.system.lock().unwrap().capture_barrier = Some(CaptureBarrier {
        entered: entered.clone(),
        release: release.clone(),
    });
    let host = fixture.host.clone();
    let session_id = running.session.as_ref().unwrap().session_id;
    let capture = tokio::spawn(async move {
        host.request(NativeDesktopRequest::Capture {
            instance_id: running.instance_id,
            expected_revision: running.revision,
            session_id,
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let generation = fixture.host.shared.activity.load(Ordering::Acquire);
    let chat = fixture.host.activity().begin_chat().unwrap();
    assert!(fixture.host.shared.activity.load(Ordering::Acquire) > generation);
    assert_eq!(fixture.host.shared.pending.load(Ordering::Acquire), 1);
    let paused = fixture.host.snapshot().unwrap();
    assert_eq!(
        paused.session.as_ref().unwrap().phase,
        NativeDesktopPhase::Paused
    );
    assert!(paused.session.as_ref().unwrap().frame_id.is_none());
    assert!(
        fixture
            .host
            .start_resume_current()
            .unwrap()
            .await
            .unwrap()
            .is_err()
    );
    drop(chat);
    let stopped = fixture
        .host
        .request(NativeDesktopRequest::Stop {
            instance_id: paused.instance_id,
            expected_revision: paused.revision,
            session_id,
        })
        .await
        .unwrap()
        .snapshot;
    assert_eq!(stopped.session.unwrap().phase, NativeDesktopPhase::Stopped);
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    assert!(capture.await.unwrap().is_err());
    let settled = fixture.host.snapshot().unwrap();
    assert_eq!(
        settled.session.as_ref().unwrap().phase,
        NativeDesktopPhase::Stopped
    );
    assert!(settled.session.as_ref().unwrap().frame_id.is_none());
    assert!(fixture.host.state.lock().await.frame.is_none());
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn pause_accepts_an_older_revision_of_the_same_in_flight_session_but_rejects_future_revisions()
 {
    let fixture = Fixture::new(true);
    fixture.start().await.unwrap();
    let running = fixture.resume().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    fixture.system.lock().unwrap().capture_barrier = Some(CaptureBarrier {
        entered: entered.clone(),
        release: release.clone(),
    });
    let host = fixture.host.clone();
    let session_id = running.session.as_ref().unwrap().session_id;
    let capture = tokio::spawn(async move {
        host.request(NativeDesktopRequest::Capture {
            instance_id: running.instance_id,
            expected_revision: running.revision,
            session_id,
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert!(fixture.host.snapshot().unwrap().revision > running.revision);
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
    assert_eq!(
        paused.session.as_ref().unwrap().phase,
        NativeDesktopPhase::Paused
    );
    assert!(
        fixture
            .host
            .request(NativeDesktopRequest::Pause {
                instance_id: paused.instance_id,
                expected_revision: paused.revision + 1,
                session_id
            })
            .await
            .is_err()
    );
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    assert!(capture.await.unwrap().is_err());
    let settled = fixture.host.snapshot().unwrap();
    assert_eq!(
        settled.session.as_ref().unwrap().phase,
        NativeDesktopPhase::Paused
    );
    assert!(settled.session.unwrap().frame_id.is_none());
    assert!(fixture.host.state.lock().await.frame.is_none());
}

#[tokio::test]
async fn window_enumeration_fits_control_budget_and_never_truncates_permission_scopes() {
    let fixture = Fixture::new(true);
    let mut base = fixture.system.lock().unwrap().window.clone().unwrap();
    base.title = "标题".repeat(256);
    base.executable = "x".repeat(4096);
    let mut too_long = base.clone();
    too_long.executable.push('x');
    let mut listed = vec![too_long];
    listed.extend(vec![base; 256]);
    fixture.system.lock().unwrap().listed = Some(listed);
    let reply = fixture
        .host
        .request(NativeDesktopRequest::Windows)
        .await
        .unwrap();
    assert!(!reply.windows.is_empty());
    assert!(reply.windows.len() < 256);
    assert!(serde_json::to_vec(&reply).unwrap().len() <= 32 * 1024);
    assert!(
        reply
            .windows
            .iter()
            .all(|window| window.title.chars().count() == 256 && window.scope.len() == 4096)
    );
    fixture
        .system
        .lock()
        .unwrap()
        .window
        .as_mut()
        .unwrap()
        .executable = "x".repeat(4097);
    assert!(fixture.start().await.unwrap_err().message.contains("4096"));
    assert!(fixture.host.snapshot().unwrap().session.is_none());
}

#[tokio::test]
async fn manually_armed_steps_work_without_vision_and_each_explicit_step_uses_a_new_frame_and_operation()
 {
    let fixture = Fixture::new(true);
    let prepared = fixture.start().await.unwrap().snapshot;
    let session_id = prepared.session.unwrap().session_id;
    let goal = fixture
        .host
        .request(NativeDesktopRequest::SetGoal {
            instance_id: prepared.instance_id,
            expected_revision: prepared.revision,
            session_id,
            goal: "synthetic goal".into(),
        })
        .await
        .unwrap()
        .snapshot;
    let action = NativeDesktopAction::Click {
        x: 0,
        y: 0,
        button: NativePointerButton::Left,
    };
    let armed = fixture
        .host
        .request(NativeDesktopRequest::SetAction {
            instance_id: goal.instance_id,
            expected_revision: goal.revision,
            session_id,
            action: action.clone(),
        })
        .await
        .unwrap()
        .snapshot;
    assert_eq!(armed.session.as_ref().unwrap().armed_action, Some(action));
    assert!(armed.session.as_ref().unwrap().goal.is_empty());
    assert!(fixture.system.lock().unwrap().calls.is_empty());
    fixture.resume().await;
    let first = fixture.host.vision_current().await.unwrap().snapshot;
    let first_session = first.session.unwrap();
    assert_eq!((first_session.frames, first_session.actions), (1, 1));
    assert!(first_session.frame_id.is_none());
    let second = fixture.host.vision_current().await.unwrap().snapshot;
    let second_session = second.session.as_ref().unwrap();
    assert_eq!((second_session.frames, second_session.actions), (2, 2));
    assert_ne!(first_session.last_operation, second_session.last_operation);
    assert_eq!(fixture.system.lock().unwrap().captures, 2);
    assert_eq!(fixture.system.lock().unwrap().calls.len(), 2);
    let goal = fixture
        .host
        .request(NativeDesktopRequest::SetGoal {
            instance_id: second.instance_id,
            expected_revision: second.revision,
            session_id,
            goal: "vision goal".into(),
        })
        .await
        .unwrap()
        .snapshot;
    assert!(goal.session.unwrap().armed_action.is_none());
    assert!(
        fixture
            .host
            .request(NativeDesktopRequest::SetAction {
                instance_id: goal.instance_id,
                expected_revision: goal.revision,
                session_id,
                action: NativeDesktopAction::KeyChord { keys: Vec::new() }
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn revoked_manual_capture_cannot_advance_into_a_keyboard_or_mouse_step() {
    let fixture = Fixture::new(true);
    let prepared = fixture.start().await.unwrap().snapshot;
    fixture
        .host
        .request(NativeDesktopRequest::SetAction {
            instance_id: prepared.instance_id,
            expected_revision: prepared.revision,
            session_id: prepared.session.unwrap().session_id,
            action: NativeDesktopAction::TypeText {
                text: "synthetic private text".into(),
            },
        })
        .await
        .unwrap();
    fixture.resume().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    fixture.system.lock().unwrap().capture_barrier = Some(CaptureBarrier {
        entered: entered.clone(),
        release: release.clone(),
    });
    let host = fixture.host.clone();
    let step = tokio::spawn(async move { host.vision_current().await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let chat = fixture.host.activity().begin_chat().unwrap();
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    assert!(step.await.unwrap().is_err());
    drop(chat);
    let snapshot = fixture.host.snapshot().unwrap();
    assert_eq!(
        snapshot.session.as_ref().unwrap().phase,
        NativeDesktopPhase::Paused
    );
    assert_eq!(snapshot.session.as_ref().unwrap().actions, 0);
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn shortcut_owners_keep_the_intent_observed_before_their_first_poll() {
    let fixture = Fixture::new(true);
    let prepared = fixture.start().await.unwrap().snapshot;
    let session_id = prepared.session.unwrap().session_id;
    fixture
        .host
        .request(NativeDesktopRequest::SetAction {
            instance_id: prepared.instance_id,
            expected_revision: prepared.revision,
            session_id,
            action: NativeDesktopAction::Click {
                x: 0,
                y: 0,
                button: NativePointerButton::Left,
            },
        })
        .await
        .unwrap();
    let running = fixture.resume().await;
    let step = fixture.host.start_current_step().unwrap();
    let resume = fixture.host.start_resume_current().unwrap();
    // This branch has only immediately-ready locks; the current-thread runtime
    // cannot poll the queued owners before the newer intent is committed.
    let replacement = NativeDesktopAction::TypeText {
        text: "newly selected intent".into(),
    };
    fixture
        .host
        .execute_owned(NativeDesktopRequest::SetAction {
            instance_id: running.instance_id,
            expected_revision: running.revision,
            session_id,
            action: replacement.clone(),
        })
        .await
        .unwrap();
    assert!(step.await.unwrap().is_err());
    assert!(resume.await.unwrap().is_err());
    let current = fixture.host.snapshot().unwrap().session.unwrap();
    assert_eq!(current.phase, NativeDesktopPhase::Running);
    assert_eq!(current.armed_action, Some(replacement));
    assert_eq!(current.actions, 0);
    assert_eq!(fixture.system.lock().unwrap().captures, 0);
    assert!(fixture.system.lock().unwrap().calls.is_empty());
}
