//! Service-owned visual proposals and independently queryable operation states.

use super::activity::{bump, validate_cursor};
use super::*;
use ai_ex_domain::{
    NativeDesktopAction, NativeDesktopPhase, NativeVisionOperation, NativeVisionOperationState,
    NativeVisionPreparation, NativeVisionPreparationState, NativeVisionProposal,
};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub(super) const PROPOSAL_TTL: Duration = Duration::from_secs(120);
const MAX_OPERATIONS: usize = 4096;

pub(super) struct Proposal {
    pub id: Uuid,
    pub session: Uuid,
    pub revision: u64,
    pub generation: u64,
    pub ticket: WindowTicket,
    pub goal: String,
    pub captured: Instant,
    pub digest: [u8; 32],
    pub action: NativeDesktopAction,
}

struct RecordedOperation {
    session: Uuid,
    request: NativeDesktopRequest,
    result: NativeVisionOperation,
}

pub(super) enum Confirmation {
    Existing,
    Started {
        proposal: Box<Proposal>,
        scope: String,
    },
}

struct Preparation {
    session: Uuid,
    generation: u64,
    state: NativeVisionPreparationState,
}

#[derive(Default)]
pub(super) struct VisionRegistry {
    proposal: Option<Proposal>,
    preparations: BTreeMap<Uuid, Preparation>,
    operations: BTreeMap<Uuid, RecordedOperation>,
}

pub(super) fn frame_digest(frame: &ScreenFrame) -> [u8; 32] {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    context.update(&frame.width.to_le_bytes());
    context.update(&frame.height.to_le_bytes());
    context.update(&frame.rgba);
    let mut result = [0; 32];
    result.copy_from_slice(context.finish().as_ref());
    result
}

impl<S: DesktopSystem + Send + 'static> NativeHost<S> {
    #[cfg(test)]
    pub(super) fn expire_vision_proposal_for_test(&self, id: Uuid) {
        let mut registry = self.proposals.lock().unwrap();
        let proposal = registry
            .proposal
            .as_mut()
            .filter(|proposal| proposal.id == id)
            .unwrap();
        proposal.captured = Instant::now() - PROPOSAL_TTL;
    }

    pub(super) fn clear_vision_proposal(&self) -> Result<(), AppError> {
        let mut registry = self
            .proposals
            .lock()
            .map_err(|_| AppError::unavailable("vision registry unavailable"))?;
        if let Some(proposal) = registry.proposal.take()
            && let Some(preparation) = registry.preparations.get_mut(&proposal.id)
        {
            preparation.state = NativeVisionPreparationState::Unavailable;
        }
        Ok(())
    }

    pub(super) fn begin_vision_preparation(
        &self,
        session: Uuid,
        proposal_id: Uuid,
        generation: u64,
    ) -> Result<(), AppError> {
        let mut registry = self
            .proposals
            .lock()
            .map_err(|_| AppError::unavailable("vision registry unavailable"))?;
        if registry.preparations.contains_key(&proposal_id)
            || registry.operations.contains_key(&proposal_id)
        {
            return Err(AppError::invalid_transition(
                "native operation id was already used",
            ));
        }
        if registry.preparations.len() + registry.operations.len() >= MAX_OPERATIONS {
            return Err(AppError::unavailable("vision operation capacity exhausted"));
        }
        if let Some(old) = registry.proposal.take()
            && let Some(preparation) = registry.preparations.get_mut(&old.id)
        {
            preparation.state = NativeVisionPreparationState::Unavailable;
        }
        registry.preparations.insert(
            proposal_id,
            Preparation {
                session,
                generation,
                state: NativeVisionPreparationState::Preparing,
            },
        );
        Ok(())
    }

    pub(super) fn fail_vision_preparation(&self, proposal_id: Uuid) {
        if let Ok(mut registry) = self.proposals.lock()
            && let Some(preparation) = registry.preparations.get_mut(&proposal_id)
        {
            preparation.state = NativeVisionPreparationState::Unavailable;
        }
    }

    pub(super) fn publish_vision_proposal(
        &self,
        proposal: Proposal,
    ) -> Result<NativeVisionProposal, AppError> {
        let value = public_proposal(&proposal);
        let mut registry = self
            .proposals
            .lock()
            .map_err(|_| AppError::unavailable("vision registry unavailable"))?;
        let preparation = registry
            .preparations
            .get_mut(&proposal.id)
            .filter(|preparation| {
                preparation.session == proposal.session
                    && preparation.generation == proposal.generation
                    && preparation.state == NativeVisionPreparationState::Preparing
            })
            .ok_or_else(|| AppError::invalid_transition("vision preparation expired"))?;
        preparation.state = NativeVisionPreparationState::Ready;
        registry.proposal = Some(proposal);
        Ok(value)
    }

    pub(super) fn get_vision_proposal(
        &self,
        instance: Uuid,
        session: Uuid,
        proposal_id: Uuid,
    ) -> Result<NativeDesktopReply, AppError> {
        let snapshot = self
            .shared
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
        if snapshot.instance_id != instance
            || instance.is_nil()
            || session.is_nil()
            || proposal_id.is_nil()
        {
            return Err(AppError::invalid_transition(
                "native proposal context changed",
            ));
        }
        let mut registry = self
            .proposals
            .lock()
            .map_err(|_| AppError::unavailable("vision registry unavailable"))?;
        if registry.proposal.as_ref().is_some_and(|proposal| {
            !proposal_live(
                proposal,
                &snapshot,
                self.shared.activity.load(Ordering::Acquire),
            )
        }) && let Some(expired) = registry.proposal.take()
            && let Some(preparation) = registry.preparations.get_mut(&expired.id)
        {
            preparation.state = NativeVisionPreparationState::Unavailable;
        }
        let proposal = registry
            .proposal
            .as_ref()
            .filter(|proposal| proposal.id == proposal_id && proposal.session == session)
            .map(public_proposal);
        let generation = self.shared.activity.load(Ordering::Acquire);
        let preparation = registry
            .preparations
            .get_mut(&proposal_id)
            .filter(|preparation| preparation.session == session)
            .map(|preparation| {
                if preparation.state == NativeVisionPreparationState::Preparing
                    && (preparation.generation != generation
                        || !snapshot.session.as_ref().is_some_and(|current| {
                            current.session_id == session
                                && current.phase == NativeDesktopPhase::Running
                        }))
                {
                    preparation.state = NativeVisionPreparationState::Unavailable;
                }
                NativeVisionPreparation {
                    proposal_id,
                    state: preparation.state,
                }
            })
            .unwrap_or(NativeVisionPreparation {
                proposal_id,
                state: NativeVisionPreparationState::Unknown,
            });
        Ok(NativeDesktopReply {
            snapshot: snapshot.clone(),
            windows: Vec::new(),
            vision_preview: None,
            vision_proposal: proposal,
            vision_preparation: Some(preparation),
            vision_operation: None,
        })
    }

    pub(super) fn get_vision_operation(
        &self,
        instance: Uuid,
        session: Uuid,
        proposal_id: Uuid,
        operation_id: Uuid,
    ) -> Result<NativeDesktopReply, AppError> {
        let snapshot = self.snapshot()?;
        if snapshot.instance_id != instance
            || instance.is_nil()
            || session.is_nil()
            || proposal_id.is_nil()
            || operation_id.is_nil()
        {
            return Err(AppError::invalid_transition(
                "native operation context changed",
            ));
        }
        let registry = self
            .proposals
            .lock()
            .map_err(|_| AppError::unavailable("vision registry unavailable"))?;
        let result = registry
            .operations
            .get(&operation_id)
            .filter(|record| record.session == session && record.result.proposal_id == proposal_id)
            .map(|record| record.result.clone())
            .unwrap_or(NativeVisionOperation {
                operation_id,
                proposal_id,
                state: NativeVisionOperationState::Unknown,
            });
        Ok(NativeDesktopReply {
            snapshot,
            windows: Vec::new(),
            vision_preview: None,
            vision_proposal: None,
            vision_preparation: None,
            vision_operation: Some(result),
        })
    }

    pub(super) fn finish_vision_operation(
        &self,
        operation: Uuid,
        status: NativeVisionOperationState,
    ) -> Result<(), AppError> {
        let mut registry = self
            .proposals
            .lock()
            .map_err(|_| AppError::unavailable("vision registry unavailable"))?;
        let record = registry
            .operations
            .get_mut(&operation)
            .ok_or_else(|| AppError::unavailable("vision operation unavailable"))?;
        record.result.state = status;
        Ok(())
    }

    pub(super) fn begin_vision_confirmation(
        &self,
        state: &mut State<S>,
        request: &NativeDesktopRequest,
    ) -> Result<Confirmation, AppError> {
        let NativeDesktopRequest::ConfirmVisionProposal {
            instance_id,
            expected_revision,
            session_id,
            proposal_id,
            operation_id,
        } = request
        else {
            return Err(AppError::protocol("vision confirmation intent required"));
        };
        if instance_id.is_nil()
            || session_id.is_nil()
            || proposal_id.is_nil()
            || operation_id.is_nil()
            || proposal_id == operation_id
            || self.snapshot()?.instance_id != *instance_id
        {
            return Err(AppError::configuration(
                "invalid vision confirmation intent",
            ));
        }
        {
            let registry = self
                .proposals
                .lock()
                .map_err(|_| AppError::unavailable("vision registry unavailable"))?;
            if let Some(record) = registry.operations.get(operation_id) {
                return if record.session == *session_id && record.request == *request {
                    Ok(Confirmation::Existing)
                } else {
                    Err(AppError::invalid_transition(
                        "native operation id was already used",
                    ))
                };
            }
        }
        let generation = self.check_running(*instance_id, *expected_revision, *session_id)?;
        let mut snapshot = self
            .shared
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
        validate_cursor(
            &snapshot,
            *instance_id,
            *expected_revision,
            Some(*session_id),
            false,
        )?;
        self.guard(generation)?;
        let current = snapshot.session.as_ref().expect("validated session");
        if current.frames >= current.max_frames
            || current.actions >= current.max_actions
            || state.seen.len() >= MAX_OPERATIONS
            || snapshot.revision == u64::MAX
        {
            return Err(AppError::unavailable("native proposal budget exhausted"));
        }
        let scope = current.window.scope.clone();
        let mut registry = self
            .proposals
            .lock()
            .map_err(|_| AppError::unavailable("vision registry unavailable"))?;
        if registry.operations.contains_key(operation_id)
            || registry.preparations.contains_key(operation_id)
            || state.seen.contains_key(operation_id)
        {
            return Err(AppError::invalid_transition(
                "native operation id was already used",
            ));
        }
        if registry.operations.len() + registry.preparations.len() >= MAX_OPERATIONS {
            return Err(AppError::unavailable("vision operation capacity exhausted"));
        }
        if registry.proposal.as_ref().is_none_or(|proposal| {
            proposal.id != *proposal_id
                || proposal.session != *session_id
                || !proposal_live(proposal, &snapshot, generation)
        }) {
            return Err(AppError::invalid_transition(
                "vision proposal is unavailable or expired",
            ));
        }
        let proposal = registry.proposal.take().expect("proposal checked");
        if let Some(preparation) = registry.preparations.get_mut(proposal_id) {
            preparation.state = NativeVisionPreparationState::Unavailable;
        }
        let result = NativeVisionOperation {
            operation_id: *operation_id,
            proposal_id: *proposal_id,
            state: NativeVisionOperationState::Pending,
        };
        registry.operations.insert(
            *operation_id,
            RecordedOperation {
                session: *session_id,
                request: request.clone(),
                result,
            },
        );
        state.seen.insert(*operation_id, request.clone());
        let current = snapshot.session.as_mut().expect("validated session");
        current.last_operation = Some(*operation_id);
        current.last_outcome = Some("in_flight".into());
        bump(&mut snapshot)?;
        Ok(Confirmation::Started {
            proposal: Box::new(proposal),
            scope,
        })
    }

    pub(super) fn discard_vision_proposal(
        &self,
        request: &NativeDesktopRequest,
    ) -> Result<NativeDesktopReply, AppError> {
        let NativeDesktopRequest::DiscardVisionProposal {
            instance_id,
            expected_revision,
            session_id,
            proposal_id,
        } = request
        else {
            return Err(AppError::protocol("vision discard intent required"));
        };
        let mut snapshot = self
            .shared
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
        validate_cursor(
            &snapshot,
            *instance_id,
            *expected_revision,
            Some(*session_id),
            false,
        )?;
        let mut registry = self
            .proposals
            .lock()
            .map_err(|_| AppError::unavailable("vision registry unavailable"))?;
        if registry
            .proposal
            .as_ref()
            .is_none_or(|proposal| proposal.id != *proposal_id || proposal.session != *session_id)
        {
            return Err(AppError::invalid_transition(
                "vision proposal is unavailable",
            ));
        }
        registry.proposal = None;
        if let Some(preparation) = registry.preparations.get_mut(proposal_id) {
            preparation.state = NativeVisionPreparationState::Unavailable;
        }
        bump(&mut snapshot)?;
        Ok(NativeDesktopReply {
            snapshot: snapshot.clone(),
            windows: Vec::new(),
            vision_preview: None,
            vision_proposal: None,
            vision_operation: None,
            vision_preparation: Some(NativeVisionPreparation {
                proposal_id: *proposal_id,
                state: NativeVisionPreparationState::Unavailable,
            }),
        })
    }
}

fn public_proposal(proposal: &Proposal) -> NativeVisionProposal {
    NativeVisionProposal {
        proposal_id: proposal.id,
        action: proposal.action.clone(),
        remaining_ms: PROPOSAL_TTL
            .saturating_sub(proposal.captured.elapsed())
            .as_millis() as u64,
    }
}

fn proposal_live(proposal: &Proposal, snapshot: &NativeDesktopSnapshot, generation: u64) -> bool {
    proposal.captured.elapsed() < PROPOSAL_TTL
        && proposal.revision == snapshot.revision
        && proposal.generation == generation
        && snapshot.session.as_ref().is_some_and(|session| {
            session.session_id == proposal.session
                && session.phase == NativeDesktopPhase::Running
                && session.geometry_revision == proposal.ticket.geometry_revision
                && session.goal == proposal.goal
                && session.armed_action.is_none()
        })
}
