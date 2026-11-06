use super::activity::{bump, validate_cursor};
use super::*;
use ai_ex_automation::{ActionResult, AutomationAction};
use ai_ex_domain::NativeDesktopPhase;
use std::sync::atomic::Ordering;
use std::time::Duration;

impl<S: DesktopSystem + Send + 'static> NativeHost<S> {
    pub(super) fn check_running(
        &self,
        instance: Uuid,
        revision: u64,
        session: Uuid,
    ) -> Result<u64, AppError> {
        let snapshot = self.snapshot()?;
        validate_cursor(&snapshot, instance, revision, Some(session), false)?;
        if self.shared.closed.load(Ordering::Acquire)
            || self.shared.cancelled.load(Ordering::Acquire)
            || snapshot
                .session
                .as_ref()
                .is_none_or(|s| s.phase != NativeDesktopPhase::Running)
            || self.shared.pending.load(Ordering::Acquire) != 0
            || self.speech.snapshot() != ai_ex_audio::SpeechActivitySnapshot::Idle
        {
            return Err(AppError::invalid_transition(
                "native session must be confirmed and conversation idle",
            ));
        }
        Ok(self.shared.activity.load(Ordering::Acquire))
    }

    pub(super) fn guard(&self, generation: u64) -> Result<(), AppError> {
        if self.shared.closed.load(Ordering::Acquire)
            || self.shared.cancelled.load(Ordering::Acquire)
            || self.shared.activity.load(Ordering::Acquire) != generation
            || self.shared.pending.load(Ordering::Acquire) != 0
            || self.speech.snapshot() != ai_ex_audio::SpeechActivitySnapshot::Idle
        {
            return Err(AppError::invalid_transition(
                "native dispatch generation revoked",
            ));
        }
        Ok(())
    }

    pub(super) async fn capture(&self, state: &mut State<S>) -> Result<Uuid, AppError> {
        self.clear_vision_proposal()?;
        let (generation, scope) = {
            let mut snapshot = self
                .shared
                .snapshot
                .lock()
                .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
            let current = snapshot
                .session
                .as_mut()
                .ok_or_else(|| AppError::invalid_transition("no native session"))?;
            if current.frames >= current.max_frames {
                return Err(AppError::unavailable("native capture budget exhausted"));
            }
            current.frames += 1;
            current.frame_id = None;
            let scope = current.window.scope.clone();
            bump(&mut snapshot)?;
            (self.shared.activity.load(Ordering::Acquire), scope)
        };
        state.frame = None;
        self.guard(generation)?;
        let result = state
            .coordinator
            .as_mut()
            .ok_or_else(|| AppError::invalid_transition("native coordinator unavailable"))?
            .execute_guarded(
                scope,
                "capture the selected window for one bounded native step",
                AutomationAction::CaptureScreen,
                || self.guard(generation),
            )
            .await;
        let receipt = match result {
            Ok(receipt) if receipt.completion_audit_persisted => receipt,
            _ => {
                self.fail_step(state, "capture did not settle with a durable completion")
                    .await?;
                return Err(AppError::unavailable(
                    "native capture failed; refresh and confirm before continuing",
                ));
            }
        };
        self.guard(generation)?;
        let ActionResult::ScreenCaptured(frame) = receipt.result else {
            return Err(AppError::protocol("native capture returned no frame"));
        };
        let id = Uuid::new_v4();
        let mut snapshot = self
            .shared
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
        self.guard(generation)?;
        snapshot
            .session
            .as_mut()
            .expect("capture session retained")
            .frame_id = Some(id);
        bump(&mut snapshot)?;
        state.frame = Some((id, Instant::now(), frame));
        Ok(id)
    }

    pub(super) async fn step(
        &self,
        state: &mut State<S>,
        request: NativeDesktopRequest,
    ) -> Result<(), AppError> {
        let (instance, revision, session_id, operation, action) = match &request {
            NativeDesktopRequest::Execute {
                instance_id,
                expected_revision,
                session_id,
                operation_id,
                frame_id,
                action,
            } => {
                action.validate()?;
                if state.frame.as_ref().is_none_or(|(id, at, _)| {
                    id != frame_id || at.elapsed() > Duration::from_secs(5)
                }) {
                    return Err(AppError::invalid_transition(
                        "native proposal frame expired; capture again",
                    ));
                }
                (
                    *instance_id,
                    *expected_revision,
                    *session_id,
                    *operation_id,
                    Some(action.clone()),
                )
            }
            NativeDesktopRequest::VisionStep {
                instance_id,
                expected_revision,
                session_id,
                operation_id,
                goal,
            } => {
                if goal.trim().is_empty() || goal.chars().count() > 1024 {
                    return Err(AppError::configuration("invalid native vision goal"));
                }
                (
                    *instance_id,
                    *expected_revision,
                    *session_id,
                    *operation_id,
                    None,
                )
            }
            _ => return Err(AppError::protocol("not a native step")),
        };
        if operation.is_nil() {
            return Err(AppError::configuration(
                "native operation id must not be nil",
            ));
        }
        let generation = self.check_running(instance, revision, session_id)?;
        self.clear_vision_proposal()?;
        let scope = {
            let mut snapshot = self
                .shared
                .snapshot
                .lock()
                .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
            validate_cursor(&snapshot, instance, revision, Some(session_id), false)?;
            let current = snapshot.session.as_mut().expect("validated session");
            if current.actions >= current.max_actions || state.seen.len() >= 4096 {
                return Err(AppError::unavailable("native action budget exhausted"));
            }
            current.actions += 1;
            current.last_operation = Some(operation);
            current.last_outcome = Some("in_flight".into());
            let scope = current.window.scope.clone();
            bump(&mut snapshot)?;
            scope
        };
        state.seen.insert(operation, request.clone());
        let action = match action {
            Some(action) => action,
            None => match self.vision_action(state, &request, generation).await {
                Ok(action) => action,
                Err(error) => {
                    self.fail_step(state, "vision proposal rejected or expired")
                        .await?;
                    return Err(error);
                }
            },
        };
        if state
            .frame
            .as_ref()
            .is_none_or(|(_, at, _)| at.elapsed() > Duration::from_secs(5))
        {
            self.fail_step(state, "proposal frame expired").await?;
            return Err(AppError::invalid_transition("native frame expired"));
        }
        state.frame = None;
        let result = state
            .coordinator
            .as_mut()
            .ok_or_else(|| AppError::invalid_transition("native coordinator unavailable"))?
            .execute_guarded(
                scope,
                "execute one window-bound native proposal",
                AutomationAction::Native { action },
                || self.guard(generation),
            )
            .await;
        if !matches!(result, Ok(ref receipt) if receipt.completion_audit_persisted) {
            self.fail_step(
                state,
                "native action failed or outcome unknown; never automatically retry",
            )
            .await?;
            return Err(AppError::unavailable(
                "native step did not settle; query status",
            ));
        }
        self.guard(generation)?;
        let mut snapshot = self
            .shared
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
        self.guard(generation)?;
        let current = snapshot.session.as_mut().expect("step session retained");
        current.frame_id = None;
        current.last_outcome = Some("completed".into());
        bump(&mut snapshot)
    }

    pub(super) async fn fail_step(
        &self,
        state: &mut State<S>,
        reason: &str,
    ) -> Result<(), AppError> {
        state.frame = None;
        self.shared.pause(reason, false)?;
        let release = state
            .desktop
            .lock()
            .map_err(|_| AppError::unavailable("adapter lock poisoned"))?
            .release_owned();
        if !release.still_owned.is_empty() {
            return Err(AppError::safety(
                "native owned input remains pressed; stop and retry release",
            ));
        }
        Ok(())
    }
}
