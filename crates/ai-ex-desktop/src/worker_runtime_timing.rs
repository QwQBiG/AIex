//! Low-frequency timing queries are isolated from the conversation worker.

use std::time::Duration;

use ai_ex_control::{ControlCommand, ControlPayload, RuntimeTiming};
use tokio::sync::watch;

use super::{AppError, ControlClient, Sender, VoiceTarget, WorkerEvent, emit};

pub(super) async fn run(
    client: &ControlClient,
    events: &Sender<WorkerEvent>,
    target: watch::Receiver<VoiceTarget>,
) {
    run_with(events, target, Duration::from_secs(2), || async {
        match client.send(ControlCommand::RuntimeTiming).await? {
            ControlPayload::RuntimeTiming(timing) => Ok(timing),
            _ => Err(AppError::protocol("invalid runtime timing response")),
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
    Fut: std::future::Future<Output = Result<RuntimeTiming, AppError>>,
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
                    Ok(Ok(timing)) => WorkerEvent::RuntimeTiming {
                        generation: captured.generation,
                        timing,
                    },
                    _ => WorkerEvent::RuntimeTimingUnavailable(captured.generation),
                };
                if !emit(events, event) { return; }
            }
        }
    }
}

#[cfg(test)]
#[path = "worker_runtime_timing_tests.rs"]
mod tests;
