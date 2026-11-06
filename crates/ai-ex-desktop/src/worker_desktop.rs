use super::*;
use ai_ex_domain::{DesktopRuntimeRequest, NativeDesktopRequest};
#[cfg(test)]
#[path = "worker_desktop/tests.rs"]
mod tests;

pub(super) struct Queued {
    pub(super) command: WorkerCommand,
    pub(super) epoch: u64,
}

pub(super) async fn run(
    client: &ControlClient,
    mut input: tokio::sync::mpsc::Receiver<Queued>,
    events: &Sender<WorkerEvent>,
    epoch: &AtomicU64,
) {
    let (settings, settings_rx) = tokio::sync::mpsc::channel(8);
    let (queries, queries_rx) = tokio::sync::mpsc::channel(8);
    let (state, state_rx) = tokio::sync::mpsc::channel(2);
    let (native, native_rx) = tokio::sync::mpsc::channel(8);
    let (status, status_rx) = tokio::sync::mpsc::channel(2);
    let (pause, pause_rx) = tokio::sync::mpsc::channel(1);
    let (stop, stop_rx) = tokio::sync::mpsc::channel(1);
    let route = async {
        while let Some(queued) = input.recv().await {
            let lane = match &queued.command {
                WorkerCommand::DesktopRuntime { request, .. } => match request {
                    DesktopRuntimeRequest::State => &state,
                    DesktopRuntimeRequest::Apply { .. } | DesktopRuntimeRequest::Reload { .. } => {
                        &settings
                    }
                    _ => &queries,
                },
                WorkerCommand::NativeDesktop { request, .. } => match request {
                    NativeDesktopRequest::Status | NativeDesktopRequest::Windows => &status,
                    NativeDesktopRequest::Pause { .. } => &pause,
                    NativeDesktopRequest::Stop { .. } => &stop,
                    _ => &native,
                },
                WorkerCommand::NativeDesktopPreview { .. } => &native,
                _ => continue,
            };
            if let Err(error) = lane.try_send(queued)
                && !reject_command(
                    events,
                    error.into_inner().command,
                    "desktop command lane full or stopped",
                )
            {
                return;
            }
        }
    };
    tokio::select! {
        _ = route => {},
        _ = async { tokio::join!(execute(client, settings_rx, events, Some(epoch)), execute(client, queries_rx, events, None),
            execute(client, state_rx, events, None), execute(client, native_rx, events, Some(epoch)), execute(client, status_rx, events, None),
            execute(client, pause_rx, events, None), execute(client, stop_rx, events, None)); } => {},
    }
}

async fn execute(
    client: &ControlClient,
    mut input: tokio::sync::mpsc::Receiver<Queued>,
    events: &Sender<WorkerEvent>,
    epoch: Option<&AtomicU64>,
) {
    while let Some(queued) = input.recv().await {
        let command = queued.command;
        if matches!(
            &command,
            WorkerCommand::NativeDesktop {
                request: NativeDesktopRequest::PreviewVision { .. },
                ..
            }
        ) {
            if !reject_command(events, command, "预览必须通过受控通道发送。") {
                return;
            }
            continue;
        }
        if epoch.is_some_and(|epoch| {
            let current = epoch.load(Ordering::Acquire);
            current == u64::MAX || current != queued.epoch
        }) {
            if !reject_command(
                events,
                command,
                "queued desktop command revoked by pause or stop",
            ) {
                return;
            }
            continue;
        }
        if let WorkerCommand::NativeDesktopPreview {
            request,
            generation,
            guard,
            send_before,
            ..
        } = &command
            && (!matches!(request, NativeDesktopRequest::PreviewVision { .. })
                || *generation == u64::MAX
                || guard.load(Ordering::Acquire) != *generation
                || std::time::Instant::now() >= *send_before)
        {
            if !reject_command(events, command, "预览请求已取消或等待过久，请重新准备。")
            {
                return;
            }
            continue;
        }
        let event = match command {
            WorkerCommand::DesktopRuntime {
                request_id,
                request,
            } => WorkerEvent::DesktopRuntime {
                request_id,
                result: client.desktop_runtime(request).await,
            },
            WorkerCommand::NativeDesktop {
                request_id,
                request,
            }
            | WorkerCommand::NativeDesktopPreview {
                request_id,
                request,
                ..
            } => {
                let result = match client.send(ControlCommand::NativeDesktop { request }).await {
                    Ok(ControlPayload::NativeDesktop(reply)) => Ok(*reply),
                    Ok(_) => Err(AppError::protocol("unexpected native desktop reply")),
                    Err(error) => Err(error),
                };
                WorkerEvent::NativeDesktop { request_id, result }
            }
            _ => continue,
        };
        if !emit(events, event) {
            return;
        }
    }
}
