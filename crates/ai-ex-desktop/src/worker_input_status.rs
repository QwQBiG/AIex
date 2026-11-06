//! Low-frequency capture availability polling, independent of chat and setup.

use std::time::Duration;

use ai_ex_domain::{NativeInputStatus, NativeReply, NativeRequest};
use tokio::sync::watch;
use uuid::Uuid;

use super::{AppError, ControlClient, Sender, VoiceTarget, WorkerEvent, emit, fetch_snapshot};

pub(super) async fn run(
    client: &ControlClient,
    events: &Sender<WorkerEvent>,
    target: watch::Receiver<VoiceTarget>,
) {
    run_with(events, target, Duration::from_secs(2), || async {
        let instance_id = fetch_snapshot(client)
            .await?
            .instance_id
            .ok_or_else(|| AppError::unavailable("service instance is unavailable"))?;
        match client
            .native(NativeRequest::InputStatus { instance_id })
            .await?
        {
            NativeReply::InputStatus {
                instance_id: returned,
                status,
            } if returned == instance_id => Ok((instance_id, status)),
            _ => Err(AppError::protocol("input status instance mismatch")),
        }
    })
    .await;
}

async fn run_with<F, Fut>(
    events: &Sender<WorkerEvent>,
    mut target: watch::Receiver<VoiceTarget>,
    period: Duration,
    mut fetch: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(Uuid, NativeInputStatus), AppError>>,
{
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = target.changed() => if changed.is_err() { return; },
            _ = interval.tick() => {
                let captured = *target.borrow_and_update();
                if !captured.connected { continue; }
                let result = tokio::select! {
                    changed = target.changed() => {
                        if changed.is_err() { return; }
                        continue;
                    }
                    result = tokio::time::timeout(Duration::from_secs(2), fetch()) => result,
                };
                if *target.borrow() != captured { continue; }
                let event = match result {
                    Ok(Ok((instance_id, status))) => WorkerEvent::NativeInputStatus {
                        generation: captured.generation,
                        instance_id,
                        status,
                    },
                    _ => WorkerEvent::NativeInputStatusUnavailable(captured.generation),
                };
                if !emit(events, event) { return; }
            }
        }
    }
}

#[cfg(test)]
#[path = "worker_input_status_tests.rs"]
mod tests;
