use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
};

/// Queries the caller-owned open file, so replacing a pathname cannot redirect
/// the link-count query to a different file between validation and copying.
pub fn file_link_count(file: &File) -> io::Result<u32> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: File owns a live file handle; the exact output layout is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if information.nNumberOfLinks == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file link count is unavailable",
        ));
    }
    Ok(information.nNumberOfLinks)
}
