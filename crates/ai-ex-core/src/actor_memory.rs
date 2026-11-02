use super::*;

pub(super) async fn manage<M, S, A, N, E>(
    runtime: &mut Runtime<M, S, A, N, E>,
    receiver: &mut mpsc::Receiver<RuntimeCommand>,
    request: MemoryRequest,
    response: oneshot::Sender<Result<MemoryResponse, AppError>>,
) -> bool
where
    M: LanguageModelPort,
    S: SpeechPort,
    A: AvatarPort,
    N: MemoryPort,
    E: EventSink,
{
    let mut stopping = false;
    let mut shutdown_response = None;
    let mut interrupted = None;
    {
        let (control, mut controls) = mpsc::channel(4);
        let operation = runtime.manage_memory(request, Some(&mut controls));
        tokio::pin!(operation);
        loop {
            tokio::select! {
                (result, shutdown) = &mut operation => {
                    stopping |= shutdown;
                    let _ignored = response.send(result);
                    break;
                }
                next = receiver.recv(), if !stopping => {
                    let busy = AppError::invalid_transition(
                        "memory management is in progress; wait for it to complete",
                    );
                    match next {
                        Some(RuntimeCommand::CancelProactive { response }) => {
                            let _ignored = response.send(Ok(()));
                        }
                        Some(RuntimeCommand::Interrupt { reason, response }) => {
                            interrupted = Some(reason.clone());
                            let _ignored = response.send(signal_control(
                                &control, RuntimeControl::Interrupt { reason },
                            ));
                        }
                        Some(RuntimeCommand::Shutdown { response }) => {
                            match signal_control(&control, RuntimeControl::Shutdown) {
                                Ok(()) => {
                                    stopping = true;
                                    shutdown_response = Some(response);
                                }
                                Err(error) => { let _ignored = response.send(Err(error)); }
                            }
                        }
                        Some(RuntimeCommand::Submit { response, admission, .. }) => {
                            if let Some(admission) = admission {
                                let _ignored = admission.send(Err(busy.clone()));
                            }
                            let _ignored = response.send(Err(busy));
                        }
                        Some(RuntimeCommand::Memory { response, .. }) => {
                            let _ignored = response.send(Err(busy));
                        }
                        Some(RuntimeCommand::SetSystemPrompt { response, .. })
                        | Some(RuntimeCommand::SetPersona { response, .. }) => {
                            let _ignored = response.send(Err(busy));
                        }
                        None => {
                            stopping = true;
                            let _ignored = signal_control(&control, RuntimeControl::Shutdown);
                        }
                    }
                }
            }
        }
    }
    if stopping {
        let result = runtime.stop().await;
        if let Some(response) = shutdown_response {
            let _ignored = response.send(result);
        }
    } else if let Some(reason) = interrupted {
        // Preserve an accepted interrupt racing with completion of the durable write.
        let _ignored = runtime.interrupt(reason).await;
    }
    stopping
}
