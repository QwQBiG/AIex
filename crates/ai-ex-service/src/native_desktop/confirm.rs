//! Explicit confirmation of one service-owned visual proposal.

use super::activity::bump;
use super::proposal::{Confirmation, PROPOSAL_TTL, frame_digest};
use super::*;
use ai_ex_automation::{AutomationAction, ExecutionPhase, GuardedExecutionFailure};
use ai_ex_domain::{NativeDesktopPhase, NativeVisionOperationState};

impl<S: DesktopSystem + Send + 'static> NativeHost<S> {
    pub(super) async fn confirm_vision_proposal(
        &self,
        state: &mut State<S>,
        request: &NativeDesktopRequest,
    ) -> Result<NativeDesktopReply, AppError> {
        let NativeDesktopRequest::ConfirmVisionProposal {
            instance_id,
            session_id,
            proposal_id,
            operation_id,
            ..
        } = request
        else {
            return Err(AppError::protocol("vision confirmation intent required"));
        };
        let (proposal, scope) = match self.begin_vision_confirmation(state, request)? {
            Confirmation::Existing => {
                return self.get_vision_operation(
                    *instance_id,
                    *session_id,
                    *proposal_id,
                    *operation_id,
                );
            }
            Confirmation::Started { proposal, scope } => (proposal, scope),
        };
        let generation = proposal.generation;
        let fresh_id = match self.capture(state).await {
            Ok(id) => id,
            Err(_) => {
                return self.reject_before_input(
                    state,
                    *instance_id,
                    *session_id,
                    *proposal_id,
                    *operation_id,
                );
            }
        };
        let matches = state.frame.as_ref().is_some_and(|(id, _, frame)| {
            *id == fresh_id && frame_digest(frame) == proposal.digest
        });
        state.frame = None;
        if !matches
            || proposal.captured.elapsed() >= PROPOSAL_TTL
            || state.ticket != Some(proposal.ticket)
            || self.guard(generation).is_err()
        {
            return self.reject_before_input(
                state,
                *instance_id,
                *session_id,
                *proposal_id,
                *operation_id,
            );
        }
        if self
            .reserve_vision_action(*session_id, *operation_id, fresh_id, generation)
            .is_err()
        {
            return self.reject_before_input(
                state,
                *instance_id,
                *session_id,
                *proposal_id,
                *operation_id,
            );
        }
        let guard = || {
            self.guard(generation)?;
            if proposal.captured.elapsed() >= PROPOSAL_TTL {
                return Err(AppError::invalid_transition("vision proposal expired"));
            }
            Ok(())
        };
        let Some(coordinator) = state.coordinator.as_mut() else {
            return self.reject_before_input(
                state,
                *instance_id,
                *session_id,
                *proposal_id,
                *operation_id,
            );
        };
        let outcome = coordinator
            .execute_guarded(
                scope,
                "execute one explicitly confirmed visual proposal",
                AutomationAction::Native {
                    action: proposal.action,
                },
                guard,
            )
            .await;
        let status = match outcome {
            Ok(ref receipt) if receipt.completion_audit_persisted => {
                NativeVisionOperationState::Completed
            }
            Err(GuardedExecutionFailure::GuardRejected { .. }) => {
                NativeVisionOperationState::Rejected
            }
            Err(GuardedExecutionFailure::Execution(ref failure))
                if failure.phase == ExecutionPhase::BeforeExecution =>
            {
                NativeVisionOperationState::Rejected
            }
            _ => NativeVisionOperationState::Unknown,
        };
        self.finish_vision_operation(*operation_id, status)?;
        self.finish_vision_snapshot(*session_id, *operation_id, status)?;
        if status != NativeVisionOperationState::Completed {
            let _ = self
                .fail_step(state, "confirmed vision action did not settle")
                .await;
        }
        self.get_vision_operation(*instance_id, *session_id, *proposal_id, *operation_id)
    }

    fn reserve_vision_action(
        &self,
        session: Uuid,
        operation: Uuid,
        frame: Uuid,
        generation: u64,
    ) -> Result<(), AppError> {
        let mut snapshot = self
            .shared
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
        self.guard(generation)?;
        let current = snapshot
            .session
            .as_mut()
            .ok_or_else(|| AppError::invalid_transition("native session disappeared"))?;
        if current.session_id != session
            || current.phase != NativeDesktopPhase::Running
            || current.last_operation != Some(operation)
            || current.frame_id != Some(frame)
            || current.actions >= current.max_actions
        {
            return Err(AppError::invalid_transition(
                "native proposal context changed",
            ));
        }
        current.frame_id = None;
        current.actions += 1;
        bump(&mut snapshot)
    }

    fn reject_before_input(
        &self,
        state: &mut State<S>,
        instance: Uuid,
        session: Uuid,
        proposal: Uuid,
        operation: Uuid,
    ) -> Result<NativeDesktopReply, AppError> {
        state.frame = None;
        self.finish_vision_operation(operation, NativeVisionOperationState::Rejected)?;
        self.finish_vision_snapshot(session, operation, NativeVisionOperationState::Rejected)?;
        self.get_vision_operation(instance, session, proposal, operation)
    }

    fn finish_vision_snapshot(
        &self,
        session: Uuid,
        operation: Uuid,
        status: NativeVisionOperationState,
    ) -> Result<(), AppError> {
        let mut snapshot = self
            .shared
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
        let Some(current) = snapshot.session.as_mut().filter(|current| {
            current.session_id == session && current.last_operation == Some(operation)
        }) else {
            return Ok(());
        };
        let frame_removed = current.frame_id.take().is_some();
        let update = current.phase == NativeDesktopPhase::Running;
        if update {
            current.last_outcome = Some(
                match status {
                    NativeVisionOperationState::Completed => "completed",
                    NativeVisionOperationState::Rejected => "vision proposal was not dispatched",
                    _ => "vision action outcome unknown; query status",
                }
                .into(),
            );
        }
        if frame_removed || update {
            bump(&mut snapshot)?;
        }
        Ok(())
    }
}
