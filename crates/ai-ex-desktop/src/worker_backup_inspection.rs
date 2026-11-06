use super::*;

#[cfg(test)]
#[path = "worker_backup_inspection_tests.rs"]
mod tests;

pub(super) async fn execute(
    client: &ControlClient,
    input: tokio::sync::mpsc::Receiver<WorkerCommand>,
    events: &Sender<WorkerEvent>,
) {
    execute_with(input, events, |command| client.send(command)).await;
}

async fn execute_with<F, Fut>(
    mut input: tokio::sync::mpsc::Receiver<WorkerCommand>,
    events: &Sender<WorkerEvent>,
    mut send: F,
) where
    F: FnMut(ControlCommand) -> Fut,
    Fut: std::future::Future<Output = Result<ControlPayload, AppError>>,
{
    while let Some(command) = input.recv().await {
        let WorkerCommand::BackupInspection {
            request_id,
            command,
            generation,
            guard,
        } = command
        else {
            continue;
        };
        let result = if generation == u64::MAX || guard.load(Ordering::Acquire) != generation {
            Err(AppError::unavailable("备份检查已失效。"))
        } else if matches!(
            command,
            ControlCommand::MemoryInspectionContext
                | ControlCommand::InspectMemoryBackup { .. }
                | ControlCommand::InspectLegacyMemoryBackup { .. }
        ) {
            send(command)
                .await
                .map_err(|_| AppError::unavailable("备份检查暂时不可用，请核对服务和文件后重试。"))
        } else {
            Err(AppError::protocol("invalid backup inspection request"))
        };
        if !emit(events, WorkerEvent::BackupInspection { request_id, result }) {
            return;
        }
    }
}
