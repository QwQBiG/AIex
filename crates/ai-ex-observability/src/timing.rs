//! Transient timings measured between accepted service events.

use std::time::Instant;

use ai_ex_domain::{ConversationState, SystemEvent, TurnId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::RuntimeSnapshot;

/// A bounded, text-free observation of one conversation turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnTiming {
    pub turn_id: TurnId,
    pub first_chunk_ms: Option<u64>,
    pub first_playback_request_ms: Option<u64>,
}

/// `sequence` identifies the event prefix from which `turn` was observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeTiming {
    pub instance_id: Uuid,
    pub sequence: u64,
    pub turn: Option<TurnTiming>,
}

#[derive(Default)]
pub(super) struct TimingTracker {
    current: Option<Measurement>,
}

struct Measurement {
    started: Instant,
    timing: TurnTiming,
    text_finished: bool,
}

impl TimingTracker {
    pub(super) fn current(&self) -> Option<TurnTiming> {
        self.current.as_ref().map(|item| item.timing.clone())
    }

    /// The snapshot must already have applied this event, under event ordering.
    pub(super) fn observe(
        &mut self,
        event: &SystemEvent,
        snapshot: &RuntimeSnapshot,
        now: Instant,
    ) {
        match event {
            SystemEvent::TurnStarted { turn_id, .. }
            | SystemEvent::ProactiveStarted { turn_id } => {
                self.current = Some(Measurement {
                    started: now,
                    timing: TurnTiming {
                        turn_id: *turn_id,
                        first_chunk_ms: None,
                        first_playback_request_ms: None,
                    },
                    text_finished: false,
                });
            }
            SystemEvent::ModelChunk { turn_id, text } => {
                if let Some(item) = &mut self.current
                    && item.timing.turn_id == *turn_id
                    && !item.text_finished
                    && item.timing.first_chunk_ms.is_none()
                    && !text.is_empty()
                {
                    item.timing.first_chunk_ms = elapsed_ms(item.started, now);
                }
            }
            SystemEvent::SpeechPlayback { playback } => {
                if let Some(item) = &mut self.current
                    && playback.active
                    && playback.turn_id == Some(item.timing.turn_id)
                    && snapshot.speech_turn == Some(item.timing.turn_id)
                    && !snapshot.speech_cancelled
                    && snapshot.playback == *playback
                    && item.timing.first_playback_request_ms.is_none()
                {
                    item.timing.first_playback_request_ms = elapsed_ms(item.started, now);
                }
            }
            SystemEvent::TurnFinished { turn_id, .. } => {
                if let Some(item) = &mut self.current
                    && item.timing.turn_id == *turn_id
                {
                    // Playback can start after text generation ends.
                    item.text_finished = true;
                }
            }
            SystemEvent::SpeechCancelled
            | SystemEvent::SpeechPreviewStarted { .. }
            | SystemEvent::TurnInterrupted { .. }
            | SystemEvent::Fault { .. }
            | SystemEvent::StateChanged {
                to: ConversationState::Stopped | ConversationState::Failed,
                ..
            } => self.current = None,
            _ => {}
        }
    }
}

fn elapsed_ms(start: Instant, now: Instant) -> Option<u64> {
    now.checked_duration_since(start)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
#[path = "timing_tests.rs"]
mod tests;
