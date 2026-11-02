//! Owns Windows process trees through a non-inherited kill-on-close job handle.
//! Callers verify executable and runtime bytes before constructing a launch.
#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::OwnedJobProcess;

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
