use std::collections::VecDeque;

use ai_ex_domain::{AppError, MemoryRequest, MemoryResponse};
use tokio::sync::{mpsc, oneshot};

use crate::runtime::TurnInput;
use crate::{
    AvatarPort, EventSink, LanguageModelPort, MemoryPort, Runtime, RuntimeControl, SpeechPort,
    TurnOutcome,
};

#[path = "actor_memory.rs"]
mod memory_management;

enum RuntimeCommand {
    Memory {
        request: MemoryRequest,
        response: oneshot::Sender<Result<MemoryResponse, AppError>>,
    },
    Submit {
        input: TurnInput,
        response: oneshot::Sender<Result<TurnOutcome, AppError>>,
        admission: Option<oneshot::Sender<Result<(), AppError>>>,
    },
    Interrupt {
        reason: String,
        response: oneshot::Sender<Result<(), AppError>>,
    },
    CancelProactive {
        response: oneshot::Sender<Result<(), AppError>>,
    },
    SetSystemPrompt {
        prompt: String,
        response: oneshot::Sender<Result<(), AppError>>,
    },
    Shutdown {
        response: oneshot::Sender<Result<(), AppError>>,
    },
    SetPersona {
        profile_id: String,
        prompt: String,
        response: oneshot::Sender<Result<(), AppError>>,
    },
}

#[derive(Clone)]
pub struct RuntimeHandle {
    sender: mpsc::Sender<RuntimeCommand>,
}

/// An owned completion. User enqueue acknowledges admission before returning;
/// a proactive intent can instead be rejected when its idle check is processed.
pub struct PendingTurn(oneshot::Receiver<Result<TurnOutcome, AppError>>);

impl PendingTurn {
    pub async fn wait(self) -> Result<TurnOutcome, AppError> {
        self.0
            .await
            .map_err(|_| AppError::unavailable("runtime response dropped"))?
    }
}

impl RuntimeHandle {
    pub async fn memory(&self, request: MemoryRequest) -> Result<MemoryResponse, AppError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(RuntimeCommand::Memory { request, response })
            .await
            .map_err(|_| AppError::unavailable("runtime actor stopped"))?;
        receiver
            .await
            .map_err(|_| AppError::unavailable("memory management response dropped"))?
    }

    pub async fn submit(&self, input: impl Into<String>) -> Result<TurnOutcome, AppError> {
        self.enqueue(input).await?.wait().await
    }

    /// Acknowledge only after the actor has reserved a turn or queue position.
    pub async fn enqueue(&self, input: impl Into<String>) -> Result<PendingTurn, AppError> {
        let (response, receiver) = oneshot::channel();
        let (admission, admitted) = oneshot::channel();
        self.sender
            .send(RuntimeCommand::Submit {
                input: TurnInput::User(input.into()),
                response,
                admission: Some(admission),
            })
            .await
            .map_err(|_| AppError::unavailable("runtime actor stopped"))?;
        admitted
            .await
            .map_err(|_| AppError::unavailable("runtime admission dropped"))??;
        Ok(PendingTurn(receiver))
    }

    /// Atomically reserve only an idle actor. Proactive output is never queued
    /// behind user work and never enters user history or durable memory.
    pub async fn proactive(&self) -> Result<TurnOutcome, AppError> {
        self.start_proactive()?.wait().await
    }

    /// Submit the typed intent synchronously so its independent owner can
    /// observe cancellation without starting an unpolled intent afterwards.
    pub fn start_proactive(&self) -> Result<PendingTurn, AppError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .try_send(RuntimeCommand::Submit {
                input: TurnInput::Proactive,
                response,
                admission: None,
            })
            .map_err(|_| AppError::unavailable("runtime is busy or stopped"))?;
        Ok(PendingTurn(receiver))
    }

    /// Cancel system-initiated generation only. An intervening user turn is
    /// never interrupted by this command.
    pub async fn cancel_proactive(&self) -> Result<(), AppError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(RuntimeCommand::CancelProactive { response })
            .await
            .map_err(|_| AppError::unavailable("runtime actor stopped"))?;
        receiver
            .await
            .map_err(|_| AppError::unavailable("proactive cancellation response dropped"))?
    }

    pub async fn interrupt(&self, reason: impl Into<String>) -> Result<(), AppError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(RuntimeCommand::Interrupt {
                reason: reason.into(),
                response,
            })
            .await
            .map_err(|_| AppError::unavailable("runtime actor stopped"))?;
        receiver
            .await
            .map_err(|_| AppError::unavailable("interrupt response dropped"))?
    }

    pub async fn set_system_prompt(&self, prompt: impl Into<String>) -> Result<(), AppError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(RuntimeCommand::SetSystemPrompt {
                prompt: prompt.into(),
                response,
            })
            .await
            .map_err(|_| AppError::unavailable("runtime actor stopped"))?;
        receiver
            .await
            .map_err(|_| AppError::unavailable("persona update response dropped"))?
    }

    pub async fn shutdown(&self) -> Result<(), AppError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(RuntimeCommand::Shutdown { response })
            .await
            .map_err(|_| AppError::unavailable("runtime actor stopped"))?;
        receiver
            .await
            .map_err(|_| AppError::unavailable("shutdown response dropped"))?
    }

    pub async fn set_persona(&self, profile_id: String, prompt: String) -> Result<(), AppError> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(RuntimeCommand::SetPersona {
                profile_id,
                prompt,
                response,
            })
            .await
            .map_err(|_| AppError::unavailable("runtime actor stopped"))?;
        receiver
            .await
            .map_err(|_| AppError::unavailable("persona update response dropped"))?
    }
}

pub fn spawn_runtime<M, S, A, N, E>(
    runtime: Runtime<M, S, A, N, E>,
    capacity: usize,
) -> Result<RuntimeHandle, AppError>
where
    M: LanguageModelPort + 'static,
    S: SpeechPort + 'static,
    A: AvatarPort + 'static,
    N: MemoryPort + 'static,
    E: EventSink + 'static,
{
    if capacity == 0 {
        return Err(AppError::configuration(
            "runtime actor capacity must be positive",
        ));
    }
    let (sender, receiver) = mpsc::channel(capacity);
    tokio::spawn(run_actor(runtime, receiver, capacity));
    Ok(RuntimeHandle { sender })
}

async fn run_actor<M, S, A, N, E>(
    mut runtime: Runtime<M, S, A, N, E>,
    mut receiver: mpsc::Receiver<RuntimeCommand>,
    pending_capacity: usize,
) where
    M: LanguageModelPort,
    S: SpeechPort,
    A: AvatarPort,
    N: MemoryPort,
    E: EventSink,
{
    let mut stopping = false;
    let mut pending = VecDeque::new();
    loop {
        let command = match pending.pop_front() {
            Some(command) => command,
            None => match receiver.recv().await {
                Some(command) => command,
                None => break,
            },
        };
        match command {
            RuntimeCommand::Submit {
                input,
                mut response,
                admission,
            } => {
                let proactive = matches!(&input, TurnInput::Proactive);
                if proactive && response.is_closed() {
                    continue;
                }
                if proactive && (!pending.is_empty() || !receiver.is_empty()) {
                    let _ignored = response.send(Err(AppError::invalid_transition(
                        "proactive output requires an idle runtime",
                    )));
                    continue;
                }
                if let Some(admission) = admission
                    && admission.send(Ok(())).is_err()
                {
                    continue;
                }
                let mut shutdown_response: Option<oneshot::Sender<Result<(), AppError>>> = None;
                let mut deferred_interrupt = None;
                {
                    let (control, mut control_receiver) = mpsc::channel(4);
                    let turn = async {
                        match input {
                            TurnInput::User(input) => {
                                runtime
                                    .run_turn_controlled(input, &mut control_receiver)
                                    .await
                            }
                            TurnInput::Proactive => {
                                runtime
                                    .run_proactive_controlled(&mut control_receiver)
                                    .await
                            }
                        }
                    };
                    tokio::pin!(turn);
                    let mut caller_gone = false;
                    loop {
                        tokio::select! {
                            biased;
                            _ = response.closed(), if proactive && !caller_gone => {
                                caller_gone = true;
                                deferred_interrupt = Some("proactive caller stopped".into());
                                let _ignored = signal_control(&control, RuntimeControl::Interrupt {
                                    reason: "proactive caller stopped".into(),
                                });
                            }
                            result = &mut turn =>
                            {
                                let _ignored = response.send(result);
                                break;
                            }
                            next = receiver.recv(), if !stopping =>
                            {
                                match next
                                {
                                    Some(RuntimeCommand::Interrupt { reason, response }) =>
                                    {
                                        deferred_interrupt = Some(reason.clone());
                                        let result = signal_control(&control, RuntimeControl::Interrupt { reason });
                                        let _ignored = response.send(result);
                                    }
                                    Some(RuntimeCommand::CancelProactive { response }) => {
                                        let result = if proactive {
                                            deferred_interrupt = Some("proactive output cancelled".into());
                                            signal_control(&control, RuntimeControl::Interrupt {
                                                reason: "proactive output cancelled".into(),
                                            })
                                        } else { Ok(()) };
                                        let _ignored = response.send(result);
                                    }
                                    Some(RuntimeCommand::SetSystemPrompt { response, .. })
                                    | Some(RuntimeCommand::SetPersona { response, .. }) =>
                                    {
                                        let _ignored = response.send(Err(AppError::invalid_transition(
                                            "cannot change persona during an active turn",
                                        )));
                                    }
                                    Some(RuntimeCommand::Memory { response, .. }) => {
                                        let _ignored = response.send(Err(AppError::invalid_transition(
                                            "memory management is unavailable during an active turn; interrupt or wait for completion",
                                        )));
                                    }
                                    Some(RuntimeCommand::Shutdown { response }) =>
                                    {
                                        let result = signal_control(&control, RuntimeControl::Shutdown);
                                        match result
                                        {
                                            Ok(()) =>
                                            {
                                                shutdown_response = Some(response);
                                                stopping = true;
                                            }
                                            Err(error) =>
                                            {
                                                let _ignored = response.send(Err(error));
                                            }
                                        }
                                    }
                                    Some(RuntimeCommand::Submit { input, response, admission }) =>
                                    {
                                        if matches!(&input, TurnInput::Proactive) {
                                            let _ignored = response.send(Err(AppError::invalid_transition(
                                                "proactive output is unavailable during an active turn",
                                            )));
                                            continue;
                                        }
                                        if pending.len() < pending_capacity {
                                            if admission.is_none_or(|admission| admission.send(Ok(())).is_ok()) {
                                                pending.push_back(RuntimeCommand::Submit { input, response, admission: None });
                                                if proactive {
                                                    deferred_interrupt = Some("user conversation superseded proactive output".into());
                                                    let _ignored = signal_control(&control, RuntimeControl::Interrupt {
                                                        reason: "user conversation superseded proactive output".into(),
                                                    });
                                                }
                                            }
                                        } else {
                                            let error = AppError::unavailable("conversation queue is full; wait for a pending turn");
                                            if let Some(admission) = admission {
                                                let _ignored = admission.send(Err(error.clone()));
                                            }
                                            let _ignored = response.send(Err(error));
                                        }
                                    }
                                    None =>
                                    {
                                        let _ignored = signal_control(&control, RuntimeControl::Shutdown);
                                        stopping = true;
                                    }
                                }
                            }
                        }
                    }
                }
                if stopping {
                    let result = runtime.stop().await;
                    if let Some(response) = shutdown_response.take() {
                        let _ignored = response.send(result);
                    }
                } else if let Some(reason) = deferred_interrupt {
                    // Covers an accepted command racing with the turn's final commit.
                    let _ignored = runtime.interrupt(reason).await;
                }
            }
            RuntimeCommand::Interrupt { reason, response } => {
                let _ignored = response.send(runtime.interrupt(reason).await);
            }
            RuntimeCommand::CancelProactive { response } => {
                let _ignored = response.send(runtime.cancel_proactive_output().await);
            }
            RuntimeCommand::Memory { request, response } => {
                stopping =
                    memory_management::manage(&mut runtime, &mut receiver, request, response).await;
            }
            RuntimeCommand::SetSystemPrompt { prompt, response } => {
                let _ignored = response.send(runtime.set_system_prompt(prompt));
            }
            RuntimeCommand::SetPersona {
                profile_id,
                prompt,
                response,
            } => {
                let _ignored = response.send(runtime.set_persona(profile_id, prompt).await);
            }
            RuntimeCommand::Shutdown { response } => {
                let result = runtime.stop().await;
                let _ignored = response.send(result);
                break;
            }
        }
        if stopping {
            break;
        }
    }
}

fn signal_control(
    sender: &mpsc::Sender<RuntimeControl>,
    command: RuntimeControl,
) -> Result<(), AppError> {
    match sender.try_send(command) {
        Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
        Err(mpsc::error::TrySendError::Closed(_)) => {
            Err(AppError::unavailable("turn control stopped"))
        }
    }
}
