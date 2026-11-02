#![forbid(unsafe_code)]

mod presentation;
#[cfg(test)]
mod preview_tests;
pub use presentation::{AnimationFrame, ExpressionFrame, PresentationAnimator, PresentationState};

use ai_ex_domain::{AppError, ConversationState, SystemEvent, TurnId};
use ai_ex_observability::{RuntimeSnapshot, SequencedEvent};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Streaming,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiTurn {
    pub turn_id: TurnId,
    pub user_text: String,
    pub assistant_text: String,
    pub status: TurnStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    Duplicate,
    GapDetected,
}

pub struct UiState {
    pub connection: ConnectionState,
    pub runtime: RuntimeSnapshot,
    pub turns: Vec<UiTurn>,
    pub needs_resync: bool,
    max_turns: usize,
    untracked_turn: Option<TurnId>,
}

impl UiState {
    pub fn new(max_turns: usize) -> Result<Self, AppError> {
        if max_turns == 0 {
            return Err(AppError::configuration("UI turn capacity must be positive"));
        }
        Ok(Self {
            connection: ConnectionState::Disconnected,
            runtime: RuntimeSnapshot::default(),
            turns: Vec::new(),
            needs_resync: false,
            max_turns,
            untracked_turn: None,
        })
    }

    pub fn apply_snapshot(&mut self, snapshot: RuntimeSnapshot) {
        self.untracked_turn = snapshot
            .active_turn
            .filter(|turn_id| !self.turns.iter().any(|turn| turn.turn_id == *turn_id));
        self.runtime = snapshot;
        self.needs_resync = false;
    }

    /// Re-establish runtime state after unavailable history or service restart.
    /// Retain text already visible, but never leave an old partial reply running.
    pub fn recover_snapshot(&mut self, snapshot: RuntimeSnapshot) {
        for turn in &mut self.turns {
            if turn.status == TurnStatus::Streaming {
                turn.status = TurnStatus::Interrupted;
            }
        }
        self.apply_snapshot(snapshot);
    }

    pub fn apply_event(&mut self, item: SequencedEvent) -> ApplyOutcome {
        if item.sequence <= self.runtime.last_sequence {
            return ApplyOutcome::Duplicate;
        }
        let gap = self.runtime.last_sequence != 0
            && item.sequence != self.runtime.last_sequence.saturating_add(1);
        if gap {
            self.needs_resync = true;
            return ApplyOutcome::GapDetected;
        }
        self.runtime.last_sequence = item.sequence;
        self.reduce(item.event);
        ApplyOutcome::Applied
    }

    fn reduce(&mut self, event: SystemEvent) {
        self.runtime.observe_speech_event(&event);
        match event {
            SystemEvent::ProactiveStarted { turn_id } => {
                self.runtime.active_turn = Some(turn_id);
                self.runtime.turns_started += 1;
                // Output is presented by subtitles/playback. No user message
                // or empty chat bubble is invented for a system intent.
                self.untracked_turn = Some(turn_id);
            }
            SystemEvent::TurnStarted { turn_id, user_text } => {
                self.runtime.active_turn = Some(turn_id);
                self.runtime.turns_started += 1;
                self.turns.push(UiTurn {
                    turn_id,
                    user_text,
                    assistant_text: String::new(),
                    status: TurnStatus::Streaming,
                });
                if self.turns.len() > self.max_turns {
                    self.turns.remove(0);
                }
            }
            SystemEvent::ModelChunk { turn_id, text } => {
                if let Some(turn) = self.find_turn(turn_id) {
                    turn.assistant_text.push_str(&text);
                } else if self.untracked_turn != Some(turn_id) {
                    self.needs_resync = true;
                }
            }
            SystemEvent::SentenceReady { .. } => self.runtime.sentences_ready += 1,
            SystemEvent::SpeechPlayback { .. }
            | SystemEvent::SpeechPreviewStarted { .. }
            | SystemEvent::SpeechProgress { .. }
            | SystemEvent::SpeechCancelled => {}
            SystemEvent::EmotionChanged { emotion, .. } => {
                self.runtime.current_emotion = Some(emotion);
            }
            SystemEvent::TurnFinished { turn_id, full_text } => {
                self.runtime.active_turn = None;
                self.runtime.current_emotion = None;
                self.runtime.turns_completed += 1;
                self.finish_turn(turn_id, full_text, TurnStatus::Completed);
            }
            SystemEvent::TurnInterrupted { turn_id, .. } => {
                self.runtime.active_turn = None;
                self.runtime.current_emotion = None;
                self.runtime.turns_interrupted += 1;
                self.set_status(turn_id, TurnStatus::Interrupted);
            }
            SystemEvent::StateChanged { to, .. } => self.runtime.state = to,
            SystemEvent::Fault { message } => {
                self.runtime.state = ConversationState::Failed;
                self.runtime.faults += 1;
                self.runtime.last_fault = Some(message);
                if let Some(turn) = self.turns.last_mut() {
                    turn.status = TurnStatus::Failed;
                }
            }
            SystemEvent::LiveEventReceived { .. }
            | SystemEvent::LiveResponseSuggested { .. }
            | SystemEvent::PersonaChanged { .. }
            | SystemEvent::ComponentHealthChanged { .. } => {}
        }
    }

    fn find_turn(&mut self, turn_id: TurnId) -> Option<&mut UiTurn> {
        self.turns
            .iter_mut()
            .rev()
            .find(|turn| turn.turn_id == turn_id)
    }

    fn finish_turn(&mut self, turn_id: TurnId, text: String, status: TurnStatus) {
        if let Some(turn) = self.find_turn(turn_id) {
            turn.assistant_text = text;
            turn.status = status;
        } else if self.untracked_turn != Some(turn_id) {
            self.needs_resync = true;
        }
    }

    fn set_status(&mut self, turn_id: TurnId, status: TurnStatus) {
        if let Some(turn) = self.find_turn(turn_id) {
            turn.status = status;
        } else if self.untracked_turn != Some(turn_id) {
            self.needs_resync = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(sequence: u64, event: SystemEvent) -> SequencedEvent {
        SequencedEvent { sequence, event }
    }

    #[test]
    fn proactive_output_has_a_playback_generation_without_a_user_bubble() {
        let mut ui = UiState::new(4).unwrap();
        let turn_id = TurnId::new();
        for (index, event) in [
            SystemEvent::SpeechCancelled,
            SystemEvent::ProactiveStarted { turn_id },
            SystemEvent::ModelChunk {
                turn_id,
                text: "A quiet greeting.".into(),
            },
            SystemEvent::SpeechPlayback {
                playback: ai_ex_domain::SpeechPlaybackSnapshot {
                    turn_id: Some(turn_id),
                    active: true,
                    mouth_level: 500,
                    ..Default::default()
                },
            },
            SystemEvent::TurnFinished {
                turn_id,
                full_text: "A quiet greeting.".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                ui.apply_event(item(index as u64 + 1, event)),
                ApplyOutcome::Applied
            );
        }
        assert!(ui.turns.is_empty());
        assert!(!ui.needs_resync);
        assert_eq!(ui.runtime.speech_turn, Some(turn_id));
        assert!(ui.runtime.playback.active);
        assert!(!ui.runtime.speech_cancelled);
        ui.apply_event(item(6, SystemEvent::SpeechCancelled));
        assert!(!ui.runtime.playback.active);
    }

    #[test]
    fn assembles_a_streaming_turn_and_ignores_duplicates() {
        let mut state = UiState::new(10).expect("UI state");
        let turn_id = TurnId::new();
        state.apply_event(item(
            1,
            SystemEvent::TurnStarted {
                turn_id,
                user_text: "hello".to_owned(),
            },
        ));
        let chunk = item(
            2,
            SystemEvent::ModelChunk {
                turn_id,
                text: "hi".to_owned(),
            },
        );
        assert_eq!(state.apply_event(chunk.clone()), ApplyOutcome::Applied);
        assert_eq!(state.apply_event(chunk), ApplyOutcome::Duplicate);
        state.apply_event(item(
            3,
            SystemEvent::TurnFinished {
                turn_id,
                full_text: "hi there".to_owned(),
            },
        ));

        assert_eq!(state.turns[0].assistant_text, "hi there");
        assert_eq!(state.turns[0].status, TurnStatus::Completed);
        assert_eq!(state.runtime.turns_completed, 1);
    }

    #[test]
    fn detects_a_sequence_gap() {
        let mut state = UiState::new(10).expect("UI state");
        state.apply_event(item(
            1,
            SystemEvent::StateChanged {
                from: ConversationState::Idle,
                to: ConversationState::Thinking,
            },
        ));

        assert_eq!(
            state.apply_event(item(
                3,
                SystemEvent::StateChanged {
                    from: ConversationState::Thinking,
                    to: ConversationState::Speaking,
                },
            )),
            ApplyOutcome::GapDetected,
        );
        assert!(state.needs_resync);
        assert_eq!(state.runtime.last_sequence, 1);
        assert_eq!(state.runtime.state, ConversationState::Thinking);
    }

    #[test]
    fn joining_an_existing_turn_does_not_require_missing_history_to_continue() {
        let turn_id = TurnId::new();
        let mut state = UiState::new(10).unwrap();
        state.recover_snapshot(RuntimeSnapshot {
            last_sequence: 10,
            active_turn: Some(turn_id),
            ..Default::default()
        });
        assert_eq!(
            state.apply_event(item(
                11,
                SystemEvent::ModelChunk {
                    turn_id,
                    text: "old turn".to_owned(),
                }
            )),
            ApplyOutcome::Applied
        );
        assert!(!state.needs_resync);
        assert_eq!(
            state.apply_event(item(
                12,
                SystemEvent::TurnFinished {
                    turn_id,
                    full_text: "old turn completed".to_owned(),
                }
            )),
            ApplyOutcome::Applied
        );
        assert!(!state.needs_resync);
        assert!(state.turns.is_empty());
        let next = TurnId::new();
        state.apply_event(item(
            13,
            SystemEvent::TurnStarted {
                turn_id: next,
                user_text: "a new conversation".to_owned(),
            },
        ));
        state.apply_event(item(
            14,
            SystemEvent::ModelChunk {
                turn_id: next,
                text: "new reply".to_owned(),
            },
        ));
        assert_eq!(state.turns[0].assistant_text, "new reply");
    }

    #[test]
    fn bounds_conversation_history() {
        let mut state = UiState::new(2).expect("UI state");
        let mut ids = Vec::new();
        for sequence in 1..=3 {
            let turn_id = TurnId::new();
            ids.push(turn_id);
            state.apply_event(item(
                sequence,
                SystemEvent::TurnStarted {
                    turn_id,
                    user_text: sequence.to_string(),
                },
            ));
        }

        assert_eq!(state.turns.len(), 2);
        assert_eq!(state.turns[0].turn_id, ids[1]);
    }
}
