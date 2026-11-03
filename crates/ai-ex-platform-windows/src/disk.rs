use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

/// Returns bytes available to the current user on the volume containing an
/// existing directory. This only queries the operating system; it creates no files.
pub fn available_disk_space(directory: &Path) -> io::Result<u64> {
    let mut path: Vec<u16> = directory.as_os_str().encode_wide().collect();
    if path.is_empty() || path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path must be nonempty and contain no NUL",
        ));
    }
    if !std::fs::metadata(directory)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "disk space query requires an existing directory",
        ));
    }
    path.push(0);
    let mut available = 0_u64;
    // SAFETY: path is NUL-terminated and lives through the call; available is
    // writable, and the two unused output pointers may be null.
    if unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn queries_an_existing_directory_without_changing_it() {
        let directory = std::env::temp_dir();
        assert!(directory.is_dir());
        available_disk_space(&directory).expect("query existing temporary directory");
    }

    #[test]
    fn nonexistent_directory_returns_an_error_without_creating_it() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "aiex-disk-query-missing-{}-{nonce}",
            std::process::id()
        ));
        assert!(!directory.exists());
        assert_eq!(
            available_disk_space(&directory).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(!directory.exists());
    }

    #[test]
    fn rejects_empty_paths_and_embedded_nul() {
        assert_eq!(
            available_disk_space(Path::new("")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            available_disk_space(Path::new("x\0y")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
