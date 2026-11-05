use ai_ex_domain::NativeRequest;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

use super::{AppError, ControlClient, Sender, WorkerEvent, emit};

pub(super) struct QueuedNative {
    request_id: u64,
    request: NativeRequest,
    epoch: u64,
}

pub(super) fn route(
    request_id: u64,
    request: NativeRequest,
    lanes: [&mpsc::Sender<QueuedNative>; 3],
    events: &Sender<WorkerEvent>,
    epoch: &AtomicU64,
) -> bool {
    if let Err(error) = request.validate() {
        return emit(
            events,
            WorkerEvent::Native {
                request_id,
                result: Err(error),
            },
        );
    }
    let index = match request {
        NativeRequest::Cancel { .. } | NativeRequest::StopPreview => 2,
        NativeRequest::State
        | NativeRequest::Packages
        | NativeRequest::Settings
        | NativeRequest::StorageSpace { .. }
        | NativeRequest::Operation { .. } => 1,
        _ => 0,
    };
    if matches!(request, NativeRequest::StopPreview) {
        let _previous = epoch.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_add(1)
        });
    }
    if let Err(error) = lanes[index].try_send(QueuedNative {
        request_id,
        request,
        epoch: epoch.load(Ordering::Acquire),
    }) {
        let queued = error.into_inner();
        return emit(
            events,
            WorkerEvent::Native {
                request_id: queued.request_id,
                result: Err(AppError::unavailable(
                    "native setup queue is full or stopped",
                )),
            },
        );
    }
    true
}

pub(super) async fn execute(
    client: &ControlClient,
    mut requests: mpsc::Receiver<QueuedNative>,
    events: &Sender<WorkerEvent>,
    epoch: &AtomicU64,
) {
    while let Some(request) = requests.recv().await {
        if matches!(request.request, NativeRequest::PreviewVoice { .. })
            && (request.epoch == u64::MAX || request.epoch != epoch.load(Ordering::Acquire))
        {
            if !emit(
                events,
                WorkerEvent::Native {
                    request_id: request.request_id,
                    result: Err(AppError::invalid_transition(
                        "preview cancelled before it was delivered",
                    )),
                },
            ) {
                return;
            }
            continue;
        }
        let result = client.native(request.request).await;
        if !emit(
            events,
            WorkerEvent::Native {
                request_id: request.request_id,
                result,
            },
        ) {
            return;
        }
    }
}
