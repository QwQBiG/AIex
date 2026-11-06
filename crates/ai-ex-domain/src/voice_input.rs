use serde::{Deserialize, Serialize};

/// Ephemeral, independently versioned voice capture or transcription activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceInputState {
    pub revision: u64,
    pub active: bool,
}
