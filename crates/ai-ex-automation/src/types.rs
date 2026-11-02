use ai_ex_domain::{AppError, ErrorKind, NativeDesktopAction};
use ai_ex_safety::Capability;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationAction {
    CaptureScreen,
    /// Window-bound native intent; dispatch still requires a live service permit.
    Native {
        action: NativeDesktopAction,
    },
    MovePointer {
        x: f32,
        y: f32,
    },
    Click {
        button: PointerButton,
    },
    KeyChord {
        keys: Vec<String>,
    },
    TypeText {
        text: String,
    },
    LaunchProcess {
        program: String,
        arguments: Vec<String>,
    },
}

impl AutomationAction {
    pub fn required_capability(&self) -> Capability {
        match self {
            Self::CaptureScreen => Capability::ScreenRead,
            Self::Native { action } => match action {
                NativeDesktopAction::Click { .. }
                | NativeDesktopAction::Drag { .. }
                | NativeDesktopAction::Scroll { .. } => Capability::MouseInput,
                NativeDesktopAction::KeyChord { .. } | NativeDesktopAction::TypeText { .. } => {
                    Capability::KeyboardInput
                }
                NativeDesktopAction::Wait { .. } | NativeDesktopAction::Noop => {
                    Capability::ScreenRead
                }
            },
            Self::MovePointer { .. } | Self::Click { .. } => Capability::MouseInput,
            Self::KeyChord { .. } | Self::TypeText { .. } => Capability::KeyboardInput,
            Self::LaunchProcess { .. } => Capability::ProcessLaunch,
        }
    }

    pub fn audit_label(&self) -> String {
        match self {
            Self::CaptureScreen => "capture_screen".to_owned(),
            Self::Native { action } => match action {
                NativeDesktopAction::Click { x, y, button } => {
                    format!("native_click({x},{y},{button:?})")
                }
                NativeDesktopAction::KeyChord { keys } => {
                    format!("native_key_chord(keys={})", keys.len())
                }
                NativeDesktopAction::TypeText { text } => {
                    format!("native_type_text(length={})", text.chars().count())
                }
                NativeDesktopAction::Drag {
                    steps, duration_ms, ..
                } => format!("native_drag(steps={steps},duration_ms={duration_ms})"),
                NativeDesktopAction::Scroll { horizontal, delta } => {
                    format!("native_scroll(horizontal={horizontal},delta={delta})")
                }
                NativeDesktopAction::Wait { milliseconds } => {
                    format!("native_wait({milliseconds})")
                }
                NativeDesktopAction::Noop => "native_noop".to_owned(),
            },
            Self::MovePointer { x, y } => format!("move_pointer({x:.3},{y:.3})"),
            Self::Click { button } => format!("click({button:?})"),
            Self::KeyChord { keys } => format!("key_chord({})", keys.join("+")),
            Self::TypeText { text } => format!("type_text(length={})", text.chars().count()),
            Self::LaunchProcess { program, arguments } => {
                format!("launch_process({program},arguments={})", arguments.len())
            }
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        match self {
            Self::Native { action } => action.validate(),
            Self::MovePointer { x, y }
                if !x.is_finite()
                    || !y.is_finite()
                    || !(0.0..=1.0).contains(x)
                    || !(0.0..=1.0).contains(y) =>
            {
                Err(AppError::configuration(
                    "pointer coordinates must be normalized",
                ))
            }
            Self::KeyChord { keys } if keys.is_empty() || keys.len() > 8 => Err(
                AppError::configuration("key chord must contain 1 to 8 keys"),
            ),
            Self::TypeText { text } if text.is_empty() || text.chars().count() > 4_096 => {
                Err(AppError::configuration("typed text length is invalid"))
            }
            Self::LaunchProcess { program, arguments }
                if program.trim().is_empty() || arguments.len() > 64 =>
            {
                Err(AppError::configuration("process launch request is invalid"))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ScreenFrame {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, AppError> {
        let expected = usize::try_from(width)
            .ok()
            .zip(usize::try_from(height).ok())
            .and_then(|(width, height)| width.checked_mul(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| AppError::protocol("screen frame dimensions overflow"))?;
        if width == 0 || height == 0 || rgba.len() != expected {
            return Err(AppError::protocol("screen frame RGBA size is invalid"));
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionResult {
    Completed,
    ScreenCaptured(ScreenFrame),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditStage {
    Requested,
    Authorized,
    Completed,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub action_id: Uuid,
    pub timestamp_ms: u64,
    pub stage: AuditStage,
    pub capability: Capability,
    pub target: String,
    pub action: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPhase {
    BeforeExecution,
    DuringExecution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionFailure {
    pub action_id: Uuid,
    pub phase: ExecutionPhase,
    pub error: AppError,
}

impl ExecutionFailure {
    pub fn retry_is_safe(&self) -> bool {
        self.phase == ExecutionPhase::BeforeExecution && self.error.kind != ErrorKind::Safety
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReceipt {
    pub action_id: Uuid,
    pub result: ActionResult,
    pub completion_audit_persisted: bool,
}

#[cfg(test)]
mod native_tests {
    use super::*;

    #[test]
    fn native_actions_require_their_real_capability_and_keep_text_out_of_audit() {
        let text = AutomationAction::Native {
            action: NativeDesktopAction::TypeText {
                text: "private reference".into(),
            },
        };
        assert_eq!(text.required_capability(), Capability::KeyboardInput);
        assert!(!text.audit_label().contains("private"));
        text.validate().unwrap();
        let encoded = serde_json::to_string(&text).unwrap();
        assert_eq!(
            serde_json::from_str::<AutomationAction>(&encoded).unwrap(),
            text
        );
        let wait = AutomationAction::Native {
            action: NativeDesktopAction::Wait { milliseconds: 5001 },
        };
        assert_eq!(wait.required_capability(), Capability::ScreenRead);
        assert!(wait.validate().is_err());
        let drag = AutomationAction::Native {
            action: NativeDesktopAction::Drag {
                from_x: -2,
                from_y: 1,
                to_x: -1,
                to_y: 2,
                button: ai_ex_domain::NativePointerButton::Left,
                steps: 64,
                duration_ms: 100,
            },
        };
        assert_eq!(drag.required_capability(), Capability::MouseInput);
        drag.validate().unwrap();
    }
}
