//! Win32 objects remain in this adapter. Construction does not open a window,
//! capture pixels, register a hotkey, or inject input.

mod capture;
mod helper;
mod hotkeys;
mod input;
mod inspect;
mod test_window;

pub use capture::WindowsDesktop;
pub use hotkeys::WindowsHotkeys;

use std::io;

/// --version/--help are headless; --test-window is the only visible entry.
pub fn run_helper(arguments: &[String]) -> io::Result<()> {
    helper::run(arguments)
}

pub(super) fn error(context: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{context}: {error}"))
}
