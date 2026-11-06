use ai_ex_domain::{AppError, ConversationState, Emotion, Message, Role, SystemEvent, TurnId};
use ai_ex_text::ResponsePreamble;

#[path = "runtime_control.rs"]
mod control;
#[path = "runtime_memory.rs"]
mod memory_management;
#[path = "runtime_persistence.rs"]
mod persistence;
use control::AvatarAction;

use crate::{
    AvatarPort, ConversationEngine, ConversationPolicy, EventSink, LanguageModelPort, MemoryPort,
    ModelRequest, SpeechPort,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeControl {
    Interrupt { reason: String },
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed(TurnId),
    Interrupted(TurnId),
    Shutdown(TurnId),
}

enum GenerationResult {
    Finished(Result<(), AppError>),
    Control(RuntimeControl),
}

pub(crate) enum TurnInput {
    User(String),
    Proactive,
}

pub struct Runtime<M, S, A, N, E>
where
    M: LanguageModelPort,
    S: SpeechPort,
    A: AvatarPort,
    N: MemoryPort,
    E: EventSink,
{
    engine: ConversationEngine,
    model: M,
    speech: S,
    avatar: A,
    memory: N,
    events: E,
    system_prompt: String,
    profile_id: String,
    memory_recall_limit: usize,
    speech_emotion: Emotion,
    avatar_healthy: bool,
    proactive_output: bool,
    soft_split: Option<usize>,
}

impl<M, S, A, N, E> Runtime<M, S, A, N, E>
where
    M: LanguageModelPort,
    S: SpeechPort,
    A: AvatarPort,
    N: MemoryPort,
    E: EventSink,
{
    pub fn new(model: M, speech: S, avatar: A, memory: N, events: E) -> Self {
        Self::from_policy(
            model,
            speech,
            avatar,
            memory,
            events,
            ConversationPolicy::default(),
        )
    }

    pub fn with_policy(
        model: M,
        speech: S,
        avatar: A,
        memory: N,
        events: E,
        policy: ConversationPolicy,
    ) -> Result<Self, AppError> {
        policy.validate()?;
        Ok(Self::from_policy(
            model, speech, avatar, memory, events, policy,
        ))
    }

    fn from_policy(
        model: M,
        speech: S,
        avatar: A,
        memory: N,
        events: E,
        policy: ConversationPolicy,
    ) -> Self {
        Self {
            engine: ConversationEngine::with_history_turn_limit(policy.history_turn_limit),
            model,
            speech,
            avatar,
            memory,
            events,
            system_prompt: policy.system_prompt,
            profile_id: "default".to_owned(),
            memory_recall_limit: policy.memory_recall_limit,
            speech_emotion: Emotion::Neutral,
            avatar_healthy: true,
            proactive_output: false,
            soft_split: None,
        }
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) -> Result<(), AppError> {
        if self.engine.active_turn().is_some() {
            return Err(AppError::invalid_transition(
                "cannot change persona during an active turn",
            ));
        }
        let prompt = prompt.into();
        if prompt.chars().count() > 16_384 {
            return Err(AppError::configuration("system prompt is too long"));
        }
        self.system_prompt = prompt;
        Ok(())
    }

    /// Configure optional long-sentence splitting between turns.
    pub fn set_soft_split(&mut self, minimum: Option<usize>) -> Result<(), AppError> {
        if self.engine.active_turn().is_some() {
            return Err(AppError::invalid_transition(
                "cannot change sentence splitting during an active turn",
            ));
        }
        let sentence = match minimum {
            Some(minimum) => ai_ex_text::SentenceBuffer::with_soft_split(minimum)?,
            None => ai_ex_text::SentenceBuffer::default(),
        };
        self.engine.set_sentence_buffer(sentence);
        self.soft_split = minimum;
        Ok(())
    }

    pub(crate) fn soft_split(&self) -> Option<usize> {
        self.soft_split
    }

    pub fn state(&self) -> ConversationState {
        self.engine.state()
    }

    pub async fn set_persona(
        &mut self,
        profile_id: String,
        prompt: String,
    ) -> Result<(), AppError> {
        if self.engine.active_turn().is_some() {
            return Err(AppError::invalid_transition(
                "cannot change persona during an active turn",
            ));
        }
        if profile_id.trim().is_empty()
            || profile_id.chars().count() > 128
            || prompt.chars().count() > 16_384
        {
            return Err(AppError::configuration(
                "persona identity or prompt is outside supported bounds",
            ));
        }
        if profile_id != self.profile_id {
            // Stop the previous body's remaining audio before changing its identity.
            self.cancel_speech().await?;
            self.avatar_output(AvatarAction::Stop).await;
            self.memory.select_profile(&profile_id).await?;
            self.engine.clear_identity_context();
            self.profile_id = profile_id;
        }
        self.system_prompt = prompt;
        Ok(())
    }

    pub async fn run_turn(&mut self, input: impl Into<String>) -> Result<TurnId, AppError> {
        match self
            .run_turn_inner(TurnInput::User(input.into()), None)
            .await?
        {
            TurnOutcome::Completed(turn_id) => Ok(turn_id),
            TurnOutcome::Interrupted(_) | TurnOutcome::Shutdown(_) => Err(
                AppError::invalid_transition("uncontrolled turn ended by control input"),
            ),
        }
    }

    pub async fn run_turn_controlled(
        &mut self,
        input: impl Into<String>,
        control: &mut tokio::sync::mpsc::Receiver<RuntimeControl>,
    ) -> Result<TurnOutcome, AppError> {
        self.run_turn_inner(TurnInput::User(input.into()), Some(control))
            .await
    }

    pub(crate) async fn run_proactive_controlled(
        &mut self,
        control: &mut tokio::sync::mpsc::Receiver<RuntimeControl>,
    ) -> Result<TurnOutcome, AppError> {
        self.run_turn_inner(TurnInput::Proactive, Some(control))
            .await
    }

    async fn run_turn_inner(
        &mut self,
        input: TurnInput,
        mut control: Option<&mut tokio::sync::mpsc::Receiver<RuntimeControl>>,
    ) -> Result<TurnOutcome, AppError> {
        let (turn_id, user_input) = match input {
            TurnInput::User(input) => (self.engine.begin_turn(&input)?, Some(input)),
            TurnInput::Proactive => (self.engine.begin_proactive()?, None),
        };
        self.proactive_output = user_input.is_none();
        let generated = if let Some(receiver) = control.as_deref_mut() {
            tokio::select! {
                biased;
                Some(command) = receiver.recv() => GenerationResult::Control(command),
                result = self.generate_turn(turn_id, user_input.as_deref()) => GenerationResult::Finished(result),
            }
        } else {
            GenerationResult::Finished(self.generate_turn(turn_id, user_input.as_deref()).await)
        };
        match generated {
            GenerationResult::Control(RuntimeControl::Interrupt { reason }) => {
                self.interrupt(reason).await?;
                return Ok(TurnOutcome::Interrupted(turn_id));
            }
            GenerationResult::Control(RuntimeControl::Shutdown) => {
                self.interrupt("runtime shutdown").await?;
                return Ok(TurnOutcome::Shutdown(turn_id));
            }
            GenerationResult::Finished(Err(error)) => {
                self.engine.fail(error.to_string());
                self.dispatch_events().await?;
                return Err(error);
            }
            GenerationResult::Finished(Ok(())) => {}
        }
        let assistant = self.engine.finish_turn(turn_id)?;
        // Never drop an in-flight durable write when a control command arrives.
        let remembered = if let Some(input) = user_input {
            self.remember_turn(turn_id, input, assistant, control).await
        } else {
            Ok(false)
        };
        self.dispatch_events().await?;
        Ok(if remembered? {
            TurnOutcome::Shutdown(turn_id)
        } else {
            TurnOutcome::Completed(turn_id)
        })
    }

    async fn generate_turn(
        &mut self,
        turn_id: TurnId,
        input: Option<&str>,
    ) -> Result<(), AppError> {
        self.cancel_speech().await?;
        self.speech_emotion = Emotion::Neutral;
        self.dispatch_events().await?;
        let memories = if let Some(input) = input {
            self.memory
                .recall_for_context(input, self.memory_recall_limit)
                .await?
        } else {
            Vec::new()
        };
        let mut messages = Vec::new();
        if !self.system_prompt.trim().is_empty() {
            messages.push(Message::new(Role::System, self.system_prompt.clone()));
        }
        messages.extend(memories);
        messages.extend_from_slice(self.engine.history());
        if input.is_none() {
            messages.push(Message::new(Role::System,
                "当前是允许主动说话的空闲时刻。结合已有语境，可以自然地说一句简短的陪伴话语，最多40字。不要虚构用户刚才说了什么，不要声称观察到未提供的环境。如果没有必要说话，请直接结束响应，不输出文字。"));
        }
        let request = ModelRequest { turn_id, messages };
        let mut stream = self.model.stream(request).await?;
        let mut preamble = ResponsePreamble::default();

        loop {
            match stream.recv().await {
                Some(Ok(chunk)) => {
                    let output = preamble.push(&chunk);
                    if let Some(emotion) = output.emotion {
                        self.engine.set_emotion(turn_id, emotion)?;
                    }
                    if !output.text.is_empty() {
                        self.engine.accept_chunk(turn_id, &output.text)?;
                    }
                    self.dispatch_events().await?;
                    if input.is_none() {
                        // Buffered system output must still yield to a user
                        // admission or cancellation before its next chunk.
                        tokio::task::yield_now().await;
                    }
                }
                Some(Err(error)) => return Err(error),
                None => {
                    let trailing = preamble.finish();
                    if !trailing.is_empty() {
                        self.engine.accept_chunk(turn_id, &trailing)?;
                        self.dispatch_events().await?;
                    }
                    break;
                }
            }
        }

        self.engine.flush_sentence(turn_id)?;
        self.dispatch_events().await?;
        Ok(())
    }

    pub async fn interrupt(&mut self, reason: impl Into<String>) -> Result<(), AppError> {
        self.proactive_output = false;
        // A control may win before generation has published its initial events.
        // Preserve that identity, but never enqueue undelivered speech during cleanup.
        for event in self.engine.drain_events() {
            if !matches!(event, SystemEvent::SentenceReady { .. }) {
                self.events.publish(event).await;
            }
        }
        let active = self.engine.active_turn();
        if active.is_some() {
            self.engine.interrupt(reason)?;
        }
        let speech = self.cancel_speech().await;
        let model = if let Some(turn_id) = active {
            control::bounded("model cancellation", self.model.cancel(turn_id)).await
        } else {
            Ok(())
        };
        self.avatar_output(AvatarAction::Stop).await;
        self.dispatch_events().await?;
        speech.and(model)
    }

    pub(crate) async fn cancel_proactive_output(&mut self) -> Result<(), AppError> {
        if self.proactive_output {
            self.interrupt("proactive output cancelled").await
        } else {
            Ok(())
        }
    }

    pub async fn stop(&mut self) -> Result<(), AppError> {
        let stopped = self.interrupt("runtime stopped").await;
        self.engine.stop();
        let published = self.dispatch_events().await;
        stopped.and(published)
    }

    async fn dispatch_events(&mut self) -> Result<(), AppError> {
        for event in self.engine.drain_events() {
            self.events.publish(event.clone()).await;
            match &event {
                SystemEvent::TurnStarted { .. } | SystemEvent::ProactiveStarted { .. } => {
                    self.avatar_output(AvatarAction::Stop).await;
                }
                SystemEvent::SentenceReady { turn_id, text } => {
                    self.speech
                        .enqueue_expressive(*turn_id, text.clone(), self.speech_emotion)
                        .await?;
                    self.avatar_output(AvatarAction::Subtitle(text.clone()))
                        .await;
                    self.avatar_output(AvatarAction::Speaking).await;
                }
                SystemEvent::EmotionChanged { emotion, .. } => {
                    self.speech_emotion = *emotion;
                    self.avatar_output(AvatarAction::Emotion(*emotion)).await;
                }
                SystemEvent::TurnFinished { .. } => {
                    self.avatar_output(AvatarAction::Neutral).await;
                }
                SystemEvent::Fault { .. } => {
                    let _cancel_result = self.cancel_speech().await;
                    self.avatar_output(AvatarAction::Stop).await;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn cancel_speech(&mut self) -> Result<(), AppError> {
        control::cancel_speech_port(&mut self.speech, &mut self.events).await
    }
}
