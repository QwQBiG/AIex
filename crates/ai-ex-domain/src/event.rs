use serde::{Deserialize, Serialize};

use crate::{ConversationState, Emotion, TurnId};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveResponseMode {
    #[default]
    Suggest,
    Automatic,
    Confirm,
}

impl LiveResponseMode {
    pub const fn allows_automatic(self) -> bool {
        matches!(self, Self::Automatic)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemEvent {
    /// A system-initiated output generation, without an invented user message.
    ProactiveStarted {
        turn_id: TurnId,
    },
    TurnStarted {
        turn_id: TurnId,
        user_text: String,
    },
    ModelChunk {
        turn_id: TurnId,
        text: String,
    },
    EmotionChanged {
        turn_id: TurnId,
        emotion: Emotion,
    },
    SentenceReady {
        turn_id: TurnId,
        text: String,
    },
    SpeechPlayback {
        playback: crate::SpeechPlaybackSnapshot,
    },
    SpeechPreviewStarted {
        turn_id: TurnId,
    },
    SpeechProgress {
        sentence_id: uuid::Uuid,
        position_ms: u64,
        mouth_level: u16,
    },
    SpeechCancelled,
    TurnFinished {
        turn_id: TurnId,
        full_text: String,
    },
    TurnInterrupted {
        turn_id: TurnId,
        reason: String,
    },
    StateChanged {
        from: ConversationState,
        to: ConversationState,
    },
    LiveEventReceived {
        event_id: uuid::Uuid,
        source: String,
        event_type: String,
        summary: String,
    },
    LiveResponseSuggested {
        event_id: uuid::Uuid,
        text: String,
        automatic: bool,
    },
    PersonaChanged {
        profile_id: String,
        revision: u64,
    },
    ComponentHealthChanged {
        component: String,
        ready: bool,
        detail: String,
    },
    Fault {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub component: String,
    pub ready: bool,
    pub detail: String,
}

impl ComponentHealth {
    pub fn ready(component: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            ready: true,
            detail: String::new(),
        }
    }

    pub fn unavailable(component: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            ready: false,
            detail: detail.into(),
        }
    }
}
