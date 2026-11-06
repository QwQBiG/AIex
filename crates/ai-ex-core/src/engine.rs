use std::collections::VecDeque;

use ai_ex_domain::{AppError, ConversationState, Emotion, Message, Role, SystemEvent, TurnId};
use ai_ex_text::SentenceBuffer;

#[derive(Debug)]
pub struct ConversationEngine {
    state: ConversationState,
    active_turn: Option<TurnId>,
    active_history_len: Option<usize>,
    active_persistent: bool,
    history: Vec<Message>,
    history_message_limit: usize,
    events: VecDeque<SystemEvent>,
    response: String,
    sentence: SentenceBuffer,
}

impl Default for ConversationEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ConversationEngine {
    pub fn new() -> Self {
        Self::with_history_turn_limit(12)
    }

    pub(crate) fn with_history_turn_limit(history_turn_limit: usize) -> Self {
        Self {
            state: ConversationState::Idle,
            active_turn: None,
            active_history_len: None,
            active_persistent: true,
            history: Vec::new(),
            history_message_limit: history_turn_limit.saturating_mul(2),
            events: VecDeque::new(),
            response: String::new(),
            sentence: SentenceBuffer::default(),
        }
    }

    pub fn state(&self) -> ConversationState {
        self.state
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub(crate) fn set_sentence_buffer(&mut self, sentence: SentenceBuffer) {
        self.sentence = sentence;
    }

    pub(crate) fn clear_identity_context(&mut self) {
        self.history.clear();
        self.response.clear();
        self.sentence.clear();
        self.active_history_len = None;
    }

    pub fn active_turn(&self) -> Option<TurnId> {
        self.active_turn
    }

    pub fn begin_turn(&mut self, input: impl Into<String>) -> Result<TurnId, AppError> {
        if !self.state.accepts_input() {
            return Err(AppError::invalid_transition(format!(
                "cannot accept input in {:?}",
                self.state
            )));
        }
        let input = input.into().trim().to_owned();
        if input.is_empty() {
            return Err(AppError::configuration("input must not be empty"));
        }

        let turn_id = TurnId::new();
        self.active_turn = Some(turn_id);
        self.active_history_len = Some(self.history.len());
        self.active_persistent = true;
        self.history.push(Message::new(Role::User, &input));
        self.response.clear();
        self.sentence.clear();
        self.transition(ConversationState::Thinking);
        self.events.push_back(SystemEvent::TurnStarted {
            turn_id,
            user_text: input,
        });
        Ok(turn_id)
    }

    pub(crate) fn begin_proactive(&mut self) -> Result<TurnId, AppError> {
        if !self.state.accepts_input() {
            return Err(AppError::invalid_transition(
                "proactive output requires an idle runtime",
            ));
        }
        let turn_id = TurnId::new();
        self.active_turn = Some(turn_id);
        self.active_history_len = Some(self.history.len());
        self.active_persistent = false;
        self.response.clear();
        self.sentence.clear();
        self.transition(ConversationState::Thinking);
        self.events
            .push_back(SystemEvent::ProactiveStarted { turn_id });
        Ok(turn_id)
    }

    pub fn accept_chunk(&mut self, turn_id: TurnId, chunk: &str) -> Result<(), AppError> {
        self.ensure_turn(turn_id)?;
        if chunk.is_empty() {
            return Ok(());
        }
        self.response.push_str(chunk);
        self.events.push_back(SystemEvent::ModelChunk {
            turn_id,
            text: chunk.to_owned(),
        });

        for sentence in self.sentence.push(chunk) {
            self.transition(ConversationState::Speaking);
            self.events.push_back(SystemEvent::SentenceReady {
                turn_id,
                text: sentence,
            });
        }
        Ok(())
    }

    pub fn set_emotion(&mut self, turn_id: TurnId, emotion: Emotion) -> Result<(), AppError> {
        self.ensure_turn(turn_id)?;
        self.events
            .push_back(SystemEvent::EmotionChanged { turn_id, emotion });
        Ok(())
    }

    pub(crate) fn flush_sentence(&mut self, turn_id: TurnId) -> Result<(), AppError> {
        self.ensure_turn(turn_id)?;
        if let Some(trailing) = self.sentence.finish() {
            self.events.push_back(SystemEvent::SentenceReady {
                turn_id,
                text: trailing,
            });
        }

        Ok(())
    }

    pub fn finish_turn(&mut self, turn_id: TurnId) -> Result<String, AppError> {
        self.flush_sentence(turn_id)?;
        let full_text = self.response.trim().to_owned();
        if self.active_persistent && !full_text.is_empty() {
            self.history.push(Message::new(Role::Assistant, &full_text));
        } else {
            self.rollback_active_history();
        }
        self.events.push_back(SystemEvent::TurnFinished {
            turn_id,
            full_text: full_text.clone(),
        });
        self.active_turn = None;
        self.active_history_len = None;
        self.prune_history();
        self.response.clear();
        self.sentence.clear();
        self.transition(ConversationState::Idle);
        Ok(full_text)
    }

    pub fn interrupt(&mut self, reason: impl Into<String>) -> Result<TurnId, AppError> {
        let turn_id = self
            .active_turn
            .ok_or_else(|| AppError::invalid_transition("there is no active turn"))?;
        self.events.push_back(SystemEvent::TurnInterrupted {
            turn_id,
            reason: reason.into(),
        });
        self.rollback_active_history();
        self.active_turn = None;
        self.response.clear();
        self.sentence.clear();
        self.transition(ConversationState::Interrupted);
        self.transition(ConversationState::Idle);
        Ok(turn_id)
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.events.push_back(SystemEvent::Fault {
            message: message.into(),
        });
        self.rollback_active_history();
        self.active_turn = None;
        self.response.clear();
        self.sentence.clear();
        self.transition(ConversationState::Failed);
        self.transition(ConversationState::Idle);
    }

    pub fn stop(&mut self) {
        self.rollback_active_history();
        self.active_turn = None;
        self.response.clear();
        self.sentence.clear();
        self.transition(ConversationState::Stopped);
    }

    pub fn drain_events(&mut self) -> Vec<SystemEvent> {
        self.events.drain(..).collect()
    }

    fn ensure_turn(&self, turn_id: TurnId) -> Result<(), AppError> {
        if self.active_turn != Some(turn_id) {
            return Err(AppError::invalid_transition(
                "event belongs to an inactive turn",
            ));
        }
        Ok(())
    }

    fn transition(&mut self, target: ConversationState) {
        if self.state == target {
            return;
        }
        let previous = self.state;
        self.state = target;
        self.events.push_back(SystemEvent::StateChanged {
            from: previous,
            to: target,
        });
    }

    fn rollback_active_history(&mut self) {
        if let Some(length) = self.active_history_len.take() {
            self.history.truncate(length);
        }
    }

    fn prune_history(&mut self) {
        let overflow = self
            .history
            .len()
            .saturating_sub(self.history_message_limit);
        if overflow > 0 {
            self.history.drain(..overflow);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_each_sentence_once() {
        let mut engine = ConversationEngine::new();
        let turn_id = engine.begin_turn("hello").expect("turn starts");
        engine
            .accept_chunk(turn_id, "first. second!")
            .expect("chunk accepted");
        engine.finish_turn(turn_id).expect("turn finishes");
        let sentences: Vec<_> = engine
            .drain_events()
            .into_iter()
            .filter_map(|event| match event {
                SystemEvent::SentenceReady { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(sentences, vec!["first.", "second!"]);
        assert_eq!(engine.state(), ConversationState::Idle);
    }

    #[test]
    fn rejects_overlapping_turns() {
        let mut engine = ConversationEngine::new();
        engine.begin_turn("first").expect("turn starts");
        assert!(engine.begin_turn("second").is_err());
    }

    #[test]
    fn stop_clears_active_state() {
        let mut engine = ConversationEngine::new();
        engine.begin_turn("hello").expect("turn starts");

        engine.stop();

        assert_eq!(engine.state(), ConversationState::Stopped);
        assert_eq!(engine.active_turn(), None);
        assert!(engine.history().is_empty());
    }

    #[test]
    fn interrupted_turn_is_not_kept_as_model_context() {
        let mut engine = ConversationEngine::new();
        engine.begin_turn("discard me").expect("turn starts");

        engine.interrupt("barge in").expect("turn interrupts");

        assert!(engine.history().is_empty());
    }

    #[test]
    fn bounds_history_by_complete_turns() {
        let mut engine = ConversationEngine::with_history_turn_limit(2);
        for index in 0..3 {
            let turn_id = engine
                .begin_turn(format!("user-{index}"))
                .expect("turn starts");
            engine
                .accept_chunk(turn_id, &format!("assistant-{index}"))
                .expect("chunk accepted");
            engine.finish_turn(turn_id).expect("turn finishes");
        }

        assert_eq!(engine.history().len(), 4);
        assert_eq!(engine.history()[0].content, "user-1");
        assert_eq!(engine.history()[3].content, "assistant-2");
    }

    #[test]
    fn model_failure_rolls_back_and_allows_retry() {
        let mut engine = ConversationEngine::new();
        engine.begin_turn("first").expect("turn starts");
        engine.fail("model unavailable");

        assert_eq!(engine.state(), ConversationState::Idle);
        assert!(engine.history().is_empty());
        assert!(engine.begin_turn("retry").is_ok());
    }
}
