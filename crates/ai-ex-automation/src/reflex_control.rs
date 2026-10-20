//! Session control shared independently of a mutably borrowed reflex runner.
//!
//! Operations hold one short mutex guard and never await. A step keeps its original
//! lease through all awaits; resuming or selecting a new target cannot revive it.
//! Revocation prevents later dispatch decisions. It does not cancel an action that
//! an adapter has already issued, nor replace the automation safety gate.

use std::sync::{Arc, Mutex, MutexGuard};

use ai_ex_domain::AppError;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexControlMode {
    Running,
    PausedForChat,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReflexControlSnapshot {
    pub session_id: Uuid,
    pub generation: u64,
    pub mode: ReflexControlMode,
}

/// Typed execution rejection, separate from adapter, audit, and matching failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexInactiveReason {
    PausedForChat,
    Stopped,
    StaleGeneration,
    /// The control mutex is poisoned; execution is refused without guessing state.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReflexInactive {
    pub reason: ReflexInactiveReason,
    /// Current control state, absent when its mutex cannot be trusted.
    pub snapshot: Option<ReflexControlSnapshot>,
}

impl std::fmt::Display for ReflexInactive {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self.reason {
            ReflexInactiveReason::PausedForChat => "reflex session is paused for chat",
            ReflexInactiveReason::Stopped => "reflex session is stopped",
            ReflexInactiveReason::StaleGeneration => {
                "reflex step belongs to an invalidated generation"
            }
            ReflexInactiveReason::Unavailable => {
                "reflex control is unavailable; execution is refused"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ReflexInactive {}

#[derive(Debug)]
struct ControlState {
    snapshot: ReflexControlSnapshot,
}

/// Cloneable commands and snapshots for one reflex session.
///
/// Clones share the same session. `new` creates a separate, initially running
/// session; it is the only way to start again after `stop`.
#[derive(Debug, Clone)]
pub struct ReflexControl {
    inner: Arc<Mutex<ControlState>>,
}

impl Default for ReflexControl {
    fn default() -> Self {
        Self::new()
    }
}

impl ReflexControl {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ControlState {
                snapshot: ReflexControlSnapshot {
                    session_id: Uuid::new_v4(),
                    generation: 0,
                    mode: ReflexControlMode::Running,
                },
            })),
        }
    }

    /// Poison is an operational error; no state or lease is inferred from it.
    pub fn snapshot(&self) -> Result<ReflexControlSnapshot, AppError> {
        Ok(self.lock()?.snapshot)
    }

    /// Running -> paused invalidates every existing lease. Already paused is a no-op.
    /// A stopped session rejects the command rather than changing its terminal state.
    pub fn pause_for_chat(&self) -> Result<ReflexControlSnapshot, AppError> {
        let mut state = self.lock()?;
        match state.snapshot.mode {
            ReflexControlMode::Running => advance(&mut state, ReflexControlMode::PausedForChat),
            ReflexControlMode::PausedForChat => Ok(state.snapshot),
            ReflexControlMode::Stopped => Err(stopped_transition()),
        }
    }

    /// Paused -> running starts a fresh generation; it never revalidates old leases.
    /// Already running is a no-op. Resume after stop is an invalid transition.
    pub fn resume(&self) -> Result<ReflexControlSnapshot, AppError> {
        let mut state = self.lock()?;
        match state.snapshot.mode {
            ReflexControlMode::PausedForChat => advance(&mut state, ReflexControlMode::Running),
            ReflexControlMode::Running => Ok(state.snapshot),
            ReflexControlMode::Stopped => Err(stopped_transition()),
        }
    }

    /// Invalidate in-flight work when a target changes, preserving running/paused mode.
    ///
    /// This only changes the control generation. The owner must publish the new target,
    /// matcher, and geometry coherently before starting another step and discard any
    /// old tracking region. It must not reset consumed action/frame budgets.
    pub fn invalidate_target(&self) -> Result<ReflexControlSnapshot, AppError> {
        let mut state = self.lock()?;
        let mode = state.snapshot.mode;
        if mode == ReflexControlMode::Stopped {
            return Err(stopped_transition());
        }
        advance(&mut state, mode)
    }

    /// Stop is terminal for this session and all clones. Repeated stop is a no-op.
    pub fn stop(&self) -> Result<ReflexControlSnapshot, AppError> {
        let mut state = self.lock()?;
        if state.snapshot.mode == ReflexControlMode::Stopped {
            return Ok(state.snapshot);
        }
        advance(&mut state, ReflexControlMode::Stopped)
    }

    /// Obtain one immutable generation lease, once at the start of a running step.
    /// Keep this lease through capture, matching, movement, click, and audit waits;
    /// obtaining a replacement lease midway would defeat pause/resume invalidation.
    pub fn begin_step(&self) -> Result<ReflexLease, ReflexInactive> {
        let state = self.inner.lock().map_err(|_| unavailable())?;
        ensure_running(state.snapshot)?;
        Ok(ReflexLease {
            inner: Arc::clone(&self.inner),
            issued: state.snapshot,
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, ControlState>, AppError> {
        self.inner.lock().map_err(|_| {
            AppError::unavailable("reflex control mutex is poisoned; execution is refused")
        })
    }
}

/// Immutable issuance provenance tied to the original shared session allocation.
/// Cloning a lease retains its original generation and cannot refresh it.
#[derive(Debug, Clone)]
pub struct ReflexLease {
    inner: Arc<Mutex<ControlState>>,
    issued: ReflexControlSnapshot,
}

impl ReflexLease {
    /// The snapshot at issuance, not a refreshed view of the current generation.
    pub fn snapshot(&self) -> ReflexControlSnapshot {
        self.issued
    }

    /// Check before dispatch and after awaits. A successful check is a point-in-time
    /// authorization decision, not a claim that native work can be cancelled later.
    pub fn ensure_current(&self) -> Result<(), ReflexInactive> {
        let state = self.inner.lock().map_err(|_| unavailable())?;
        let current = state.snapshot;
        ensure_running(current)?;
        if current.session_id != self.issued.session_id
            || current.generation != self.issued.generation
        {
            return Err(ReflexInactive {
                reason: ReflexInactiveReason::StaleGeneration,
                snapshot: Some(current),
            });
        }
        Ok(())
    }
}

fn ensure_running(snapshot: ReflexControlSnapshot) -> Result<(), ReflexInactive> {
    let reason = match snapshot.mode {
        ReflexControlMode::Running => return Ok(()),
        ReflexControlMode::PausedForChat => ReflexInactiveReason::PausedForChat,
        ReflexControlMode::Stopped => ReflexInactiveReason::Stopped,
    };
    Err(ReflexInactive {
        reason,
        snapshot: Some(snapshot),
    })
}

fn unavailable() -> ReflexInactive {
    ReflexInactive {
        reason: ReflexInactiveReason::Unavailable,
        snapshot: None,
    }
}

fn stopped_transition() -> AppError {
    AppError::invalid_transition("stopped reflex sessions require a new control session")
}

fn advance(
    state: &mut ControlState,
    mode: ReflexControlMode,
) -> Result<ReflexControlSnapshot, AppError> {
    let Some(generation) = state.snapshot.generation.checked_add(1) else {
        // Stop before returning the error: generation zero must never be reused, and
        // even a lease issued at u64::MAX becomes permanently inactive.
        state.snapshot.mode = ReflexControlMode::Stopped;
        return Err(AppError::unavailable(
            "reflex control generation exhausted; session stopped",
        ));
    };
    state.snapshot.generation = generation;
    state.snapshot.mode = mode;
    Ok(state.snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_ex_domain::ErrorKind;

    #[test]
    fn pause_resume_permanently_invalidates_old_leases_and_allows_a_fresh_step() {
        let control = ReflexControl::default();
        let original = control.begin_step().unwrap();
        let cloned_lease = original.clone();
        let issued = original.snapshot();
        assert_eq!(issued.generation, 0);
        assert_eq!(issued.mode, ReflexControlMode::Running);
        original.ensure_current().unwrap();

        let paused = control.pause_for_chat().unwrap();
        assert_eq!(paused.generation, 1);
        assert_eq!(paused.mode, ReflexControlMode::PausedForChat);
        assert_eq!(control.pause_for_chat().unwrap(), paused);
        assert_eq!(
            control.begin_step().unwrap_err().reason,
            ReflexInactiveReason::PausedForChat
        );
        assert_eq!(
            original.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::PausedForChat
        );

        let resumed = control.resume().unwrap();
        assert_eq!(resumed.generation, 2);
        assert_eq!(resumed.mode, ReflexControlMode::Running);
        assert_eq!(resumed.session_id, issued.session_id);
        assert_eq!(control.resume().unwrap(), resumed);
        for old in [original, cloned_lease] {
            assert_eq!(old.snapshot(), issued);
            let stale = old.ensure_current().unwrap_err();
            assert_eq!(stale.reason, ReflexInactiveReason::StaleGeneration);
            assert_eq!(stale.snapshot, Some(resumed));
        }
        let fresh = control.begin_step().unwrap();
        assert_eq!(fresh.snapshot(), resumed);
        fresh.ensure_current().unwrap();
    }

    #[test]
    fn stop_is_terminal_idempotent_and_cannot_be_reversed_by_a_new_session() {
        let control = ReflexControl::new();
        let old = control.begin_step().unwrap();
        let stopped = control.stop().unwrap();
        assert_eq!(stopped.mode, ReflexControlMode::Stopped);
        assert_eq!(stopped.generation, 1);
        assert_eq!(control.stop().unwrap(), stopped);
        assert_eq!(
            control.resume().unwrap_err().kind,
            ErrorKind::InvalidTransition
        );
        assert_eq!(
            control.pause_for_chat().unwrap_err().kind,
            ErrorKind::InvalidTransition
        );
        assert_eq!(
            control.invalidate_target().unwrap_err().kind,
            ErrorKind::InvalidTransition
        );
        assert_eq!(
            control.begin_step().unwrap_err().reason,
            ReflexInactiveReason::Stopped
        );
        assert_eq!(
            old.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::Stopped
        );
        let new_session = ReflexControl::new();
        assert_ne!(
            new_session.snapshot().unwrap().session_id,
            stopped.session_id
        );
        new_session.begin_step().unwrap().ensure_current().unwrap();
        assert_eq!(
            old.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::Stopped
        );
    }

    #[test]
    fn target_changes_invalidate_running_and_paused_generations() {
        let control = ReflexControl::new();
        let old = control.begin_step().unwrap();
        let changed = control.invalidate_target().unwrap();
        assert_eq!(
            (changed.generation, changed.mode),
            (1, ReflexControlMode::Running)
        );
        assert_eq!(
            old.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::StaleGeneration
        );
        let after_change = control.begin_step().unwrap();
        after_change.ensure_current().unwrap();
        control.pause_for_chat().unwrap();
        let changed_while_paused = control.invalidate_target().unwrap();
        assert_eq!(
            (changed_while_paused.generation, changed_while_paused.mode),
            (3, ReflexControlMode::PausedForChat)
        );
        assert_eq!(
            control.begin_step().unwrap_err().reason,
            ReflexInactiveReason::PausedForChat
        );
        control.resume().unwrap();
        assert_eq!(
            after_change.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::StaleGeneration
        );
        control.begin_step().unwrap().ensure_current().unwrap();
    }

    #[test]
    fn cloned_handles_share_control_across_threads() {
        let control = ReflexControl::new();
        let lease = control.begin_step().unwrap();
        let other = control.clone();
        let resumed = std::thread::spawn(move || {
            other.pause_for_chat().unwrap();
            other.resume().unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(control.snapshot().unwrap(), resumed);
        assert_eq!(
            lease.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::StaleGeneration
        );
        control.begin_step().unwrap().ensure_current().unwrap();
        control.clone().stop().unwrap();
        assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Stopped);
    }

    #[test]
    fn every_generation_overflow_stops_without_wrapping_or_reviving_a_lease() {
        for operation in ["pause", "resume", "target", "stop"] {
            let control = ReflexControl::new();
            if operation == "resume" {
                control.pause_for_chat().unwrap();
            }
            control.inner.lock().unwrap().snapshot.generation = u64::MAX;
            let possible_lease = control.begin_step().ok();
            let failure = match operation {
                "pause" => control.pause_for_chat(),
                "resume" => control.resume(),
                "target" => control.invalidate_target(),
                "stop" => control.stop(),
                _ => unreachable!(),
            }
            .unwrap_err();
            assert_eq!(failure.kind, ErrorKind::Unavailable);
            let snapshot = control.snapshot().unwrap();
            assert_eq!(snapshot.generation, u64::MAX);
            assert_eq!(snapshot.mode, ReflexControlMode::Stopped);
            assert_eq!(control.stop().unwrap(), snapshot);
            assert_eq!(
                control.resume().unwrap_err().kind,
                ErrorKind::InvalidTransition
            );
            assert_eq!(
                control.begin_step().unwrap_err().reason,
                ReflexInactiveReason::Stopped
            );
            if let Some(lease) = possible_lease {
                assert_eq!(
                    lease.ensure_current().unwrap_err().reason,
                    ReflexInactiveReason::Stopped
                );
            }
        }
    }

    #[test]
    fn poisoned_control_rejects_commands_and_existing_or_new_leases() {
        let control = ReflexControl::new();
        let lease = control.begin_step().unwrap();
        let inner = Arc::clone(&control.inner);
        let poison = std::thread::spawn(move || {
            let _guard = inner.lock().unwrap();
            panic!("intentional control mutex poison in test thread");
        });
        assert!(poison.join().is_err());
        for result in [
            control.snapshot(),
            control.pause_for_chat(),
            control.resume(),
            control.invalidate_target(),
            control.stop(),
        ] {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Unavailable);
        }
        let inactive = control.begin_step().unwrap_err();
        assert_eq!(inactive.reason, ReflexInactiveReason::Unavailable);
        assert_eq!(inactive.snapshot, None);
        let inactive = lease.ensure_current().unwrap_err();
        assert_eq!(inactive.reason, ReflexInactiveReason::Unavailable);
        assert_eq!(inactive.snapshot, None);
    }

    #[test]
    fn snapshots_are_serializable_without_exposing_mutex_or_lease_internals() {
        let control = ReflexControl::new();
        let snapshot = control.pause_for_chat().unwrap();
        let value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["session_id"], snapshot.session_id.to_string());
        assert_eq!(value["generation"], 1);
        assert_eq!(value["mode"], "paused_for_chat");
        assert_eq!(value.as_object().unwrap().len(), 3);
    }
}
