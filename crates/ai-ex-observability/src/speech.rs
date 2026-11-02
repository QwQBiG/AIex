use ai_ex_domain::{ConversationState, SystemEvent};

use crate::RuntimeSnapshot;

impl RuntimeSnapshot {
    /// Shared by the service snapshot and client event replay.
    pub fn observe_speech_event(&mut self, event: &SystemEvent) {
        match event {
            SystemEvent::TurnStarted { turn_id, .. }
            | SystemEvent::ProactiveStarted { turn_id }
            | SystemEvent::SpeechPreviewStarted { turn_id } => {
                self.speech_turn = Some(*turn_id);
                self.speech_cancelled = false;
                self.playback = Default::default();
            }
            SystemEvent::SpeechCancelled
            | SystemEvent::TurnInterrupted { .. }
            | SystemEvent::StateChanged {
                to: ConversationState::Stopped | ConversationState::Failed,
                ..
            } => {
                self.speech_cancelled = true;
                self.playback = Default::default();
                self.current_emotion = None;
            }
            SystemEvent::SpeechPlayback { playback }
                if (!playback.active
                    && (playback.sentence_id.is_none()
                        || (playback.sentence_id == self.playback.sentence_id
                            && playback.turn_id == self.playback.turn_id)))
                    || (!self.speech_cancelled
                        && playback.active
                        && (self.speech_turn.is_none()
                            || self.speech_turn == playback.turn_id)) =>
            {
                self.playback = if playback.active {
                    playback.clone()
                } else {
                    Default::default()
                };
            }
            SystemEvent::SpeechProgress {
                sentence_id,
                position_ms,
                mouth_level,
            } if !self.speech_cancelled
                && self.playback.active
                && self.playback.sentence_id == Some(*sentence_id) =>
            {
                self.playback.position_ms = (*position_ms).min(self.playback.duration_ms);
                self.playback.mouth_level = (*mouth_level).min(1000);
            }
            _ => {}
        }
    }
}
