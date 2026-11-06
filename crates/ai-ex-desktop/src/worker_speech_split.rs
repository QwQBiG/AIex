use super::*;

#[cfg(test)]
#[path = "worker_speech_split_tests.rs"]
mod tests;

pub(super) async fn execute(
    client: &ControlClient,
    mut input: tokio::sync::mpsc::Receiver<WorkerCommand>,
    events: &Sender<WorkerEvent>,
) {
    while let Some(command) = input.recv().await {
        let WorkerCommand::SpeechSplit {
            request_id,
            request,
        } = command
        else {
            continue;
        };
        let command = match request {
            SpeechSplitRequest::Query => ControlCommand::SpeechSplit,
            SpeechSplitRequest::Set {
                instance_id,
                expected_revision,
                minimum,
            }
            | SpeechSplitRequest::Fence {
                instance_id,
                expected_revision,
                minimum,
            } => ControlCommand::SetSpeechSplit {
                instance_id,
                expected_revision,
                minimum,
            },
        };
        let result = match client.send(command).await {
            Ok(ControlPayload::SpeechSplit {
                instance_id,
                revision,
                minimum,
            }) => Ok(SpeechSplitStatus {
                instance_id,
                revision,
                minimum,
            }),
            Ok(_) => Err(AppError::protocol("unexpected speech split reply")),
            Err(error) => Err(error),
        };
        if !emit(events, WorkerEvent::SpeechSplit { request_id, result }) {
            return;
        }
    }
}
