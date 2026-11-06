//! Service-owned native sessions; synthetic replay retains its separate host.
mod activity;
mod commands;
mod dispatch;
mod preview;
mod system;
mod vision;

use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize},
};
use std::time::Instant;

use ai_ex_audio::SpeechActivity;
use ai_ex_audit::JsonlAuditLog;
use ai_ex_automation::{AutomationCoordinator, ScreenFrame};
use ai_ex_domain::{AppError, NativeDesktopReply, NativeDesktopRequest, NativeDesktopSnapshot};
use ai_ex_platform_windows::desktop::{BoundDesktop, DesktopSystem, WindowTicket};
use ai_ex_safety::SafetyGate;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use uuid::Uuid;

use crate::native_desktop_port::NativeDesktopPort;
use crate::strategy_loop::StrategyProviderFactory;
pub(crate) use activity::{NativeActivity, NativeChatGuard};
pub(crate) type NativeDesktopHost = NativeHost<system::SystemDesktop>;
pub(crate) use system::create;

pub(crate) struct NativeHost<S: DesktopSystem + Send + 'static> {
    shared: NativeActivity,
    state: Arc<AsyncMutex<State<S>>>,
    admission: Arc<Semaphore>,
    safety: Arc<SafetyGate>,
    speech: SpeechActivity,
    audit_path: PathBuf,
    vision: Option<StrategyProviderFactory>,
}

struct State<S: DesktopSystem + Send + 'static> {
    desktop: Arc<Mutex<BoundDesktop<S>>>,
    ticket: Option<WindowTicket>,
    coordinator: Option<AutomationCoordinator<NativeDesktopPort<S>, JsonlAuditLog>>,
    frame: Option<(Uuid, Instant, ScreenFrame)>,
    start: Option<NativeDesktopRequest>,
    seen: std::collections::BTreeMap<Uuid, NativeDesktopRequest>,
}

impl<S: DesktopSystem + Send + 'static> Clone for NativeHost<S> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            state: self.state.clone(),
            admission: self.admission.clone(),
            safety: self.safety.clone(),
            speech: self.speech.clone(),
            audit_path: self.audit_path.clone(),
            vision: self.vision.clone(),
        }
    }
}

impl<S: DesktopSystem + Send + 'static> NativeHost<S> {
    pub(crate) fn new(
        instance_id: Uuid,
        system: S,
        safety: Arc<SafetyGate>,
        speech: SpeechActivity,
        audit_path: PathBuf,
        vision: Option<StrategyProviderFactory>,
    ) -> Self {
        Self {
            shared: NativeActivity {
                snapshot: Arc::new(Mutex::new(NativeDesktopSnapshot {
                    instance_id,
                    revision: 0,
                    execution_mode: "native_windows".into(),
                    available: cfg!(windows),
                    session: None,
                })),
                cancelled: Arc::new(AtomicBool::new(true)),
                activity: Arc::new(AtomicU64::new(0)),
                pending: Arc::new(AtomicUsize::new(0)),
                closed: Arc::new(AtomicBool::new(false)),
            },
            state: Arc::new(AsyncMutex::new(State {
                desktop: Arc::new(Mutex::new(BoundDesktop::new(system))),
                ticket: None,
                coordinator: None,
                frame: None,
                start: None,
                seen: Default::default(),
            })),
            admission: Arc::new(Semaphore::new(1)),
            safety,
            speech,
            audit_path,
            vision,
        }
    }

    pub(crate) fn activity(&self) -> NativeActivity {
        self.shared.clone()
    }
    pub(crate) fn snapshot(&self) -> Result<NativeDesktopSnapshot, AppError> {
        self.shared.snapshot()
    }

    pub(crate) fn stop_current(&self, reason: &str, terminal: bool) -> Result<(), AppError> {
        self.shared.pause(reason, terminal)?;
        self.cleanup_detached();
        Ok(())
    }

    /// The independent owner settles admitted work even when its client times out.
    pub(crate) async fn request(
        &self,
        request: NativeDesktopRequest,
    ) -> Result<NativeDesktopReply, AppError> {
        if matches!(request, NativeDesktopRequest::Status) {
            return self.reply(Vec::new());
        }
        if self.shared.revoke_request(&request)? {
            self.cleanup_detached();
            return self.reply(Vec::new());
        }
        let permit = self.admission.clone().try_acquire_owned().map_err(|_| {
            AppError::unavailable("native operation is settling; query status before retrying")
        })?;
        let host = self.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let result = host.execute_owned(request).await;
            // The next explicit step can be admitted once the completed owner replies.
            drop(permit);
            let _delivered = sender.send(result);
        });
        receiver
            .await
            .map_err(|_| AppError::unavailable("native operation owner stopped"))?
    }

    fn reply(
        &self,
        windows: Vec<ai_ex_domain::NativeWindow>,
    ) -> Result<NativeDesktopReply, AppError> {
        Ok(NativeDesktopReply {
            snapshot: self.snapshot()?,
            windows,
            vision_preview: None,
        })
    }

    fn cleanup_detached(&self) {
        let Ok(permit) = self.admission.clone().try_acquire_owned() else {
            return;
        };
        let host = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut state = host.state.lock().await;
            state.frame = None;
            if let Ok(mut desktop) = state.desktop.lock() {
                let _released = desktop.release_owned();
            }
        });
    }

    pub(crate) async fn shutdown(&self) -> Result<(), AppError> {
        self.shared.close()?;
        let _permit = self
            .admission
            .acquire()
            .await
            .map_err(|_| AppError::unavailable("native admission stopped"))?;
        let state = self.state.lock().await;
        state
            .desktop
            .lock()
            .map_err(|_| AppError::unavailable("native adapter lock poisoned"))?
            .shutdown()
            .map_err(io_error)?;
        Ok(())
    }
}

fn io_error(error: std::io::Error) -> AppError {
    AppError::unavailable(format!("native desktop: {error}"))
}

#[cfg(test)]
mod tests;
