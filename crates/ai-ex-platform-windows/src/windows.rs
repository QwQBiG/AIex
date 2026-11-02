use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::ptr;

use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, ResumeThread, WaitForSingleObject,
};

use crate::ProcessLaunch;

mod encoding;
mod listener;
mod spawn;

/// The job handle is never inherited. Closing its last handle kills all members.
pub struct OwnedJobProcess {
    job: OwnedHandle,
    process: OwnedHandle,
    id: u32,
}

impl OwnedJobProcess {
    pub fn spawn(launch: ProcessLaunch) -> io::Result<Self> {
        let job = spawn::new_job()?;
        let (process, thread, id) = spawn::create_suspended(launch)?;
        // SAFETY: both handles are owned, live handles returned by Windows.
        if unsafe { AssignProcessToJobObject(raw(&job), raw(&process)) } == 0 {
            let original = last_error("AssignProcessToJobObject");
            if let Err(cleanup) = spawn::stop_unassigned(&process) {
                return Err(io::Error::new(
                    original.kind(),
                    format!("{original}; {cleanup}"),
                ));
            }
            return Err(original);
        }
        // SAFETY: the primary thread is suspended and its process belongs to job.
        if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
            return Err(last_error("ResumeThread"));
        }
        Ok(Self { job, process, id })
    }

    pub fn process_id(&self) -> u32 {
        self.id
    }

    pub fn exit_code(&self) -> io::Result<Option<u32>> {
        // SAFETY: the process handle remains owned for this entire call.
        match unsafe { WaitForSingleObject(raw(&self.process), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: output points to a live u32; process is a valid handle.
                if unsafe { GetExitCodeProcess(raw(&self.process), &mut code) } == 0 {
                    return Err(last_error("GetExitCodeProcess"));
                }
                Ok(Some(code))
            }
            _ => Err(last_error("WaitForSingleObject")),
        }
    }

    pub fn active_processes(&self) -> io::Result<u32> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: info has the documented layout and size for this query class.
        if unsafe {
            QueryInformationJobObject(
                raw(&self.job),
                JobObjectBasicAccountingInformation,
                ptr::from_mut(&mut info).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error("QueryInformationJobObject"));
        }
        Ok(info.ActiveProcesses)
    }

    /// Requests termination only through the job created by this object.
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: the job is exclusively owned and was created by this object.
        if unsafe { TerminateJobObject(raw(&self.job), 1) } == 0 {
            return Err(last_error("TerminateJobObject"));
        }
        Ok(())
    }

    pub fn owns_loopback_listener(&self, port: u16) -> io::Result<bool> {
        if self.exit_code()?.is_some() {
            return Ok(false);
        }
        listener::owned(self.id, port)
    }
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}

fn last_error(operation: &str) -> io::Error {
    let error = io::Error::last_os_error();
    io::Error::new(error.kind(), format!("{operation}: {error}"))
}
