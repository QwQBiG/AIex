//! Owns Windows process trees through a non-inherited kill-on-close job handle.
//! Callers verify executable and runtime bytes before constructing a launch.
#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

pub mod desktop;
pub mod hotkeys;

#[cfg(windows)]
mod disk;
#[cfg(windows)]
pub use disk::available_disk_space;

/// This platform adapter provides disk space queries on Windows only.
#[cfg(not(windows))]
pub fn available_disk_space(_directory: &std::path::Path) -> std::io::Result<u64> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Windows disk space queries require Windows",
    ))
}

/// Local wall-clock minute for configured quiet hours, using the OS timezone.
#[cfg(windows)]
pub fn local_minute() -> Option<u16> {
    let mut time = windows_sys::Win32::Foundation::SYSTEMTIME::default();
    // SAFETY: time is a writable SYSTEMTIME with the documented layout.
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut time) };
    (time.wHour < 24 && time.wMinute < 60).then_some(time.wHour * 60 + time.wMinute)
}

#[cfg(not(windows))]
pub fn local_minute() -> Option<u16> {
    None
}

#[cfg(windows)]
pub mod native;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::OwnedJobProcess;

#[cfg(windows)]
mod file_identity;
#[cfg(windows)]
pub use file_identity::file_link_count;

#[cfg(not(windows))]
pub fn file_link_count(_file: &std::fs::File) -> std::io::Result<u32> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Windows file identity requires Windows",
    ))
}

pub struct ProcessLaunch {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub directory: PathBuf,
    /// Replaces the environment, rather than extending the caller's environment.
    pub environment: BTreeMap<OsString, OsString>,
}

#[cfg(not(windows))]
mod unsupported {
    use std::io;

    use crate::ProcessLaunch;

    pub struct OwnedJobProcess {
        _private: (),
    }

    fn error() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "owned Windows jobs require Windows",
        )
    }

    impl OwnedJobProcess {
        pub fn spawn(_launch: ProcessLaunch) -> io::Result<Self> {
            Err(error())
        }
        pub fn process_id(&self) -> u32 {
            0
        }
        pub fn exit_code(&self) -> io::Result<Option<u32>> {
            Err(error())
        }
        pub fn active_processes(&self) -> io::Result<u32> {
            Err(error())
        }
        pub fn terminate(&self) -> io::Result<()> {
            Err(error())
        }
        pub fn owns_loopback_listener(&self, _port: u16) -> io::Result<bool> {
            Err(error())
        }
    }
}

#[cfg(not(windows))]
pub use unsupported::OwnedJobProcess;
