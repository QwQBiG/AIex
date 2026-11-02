use std::collections::{BTreeSet, VecDeque};
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

#[derive(Default)]
struct Calls {
    snapshot: Option<WindowSnapshot>,
    inputs: Vec<Vec<InputEvent>>,
    accepted: VecDeque<usize>,
    user_pressed: BTreeSet<OwnedInput>,
    injected: BTreeSet<OwnedInput>,
    after_send: Option<WindowSnapshot>,
    after_capture: Option<WindowSnapshot>,
    captures: usize,
}

struct Fake(Arc<Mutex<Calls>>);
impl DesktopSystem for Fake {
    fn windows(&mut self) -> io::Result<Vec<WindowSnapshot>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .snapshot
            .clone()
            .into_iter()
            .collect())
    }
    fn inspect(&mut self, _handle: u64) -> io::Result<Option<WindowSnapshot>> {
        Ok(self.0.lock().unwrap().snapshot.clone())
    }
    fn capture(
        &mut self,
        target: &WindowSnapshot,
        _timeout: Duration,
        _cancelled: &AtomicBool,
    ) -> io::Result<RgbaFrame> {
        let mut calls = self.0.lock().unwrap();
        calls.captures += 1;
        if let Some(snapshot) = calls.after_capture.take() {
            calls.snapshot = Some(snapshot);
        }
        Ok(RgbaFrame {
            width: target.client.width,
            height: target.client.height,
            rgba: vec![255; target.client.width as usize * target.client.height as usize * 4],
        })
    }
    fn send_input(&mut self, events: &[InputEvent]) -> io::Result<usize> {
        let mut calls = self.0.lock().unwrap();
        calls.inputs.push(events.to_vec());
        let accepted = calls.accepted.pop_front().unwrap_or(events.len());
        for event in events.iter().take(accepted) {
            let held = match *event {
                InputEvent::Button { button, down } => Some((OwnedInput::Mouse(button), down)),
                InputEvent::Key { key, down } => Some((OwnedInput::Key(key), down)),
                InputEvent::Unicode { unit, down } => Some((OwnedInput::Unicode(unit), down)),
                _ => None,
            };
            if let Some((input, down)) = held {
                if down {
                    calls.injected.insert(input);
                } else {
                    calls.injected.remove(&input);
                }
            }
        }
        if let Some(snapshot) = calls.after_send.take() {
            calls.snapshot = Some(snapshot);
        }
        Ok(accepted)
    }
    fn is_pressed(&mut self, input: OwnedInput) -> io::Result<bool> {
        let calls = self.0.lock().unwrap();
        Ok(calls.user_pressed.contains(&input) || calls.injected.contains(&input))
    }
    fn cursor(&mut self) -> io::Result<(i32, i32)> {
        Ok((-500, 150))
    }
}

fn snapshot() -> WindowSnapshot {
    WindowSnapshot {
        identity: WindowIdentity {
            handle: 7,
            process_id: 12,
            process_started_ticks: 34,
        },
        title: "synthetic canvas".into(),
        executable: "fixture.exe".into(),
        client: PixelBounds {
            left: -600,
            top: 100,
            width: 400,
            height: 400,
        },
        desktop: PixelBounds {
            left: -1920,
            top: 0,
            width: 3840,
            height: 1080,
        },
        dpi: 144,
        foreground: true,
    }
}
fn fixture() -> (BoundDesktop<Fake>, WindowTicket, Arc<Mutex<Calls>>) {
    let calls = Arc::new(Mutex::new(Calls {
        snapshot: Some(snapshot()),
        ..Default::default()
    }));
    let mut desktop = BoundDesktop::new(Fake(calls.clone()));
    let ticket = desktop.bind(snapshot().identity).unwrap();
    (desktop, ticket, calls)
}
fn allowance() -> DispatchAllowance {
    DispatchAllowance::after_service_permission_check()
}
fn click() -> NativeAction {
    NativeAction::ClickAt {
        x: -500,
        y: 150,
        button: PointerButton::Left,
    }
}

#[test]
fn virtual_desktop_normalization_preserves_negative_origin_and_last_pixel() {
    let desktop = snapshot().desktop;
    assert_eq!(desktop.normalize(-1920, 0).unwrap(), (8, 30));
    assert_eq!(desktop.normalize(1919, 1079).unwrap(), (65527, 65505));
    assert!(desktop.normalize(-1921, 0).is_err());
    assert_eq!(
        PixelBounds {
            left: -2,
            top: -3,
            width: 1,
            height: 1
        }
        .normalize(-2, -3)
        .unwrap(),
        (32768, 32768)
    );
}

#[test]
fn normalized_pixel_centers_round_trip_and_never_cross_a_client_edge() {
    let decode = |value: i32, origin: i32, size: u32| {
        (i64::from(value) * i64::from(size) / 65536 + i64::from(origin)) as i32
    };
    for desktop in [
        PixelBounds {
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
        },
        PixelBounds {
            left: -1920,
            top: -1080,
            width: 3840,
            height: 2160,
        },
    ] {
        for offset in 0..desktop.width {
            let x = desktop.left + offset as i32;
            let y = desktop.top + desktop.height as i32 / 3;
            let (normalized_x, normalized_y) = desktop.normalize(x, y).unwrap();
            assert_eq!(decode(normalized_x, desktop.left, desktop.width), x);
            assert_eq!(decode(normalized_y, desktop.top, desktop.height), y);
        }
        let client = PixelBounds {
            left: desktop.left + 10,
            top: desktop.top + 20,
            width: 800,
            height: 600,
        };
        let (x, y) = desktop.normalize(client.left, client.top).unwrap();
        assert!(client.contains(
            decode(x, desktop.left, desktop.width),
            decode(y, desktop.top, desktop.height)
        ));
    }
    let wide = PixelBounds {
        left: 0,
        top: 0,
        width: 131072,
        height: 100,
    };
    assert!(wide.normalize(1, 0).is_err());
    assert_eq!(decode(wide.normalize(2, 0).unwrap().0, 0, wide.width), 2);
}

#[test]
fn click_is_one_serial_batch_and_partial_insertion_only_releases_accepted_down() {
    let (mut desktop, ticket, calls) = fixture();
    calls.lock().unwrap().accepted.push_back(2);
    let error = desktop
        .execute(
            &ticket,
            allowance(),
            &click(),
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.kind, InputFailureKind::PartialInsertion);
    assert_eq!(
        error.receipt,
        InputReceipt {
            attempted_events: 3,
            accepted_events: 2
        }
    );
    assert_eq!(error.cleanup.released, 1);
    assert!(error.cleanup.still_owned.is_empty());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.inputs[0].len(), 3);
    assert_eq!(
        calls.inputs[1],
        vec![InputEvent::Button {
            button: PointerButton::Left,
            down: false
        }]
    );
    assert_eq!(calls.inputs.len(), 2, "never replay the click");
}

#[test]
fn permission_and_cancellation_are_checked_before_system_input() {
    let (mut desktop, ticket, calls) = fixture();
    let error = desktop
        .execute(
            &ticket,
            allowance(),
            &click(),
            &AtomicBool::new(false),
            || {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "revoked permit",
                ))
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, InputFailureKind::PermissionRevoked);
    let error = desktop
        .execute(
            &ticket,
            allowance(),
            &click(),
            &AtomicBool::new(true),
            || Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.kind, InputFailureKind::Cancelled);
    assert!(calls.lock().unwrap().inputs.is_empty());
}

#[test]
fn identity_focus_and_geometry_changes_require_explicit_new_binding() {
    let (mut desktop, old, calls) = fixture();
    for reason in [
        TargetChange::IdentityChanged,
        TargetChange::GeometryChanged,
        TargetChange::NotForeground,
    ] {
        let mut current = snapshot();
        match reason {
            TargetChange::IdentityChanged => current.identity.process_started_ticks += 1,
            TargetChange::GeometryChanged => {
                current.client.left += 20;
                current.dpi = 192;
            }
            TargetChange::NotForeground => current.foreground = false,
            _ => unreachable!(),
        }
        calls.lock().unwrap().snapshot = Some(current);
        assert!(
            matches!(desktop.check(&old).unwrap(), TargetCheck::NeedsConfirmation { reason: actual, .. } if actual == reason)
        );
        assert_eq!(
            desktop.ticket(),
            Some(old),
            "observation cannot silently accept a new geometry"
        );
    }
    calls.lock().unwrap().snapshot = Some(snapshot());
    let newer = desktop.bind(snapshot().identity).unwrap();
    assert!(newer.geometry_revision > old.geometry_revision);
    assert!(matches!(
        desktop.check(&old).unwrap(),
        TargetCheck::NeedsConfirmation {
            reason: TargetChange::TicketRetired,
            ..
        }
    ));
    calls.lock().unwrap().snapshot = None;
    assert!(matches!(
        desktop.check(&newer).unwrap(),
        TargetCheck::NeedsConfirmation {
            reason: TargetChange::WindowGone,
            ..
        }
    ));
}

#[test]
fn user_held_modifier_is_never_released_by_rejected_action() {
    let (mut desktop, ticket, calls) = fixture();
    let shift = OwnedInput::Key(KeyCode::parse("SHIFT").unwrap());
    calls.lock().unwrap().user_pressed.insert(shift);
    let error = desktop
        .execute(
            &ticket,
            allowance(),
            &NativeAction::KeyChord {
                keys: vec![KeyCode::parse("A").unwrap()],
            },
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.kind, InputFailureKind::AlreadyPressed);
    assert!(calls.lock().unwrap().inputs.is_empty());
    assert!(calls.lock().unwrap().user_pressed.contains(&shift));
    let error = desktop
        .execute(
            &ticket,
            allowance(),
            &NativeAction::ClickAt {
                x: ticket.client.left,
                y: ticket.client.top,
                button: PointerButton::Left,
            },
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.kind, InputFailureKind::AlreadyPressed);
    assert!(calls.lock().unwrap().inputs.is_empty());
    assert!(calls.lock().unwrap().user_pressed.contains(&shift));
}

#[test]
fn failed_cleanup_keeps_ownership_until_explicit_release_succeeds() {
    let (mut desktop, ticket, calls) = fixture();
    calls.lock().unwrap().accepted.extend([1, 0]);
    let ctrl = KeyCode::parse("CTRL").unwrap();
    let error = desktop
        .execute(
            &ticket,
            allowance(),
            &NativeAction::KeyChord {
                keys: vec![ctrl, KeyCode::parse("A").unwrap()],
            },
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.cleanup.still_owned, vec![OwnedInput::Key(ctrl)]);
    assert_eq!(desktop.owned_inputs(), vec![OwnedInput::Key(ctrl)]);
    assert!(desktop.release_owned().still_owned.is_empty());
    assert_eq!(calls.lock().unwrap().inputs.len(), 3);
    assert_eq!(
        calls.lock().unwrap().inputs[2],
        vec![InputEvent::Key {
            key: ctrl,
            down: false
        }]
    );
}

#[test]
fn focus_change_during_drag_releases_only_own_button_without_continuing_drag() {
    let (mut desktop, ticket, calls) = fixture();
    calls.lock().unwrap().after_send = Some(WindowSnapshot {
        foreground: false,
        ..snapshot()
    });
    let error = desktop
        .execute(
            &ticket,
            allowance(),
            &NativeAction::Drag {
                from: (-500, 150),
                to: (-400, 200),
                button: PointerButton::Left,
                steps: 8,
                duration_ms: 40,
            },
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.kind, InputFailureKind::TargetChanged);
    assert_eq!(error.receipt.accepted_events, 2);
    assert!(error.cleanup.still_owned.is_empty());
    assert_eq!(calls.lock().unwrap().inputs.len(), 2);
}

#[test]
fn moved_capture_is_discarded_and_cancelled_capture_never_calls_system() {
    let (mut desktop, ticket, calls) = fixture();
    let mut moved = snapshot();
    moved.client.top += 1;
    calls.lock().unwrap().after_capture = Some(moved);
    let error = desktop
        .capture(
            &ticket,
            allowance(),
            Duration::from_millis(500),
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.kind, InputFailureKind::TargetChanged);
    calls.lock().unwrap().snapshot = Some(snapshot());
    let error = desktop
        .capture(
            &ticket,
            allowance(),
            Duration::from_millis(500),
            &AtomicBool::new(true),
            || Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.kind, InputFailureKind::Cancelled);
    assert_eq!(calls.lock().unwrap().captures, 1);
}

#[test]
fn successful_actions_and_chunks_have_bounded_serial_input() {
    let (mut desktop, ticket, calls) = fixture();
    let receipt = desktop
        .execute(
            &ticket,
            allowance(),
            &click(),
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap();
    assert_eq!(receipt.accepted_events, 3);
    let receipt = desktop
        .execute(
            &ticket,
            allowance(),
            &NativeAction::TypeText {
                text: "中😀".repeat(30),
            },
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap();
    assert_eq!(receipt.accepted_events, 180);
    assert!(
        calls
            .lock()
            .unwrap()
            .inputs
            .iter()
            .all(|batch| batch.len() <= 64)
    );
    assert!(desktop.owned_inputs().is_empty());
    assert!(
        desktop
            .execute(
                &ticket,
                allowance(),
                &NativeAction::Wait { milliseconds: 5001 },
                &AtomicBool::new(false),
                || Ok(())
            )
            .is_err()
    );
}
