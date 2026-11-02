use std::collections::BTreeSet;
use std::io;

use ai_ex_domain::ErrorKind;
use ai_ex_platform_windows::desktop::{
    InputEvent, OwnedInput, PixelBounds, RgbaFrame, WindowIdentity, WindowSnapshot,
};
use ai_ex_safety::{ActionRequest, Capability, SafetyGate, SafetyPolicy};

use super::*;

struct FakeSystem {
    snapshot: WindowSnapshot,
    calls: Arc<Mutex<Vec<Vec<InputEvent>>>>,
    revoke_on_input: Option<Arc<SafetyGate>>,
    activity_on_capture: Option<Arc<AtomicU64>>,
}

impl DesktopSystem for FakeSystem {
    fn windows(&mut self) -> io::Result<Vec<WindowSnapshot>> {
        Ok(vec![self.snapshot.clone()])
    }
    fn inspect(&mut self, _: u64) -> io::Result<Option<WindowSnapshot>> {
        Ok(Some(self.snapshot.clone()))
    }
    fn capture(
        &mut self,
        _: &WindowSnapshot,
        timeout: Duration,
        _: &AtomicBool,
    ) -> io::Result<RgbaFrame> {
        assert_eq!(timeout, Duration::from_secs(1));
        if let Some(activity) = &self.activity_on_capture {
            activity.fetch_add(1, Ordering::AcqRel);
        }
        Ok(RgbaFrame {
            width: 2,
            height: 2,
            rgba: vec![255; 16],
        })
    }
    fn send_input(&mut self, events: &[InputEvent]) -> io::Result<usize> {
        self.calls.lock().unwrap().push(events.to_vec());
        if let Some(gate) = &self.revoke_on_input {
            gate.trigger_emergency_stop();
        }
        Ok(events.len())
    }
    fn is_pressed(&mut self, _: OwnedInput) -> io::Result<bool> {
        Ok(false)
    }
    fn cursor(&mut self) -> io::Result<(i32, i32)> {
        Ok((0, 0))
    }
}

struct Fixture {
    port: NativeDesktopPort<FakeSystem>,
    calls: Arc<Mutex<Vec<Vec<InputEvent>>>>,
    gate: Arc<SafetyGate>,
    activity: Arc<AtomicU64>,
}

impl Fixture {
    fn new(revoke_on_input: bool, activity_on_capture: bool) -> Self {
        let gate = Arc::new(SafetyGate::new(SafetyPolicy {
            automation_enabled: true,
            allowed_capabilities: BTreeSet::from([
                Capability::ScreenRead,
                Capability::KeyboardInput,
                Capability::MouseInput,
            ]),
            allowed_targets: vec!["C:\\fixture.exe".into(), "wrong-scope".into()],
        }));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let activity = Arc::new(AtomicU64::new(7));
        let identity = WindowIdentity {
            handle: 1,
            process_id: 2,
            process_started_ticks: 3,
        };
        let bounds = PixelBounds {
            left: -2,
            top: 0,
            width: 2,
            height: 2,
        };
        let system = FakeSystem {
            snapshot: WindowSnapshot {
                identity,
                title: "fixture".into(),
                executable: "C:\\fixture.exe".into(),
                client: bounds,
                desktop: bounds,
                dpi: 144,
                foreground: true,
            },
            calls: calls.clone(),
            revoke_on_input: revoke_on_input.then(|| gate.clone()),
            activity_on_capture: activity_on_capture.then(|| activity.clone()),
        };
        let mut desktop = BoundDesktop::new(system);
        let ticket = desktop.bind(identity).unwrap();
        let port = NativeDesktopPort::new(
            Arc::new(Mutex::new(desktop)),
            ticket,
            Arc::new(AtomicBool::new(false)),
            activity.clone(),
            7,
        );
        Self {
            port,
            calls,
            gate,
            activity,
        }
    }
    fn permit(&self, capability: Capability, scope: &str) -> Permit {
        self.gate
            .authorize(ActionRequest {
                capability,
                target: scope.into(),
                rationale: "test".into(),
            })
            .unwrap()
    }
}

#[tokio::test]
async fn revocation_between_unicode_chunks_stops_without_logging_private_text() {
    let mut fixture = Fixture::new(true, false);
    let permit = fixture.permit(Capability::KeyboardInput, "C:\\fixture.exe");
    let text = "private character text ".repeat(10);
    let action = AutomationAction::Native {
        action: NativeDesktopAction::TypeText { text: text.clone() },
    };
    assert!(!action.audit_label().contains("private"));
    let error = fixture.port.execute(&permit, &action).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Safety);
    assert!(!error.message.contains(&text));
    assert_eq!(fixture.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn capture_discards_frame_after_session_activity_changes() {
    let mut fixture = Fixture::new(false, true);
    let permit = fixture.permit(Capability::ScreenRead, "C:\\fixture.exe");
    let error = fixture
        .port
        .execute(&permit, &AutomationAction::CaptureScreen)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Safety);
    assert_eq!(fixture.activity.load(Ordering::Acquire), 8);
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejects_legacy_actions_and_wrong_scope_and_accepts_exact_scope_capture() {
    let mut fixture = Fixture::new(false, false);
    let mouse = fixture.permit(Capability::MouseInput, "C:\\fixture.exe");
    assert_eq!(
        fixture
            .port
            .execute(&mouse, &AutomationAction::MovePointer { x: 0.5, y: 0.5 })
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Safety
    );
    let wrong = fixture.permit(Capability::ScreenRead, "wrong-scope");
    assert_eq!(
        fixture
            .port
            .execute(&wrong, &AutomationAction::CaptureScreen)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Safety
    );
    let screen = fixture.permit(Capability::ScreenRead, "C:\\fixture.exe");
    let ActionResult::ScreenCaptured(frame) = fixture
        .port
        .execute(&screen, &AutomationAction::CaptureScreen)
        .await
        .unwrap()
    else {
        panic!("frame expected")
    };
    assert_eq!((frame.width, frame.height, frame.rgba.len()), (2, 2, 16));
    assert!(fixture.calls.lock().unwrap().is_empty());
}
