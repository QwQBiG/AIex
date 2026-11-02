use std::sync::atomic::Ordering;
use std::time::Duration;

use ai_ex_domain::AppError;
use ai_ex_inference::InferenceWorker;
use tokio::sync::{mpsc, watch};

use crate::bundle::PreparedWorker;
use crate::pool::{Call, CompletedCall, NativeWorkerState};

pub(crate) async fn run(
    prepared: PreparedWorker,
    mut calls: mpsc::Receiver<Call>,
    mut stop: watch::Receiver<bool>,
    state: watch::Sender<NativeWorkerState>,
    timeout: Duration,
) -> Result<(), AppError> {
    let result = run_loop(&prepared, &mut calls, &mut stop, &state, timeout).await;
    state.send_replace(match &result {
        Ok(()) => NativeWorkerState::Stopped,
        Err(error) => NativeWorkerState::Error {
            message: error.message.clone(),
        },
    });
    result
}

async fn run_loop(
    prepared: &PreparedWorker,
    calls: &mut mpsc::Receiver<Call>,
    stop: &mut watch::Receiver<bool>,
    state: &watch::Sender<NativeWorkerState>,
    timeout: Duration,
) -> Result<(), AppError> {
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        state.send_replace(NativeWorkerState::Loading);
        let mut worker =
            match InferenceWorker::spawn_cancellable(prepared.launch(), stop.clone()).await {
                Ok(worker) => worker,
                Err(_) if *stop.borrow() => return Ok(()),
                Err(error) => return Err(error),
            };
        state.send_replace(NativeWorkerState::Ready);
        let restart = loop {
            let call = tokio::select! {
                biased;
                _ = stopped(stop) => None,
                call = calls.recv() => call,
            };
            let Some(mut call) = call else {
                worker.shutdown().await?;
                return Ok(());
            };
            if call.response.is_closed() || *call.cancelled.borrow() {
                continue;
            }
            call.active.store(true, Ordering::SeqCst);
            let mut sent = false;
            let deadline = tokio::time::Instant::now() + timeout;
            let outcome = {
                let inference = async {
                    worker.start_until(call.request.clone(), deadline).await?;
                    sent = true;
                    worker.wait().await
                };
                tokio::select! {
                    biased;
                    _ = stopped(stop) => Outcome::Stopped,
                    _ = crate::pool::requests::requested(&mut call.cancelled) => Outcome::Abandoned,
                    _ = call.response.closed() => Outcome::Abandoned,
                    result = tokio::time::timeout_at(deadline, inference) => Outcome::Result(result),
                }
            };
            match outcome {
                Outcome::Result(Ok(result)) => {
                    let _delivery = call.response.send(result.map(|output| CompletedCall {
                        output,
                        files: call.files,
                    }));
                    drop(call._completion);
                    if worker.process_id().is_none() {
                        break true;
                    }
                }
                cancelled => {
                    state.send_replace(NativeWorkerState::Loading);
                    if sent {
                        worker.cancel().await?;
                    } else {
                        // A cancelled send may leave a partial JSON frame. It is
                        // never reused or followed by another protocol command.
                        worker.terminate().await?;
                    }
                    // The acknowledgement follows actual cancellation/reaping,
                    // while re-warmup may continue independently for the next turn.
                    drop(call.files);
                    drop(call._completion);
                    if matches!(cancelled, Outcome::Result(Err(_))) {
                        let _delivery = call
                            .response
                            .send(Err(AppError::unavailable("native inference timed out")));
                    }
                    if matches!(cancelled, Outcome::Stopped) {
                        worker.shutdown().await?;
                        return Ok(());
                    }
                    if worker.process_id().is_none() {
                        break true;
                    }
                    state.send_replace(NativeWorkerState::Ready);
                }
            }
        };
        if restart {
            worker.shutdown().await?;
        }
    }
}

enum Outcome {
    Stopped,
    Abandoned,
    Result(Result<Result<ai_ex_inference::InferenceOutput, AppError>, tokio::time::error::Elapsed>),
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        let stopping = *stop.borrow_and_update();
        if stopping || stop.changed().await.is_err() {
            return;
        }
    }
}
