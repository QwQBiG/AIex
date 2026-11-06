//! Recent, content-free timing, fenced against the observed conversation state.

use ai_ex_domain::{ConversationState, SystemEvent};
use ai_ex_observability::{RuntimeSnapshot, RuntimeTiming, TurnTiming};

#[derive(Default)]
pub(crate) struct TimingView {
    sample: Option<RuntimeTiming>,
    barrier: u64,
}

impl TimingView {
    pub(crate) fn clear(&mut self) {
        self.sample = None;
        self.barrier = 0;
    }

    pub(crate) fn unavailable(&mut self) {
        self.sample = None;
    }

    pub(crate) fn observe(&mut self, sequence: u64, event: &SystemEvent) {
        if matches!(
            event,
            SystemEvent::TurnStarted { .. }
                | SystemEvent::ProactiveStarted { .. }
                | SystemEvent::SpeechPreviewStarted { .. }
                | SystemEvent::SpeechCancelled
                | SystemEvent::TurnInterrupted { .. }
                | SystemEvent::Fault { .. }
                | SystemEvent::StateChanged {
                    to: ConversationState::Stopped | ConversationState::Failed,
                    ..
                }
        ) {
            self.barrier = self.barrier.max(sequence);
            if self
                .sample
                .as_ref()
                .is_some_and(|sample| sample.sequence < self.barrier)
            {
                self.sample = None;
            }
        }
    }

    pub(crate) fn accept(&mut self, sample: RuntimeTiming, instance: Option<uuid::Uuid>) {
        if instance != Some(sample.instance_id) {
            self.unavailable();
            return;
        }
        if sample.sequence < self.barrier {
            return;
        }
        if self
            .sample
            .as_ref()
            .is_some_and(|old| old.sequence > sample.sequence)
        {
            return;
        }
        self.sample = Some(sample);
    }

    pub(crate) fn visible<'a>(
        &'a self,
        runtime: &RuntimeSnapshot,
        connected: bool,
        needs_resync: bool,
    ) -> Option<&'a TurnTiming> {
        let sample = self.sample.as_ref()?;
        let turn = sample.turn.as_ref()?;
        if !connected
            || needs_resync
            || runtime.instance_id != Some(sample.instance_id)
            || sample.sequence > runtime.last_sequence
            || sample.sequence < self.barrier
            || runtime.speech_cancelled
            || runtime.speech_turn != Some(turn.turn_id)
            || matches!(
                runtime.state,
                ConversationState::Stopped | ConversationState::Failed
            )
        {
            return None;
        }
        Some(turn)
    }
}

#[cfg(test)]
#[path = "runtime_timing_tests.rs"]
mod tests;
