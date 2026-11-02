use std::io;

use super::KeyCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelBounds {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl PixelBounds {
    pub fn validate(self) -> io::Result<()> {
        if self.width == 0
            || self.height == 0
            || i64::from(self.left) + i64::from(self.width) > i64::from(i32::MAX)
            || i64::from(self.top) + i64::from(self.height) > i64::from(i32::MAX)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid physical bounds",
            ));
        }
        Ok(())
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        i64::from(x) >= i64::from(self.left)
            && i64::from(y) >= i64::from(self.top)
            && i64::from(x) < i64::from(self.left) + i64::from(self.width)
            && i64::from(y) < i64::from(self.top) + i64::from(self.height)
    }

    /// SendInput coordinates target the center of a physical pixel in the
    /// virtual desktop, including negative origins, rather than its rounded edge.
    /// https://source.dot.net/UIAutomationClientSideProviders/MS/Internal/AutomationProxies/Input.cs.html
    pub fn normalize(self, x: i32, y: i32) -> io::Result<(i32, i32)> {
        self.validate()?;
        if !self.contains(x, y) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "point outside virtual desktop",
            ));
        }
        let axis = |value: i32, origin: i32, size: u32| -> io::Result<i32> {
            let offset = i64::from(value) - i64::from(origin);
            let normalized = ((2 * offset + 1) * 65536 / (2 * i64::from(size))).clamp(0, 65535);
            // Beyond 65536 physical pixels, not every pixel has a normalized
            // coordinate. Reject an unreachable pixel instead of clicking next to it.
            if normalized * i64::from(size) / 65536 != offset {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "physical pixel is not representable by SendInput",
                ));
            }
            Ok(normalized as i32)
        };
        Ok((
            axis(x, self.left, self.width)?,
            axis(y, self.top, self.height)?,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowIdentity {
    pub handle: u64,
    pub process_id: u32,
    pub process_started_ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub identity: WindowIdentity,
    pub title: String,
    pub executable: String,
    pub client: PixelBounds,
    pub desktop: PixelBounds,
    pub dpi: u32,
    pub foreground: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowTicket {
    pub identity: WindowIdentity,
    pub client: PixelBounds,
    pub desktop: PixelBounds,
    pub dpi: u32,
    pub geometry_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetChange {
    TicketRetired,
    WindowGone,
    IdentityChanged,
    GeometryChanged,
    NotForeground,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetCheck {
    Ready(WindowSnapshot),
    NeedsConfirmation {
        reason: TargetChange,
        observed: Option<WindowSnapshot>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl RgbaFrame {
    pub fn validate(&self) -> io::Result<()> {
        let bytes = u64::from(self.width) * u64::from(self.height) * 4;
        if self.width == 0
            || self.height == 0
            || bytes > 64 * 1024 * 1024
            || usize::try_from(bytes).ok() != Some(self.rgba.len())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid bounded RGBA frame",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PointerButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OwnedInput {
    Key(KeyCode),
    Unicode(u16),
    Mouse(PointerButton),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    MoveAbsolute { x: i32, y: i32 },
    Button { button: PointerButton, down: bool },
    Key { key: KeyCode, down: bool },
    Unicode { unit: u16, down: bool },
    Scroll { horizontal: bool, delta: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeAction {
    MoveTo {
        x: i32,
        y: i32,
    },
    ClickAt {
        x: i32,
        y: i32,
        button: PointerButton,
    },
    KeyChord {
        keys: Vec<KeyCode>,
    },
    TypeText {
        text: String,
    },
    Drag {
        from: (i32, i32),
        to: (i32, i32),
        button: PointerButton,
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

/// Acknowledges the service's responsibility to validate its real permit. The
/// live guard is still called immediately before every OS input batch.
#[derive(Debug, Clone, Copy)]
pub struct DispatchAllowance(());
impl DispatchAllowance {
    pub fn after_service_permission_check() -> Self {
        Self(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFailureKind {
    InvalidAction,
    TargetChanged,
    PermissionRevoked,
    Cancelled,
    AlreadyPressed,
    SystemCall,
    PartialInsertion,
    ReleaseIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReleaseReport {
    pub attempted: usize,
    pub released: usize,
    pub still_owned: Vec<OwnedInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InputReceipt {
    pub attempted_events: usize,
    pub accepted_events: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFailure {
    pub kind: InputFailureKind,
    pub detail: String,
    pub receipt: InputReceipt,
    pub cleanup: ReleaseReport,
}

impl std::fmt::Display for InputFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (accepted {}/{}, still owned {})",
            self.detail,
            self.receipt.accepted_events,
            self.receipt.attempted_events,
            self.cleanup.still_owned.len()
        )
    }
}
impl std::error::Error for InputFailure {}
