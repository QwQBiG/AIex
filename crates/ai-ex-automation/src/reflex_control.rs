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

struct ControlState {
    snapshot: ReflexControlSnapshot,
    owner: Option<Uuid>,
}

/// Cloneable commands and snapshots for one reflex session.
///
/// Clones share the same session. `new` creates a separate, initially running
/// session; it is the only way to start again after `stop`.
#[derive(Clone)]
pub struct ReflexControl {
    inner: Arc<Mutex<ControlState>>,
}

impl std::fmt::Debug for ReflexControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReflexControl")
            .finish_non_exhaustive()
    }
}

/// A new owned pause, or an existing pause/stop which the caller must not resume.
#[derive(Debug, Clone)]
pub enum ReflexPauseClaim {
    Claimed(OwnedReflexPause),
    Unowned(ReflexControlSnapshot),
}

/// The result of atomically attempting to resume one particular owned pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflexPauseResume {
    Resumed(ReflexControlSnapshot),
    Superseded(ReflexControlSnapshot),
}

/// Opaque authority to resume only the pause created by `try_pause_owned`.
///
/// Clones retain the same authority. Ordinary control commands revoke it, even
/// an already-paused `pause_for_chat`. Dropping it leaves the session paused;
/// an existing pause cannot be adopted by a new owner.
#[derive(Clone)]
pub struct OwnedReflexPause {
    inner: Arc<Mutex<ControlState>>,
    issued: ReflexControlSnapshot,
    owner: Uuid,
}

impl std::fmt::Debug for OwnedReflexPause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedReflexPause")
            .field("issued", &self.issued)
            .finish_non_exhaustive()
    }
}

impl OwnedReflexPause {
    /// Observe whether this claim still owns the current pause. Use
    /// `resume_if_current` for the atomic check and transition when resuming.
    pub fn is_current(&self) -> Result<bool, AppError> {
        let state = lock_control(&self.inner)?;
        Ok(self.matches(&state))
    }

    /// Resume only while the original pause and private owner still match under
    /// the same lock. A superseded claim does not change a later owner's state.
    pub fn resume_if_current(&self) -> Result<ReflexPauseResume, AppError> {
        let mut state = lock_control(&self.inner)?;
        if !self.matches(&state) {
            return Ok(ReflexPauseResume::Superseded(state.snapshot));
        }
        advance(&mut state, ReflexControlMode::Running).map(ReflexPauseResume::Resumed)
    }

    fn matches(&self, state: &ControlState) -> bool {
        state.snapshot.mode == ReflexControlMode::PausedForChat
            && state.snapshot.session_id == self.issued.session_id
            && state.snapshot.generation == self.issued.generation
            && state.owner == Some(self.owner)
    }
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
                owner: None,
            })),
        }
    }

    /// Poison is an operational error; no state or lease is inferred from it.
    pub fn snapshot(&self) -> Result<ReflexControlSnapshot, AppError> {
        Ok(self.lock()?.snapshot)
    }

    /// Running -> paused invalidates every existing lease. An already-paused
    /// snapshot is unchanged, but this explicit command revokes any owned pause.
    /// A stopped session rejects the command rather than changing its terminal state.
    pub fn pause_for_chat(&self) -> Result<ReflexControlSnapshot, AppError> {
        let mut state = self.lock()?;
        state.owner = None;
        match state.snapshot.mode {
            ReflexControlMode::Running => advance(&mut state, ReflexControlMode::PausedForChat),
            ReflexControlMode::PausedForChat => Ok(state.snapshot),
            ReflexControlMode::Stopped => Err(stopped_transition()),
        }
    }

    /// Atomically pause a running session and claim only that new pause. Existing
    /// paused/stopped sessions are returned unchanged, including another owner's
    /// private claim. This operation never adopts an unknown or manual pause.
    pub fn try_pause_owned(&self) -> Result<ReflexPauseClaim, AppError> {
        let mut state = self.lock()?;
        match state.snapshot.mode {
            ReflexControlMode::Running => {
                let issued = advance(&mut state, ReflexControlMode::PausedForChat)?;
                let owner = Uuid::new_v4();
                state.owner = Some(owner);
                Ok(ReflexPauseClaim::Claimed(OwnedReflexPause {
                    inner: Arc::clone(&self.inner),
                    issued,
                    owner,
                }))
            }
            ReflexControlMode::PausedForChat | ReflexControlMode::Stopped => {
                Ok(ReflexPauseClaim::Unowned(state.snapshot))
            }
        }
    }

    /// Paused -> running starts a fresh generation; it never revalidates old leases.
    /// Already running is a no-op. Resume after stop is an invalid transition.
    pub fn resume(&self) -> Result<ReflexControlSnapshot, AppError> {
        let mut state = self.lock()?;
        state.owner = None;
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
        state.owner = None;
        let mode = state.snapshot.mode;
        if mode == ReflexControlMode::Stopped {
            return Err(stopped_transition());
        }
        advance(&mut state, mode)
    }

    /// Stop is terminal for this session and all clones. Repeated stop is a no-op.
    pub fn stop(&self) -> Result<ReflexControlSnapshot, AppError> {
        let mut state = self.lock()?;
        state.owner = None;
        if state.snapshot.mode == ReflexControlMode::Stopped {
            return Ok(state.snapshot);
        }
        advance(&mut state, ReflexControlMode::Stopped)
    }

    /// Commit a controller update while its exact running control remains locked.
    /// The callback is private to the crate, synchronous and infallible; it runs
    /// only after a checked generation advance and never receives control internals.
    pub(crate) fn advance_running_if_current(
        &self,
        expected: ReflexControlSnapshot,
        apply: impl FnOnce(ReflexControlSnapshot),
    ) -> Result<Option<ReflexControlSnapshot>, AppError> {
        let mut state = self.lock()?;
        if expected.mode != ReflexControlMode::Running
            || state.snapshot.mode != ReflexControlMode::Running
            || state.snapshot != expected
        {
            return Ok(None);
        }
        let snapshot = advance(&mut state, ReflexControlMode::Running)?;
        apply(snapshot);
        Ok(Some(snapshot))
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
        lock_control(&self.inner)
    }
}

/// Immutable issuance provenance tied to the original shared session allocation.
/// Cloning a lease retains its original generation and cannot refresh it.
#[derive(Clone)]
pub struct ReflexLease {
    inner: Arc<Mutex<ControlState>>,
    issued: ReflexControlSnapshot,
}

impl std::fmt::Debug for ReflexLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReflexLease")
            .field("issued", &self.issued)
            .finish_non_exhaustive()
    }
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

fn lock_control(inner: &Mutex<ControlState>) -> Result<MutexGuard<'_, ControlState>, AppError> {
    inner.lock().map_err(|_| {
        AppError::unavailable("reflex control mutex is poisoned; execution is refused")
    })
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
    state.owner = None;
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

    fn claimed_pause(control: &ReflexControl) -> OwnedReflexPause {
        match control.try_pause_owned().unwrap() {
            ReflexPauseClaim::Claimed(pause) => pause,
            ReflexPauseClaim::Unowned(snapshot) => {
                panic!("expected a new owned pause: {snapshot:?}")
            }
        }
    }

    #[test]
    fn owned_pause_resume_is_single_use_and_never_revives_old_step_leases() {
        let control = ReflexControl::new();
        let old_step = control.begin_step().unwrap();
        let pause = claimed_pause(&control);
        let cloned = pause.clone();
        let paused = control.snapshot().unwrap();
        assert_eq!(paused.generation, 1);
        assert_eq!(paused.mode, ReflexControlMode::PausedForChat);
        assert!(pause.is_current().unwrap());
        assert!(cloned.is_current().unwrap());
        assert_eq!(
            old_step.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::PausedForChat
        );

        let ReflexPauseResume::Resumed(resumed) = pause.resume_if_current().unwrap() else {
            panic!("current owner must resume its own pause");
        };
        assert_eq!(resumed.generation, 2);
        assert_eq!(resumed.mode, ReflexControlMode::Running);
        assert!(!pause.is_current().unwrap());
        assert!(!cloned.is_current().unwrap());
        assert_eq!(
            cloned.resume_if_current().unwrap(),
            ReflexPauseResume::Superseded(resumed)
        );
        assert_eq!(control.snapshot().unwrap(), resumed);
        assert_eq!(
            old_step.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::StaleGeneration
        );
        control.begin_step().unwrap().ensure_current().unwrap();
    }

    #[test]
    fn repeated_manual_pause_revokes_ownership_without_changing_the_snapshot() {
        let control = ReflexControl::new();
        let pause = claimed_pause(&control);
        let paused = control.snapshot().unwrap();
        assert_eq!(control.pause_for_chat().unwrap(), paused);
        assert_eq!(control.snapshot().unwrap(), paused);
        assert!(!pause.is_current().unwrap());
        assert_eq!(
            pause.resume_if_current().unwrap(),
            ReflexPauseResume::Superseded(paused)
        );
        assert_eq!(control.snapshot().unwrap(), paused);
        assert_eq!(control.resume().unwrap().generation, paused.generation + 1);
    }

    #[test]
    fn existing_pauses_and_stops_are_unowned_without_stealing_another_claim() {
        let control = ReflexControl::new();
        let pause = claimed_pause(&control);
        let paused = control.snapshot().unwrap();
        for _ in 0..2 {
            assert!(matches!(
                control.try_pause_owned().unwrap(),
                ReflexPauseClaim::Unowned(snapshot) if snapshot == paused
            ));
            assert!(pause.is_current().unwrap());
        }
        let other = ReflexControl::new();
        let manual = other.pause_for_chat().unwrap();
        assert!(matches!(
            other.try_pause_owned().unwrap(),
            ReflexPauseClaim::Unowned(snapshot) if snapshot == manual
        ));
        assert_eq!(other.snapshot().unwrap(), manual);
        let stopped = other.stop().unwrap();
        assert!(matches!(
            other.try_pause_owned().unwrap(),
            ReflexPauseClaim::Unowned(snapshot) if snapshot == stopped
        ));
        assert_eq!(other.snapshot().unwrap(), stopped);
        assert!(pause.is_current().unwrap());
    }

    #[test]
    fn ordinary_resume_target_change_and_stop_supersede_owned_pauses() {
        for command in ["resume", "target", "stop"] {
            let control = ReflexControl::new();
            let pause = claimed_pause(&control);
            let snapshot = match command {
                "resume" => control.resume().unwrap(),
                "target" => control.invalidate_target().unwrap(),
                "stop" => control.stop().unwrap(),
                _ => unreachable!(),
            };
            assert!(!pause.is_current().unwrap(), "command: {command}");
            assert_eq!(
                pause.resume_if_current().unwrap(),
                ReflexPauseResume::Superseded(snapshot),
                "command: {command}"
            );
            assert_eq!(control.snapshot().unwrap(), snapshot);
            assert!(control.inner.lock().unwrap().owner.is_none());
        }
    }

    #[test]
    fn old_claims_cannot_clear_or_resume_a_later_owners_pause() {
        let control = ReflexControl::new();
        let old = claimed_pause(&control);
        let original_pause = control.snapshot().unwrap();
        assert_eq!(control.pause_for_chat().unwrap(), original_pause);
        assert!(!old.is_current().unwrap());
        control.resume().unwrap();
        let current = claimed_pause(&control);
        let paused = control.snapshot().unwrap();
        assert_eq!(paused.generation, 3);
        assert!(!old.is_current().unwrap());
        assert_eq!(
            old.resume_if_current().unwrap(),
            ReflexPauseResume::Superseded(paused)
        );
        assert!(matches!(
            control.try_pause_owned().unwrap(),
            ReflexPauseClaim::Unowned(snapshot) if snapshot == paused
        ));
        assert!(current.is_current().unwrap());
        assert!(matches!(
            current.resume_if_current().unwrap(),
            ReflexPauseResume::Resumed(snapshot) if snapshot.generation == 4
        ));
    }

    #[test]
    fn dropping_an_owned_claim_leaves_a_pause_which_cannot_be_adopted() {
        let control = ReflexControl::new();
        let pause = claimed_pause(&control);
        let paused = control.snapshot().unwrap();
        drop(pause);
        assert_eq!(control.snapshot().unwrap(), paused);
        assert!(matches!(
            control.try_pause_owned().unwrap(),
            ReflexPauseClaim::Unowned(snapshot) if snapshot == paused
        ));
        assert_eq!(control.resume().unwrap().mode, ReflexControlMode::Running);
        assert!(claimed_pause(&control).is_current().unwrap());
    }

    #[test]
    fn an_explicit_pause_from_another_thread_revokes_the_same_claim() {
        let control = ReflexControl::new();
        let pause = claimed_pause(&control);
        let paused = control.snapshot().unwrap();
        let manual = control.clone();
        assert_eq!(
            std::thread::spawn(move || manual.pause_for_chat().unwrap())
                .join()
                .unwrap(),
            paused
        );
        assert!(!pause.is_current().unwrap());
        assert_eq!(
            pause.resume_if_current().unwrap(),
            ReflexPauseResume::Superseded(paused)
        );
    }

    #[test]
    fn racing_a_manual_pause_with_an_owned_resume_always_finishes_paused() {
        let control = ReflexControl::new();
        let pause = claimed_pause(&control);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let manual = control.clone();
        let manual_barrier = Arc::clone(&barrier);
        let manual_thread = std::thread::spawn(move || {
            manual_barrier.wait();
            manual.pause_for_chat().unwrap()
        });
        let resume = pause.clone();
        let resume_barrier = Arc::clone(&barrier);
        let resume_thread = std::thread::spawn(move || {
            resume_barrier.wait();
            resume.resume_if_current().unwrap()
        });
        barrier.wait();
        let manual_snapshot = manual_thread.join().unwrap();
        let result = resume_thread.join().unwrap();
        let final_snapshot = control.snapshot().unwrap();
        assert_eq!(final_snapshot, manual_snapshot);
        assert_eq!(final_snapshot.mode, ReflexControlMode::PausedForChat);
        match result {
            ReflexPauseResume::Resumed(snapshot) => {
                assert_eq!(snapshot.generation, 2);
                assert_eq!(final_snapshot.generation, 3);
            }
            ReflexPauseResume::Superseded(snapshot) => {
                assert_eq!(snapshot, final_snapshot);
                assert_eq!(final_snapshot.generation, 1);
            }
        }
        assert!(!pause.is_current().unwrap());
    }

    #[test]
    fn cloned_claims_racing_to_resume_can_advance_the_generation_only_once() {
        let control = ReflexControl::new();
        let pause = claimed_pause(&control);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let first = pause.clone();
        let first_barrier = Arc::clone(&barrier);
        let first_thread = std::thread::spawn(move || {
            first_barrier.wait();
            first.resume_if_current().unwrap()
        });
        let second_barrier = Arc::clone(&barrier);
        let second_thread = std::thread::spawn(move || {
            second_barrier.wait();
            pause.resume_if_current().unwrap()
        });
        barrier.wait();
        let results = [first_thread.join().unwrap(), second_thread.join().unwrap()];
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, ReflexPauseResume::Resumed(_)))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, ReflexPauseResume::Superseded(_)))
                .count(),
            1
        );
        let snapshot = control.snapshot().unwrap();
        assert_eq!(snapshot.generation, 2);
        assert_eq!(snapshot.mode, ReflexControlMode::Running);
    }

    #[test]
    fn owned_pause_creation_overflow_stops_and_clears_ownership() {
        let control = ReflexControl::new();
        control.inner.lock().unwrap().snapshot.generation = u64::MAX;
        let lease = control.begin_step().unwrap();
        assert_eq!(
            control.try_pause_owned().unwrap_err().kind,
            ErrorKind::Unavailable
        );
        let stopped = control.snapshot().unwrap();
        assert_eq!(stopped.generation, u64::MAX);
        assert_eq!(stopped.mode, ReflexControlMode::Stopped);
        assert!(control.inner.lock().unwrap().owner.is_none());
        assert_eq!(
            lease.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::Stopped
        );
        assert!(matches!(
            control.try_pause_owned().unwrap(),
            ReflexPauseClaim::Unowned(snapshot) if snapshot == stopped
        ));
    }

    #[test]
    fn owned_resume_overflow_stops_without_reusing_the_claim() {
        let control = ReflexControl::new();
        control.inner.lock().unwrap().snapshot.generation = u64::MAX - 1;
        let lease = control.begin_step().unwrap();
        let pause = claimed_pause(&control);
        assert!(pause.is_current().unwrap());
        assert_eq!(control.snapshot().unwrap().generation, u64::MAX);
        assert_eq!(
            pause.resume_if_current().unwrap_err().kind,
            ErrorKind::Unavailable
        );
        let stopped = control.snapshot().unwrap();
        assert_eq!(stopped.generation, u64::MAX);
        assert_eq!(stopped.mode, ReflexControlMode::Stopped);
        assert!(control.inner.lock().unwrap().owner.is_none());
        assert!(!pause.is_current().unwrap());
        assert_eq!(
            pause.resume_if_current().unwrap(),
            ReflexPauseResume::Superseded(stopped)
        );
        assert_eq!(
            lease.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::Stopped
        );
    }

    #[test]
    fn poisoned_control_refuses_claim_creation_and_owned_checks_or_resume() {
        let control = ReflexControl::new();
        let pause = claimed_pause(&control);
        let inner = Arc::clone(&control.inner);
        let poison = std::thread::spawn(move || {
            let _guard = inner.lock().unwrap();
            panic!("intentional owned pause mutex poison in test thread");
        });
        assert!(poison.join().is_err());
        assert_eq!(
            control.try_pause_owned().unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(pause.is_current().unwrap_err().kind, ErrorKind::Unavailable);
        assert_eq!(
            pause.resume_if_current().unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert!(!format!("{control:?} {pause:?}").contains("inner"));
    }

    #[test]
    fn ownership_is_absent_from_snapshot_and_debug_never_locks_or_reveals_the_key() {
        let control = ReflexControl::new();
        let lease = control.begin_step().unwrap();
        let pause = claimed_pause(&control);
        let claim = ReflexPauseClaim::Claimed(pause.clone());
        let state = control.inner.lock().unwrap();
        let private_key = state.owner.unwrap().to_string();
        let debug = format!("{control:?} {lease:?} {pause:?} {claim:?}");
        assert!(!debug.contains(&private_key));
        assert!(!debug.contains("inner"));
        assert!(!debug.contains("owner:"));
        assert_eq!(format!("{control:?}"), "ReflexControl { .. }");
        let value = serde_json::to_value(state.snapshot).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
        assert_eq!(value["session_id"], state.snapshot.session_id.to_string());
        assert_eq!(value["generation"], 1);
        assert_eq!(value["mode"], "paused_for_chat");
    }
    #[test]
    fn running_generation_update_keeps_the_control_lock_through_commit() {
        let control = ReflexControl::new();
        let observed = control.clone();
        let expected = control.snapshot().unwrap();
        let mut applied = false;
        let updated = control
            .advance_running_if_current(expected, |snapshot| {
                assert!(matches!(
                    observed.inner.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ));
                assert_eq!(snapshot.generation, expected.generation + 1);
                assert_eq!(snapshot.mode, ReflexControlMode::Running);
                applied = true;
            })
            .unwrap()
            .unwrap();
        assert!(applied);
        assert_eq!(control.snapshot().unwrap(), updated);
        let mut applied_again = false;
        assert_eq!(
            control
                .advance_running_if_current(expected, |_| applied_again = true)
                .unwrap(),
            None
        );
        assert!(!applied_again);
    }

    #[test]
    fn running_generation_update_overflow_stops_without_applying_controller_change() {
        let control = ReflexControl::new();
        control.inner.lock().unwrap().snapshot.generation = u64::MAX;
        let lease = control.begin_step().unwrap();
        let expected = control.snapshot().unwrap();
        let mut applied = false;
        assert_eq!(
            control
                .advance_running_if_current(expected, |_| applied = true)
                .unwrap_err()
                .kind,
            ErrorKind::Unavailable
        );
        assert!(!applied);
        let stopped = control.snapshot().unwrap();
        assert_eq!(stopped.generation, u64::MAX);
        assert_eq!(stopped.mode, ReflexControlMode::Stopped);
        assert_eq!(
            lease.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::Stopped
        );
    }
}
