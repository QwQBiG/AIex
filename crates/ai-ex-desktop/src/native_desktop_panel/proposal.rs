//! Explicit review and one-shot execution of a service-owned visual suggestion.

use super::*;
use ai_ex_domain::{
    NativeDesktopPhase, NativeVisionOperationState, NativeVisionPreparationState,
    NativeVisionProposal,
};
use std::sync::atomic::Ordering;

#[cfg(test)]
mod tests;
mod view;

#[derive(Clone)]
pub(super) struct BoundContext {
    instance: Uuid,
    revision: u64,
    session: Uuid,
    goal: String,
}

pub(super) struct Preparation {
    context: BoundContext,
    id: Uuid,
    pub(super) state: NativeVisionPreparationState,
}

pub(super) struct Review {
    context: BoundContext,
    pub(super) value: NativeVisionProposal,
    pub(super) expires: Instant,
}

pub(super) struct Operation {
    context: BoundContext,
    pub(super) proposal_id: Uuid,
    pub(super) id: Uuid,
    pub(super) state: NativeVisionOperationState,
    unknown_after_pending: bool,
}

enum Intent {
    Prepare {
        proposal_id: Uuid,
    },
    Confirm {
        proposal_id: Uuid,
        operation_id: Uuid,
    },
}

struct Armed {
    context: BoundContext,
    intent: Intent,
    send_at: Instant,
    expires: Option<Instant>,
}

#[derive(Default)]
pub(super) struct ProposalUi {
    guard: Arc<AtomicU64>,
    arm: Option<Armed>,
    pending: Option<(u64, u64)>,
    pub(super) preparation: Option<Preparation>,
    pub(super) review: Option<Review>,
    pub(super) operation: Option<Operation>,
    pub(super) message: Option<&'static str>,
    confirm_unknown_end: bool,
}

impl ProposalUi {
    pub(super) fn idle(&self) -> bool {
        self.arm.is_none()
            && self.pending.is_none()
            && self.preparation.is_none()
            && self.review.is_none()
            && self.operation.is_none()
    }
}

pub(super) fn is_proposal_request(request: &NativeDesktopRequest) -> bool {
    matches!(
        request,
        NativeDesktopRequest::PrepareVisionProposal { .. }
            | NativeDesktopRequest::GetVisionProposal { .. }
            | NativeDesktopRequest::DiscardVisionProposal { .. }
            | NativeDesktopRequest::ConfirmVisionProposal { .. }
            | NativeDesktopRequest::GetVisionOperation { .. }
    )
}

fn preparation_advances(
    current: NativeVisionPreparationState,
    next: NativeVisionPreparationState,
) -> bool {
    use NativeVisionPreparationState::*;
    matches!(
        (current, next),
        (Unknown, _)
            | (Preparing, Preparing | Ready | Unavailable)
            | (Ready, Ready | Unavailable)
            | (Unavailable, Unavailable)
    )
}

fn operation_advances(
    current: NativeVisionOperationState,
    next: NativeVisionOperationState,
) -> bool {
    use NativeVisionOperationState::*;
    matches!(
        (current, next),
        (Unknown, _)
            | (Pending, Pending | Completed | Rejected | Unknown)
            | (Completed, Completed)
            | (Rejected, Rejected)
    )
}

impl NativeDesktopPanel {
    fn bound_context(&self) -> Option<BoundContext> {
        let snapshot = self.confirmed.as_ref()?;
        let session = snapshot.session.as_ref()?;
        if !self.connected
            || !self.fresh
            || !snapshot.available
            || self.manual_mode
            || session.phase != NativeDesktopPhase::Running
            || session.armed_action.is_some()
            || session.goal.trim().is_empty()
            || session.goal != self.goal
        {
            return None;
        }
        Some(BoundContext {
            instance: snapshot.instance_id,
            revision: snapshot.revision,
            session: session.session_id,
            goal: session.goal.clone(),
        })
    }

    fn context_matches(&self, context: &BoundContext) -> bool {
        self.bound_context().is_some_and(|current| {
            current.instance == context.instance
                && current.revision == context.revision
                && current.session == context.session
                && current.goal == context.goal
        })
    }

    pub(super) fn cancel_proposal_context(&mut self) {
        let pending = self.proposal.pending.take().is_some();
        let mut operation_uncertain = false;
        if let Some(operation) = &mut self.proposal.operation
            && operation.state == NativeVisionOperationState::Pending
        {
            operation.state = NativeVisionOperationState::Unknown;
            operation.unknown_after_pending = true;
            operation_uncertain = true;
        }
        if self.proposal.arm.is_some() || pending {
            let _ =
                self.proposal
                    .guard
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                        value.checked_add(1)
                    });
        }
        self.proposal.arm = None;
        self.proposal.preparation = None;
        self.proposal.review = None;
        if pending || operation_uncertain {
            self.proposal.message = Some("请求已提交，可能仍在处理；不会自动重试。");
        }
    }

    pub fn proposal_operation_unresolved(&self) -> bool {
        self.proposal.operation.as_ref().is_some_and(|operation| {
            matches!(
                operation.state,
                NativeVisionOperationState::Pending | NativeVisionOperationState::Unknown
            )
        })
    }

    pub(super) fn proposal_due(&self) -> bool {
        self.proposal
            .arm
            .as_ref()
            .is_some_and(|arm| Instant::now() >= arm.send_at)
    }

    pub(super) fn expire_proposal(&mut self) {
        if self
            .proposal
            .review
            .as_ref()
            .is_some_and(|review| Instant::now() >= review.expires)
        {
            if self.proposal.arm.take().is_some() {
                let _ = self.proposal.guard.fetch_update(
                    Ordering::AcqRel,
                    Ordering::Acquire,
                    |value| value.checked_add(1),
                );
            }
            self.proposal.review = None;
            if let Some(preparation) = &mut self.proposal.preparation {
                preparation.state = NativeVisionPreparationState::Unknown;
            }
            self.proposal.message = Some("审核时间已到，请查询原编号；没有自动执行。");
        }
    }

    pub(super) fn cancel_proposal_countdown(&mut self) {
        if self.proposal.arm.take().is_some() {
            let _ =
                self.proposal
                    .guard
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                        value.checked_add(1)
                    });
            self.proposal.message = Some("倒计时已取消，未发送。");
        }
    }

    pub(super) fn observe_proposal_snapshot(&mut self, reply: &NativeDesktopReply) {
        let session = reply.snapshot.session.as_ref();
        let context_changed = |context: &BoundContext| {
            reply.snapshot.instance_id != context.instance
                || !session.is_some_and(|current| {
                    current.session_id == context.session
                        && current.goal == context.goal
                        && current.phase == NativeDesktopPhase::Running
                        && current.armed_action.is_none()
                })
        };
        let arm_changed = self.proposal.arm.as_ref().is_some_and(|arm| {
            context_changed(&arm.context) || reply.snapshot.revision != arm.context.revision
        });
        let review_changed = self.proposal.review.as_ref().is_some_and(|review| {
            context_changed(&review.context) || reply.snapshot.revision != review.context.revision
        });
        let preparation_changed = self
            .proposal
            .preparation
            .as_ref()
            .is_some_and(|preparation| context_changed(&preparation.context));
        if arm_changed || review_changed || preparation_changed {
            self.cancel_proposal_context();
        }
    }

    pub(super) fn arm_proposal_preparation(&mut self) {
        if !self.preview_available()
            || !self.proposal.idle()
            || self.preview_arm.is_some()
            || self.preview_pending.is_some()
            || self.proposal.guard.load(Ordering::Acquire) == u64::MAX
        {
            return;
        }
        let Some(context) = self.bound_context() else {
            return;
        };
        self.cancel_preview();
        self.proposal.arm = Some(Armed {
            context,
            intent: Intent::Prepare {
                proposal_id: Uuid::new_v4(),
            },
            send_at: Instant::now() + Duration::from_secs(3),
            expires: None,
        });
        self.proposal.message = Some("请在约 3 秒内切回专用窗口；此次只准备建议，不发送键鼠。");
    }

    pub(super) fn arm_proposal_confirmation(&mut self) {
        let Some(review) = &self.proposal.review else {
            return;
        };
        if self.proposal.arm.is_some()
            || self.proposal.pending.is_some()
            || self.proposal.operation.is_some()
            || !self.context_matches(&review.context)
            || self.pending.values().any(|request| lane(request) == 1)
            || review.expires <= Instant::now() + Duration::from_secs(3)
            || self.proposal.guard.load(Ordering::Acquire) == u64::MAX
        {
            return;
        }
        self.proposal.arm = Some(Armed {
            context: review.context.clone(),
            intent: Intent::Confirm {
                proposal_id: review.value.proposal_id,
                operation_id: Uuid::new_v4(),
            },
            send_at: Instant::now() + Duration::from_secs(3),
            expires: Some(review.expires),
        });
        self.proposal.message = Some("请在约 3 秒内切回专用窗口；确认后可能实际发送一次键鼠。");
    }

    pub(super) fn dispatch_proposal(&mut self) -> Option<WorkerCommand> {
        let arm = self.proposal.arm.take()?;
        let send_before = arm
            .expires
            .map_or(arm.send_at + Duration::from_secs(2), |expires| {
                expires.min(arm.send_at + Duration::from_secs(2))
            });
        if Instant::now() >= send_before
            || !self.context_matches(&arm.context)
            || self.pending.values().any(|request| lane(request) == 1)
        {
            self.proposal.message = Some("状态已变化或等待过久，操作未发送；请重新核对。");
            return None;
        }
        let context = arm.context;
        let request = match arm.intent {
            Intent::Prepare { proposal_id } => {
                self.proposal.preparation = Some(Preparation {
                    context: context.clone(),
                    id: proposal_id,
                    state: NativeVisionPreparationState::Unknown,
                });
                NativeDesktopRequest::PrepareVisionProposal {
                    instance_id: context.instance,
                    expected_revision: context.revision,
                    session_id: context.session,
                    operation_id: proposal_id,
                    goal: context.goal.clone(),
                }
            }
            Intent::Confirm {
                proposal_id,
                operation_id,
            } => {
                self.proposal.operation = Some(Operation {
                    context: context.clone(),
                    proposal_id,
                    id: operation_id,
                    state: NativeVisionOperationState::Unknown,
                    unknown_after_pending: false,
                });
                self.proposal.review = None;
                NativeDesktopRequest::ConfirmVisionProposal {
                    instance_id: context.instance,
                    expected_revision: context.revision,
                    session_id: context.session,
                    proposal_id,
                    operation_id,
                }
            }
        };
        self.sequence = self.sequence.checked_add(1)?;
        let generation = self.proposal.guard.load(Ordering::Acquire);
        self.pending.insert(self.sequence, request.clone());
        self.proposal.pending = Some((self.sequence, generation));
        self.proposal.message = Some("请求已提交，可能仍在处理；不会自动重试。");
        Some(WorkerCommand::NativeDesktopGuarded {
            request_id: self.sequence,
            request,
            generation,
            guard: Arc::clone(&self.proposal.guard),
            send_before,
        })
    }

    pub(super) fn query_preparation(&self) -> Option<NativeDesktopRequest> {
        let preparation = self.proposal.preparation.as_ref()?;
        if !self.same_connection(&preparation.context) {
            return None;
        }
        Some(NativeDesktopRequest::GetVisionProposal {
            instance_id: preparation.context.instance,
            session_id: preparation.context.session,
            proposal_id: preparation.id,
        })
    }

    pub(super) fn query_operation(&self) -> Option<NativeDesktopRequest> {
        let operation = self.proposal.operation.as_ref()?;
        if !self.connected || self.instance != Some(operation.context.instance) {
            return None;
        }
        Some(NativeDesktopRequest::GetVisionOperation {
            instance_id: operation.context.instance,
            session_id: operation.context.session,
            proposal_id: operation.proposal_id,
            operation_id: operation.id,
        })
    }

    fn same_connection(&self, context: &BoundContext) -> bool {
        self.connected
            && self.instance == Some(context.instance)
            && self
                .confirmed
                .as_ref()
                .and_then(|state| state.session.as_ref())
                .is_some_and(|session| session.session_id == context.session)
    }

    pub(super) fn discard_request(&self) -> Option<NativeDesktopRequest> {
        let review = self.proposal.review.as_ref()?;
        if !self.context_matches(&review.context) {
            return None;
        }
        Some(NativeDesktopRequest::DiscardVisionProposal {
            instance_id: review.context.instance,
            expected_revision: review.context.revision,
            session_id: review.context.session,
            proposal_id: review.value.proposal_id,
        })
    }

    pub(super) fn receive_proposal(
        &mut self,
        id: u64,
        request: NativeDesktopRequest,
        result: Result<NativeDesktopReply, AppError>,
    ) {
        let guarded = matches!(
            request,
            NativeDesktopRequest::PrepareVisionProposal { .. }
                | NativeDesktopRequest::ConfirmVisionProposal { .. }
        );
        let generation_valid = if guarded {
            self.proposal
                .pending
                .take()
                .is_some_and(|(pending, generation)| {
                    pending == id && generation == self.proposal.guard.load(Ordering::Acquire)
                })
        } else {
            true
        };
        let confirming = matches!(request, NativeDesktopRequest::ConfirmVisionProposal { .. });
        if !generation_valid && !confirming {
            return;
        }
        if let NativeDesktopRequest::ConfirmVisionProposal {
            operation_id,
            proposal_id,
            ..
        }
        | NativeDesktopRequest::GetVisionOperation {
            operation_id,
            proposal_id,
            ..
        } = &request
            && self.proposal.operation.as_ref().is_none_or(|operation| {
                operation.id != *operation_id || operation.proposal_id != *proposal_id
            })
        {
            return;
        }
        if matches!(
            request,
            NativeDesktopRequest::ConfirmVisionProposal { .. }
                | NativeDesktopRequest::GetVisionOperation { .. }
        ) && self.proposal.operation.as_ref().is_some_and(|operation| {
            matches!(
                operation.state,
                NativeVisionOperationState::Completed | NativeVisionOperationState::Rejected
            )
        }) {
            return;
        }
        if matches!(request, NativeDesktopRequest::GetVisionProposal { .. })
            && self
                .proposal
                .preparation
                .as_ref()
                .is_some_and(|preparation| {
                    matches!(
                        preparation.state,
                        NativeVisionPreparationState::Ready
                            | NativeVisionPreparationState::Unavailable
                    )
                })
            && result.is_err()
        {
            return;
        }
        let Ok(reply) = result else {
            if matches!(
                request,
                NativeDesktopRequest::ConfirmVisionProposal { .. }
                    | NativeDesktopRequest::GetVisionOperation { .. }
            ) && let Some(operation) = &mut self.proposal.operation
                && operation.state == NativeVisionOperationState::Pending
            {
                operation.state = NativeVisionOperationState::Unknown;
                operation.unknown_after_pending = true;
            }
            if matches!(request, NativeDesktopRequest::DiscardVisionProposal { .. }) {
                self.proposal.review = None;
                if let Some(preparation) = &mut self.proposal.preparation {
                    preparation.state = NativeVisionPreparationState::Unknown;
                }
            }
            self.proposal.message = Some("结果未知，请查询原编号；不会自动重试。");
            return;
        };
        if Some(reply.snapshot.instance_id) != self.instance {
            self.proposal.message = Some("服务实例变化，原操作结果未知；请核对目标。");
            return;
        }
        match &request {
            NativeDesktopRequest::PrepareVisionProposal { operation_id, .. }
            | NativeDesktopRequest::GetVisionProposal {
                proposal_id: operation_id,
                ..
            } => {
                self.accept_preparation(*operation_id, &reply);
            }
            NativeDesktopRequest::DiscardVisionProposal { proposal_id, .. } => {
                let discarded = reply.vision_preparation.as_ref().is_some_and(|status| {
                    status.proposal_id == *proposal_id
                        && status.state == NativeVisionPreparationState::Unavailable
                });
                if discarded {
                    self.proposal.preparation = None;
                    self.proposal.review = None;
                    self.proposal.message = Some("建议已丢弃，没有发送键鼠。");
                } else {
                    self.proposal.review = None;
                    if let Some(preparation) = &mut self.proposal.preparation {
                        preparation.state = NativeVisionPreparationState::Unknown;
                    }
                    self.proposal.message = Some("丢弃结果未知，请查询原编号。");
                }
            }
            NativeDesktopRequest::ConfirmVisionProposal {
                operation_id,
                proposal_id,
                ..
            }
            | NativeDesktopRequest::GetVisionOperation {
                operation_id,
                proposal_id,
                ..
            } => {
                if let Some(operation) = &mut self.proposal.operation
                    && operation.id == *operation_id
                    && operation.proposal_id == *proposal_id
                    && let Some(result) = &reply.vision_operation
                    && result.operation_id == operation.id
                    && result.proposal_id == operation.proposal_id
                {
                    if (operation.unknown_after_pending
                        && result.state == NativeVisionOperationState::Pending)
                        || !operation_advances(operation.state, result.state)
                    {
                        return;
                    }
                    if operation.state == NativeVisionOperationState::Pending
                        && result.state == NativeVisionOperationState::Unknown
                    {
                        operation.unknown_after_pending = true;
                    }
                    operation.state = result.state;
                    self.proposal.message = Some(match result.state {
                        NativeVisionOperationState::Pending => "操作正在处理；不会自动重试。",
                        NativeVisionOperationState::Completed => {
                            "服务报告操作已完成；请核对目标窗口。"
                        }
                        NativeVisionOperationState::Rejected => {
                            "服务报告操作未执行；请核对目标后重新准备。"
                        }
                        NativeVisionOperationState::Unknown => {
                            "操作结果未知；请先查询并核对目标，不能自动重试。"
                        }
                    });
                } else {
                    self.proposal.message = Some("操作结果无法匹配原编号，请核对目标。");
                }
            }
            _ => {}
        }
    }

    fn accept_preparation(&mut self, id: Uuid, reply: &NativeDesktopReply) {
        let Some(preparation) = &mut self.proposal.preparation else {
            return;
        };
        if preparation.id != id {
            return;
        }
        let Some(status) = &reply.vision_preparation else {
            preparation.state = NativeVisionPreparationState::Unknown;
            self.proposal.message = Some("建议状态未知，请查询原编号。");
            return;
        };
        if status.proposal_id != id {
            preparation.state = NativeVisionPreparationState::Unknown;
            self.proposal.message = Some("建议状态无法匹配原编号。");
            return;
        }
        if !preparation_advances(preparation.state, status.state)
            || self
                .confirmed
                .as_ref()
                .is_some_and(|current| current.revision > reply.snapshot.revision)
        {
            return;
        }
        preparation.state = status.state;
        if status.state != NativeVisionPreparationState::Ready {
            self.proposal.review = None;
            self.proposal.message = Some(match status.state {
                NativeVisionPreparationState::Preparing => "建议仍在准备；可查询原编号。",
                NativeVisionPreparationState::Unavailable => "建议不可用或已失效，没有发送键鼠。",
                NativeVisionPreparationState::Unknown => "建议状态未知，请查询原编号。",
                NativeVisionPreparationState::Ready => unreachable!(),
            });
            return;
        }
        let Some(value) = reply.vision_proposal.as_ref().filter(|value| {
            value.proposal_id == id
                && value.remaining_ms > 0
                && value.remaining_ms <= 120_000
                && value.action.validate().is_ok()
        }) else {
            preparation.state = NativeVisionPreparationState::Unknown;
            self.proposal.message = Some("建议内容无法核对，请重新查询。");
            return;
        };
        if self.proposal.review.as_ref().is_some_and(|review| {
            review.value.proposal_id == id && review.value.action != value.action
        }) {
            self.proposal.review = None;
            preparation.state = NativeVisionPreparationState::Unknown;
            self.proposal.message = Some("同一编号的建议内容不一致，已隐藏，请重新核对。");
            return;
        }
        let Some(session) = &reply.snapshot.session else {
            self.proposal.review = None;
            preparation.state = NativeVisionPreparationState::Unknown;
            self.proposal.message = Some("建议目标无法核对，请查询原编号。");
            return;
        };
        let current = self.confirmed.as_ref();
        if reply.snapshot.instance_id != preparation.context.instance
            || session.session_id != preparation.context.session
            || session.goal != preparation.context.goal
            || session.phase != NativeDesktopPhase::Running
            || session.armed_action.is_some()
            || self.manual_mode
            || self.goal != session.goal
            || current.is_some_and(|current| current.revision > reply.snapshot.revision)
        {
            self.proposal.review = None;
            preparation.state = NativeVisionPreparationState::Unknown;
            self.proposal.message = Some("建议已过期或目标变化，请重新核对。");
            return;
        }
        let context = BoundContext {
            instance: reply.snapshot.instance_id,
            revision: reply.snapshot.revision,
            session: session.session_id,
            goal: session.goal.clone(),
        };
        preparation.context = context.clone();
        self.confirmed = Some(reply.snapshot.clone());
        self.fresh = true;
        let expires = Instant::now() + Duration::from_millis(value.remaining_ms);
        let expires = self
            .proposal
            .review
            .as_ref()
            .map_or(expires, |previous| previous.expires.min(expires));
        self.proposal.review = Some(Review {
            context,
            value: value.clone(),
            expires,
        });
        self.proposal.message = Some("请完整审阅建议；尚未发送键鼠。");
    }
}
