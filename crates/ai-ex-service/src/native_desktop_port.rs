//! Service permission adapter for the bounded native platform implementation.
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ai_ex_automation::{ActionResult, AutomationAction, AutomationPort, ScreenFrame};
use ai_ex_domain::{AppError, NativeDesktopAction, NativePointerButton};
use ai_ex_platform_windows::desktop::{
    BoundDesktop, DesktopSystem, DispatchAllowance, InputFailure, InputFailureKind, KeyCode,
    NativeAction, PointerButton, TargetCheck, WindowTicket,
};
use ai_ex_safety::Permit;
use async_trait::async_trait;

pub(crate) struct NativeDesktopPort<S: DesktopSystem> {
    desktop: Arc<Mutex<BoundDesktop<S>>>,
    ticket: WindowTicket,
    cancelled: Arc<AtomicBool>,
    activity: Arc<AtomicU64>,
    expected_activity: u64,
}

impl<S: DesktopSystem> NativeDesktopPort<S> {
    pub(crate) fn new(
        desktop: Arc<Mutex<BoundDesktop<S>>>,
        ticket: WindowTicket,
        cancelled: Arc<AtomicBool>,
        activity: Arc<AtomicU64>,
        expected_activity: u64,
    ) -> Self {
        Self {
            desktop,
            ticket,
            cancelled,
            activity,
            expected_activity,
        }
    }
}

#[async_trait]
impl<S: DesktopSystem + Send + 'static> AutomationPort for NativeDesktopPort<S> {
    async fn execute(
        &mut self,
        permit: &Permit,
        action: &AutomationAction,
    ) -> Result<ActionResult, AppError> {
        permit.ensure_active()?;
        action.validate()?;
        if permit.capability() != action.required_capability() {
            return Err(AppError::safety(
                "native permit capability does not match action",
            ));
        }
        let native = match action {
            AutomationAction::CaptureScreen => None,
            AutomationAction::Native { action } => Some(convert(action)?),
            _ => {
                return Err(AppError::safety(
                    "native port only accepts bounded native actions",
                ));
            }
        };
        let desktop = self.desktop.clone();
        let cancelled = self.cancelled.clone();
        let activity = self.activity.clone();
        let expected = self.expected_activity;
        let ticket = self.ticket;
        // Clones retain the original revocable lease; this issues no new permit.
        let permit = permit.clone();
        tokio::task::spawn_blocking(move || {
            let guard = || {
                permit.ensure_active().map_err(io::Error::other)?;
                if activity.load(Ordering::Acquire) != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "native session activity changed",
                    ));
                }
                Ok(())
            };
            guard().map_err(|error| AppError::safety(error.to_string()))?;
            let mut desktop = desktop
                .lock()
                .map_err(|_| AppError::unavailable("native adapter lock poisoned"))?;
            let snapshot = match desktop.check(&ticket).map_err(system_error)? {
                TargetCheck::Ready(snapshot) => snapshot,
                TargetCheck::NeedsConfirmation { reason, .. } => {
                    return Err(AppError::safety(format!(
                        "native target needs confirmation: {reason:?}"
                    )));
                }
            };
            if permit.target() != snapshot.executable {
                return Err(AppError::safety(
                    "native permit scope does not match bound executable",
                ));
            }
            let allowance = DispatchAllowance::after_service_permission_check();
            if let Some(native) = native {
                desktop
                    .execute(&ticket, allowance, &native, &cancelled, guard)
                    .map_err(input_error)?;
                Ok(ActionResult::Completed)
            } else {
                let frame = desktop
                    .capture(
                        &ticket,
                        allowance,
                        Duration::from_secs(1),
                        &cancelled,
                        guard,
                    )
                    .map_err(input_error)?;
                Ok(ActionResult::ScreenCaptured(ScreenFrame::new(
                    frame.width,
                    frame.height,
                    frame.rgba,
                )?))
            }
        })
        .await
        .map_err(|_| {
            AppError::unavailable("native worker failed; query status before any new action")
        })?
    }
}

fn system_error(error: io::Error) -> AppError {
    AppError::unavailable(format!("native adapter: {error}"))
}

fn input_error(error: InputFailure) -> AppError {
    let detail = format!("native {:?}: {error}; action is not replayed", error.kind);
    match error.kind {
        InputFailureKind::InvalidAction => AppError::configuration(detail),
        InputFailureKind::SystemCall => AppError::unavailable(detail),
        _ => AppError::safety(detail),
    }
}

fn convert(action: &NativeDesktopAction) -> Result<NativeAction, AppError> {
    let button = |button| match button {
        NativePointerButton::Left => PointerButton::Left,
        NativePointerButton::Right => PointerButton::Right,
        NativePointerButton::Middle => PointerButton::Middle,
    };
    Ok(match action {
        NativeDesktopAction::Click {
            x,
            y,
            button: value,
        } => NativeAction::ClickAt {
            x: *x,
            y: *y,
            button: button(*value),
        },
        NativeDesktopAction::KeyChord { keys } => NativeAction::KeyChord {
            keys: keys
                .iter()
                .map(|key| KeyCode::parse(key))
                .collect::<Result<_, _>>()
                .map_err(|_| {
                    AppError::configuration("native key chord contains an unsupported key")
                })?,
        },
        NativeDesktopAction::TypeText { text } => NativeAction::TypeText { text: text.clone() },
        NativeDesktopAction::Drag {
            from_x,
            from_y,
            to_x,
            to_y,
            button: value,
            steps,
            duration_ms,
        } => NativeAction::Drag {
            from: (*from_x, *from_y),
            to: (*to_x, *to_y),
            button: button(*value),
            steps: *steps,
            duration_ms: *duration_ms,
        },
        NativeDesktopAction::Scroll { horizontal, delta } => NativeAction::Scroll {
            horizontal: *horizontal,
            delta: *delta,
        },
        NativeDesktopAction::Wait { milliseconds } => NativeAction::Wait {
            milliseconds: *milliseconds,
        },
        NativeDesktopAction::Noop => NativeAction::Noop,
    })
}

#[cfg(test)]
mod tests;
