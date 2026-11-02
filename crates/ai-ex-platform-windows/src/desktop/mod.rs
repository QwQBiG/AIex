//! Window-bound physical coordinates and input ownership. The service supplies
//! its live permission guard; this module does not grant automation permission.
#![forbid(unsafe_code)]

mod actions;
mod bound;
mod execution;
mod keys;
mod types;

#[cfg(test)]
mod tests;

pub use bound::BoundDesktop;
pub use keys::KeyCode;
pub use types::*;

use std::io;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// System calls are injectable so ordinary tests never capture or inject input.
pub trait DesktopSystem {
    fn windows(&mut self) -> io::Result<Vec<WindowSnapshot>>;
    fn inspect(&mut self, handle: u64) -> io::Result<Option<WindowSnapshot>>;
    fn capture(
        &mut self,
        target: &WindowSnapshot,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> io::Result<RgbaFrame>;
    fn send_input(&mut self, events: &[InputEvent]) -> io::Result<usize>;
    fn is_pressed(&mut self, input: OwnedInput) -> io::Result<bool>;
    fn cursor(&mut self) -> io::Result<(i32, i32)>;
    fn shutdown(&mut self) -> io::Result<()> {
        Ok(())
    }
}
