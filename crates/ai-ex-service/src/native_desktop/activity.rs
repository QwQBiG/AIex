use ai_ex_domain::{AppError, NativeDesktopPhase, NativeDesktopRequest, NativeDesktopSnapshot};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct NativeActivity {
    pub(super) snapshot: Arc<Mutex<NativeDesktopSnapshot>>,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) activity: Arc<AtomicU64>,
    pub(super) pending: Arc<AtomicUsize>,
    pub(super) closed: Arc<AtomicBool>,
}

pub(crate) struct NativeChatGuard(NativeActivity);
impl Drop for NativeChatGuard {
    fn drop(&mut self) {
        self.0.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

impl NativeActivity {
    pub(crate) fn snapshot(&self) -> Result<NativeDesktopSnapshot, AppError> {
        self.snapshot
            .lock()
            .map(|state| state.clone())
            .map_err(|_| AppError::unavailable("native projection lock poisoned"))
    }
    pub(crate) fn begin_chat(&self) -> Result<NativeChatGuard, AppError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("native host closed"));
        }
        self.pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 128).then_some(n + 1)
            })
            .map_err(|_| AppError::unavailable("native chat admission full"))?;
        if let Err(error) = self.pause("conversation admitted", false) {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return Err(error);
        }
        Ok(NativeChatGuard(self.clone()))
    }
    pub(crate) fn pause(&self, reason: &str, stop: bool) -> Result<(), AppError> {
        let mut state = self
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("native projection lock poisoned"))?;
        self.revoke(&mut state)?;
        if let Some(session) = &mut state.session
            && !matches!(
                session.phase,
                NativeDesktopPhase::Stopped | NativeDesktopPhase::Failed
            )
        {
            session.phase = if stop {
                NativeDesktopPhase::Stopped
            } else {
                NativeDesktopPhase::Paused
            };
            session.frame_id = None;
            session.last_outcome = Some(reason.to_owned());
        }
        Ok(())
    }
    pub(super) fn revoke(&self, state: &mut NativeDesktopSnapshot) -> Result<(), AppError> {
        self.cancelled.store(true, Ordering::Release);
        self.activity
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| AppError::unavailable("native activity exhausted"))?;
        bump(state)
    }
    pub(super) fn revoke_request(&self, request: &NativeDesktopRequest) -> Result<bool, AppError> {
        let (instance, revision, session, stop) = match request {
            NativeDesktopRequest::Pause {
                instance_id,
                expected_revision,
                session_id,
            } => (*instance_id, *expected_revision, *session_id, false),
            NativeDesktopRequest::Stop {
                instance_id,
                expected_revision,
                session_id,
            } => (*instance_id, *expected_revision, *session_id, true),
            _ => return Ok(false),
        };
        let mut state = self
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("native projection lock poisoned"))?;
        // Revocation accepts an older observation of this exact session; execution
        // and resume still require the current revision. Future revisions fail.
        validate_cursor(&state, instance, revision, Some(session), true)?;
        self.revoke(&mut state)?;
        if let Some(current) = &mut state.session
            && (stop
                || !matches!(
                    current.phase,
                    NativeDesktopPhase::Stopped | NativeDesktopPhase::Failed
                ))
        {
            current.phase = if stop {
                NativeDesktopPhase::Stopped
            } else {
                NativeDesktopPhase::Paused
            };
            current.frame_id = None;
            current.last_outcome = Some(if stop { "stopped" } else { "paused" }.into());
        }
        Ok(true)
    }
    pub(super) fn close(&self) -> Result<(), AppError> {
        self.closed.store(true, Ordering::Release);
        self.pause("host closed", true)?;
        self.snapshot
            .lock()
            .map_err(|_| AppError::unavailable("native projection lock poisoned"))?
            .available = false;
        Ok(())
    }
}

pub(super) fn bump(state: &mut NativeDesktopSnapshot) -> Result<(), AppError> {
    state.revision = state
        .revision
        .checked_add(1)
        .ok_or_else(|| AppError::unavailable("native revision exhausted"))?;
    Ok(())
}

pub(super) fn validate_cursor(
    state: &NativeDesktopSnapshot,
    instance: Uuid,
    revision: u64,
    session: Option<Uuid>,
    terminal: bool,
) -> Result<(), AppError> {
    if instance.is_nil()
        || instance != state.instance_id
        || (if terminal {
            revision > state.revision
        } else {
            revision != state.revision
        })
        || session.is_some_and(|id| {
            id.is_nil() || state.session.as_ref().is_none_or(|s| s.session_id != id)
        })
    {
        return Err(AppError::invalid_transition(
            "native cursor changed; query status before deciding",
        ));
    }
    Ok(())
}
