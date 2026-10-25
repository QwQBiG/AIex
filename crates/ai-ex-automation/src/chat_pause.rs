//! Injected-time SDK policy; no host chat hook, timer, network, or input is installed.
//!
//! Only an opaque owned pause can be resumed. Manual control or target invalidation
//! supersedes that ownership, even when the public paused snapshot does not change.
//! Time values must share one monotonic domain; equal timestamps are allowed.

use ai_ex_domain::AppError;
use serde::Serialize;

use crate::reflex_control::{
    OwnedReflexPause, ReflexControl, ReflexControlSnapshot, ReflexPauseClaim, ReflexPauseResume,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ChatPauseOptions {
    pub enabled: bool,
    pub timeout_ms: u64,
}

impl Default for ChatPauseOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: 30_000,
        }
    }
}

/// The host must account for queued work and event gaps before reporting Idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeEligibility {
    Idle,
    Busy,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatPauseOutcome {
    Disabled,
    Claimed,
    Extended,
    Waiting,
    HeldBusy,
    HeldUnknown,
    Resumed,
    Unowned,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatPauseReport {
    pub outcome: ChatPauseOutcome,
    pub observed_ms: u64,
    pub last_chat_ms: Option<u64>,
    /// Present only while this policy retains an active owned-pause deadline.
    pub deadline_ms: Option<u64>,
    /// An optional point-in-time observation, never a serializable ownership token.
    pub control: Option<ReflexControlSnapshot>,
    /// None for on_chat; Some for every poll, including disabled polls.
    pub eligibility: Option<ResumeEligibility>,
}

/// Constant-space synchronous policy with one private pause claim.
///
/// The caller supplies time and Idle/Busy/Unknown; this policy does not infer host
/// activity. Pause revokes old reflex leases, but cannot cancel already-issued work.
/// Polling without a claim never pauses or resumes control. Settings are immutable.
pub struct ChatPausePolicy {
    control: ReflexControl,
    options: ChatPauseOptions,
    claim: Option<OwnedReflexPause>,
    last_observed_ms: Option<u64>,
    last_chat_ms: Option<u64>,
    deadline_ms: Option<u64>,
}

impl ChatPausePolicy {
    pub fn new(control: ReflexControl, options: ChatPauseOptions) -> Result<Self, AppError> {
        if options.timeout_ms < 5_000 {
            return Err(AppError::configuration(
                "chat pause timeout must be at least 5000 milliseconds",
            ));
        }
        Ok(Self {
            control,
            options,
            claim: None,
            last_observed_ms: None,
            last_chat_ms: None,
            deadline_ms: None,
        })
    }

    /// Record enabled chat activity and acquire or extend only this policy's pause.
    /// Deadline overflow and clock regression fail before policy/control mutation.
    pub fn on_chat(&mut self, now_ms: u64) -> Result<ChatPauseReport, AppError> {
        self.validate_time(now_ms)?;
        if !self.options.enabled {
            let control = self.control.snapshot().ok();
            return Ok(self.report(now_ms, ChatPauseOutcome::Disabled, None, control));
        }
        let deadline = now_ms.checked_add(self.options.timeout_ms).ok_or_else(|| {
            AppError::invalid_transition("chat pause deadline exceeds the injected time range")
        })?;
        let still_owned = match self.claim.as_ref() {
            Some(claim) => claim.is_current()?,
            None => false,
        };
        if still_owned {
            // Calling ordinary pause_for_chat here would invalidate our own claim.
            self.last_chat_ms = Some(now_ms);
            self.deadline_ms = Some(deadline);
            let control = self.control.snapshot().ok();
            return Ok(self.report(now_ms, ChatPauseOutcome::Extended, None, control));
        }

        let had_claim = self.claim.is_some();
        let (outcome, control) = match self.control.try_pause_owned()? {
            ReflexPauseClaim::Claimed(claim) => {
                self.claim = Some(claim);
                self.deadline_ms = Some(deadline);
                // The claim is retained even if a concurrent control change makes the
                // optional observation unavailable. Its next check still fails closed.
                (ChatPauseOutcome::Claimed, self.control.snapshot().ok())
            }
            ReflexPauseClaim::Unowned(snapshot) => {
                self.claim = None;
                self.deadline_ms = None;
                (
                    if had_claim {
                        ChatPauseOutcome::Superseded
                    } else {
                        ChatPauseOutcome::Unowned
                    },
                    Some(snapshot),
                )
            }
        };
        self.last_chat_ms = Some(now_ms);
        Ok(self.report(now_ms, outcome, None, control))
    }

    /// Resume only a still-owned due pause and only when the host explicitly says Idle.
    /// Busy/Unknown preserve the deadline. An atomic conditional resume closes the
    /// race between observing ownership and a subsequent manual control operation.
    pub fn poll(
        &mut self,
        now_ms: u64,
        eligibility: ResumeEligibility,
    ) -> Result<ChatPauseReport, AppError> {
        self.validate_time(now_ms)?;
        if !self.options.enabled {
            let control = self.control.snapshot().ok();
            return Ok(self.report(
                now_ms,
                ChatPauseOutcome::Disabled,
                Some(eligibility),
                control,
            ));
        }
        let Some(claim) = self.claim.as_ref() else {
            let control = self.control.snapshot().ok();
            return Ok(self.report(
                now_ms,
                ChatPauseOutcome::Unowned,
                Some(eligibility),
                control,
            ));
        };
        if !claim.is_current()? {
            self.claim = None;
            self.deadline_ms = None;
            let control = self.control.snapshot().ok();
            return Ok(self.report(
                now_ms,
                ChatPauseOutcome::Superseded,
                Some(eligibility),
                control,
            ));
        }
        let deadline = self
            .deadline_ms
            .ok_or_else(|| AppError::invalid_transition("owned chat pause has no deadline"))?;
        let held = if now_ms < deadline {
            Some(ChatPauseOutcome::Waiting)
        } else {
            match eligibility {
                ResumeEligibility::Busy => Some(ChatPauseOutcome::HeldBusy),
                ResumeEligibility::Unknown => Some(ChatPauseOutcome::HeldUnknown),
                ResumeEligibility::Idle => None,
            }
        };
        if let Some(outcome) = held {
            let control = self.control.snapshot().ok();
            return Ok(self.report(now_ms, outcome, Some(eligibility), control));
        }
        let (outcome, snapshot) = match claim.resume_if_current()? {
            ReflexPauseResume::Resumed(snapshot) => (ChatPauseOutcome::Resumed, snapshot),
            ReflexPauseResume::Superseded(snapshot) => (ChatPauseOutcome::Superseded, snapshot),
        };
        self.claim = None;
        self.deadline_ms = None;
        Ok(self.report(now_ms, outcome, Some(eligibility), Some(snapshot)))
    }

    fn validate_time(&self, now_ms: u64) -> Result<(), AppError> {
        if self
            .last_observed_ms
            .is_some_and(|previous| now_ms < previous)
        {
            return Err(AppError::invalid_transition(
                "chat pause timestamps must be nondecreasing",
            ));
        }
        Ok(())
    }

    fn report(
        &mut self,
        now_ms: u64,
        outcome: ChatPauseOutcome,
        eligibility: Option<ResumeEligibility>,
        control: Option<ReflexControlSnapshot>,
    ) -> ChatPauseReport {
        self.last_observed_ms = Some(now_ms);
        ChatPauseReport {
            outcome,
            observed_ms: now_ms,
            last_chat_ms: self.last_chat_ms,
            deadline_ms: self.deadline_ms,
            control,
            eligibility,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflex_control::{ReflexControlMode, ReflexInactiveReason};
    use ai_ex_domain::ErrorKind;

    fn policy(control: &ReflexControl) -> ChatPausePolicy {
        ChatPausePolicy::new(control.clone(), ChatPauseOptions::default()).unwrap()
    }

    #[test]
    fn default_deadline_resumes_at_30000_not_29999_and_never_revives_an_old_lease() {
        let control = ReflexControl::new();
        let old = control.begin_step().unwrap();
        let mut policy = policy(&control);
        let claimed = policy.on_chat(0).unwrap();
        assert_eq!(claimed.outcome, ChatPauseOutcome::Claimed);
        assert_eq!(claimed.last_chat_ms, Some(0));
        assert_eq!(claimed.deadline_ms, Some(30_000));
        assert_eq!(claimed.eligibility, None);
        assert_eq!(
            control.snapshot().unwrap().mode,
            ReflexControlMode::PausedForChat
        );
        assert_eq!(
            old.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::PausedForChat
        );
        assert_eq!(
            policy
                .poll(29_999, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Waiting
        );
        let resumed = policy.poll(30_000, ResumeEligibility::Idle).unwrap();
        assert_eq!(resumed.outcome, ChatPauseOutcome::Resumed);
        assert_eq!(resumed.last_chat_ms, Some(0));
        assert_eq!(resumed.deadline_ms, None);
        assert_eq!(resumed.eligibility, Some(ResumeEligibility::Idle));
        assert_eq!(control.snapshot().unwrap().mode, ReflexControlMode::Running);
        assert_eq!(
            old.ensure_current().unwrap_err().reason,
            ReflexInactiveReason::StaleGeneration
        );
        control.begin_step().unwrap().ensure_current().unwrap();
        assert_eq!(
            policy
                .poll(30_000, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Unowned
        );
    }

    #[test]
    fn repeated_chat_extends_without_replacing_the_owned_pause() {
        let control = ReflexControl::new();
        let mut policy = policy(&control);
        policy.on_chat(0).unwrap();
        let paused = control.snapshot().unwrap();
        let extended = policy.on_chat(10_000).unwrap();
        assert_eq!(extended.outcome, ChatPauseOutcome::Extended);
        assert_eq!(extended.deadline_ms, Some(40_000));
        assert_eq!(control.snapshot().unwrap(), paused);
        assert_eq!(
            policy
                .poll(30_000, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Waiting
        );
        assert_eq!(policy.on_chat(40_000).unwrap().deadline_ms, Some(70_000));
        assert_eq!(
            policy
                .poll(69_999, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Waiting
        );
        assert_eq!(
            policy
                .poll(70_000, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Resumed
        );
    }

    #[test]
    fn busy_and_unknown_hold_a_due_pause_without_extending_quiet_time() {
        let control = ReflexControl::new();
        let mut policy = policy(&control);
        policy.on_chat(0).unwrap();
        for (now, eligibility, outcome) in [
            (30_000, ResumeEligibility::Busy, ChatPauseOutcome::HeldBusy),
            (
                40_000,
                ResumeEligibility::Unknown,
                ChatPauseOutcome::HeldUnknown,
            ),
        ] {
            let held = policy.poll(now, eligibility).unwrap();
            assert_eq!(held.outcome, outcome);
            assert_eq!(held.last_chat_ms, Some(0));
            assert_eq!(held.deadline_ms, Some(30_000));
            assert_eq!(held.eligibility, Some(eligibility));
            assert_eq!(
                control.snapshot().unwrap().mode,
                ReflexControlMode::PausedForChat
            );
        }
        assert_eq!(
            policy
                .poll(40_000, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Resumed
        );
    }

    #[test]
    fn disabled_policy_neither_claims_nor_resumes_and_does_not_record_chat_activity() {
        let control = ReflexControl::new();
        let mut policy = ChatPausePolicy::new(
            control.clone(),
            ChatPauseOptions {
                enabled: false,
                ..ChatPauseOptions::default()
            },
        )
        .unwrap();
        let running = control.snapshot().unwrap();
        for report in [
            policy.on_chat(0).unwrap(),
            policy.poll(30_000, ResumeEligibility::Idle).unwrap(),
        ] {
            assert_eq!(report.outcome, ChatPauseOutcome::Disabled);
            assert_eq!(report.last_chat_ms, None);
            assert_eq!(report.deadline_ms, None);
        }
        assert_eq!(control.snapshot().unwrap(), running);
        let paused = control.pause_for_chat().unwrap();
        policy.on_chat(30_001).unwrap();
        policy.poll(60_001, ResumeEligibility::Idle).unwrap();
        assert_eq!(control.snapshot().unwrap(), paused);
    }

    #[test]
    fn timeout_minimum_is_validated_even_when_disabled_and_5000_is_accepted() {
        for enabled in [true, false] {
            for timeout_ms in [0, 4_999] {
                assert!(
                    ChatPausePolicy::new(
                        ReflexControl::new(),
                        ChatPauseOptions {
                            enabled,
                            timeout_ms
                        }
                    )
                    .is_err()
                );
            }
        }
        let control = ReflexControl::new();
        let mut policy = ChatPausePolicy::new(
            control,
            ChatPauseOptions {
                enabled: true,
                timeout_ms: 5_000,
            },
        )
        .unwrap();
        policy.on_chat(0).unwrap();
        assert_eq!(
            policy.poll(4_999, ResumeEligibility::Idle).unwrap().outcome,
            ChatPauseOutcome::Waiting
        );
        assert_eq!(
            policy.poll(5_000, ResumeEligibility::Idle).unwrap().outcome,
            ChatPauseOutcome::Resumed
        );
    }

    #[test]
    fn a_preexisting_manual_pause_is_never_claimed_or_resumed() {
        let control = ReflexControl::new();
        let paused = control.pause_for_chat().unwrap();
        let mut policy = policy(&control);
        let chat = policy.on_chat(0).unwrap();
        assert_eq!(chat.outcome, ChatPauseOutcome::Unowned);
        assert_eq!(chat.last_chat_ms, Some(0));
        assert_eq!(chat.deadline_ms, None);
        assert_eq!(
            policy
                .poll(30_000, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Unowned
        );
        assert_eq!(control.snapshot().unwrap(), paused);
    }

    #[test]
    fn repeated_manual_pause_revokes_auto_resume_even_when_the_snapshot_is_unchanged() {
        let control = ReflexControl::new();
        let mut policy = policy(&control);
        policy.on_chat(0).unwrap();
        let paused = control.snapshot().unwrap();
        assert_eq!(control.pause_for_chat().unwrap(), paused);
        let superseded = policy.poll(30_000, ResumeEligibility::Idle).unwrap();
        assert_eq!(superseded.outcome, ChatPauseOutcome::Superseded);
        assert_eq!(superseded.deadline_ms, None);
        assert_eq!(control.snapshot().unwrap(), paused);
        assert_eq!(
            policy.on_chat(30_001).unwrap().outcome,
            ChatPauseOutcome::Unowned
        );
        assert_eq!(
            policy
                .poll(60_001, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Unowned
        );
        assert_eq!(control.snapshot().unwrap(), paused);
    }

    #[test]
    fn stop_and_target_invalidation_supersede_the_claim_without_auto_resume() {
        for stop in [false, true] {
            let control = ReflexControl::new();
            let mut policy = policy(&control);
            policy.on_chat(0).unwrap();
            let changed = if stop {
                control.stop().unwrap()
            } else {
                control.invalidate_target().unwrap()
            };
            assert_eq!(
                policy
                    .poll(30_000, ResumeEligibility::Idle)
                    .unwrap()
                    .outcome,
                ChatPauseOutcome::Superseded
            );
            assert_eq!(
                policy.on_chat(30_001).unwrap().outcome,
                ChatPauseOutcome::Unowned
            );
            assert_eq!(
                policy
                    .poll(60_001, ResumeEligibility::Idle)
                    .unwrap()
                    .outcome,
                ChatPauseOutcome::Unowned
            );
            assert_eq!(control.snapshot().unwrap(), changed);
        }
    }

    #[test]
    fn external_resume_does_not_make_poll_repause_but_new_chat_can_claim_running_again() {
        let control = ReflexControl::new();
        let mut policy = policy(&control);
        policy.on_chat(0).unwrap();
        let running = control.resume().unwrap();
        assert_eq!(
            policy.poll(1, ResumeEligibility::Idle).unwrap().outcome,
            ChatPauseOutcome::Superseded
        );
        assert_eq!(
            policy.poll(2, ResumeEligibility::Idle).unwrap().outcome,
            ChatPauseOutcome::Unowned
        );
        assert_eq!(control.snapshot().unwrap(), running);
        assert_eq!(
            policy.on_chat(10).unwrap().outcome,
            ChatPauseOutcome::Claimed
        );
        control.resume().unwrap();
        // The stale claim is still retained locally here; new chat may replace it
        // only because try_pause_owned atomically observes Running.
        let fresh = policy.on_chat(20).unwrap();
        assert_eq!(fresh.outcome, ChatPauseOutcome::Claimed);
        assert_eq!(fresh.deadline_ms, Some(30_020));
        assert_eq!(
            policy
                .poll(30_020, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Resumed
        );
    }

    #[test]
    fn another_policy_cannot_steal_or_resume_the_first_policys_pause() {
        let control = ReflexControl::new();
        let mut first = policy(&control);
        let mut second = policy(&control);
        first.on_chat(0).unwrap();
        let paused = control.snapshot().unwrap();
        assert_eq!(
            second.on_chat(0).unwrap().outcome,
            ChatPauseOutcome::Unowned
        );
        assert_eq!(
            second
                .poll(30_000, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Unowned
        );
        assert_eq!(control.snapshot().unwrap(), paused);
        assert_eq!(
            first.poll(30_000, ResumeEligibility::Idle).unwrap().outcome,
            ChatPauseOutcome::Resumed
        );
    }

    #[test]
    fn backwards_times_fail_without_changing_control_activity_or_deadline() {
        let control = ReflexControl::new();
        let mut policy = policy(&control);
        policy.on_chat(100).unwrap();
        policy.poll(200, ResumeEligibility::Busy).unwrap();
        let paused = control.snapshot().unwrap();
        assert_eq!(
            policy.on_chat(199).unwrap_err().kind,
            ErrorKind::InvalidTransition
        );
        assert_eq!(
            policy.poll(199, ResumeEligibility::Idle).unwrap_err().kind,
            ErrorKind::InvalidTransition
        );
        assert_eq!(policy.last_observed_ms, Some(200));
        assert_eq!(policy.last_chat_ms, Some(100));
        assert_eq!(policy.deadline_ms, Some(30_100));
        assert_eq!(control.snapshot().unwrap(), paused);
        assert_eq!(
            policy
                .poll(30_100, ResumeEligibility::Idle)
                .unwrap()
                .outcome,
            ChatPauseOutcome::Resumed
        );
    }

    #[test]
    fn deadline_overflow_precedes_control_mutation_and_preserves_an_existing_claim() {
        let too_late = u64::MAX - 29_999;
        let control = ReflexControl::new();
        let mut fresh = policy(&control);
        let running = control.snapshot().unwrap();
        assert!(fresh.on_chat(too_late).is_err());
        assert_eq!(control.snapshot().unwrap(), running);
        assert_eq!(
            (
                fresh.last_observed_ms,
                fresh.last_chat_ms,
                fresh.deadline_ms
            ),
            (None, None, None)
        );
        fresh.on_chat(100).unwrap();
        let paused = control.snapshot().unwrap();
        assert!(fresh.on_chat(too_late).is_err());
        assert_eq!(control.snapshot().unwrap(), paused);
        assert_eq!(
            (
                fresh.last_observed_ms,
                fresh.last_chat_ms,
                fresh.deadline_ms
            ),
            (Some(100), Some(100), Some(30_100))
        );
        assert_eq!(
            fresh.poll(30_100, ResumeEligibility::Idle).unwrap().outcome,
            ChatPauseOutcome::Resumed
        );
    }

    #[test]
    fn report_serialization_contains_observations_but_no_claim_or_owner_key() {
        let control = ReflexControl::new();
        let mut policy = policy(&control);
        let value = serde_json::to_value(policy.on_chat(5).unwrap()).unwrap();
        assert_eq!(value["outcome"], "claimed");
        assert_eq!(value["observed_ms"], 5);
        assert_eq!(value["last_chat_ms"], 5);
        assert_eq!(value["deadline_ms"], 30_005);
        assert!(value["eligibility"].is_null());
        assert_eq!(value.as_object().unwrap().len(), 6);
        let control = value["control"].as_object().unwrap();
        assert_eq!(control.len(), 3);
        assert!(
            control.contains_key("session_id")
                && control.contains_key("generation")
                && control.contains_key("mode")
        );
    }
}
