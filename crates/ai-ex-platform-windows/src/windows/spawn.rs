use std::io;
use std::mem::size_of;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::ptr;

use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Threading::*;

use super::{encoding, last_error, raw};
use crate::ProcessLaunch;

pub fn new_job() -> io::Result<OwnedHandle> {
    // SAFETY: null attributes create a non-inheritable unnamed job handle.
    let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if handle.is_null() {
        return Err(last_error("CreateJobObjectW"));
    }
    // SAFETY: ownership is transferred once from the successful API result.
    let job = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PRIORITY_CLASS;
    limits.BasicLimitInformation.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS;
    // SAFETY: limits has the documented layout and size, and job is a live handle.
    if unsafe {
        SetInformationJobObject(
            raw(&job),
            JobObjectExtendedLimitInformation,
            ptr::from_ref(&limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(last_error("SetInformationJobObject"));
    }
    Ok(job)
}

pub fn create_suspended(launch: ProcessLaunch) -> io::Result<(OwnedHandle, OwnedHandle, u32)> {
    if !launch.executable.is_absolute()
        || !launch.executable.is_file()
        || !launch.directory.is_absolute()
        || !launch.directory.is_dir()
    {
        return Err(encoding::invalid(
            "executable and working directory must be existing absolute paths",
        ));
    }
    let executable = encoding::wide(launch.executable.as_os_str())?;
    let directory = encoding::wide(launch.directory.as_os_str())?;
    let mut command = encoding::command(&launch)?;
    let environment = encoding::environment(&launch)?;
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: all pointers refer to live, terminated UTF16 storage or correctly
    // sized API structs. No caller handles are inherited; execution stays suspended.
    if unsafe {
        CreateProcessW(
            executable.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | BELOW_NORMAL_PRIORITY_CLASS,
            environment.as_ptr().cast(),
            directory.as_ptr(),
            &startup,
            &mut info,
        )
    } == 0
    {
        return Err(last_error("CreateProcessW"));
    }
    // SAFETY: CreateProcessW transferred one ownership reference for each handle.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
    // SAFETY: primary thread handle is a distinct owned handle from CreateProcessW.
    let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
    Ok((process, thread, info.dwProcessId))
}

pub fn stop_unassigned(process: &OwnedHandle) -> io::Result<()> {
    // SAFETY: only the handle just returned by our suspended CreateProcessW is used.
    let stopped = unsafe { TerminateProcess(raw(process), 1) };
    let error = if stopped == 0 {
        Some(last_error("TerminateProcess during launch cleanup"))
    } else {
        None
    };
    // SAFETY: process remains owned while Windows signals termination.
    if unsafe { WaitForSingleObject(raw(process), 1000) } != WAIT_OBJECT_0 {
        return Err(error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "suspended process cleanup timed out",
            )
        }));
    }
    Ok(())
}
