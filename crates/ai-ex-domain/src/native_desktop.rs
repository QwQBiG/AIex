//! Window-bound native operations. Requests are intents, never input authority.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativePointerButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeDesktopAction {
    Click {
        x: i32,
        y: i32,
        button: NativePointerButton,
    },
    KeyChord {
        keys: Vec<String>,
    },
    TypeText {
        text: String,
    },
    Drag {
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
        button: NativePointerButton,
        steps: u16,
        duration_ms: u32,
    },
    Scroll {
        horizontal: bool,
        delta: i32,
    },
    Wait {
        milliseconds: u32,
    },
    Noop,
}

impl NativeDesktopAction {
    pub fn validate(&self) -> Result<(), AppError> {
        let valid = match self {
            Self::KeyChord { keys } => {
                !keys.is_empty()
                    && keys.len() <= 8
                    && keys.iter().all(|key| {
                        !key.is_empty() && key.len() <= 32 && !key.chars().any(char::is_control)
                    })
            }
            Self::TypeText { text } => !text.is_empty() && text.chars().count() <= 4096,
            Self::Drag {
                steps, duration_ms, ..
            } => (1..=64).contains(steps) && *duration_ms <= 5000,
            Self::Scroll { delta, .. } => *delta != 0 && delta.unsigned_abs() <= 12000,
            Self::Wait { milliseconds } => *milliseconds <= 5000,
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(AppError::configuration("invalid bounded native action"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeWindow {
    pub handle: u64,
    pub process_id: u32,
    pub process_started_ticks: u64,
    pub title: String,
    /// Exact executable scope checked against the service's global policy.
    pub scope: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub foreground: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeDesktopRequest {
    Status,
    /// Enumeration is explicit; starting the service does not inspect windows.
    Windows,
    StartNative {
        instance_id: Uuid,
        expected_revision: u64,
        operation_id: Uuid,
        window: NativeWindow,
        max_frames: u32,
        max_actions: u32,
    },
    Resume {
        instance_id: Uuid,
        expected_revision: u64,
        session_id: Uuid,
    },
    Pause {
        instance_id: Uuid,
        expected_revision: u64,
        session_id: Uuid,
    },
    Stop {
        instance_id: Uuid,
        expected_revision: u64,
        session_id: Uuid,
    },
    Capture {
        instance_id: Uuid,
        expected_revision: u64,
        session_id: Uuid,
    },
    SetGoal {
        instance_id: Uuid,
        expected_revision: u64,
        session_id: Uuid,
        goal: String,
    },
    SetAction {
        instance_id: Uuid,
        expected_revision: u64,
        session_id: Uuid,
        action: NativeDesktopAction,
    },
    Execute {
        instance_id: Uuid,
        expected_revision: u64,
        session_id: Uuid,
        operation_id: Uuid,
        frame_id: Uuid,
        action: NativeDesktopAction,
    },
    VisionStep {
        instance_id: Uuid,
        expected_revision: u64,
        session_id: Uuid,
        operation_id: Uuid,
        goal: String,
    },
    /// Analyze one fresh frame without executing the resulting suggestion.
    PreviewVision {
        instance_id: Uuid,
        expected_revision: u64,
        session_id: Uuid,
        operation_id: Uuid,
        goal: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeDesktopPhase {
    Prepared,
    Running,
    Paused,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeDesktopSession {
    pub session_id: Uuid,
    pub operation_id: Uuid,
    pub window: NativeWindow,
    pub geometry_revision: u64,
    pub phase: NativeDesktopPhase,
    pub frames: u32,
    pub actions: u32,
    pub max_frames: u32,
    pub max_actions: u32,
    pub frame_id: Option<Uuid>,
    pub last_operation: Option<Uuid>,
    pub last_outcome: Option<String>,
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub armed_action: Option<NativeDesktopAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeDesktopSnapshot {
    pub instance_id: Uuid,
    pub revision: u64,
    pub execution_mode: String,
    pub available: bool,
    pub session: Option<NativeDesktopSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeDesktopReply {
    pub snapshot: NativeDesktopSnapshot,
    pub windows: Vec<NativeWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision_preview: Option<NativeVisionPreview>,
}

/// Read-only suggestion from a captured frame; it cannot be executed as a proposal.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeVisionPreview {
    pub operation_id: Uuid,
    pub action: NativeDesktopAction,
}

impl std::fmt::Debug for NativeVisionPreview {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeVisionPreview")
            .field("operation_id", &self.operation_id)
            .field("action", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    #[test]
    fn preview_reply_remains_compatible_and_debug_redacts_action() {
        let old = serde_json::json!({
            "snapshot": {
                "instance_id": Uuid::new_v4(), "revision": 1,
                "execution_mode": "native_windows", "available": true, "session": null
            },
            "windows": []
        });
        let mut reply: NativeDesktopReply = serde_json::from_value(old).unwrap();
        assert!(reply.vision_preview.is_none());
        assert!(
            serde_json::to_value(&reply)
                .unwrap()
                .get("vision_preview")
                .is_none()
        );
        reply.vision_preview = Some(NativeVisionPreview {
            operation_id: Uuid::new_v4(),
            action: NativeDesktopAction::TypeText {
                text: "private-preview".into(),
            },
        });
        assert!(!format!("{reply:?}").contains("private-preview"));
        let encoded = serde_json::to_vec(&reply).unwrap();
        assert_eq!(
            serde_json::from_slice::<NativeDesktopReply>(&encoded).unwrap(),
            reply
        );
    }
}
