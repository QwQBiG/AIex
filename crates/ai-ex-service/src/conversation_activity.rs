//! Authoritative conversation admissions, including queued turns and detached waits.
//! Initiated submissions continue to settle after a client disconnects. Only the
//! task owning RuntimeHandle::enqueue and PendingTurn::wait releases its activity.
use ai_ex_core::{PendingTurn, RuntimeHandle, TurnOutcome};
use ai_ex_domain::{AppError, MemoryRequest, MemoryResponse};
use tokio::sync::oneshot;

use crate::template_host::TemplateHost;

#[derive(Clone)]
pub(crate) struct ConversationActivity {
    runtime: RuntimeHandle,
    host: TemplateHost,
}

pub(crate) struct PendingSubmission {
    admission: oneshot::Receiver<Result<(), AppError>>,
    result: oneshot::Receiver<Result<TurnOutcome, AppError>>,
}

impl PendingSubmission {
    async fn admitted(self) -> Result<PendingConversation, AppError> {
        self.admission
            .await
            .map_err(|_| AppError::unavailable("conversation admission task stopped"))??;
        Ok(PendingConversation {
            result: self.result,
        })
    }

    pub(crate) async fn wait(self) -> Result<TurnOutcome, AppError> {
        self.admitted().await?.wait().await
    }
}

pub(crate) struct PendingConversation {
    result: oneshot::Receiver<Result<TurnOutcome, AppError>>,
}

impl PendingConversation {
    pub(crate) async fn wait(self) -> Result<TurnOutcome, AppError> {
        self.result
            .await
            .map_err(|_| AppError::unavailable("conversation completion task stopped"))?
    }
}

impl ConversationActivity {
    pub(crate) fn new(runtime: RuntimeHandle, host: TemplateHost) -> Self {
        Self { runtime, host }
    }

    pub(crate) fn start_submit(
        &self,
        input: impl Into<String>,
    ) -> Result<PendingSubmission, AppError> {
        let input = input.into();
        if input.trim().is_empty() {
            return Err(AppError::configuration(
                "conversation text must not be empty",
            ));
        }
        let guard = self.host.begin_chat()?;
        let runtime = self.runtime.clone();
        let (admission, admitted) = oneshot::channel();
        let (completion, result) = oneshot::channel();
        // No await between the reservation and spawning its independent owner.
        // An enqueue caller can disappear immediately after actor admission; the
        // actor still runs that queued turn, so the caller must not own the guard.
        tokio::spawn(async move {
            let _guard = guard;
            match runtime.enqueue(input).await {
                Ok(pending) => {
                    let _acknowledged = admission.send(Ok(()));
                    finish_pending(pending, completion).await;
                }
                Err(error) => {
                    let _acknowledged = admission.send(Err(error.clone()));
                    let _completed = completion.send(Err(error));
                }
            }
        });
        Ok(PendingSubmission {
            admission: admitted,
            result,
        })
    }

    pub(crate) async fn enqueue(
        &self,
        input: impl Into<String>,
    ) -> Result<PendingConversation, AppError> {
        self.start_submit(input)?.admitted().await
    }

    pub(crate) async fn submit(&self, input: impl Into<String>) -> Result<TurnOutcome, AppError> {
        self.enqueue(input).await?.wait().await
    }

    pub(crate) async fn interrupt(&self, reason: impl Into<String>) -> Result<(), AppError> {
        self.runtime.interrupt(reason).await
    }

    pub(crate) async fn shutdown(&self) -> Result<(), AppError> {
        self.runtime.shutdown().await
    }

    pub(crate) async fn memory(&self, request: MemoryRequest) -> Result<MemoryResponse, AppError> {
        self.runtime.memory(request).await
    }

    pub(crate) async fn set_persona(
        &self,
        profile_id: String,
        prompt: String,
    ) -> Result<(), AppError> {
        self.runtime.set_persona(profile_id, prompt).await
    }
}

async fn finish_pending(
    pending: PendingTurn,
    completion: oneshot::Sender<Result<TurnOutcome, AppError>>,
) {
    let outcome = pending.wait().await;
    let _delivered = completion.send(outcome);
}

#[cfg(test)]
#[path = "conversation_activity_tests.rs"]
mod tests;
