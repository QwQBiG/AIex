use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyCode(u16);

impl KeyCode {
    pub fn new(value: u16) -> io::Result<Self> {
        if !(8..=254).contains(&value) || value == 0xE7 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid keyboard virtual key",
            ));
        }
        Ok(Self(value))
    }
    pub fn value(self) -> u16 {
        self.0
    }
    pub fn parse(name: &str) -> io::Result<Self> {
        let key = name.trim().to_ascii_uppercase();
        let value = match key.as_str() {
            "CTRL" | "CONTROL" => 0x11,
            "LCTRL" => 0xA2,
            "RCTRL" => 0xA3,
            "SHIFT" => 0x10,
            "LSHIFT" => 0xA0,
            "RSHIFT" => 0xA1,
            "ALT" => 0x12,
            "LALT" => 0xA4,
            "RALT" => 0xA5,
            "WIN" | "META" | "LWIN" => 0x5B,
            "RWIN" => 0x5C,
            "ENTER" | "RETURN" => 0x0D,
            "ESC" | "ESCAPE" => 0x1B,
            "TAB" => 9,
            "SPACE" => 0x20,
            "BACKSPACE" => 8,
            "DELETE" | "DEL" => 0x2E,
            "INSERT" => 0x2D,
            "HOME" => 0x24,
            "END" => 0x23,
            "PAGEUP" => 0x21,
            "PAGEDOWN" => 0x22,
            "LEFT" => 0x25,
            "UP" => 0x26,
            "RIGHT" => 0x27,
            "DOWN" => 0x28,
            _ if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() => {
                u16::from(key.as_bytes()[0])
            }
            _ if key.starts_with('F') => key[1..]
                .parse::<u16>()
                .ok()
                .filter(|n| (1..=24).contains(n))
                .map(|n| 0x70 + n - 1)
                .unwrap_or(0),
            _ => 0,
        };
        Self::new(value)
    }
    pub fn extended(self) -> bool {
        matches!(
            self.0,
            0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0xA3 | 0xA5
        )
    }
    pub(super) fn family(self) -> u16 {
        match self.0 {
            0xA0 | 0xA1 => 0x10,
            0xA2 | 0xA3 => 0x11,
            0xA4 | 0xA5 => 0x12,
            _ => self.0,
        }
    }
}
