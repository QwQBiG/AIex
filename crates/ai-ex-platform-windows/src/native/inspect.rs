use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{FILETIME, HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::desktop::{PixelBounds, WindowIdentity, WindowSnapshot};

pub(super) struct DpiContext(DPI_AWARENESS_CONTEXT);
impl DpiContext {
    pub fn enter() -> io::Result<Self> {
        // SAFETY: changes only this thread; the previous context is restored.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.is_null() {
            Err(super::error("SetThreadDpiAwarenessContext"))
        } else {
            Ok(Self(previous))
        }
    }
}
impl Drop for DpiContext {
    fn drop(&mut self) {
        // SAFETY: the context was returned by this thread's earlier call.
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

pub(super) fn handle(value: u64) -> io::Result<HWND> {
    let value = usize::try_from(value).map_err(|_| io::Error::other("window handle overflow"))?;
    if value == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "null window handle",
        ));
    }
    Ok(value as HWND)
}

pub(super) fn inspect(value: u64) -> io::Result<Option<WindowSnapshot>> {
    let hwnd = handle(value)?;
    let _dpi = DpiContext::enter()?;
    // SAFETY: window handles are opaque OS identifiers; no memory is dereferenced.
    if unsafe { IsWindow(hwnd) == 0 || IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 } {
        return Ok(None);
    }
    let mut pid = 0;
    // SAFETY: output references a live u32.
    if unsafe { GetWindowThreadProcessId(hwnd, &mut pid) } == 0 {
        return Ok(None);
    }
    // SAFETY: only read-only identity rights, no inheritable handle.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(super::error("OpenProcess target identity"));
    }
    // SAFETY: successful OpenProcess transfers one handle reference.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut times = [FILETIME::default(); 4];
    // SAFETY: each output has the documented layout and is distinct.
    let [created, exit, kernel, user] = &mut times;
    if unsafe { GetProcessTimes(process.as_raw_handle(), created, exit, kernel, user) } == 0 {
        return Err(super::error("GetProcessTimes target identity"));
    }
    let process_started_ticks =
        u64::from(created.dwLowDateTime) | (u64::from(created.dwHighDateTime) << 32);
    let mut path = vec![0_u16; 32768];
    let mut length = path.len() as u32;
    // SAFETY: length and buffer are live and their capacity is accurately given.
    if unsafe {
        QueryFullProcessImageNameW(process.as_raw_handle(), 0, path.as_mut_ptr(), &mut length)
    } == 0
    {
        return Err(super::error("QueryFullProcessImageNameW"));
    }
    let executable = String::from_utf16_lossy(&path[..length as usize]);
    let mut title = [0_u16; 513];
    // SAFETY: the bounded title buffer remains live for the call.
    let title_length =
        unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) }.max(0) as usize;
    let mut rect = RECT::default();
    // SAFETY: output has the RECT layout expected by GetClientRect.
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return Err(super::error("GetClientRect"));
    }
    let mut first = POINT {
        x: rect.left,
        y: rect.top,
    };
    let mut last = POINT {
        x: rect.right,
        y: rect.bottom,
    };
    // SAFETY: both POINT references are distinct live output buffers.
    if unsafe { ClientToScreen(hwnd, &mut first) == 0 || ClientToScreen(hwnd, &mut last) == 0 } {
        return Err(super::error("ClientToScreen"));
    }
    let width = u32::try_from(i64::from(last.x) - i64::from(first.x))
        .map_err(|_| io::Error::other("invalid client width"))?;
    let height = u32::try_from(i64::from(last.y) - i64::from(first.y))
        .map_err(|_| io::Error::other("invalid client height"))?;
    let client = PixelBounds {
        left: first.x,
        top: first.y,
        width,
        height,
    };
    // SAFETY: metrics and DPI/foreground queries take no borrowed memory.
    let desktop = unsafe {
        PixelBounds {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN) as u32,
        }
    };
    client.validate()?;
    desktop.validate()?;
    // SAFETY: the OS validates hwnd; it is rechecked after these observations.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let foreground = unsafe { GetForegroundWindow() } == hwnd;
    let mut latest_pid = 0;
    // SAFETY: latest_pid is a live output; this detects a destroyed/reused handle.
    if unsafe { GetWindowThreadProcessId(hwnd, &mut latest_pid) } == 0 || latest_pid != pid {
        return Ok(None);
    }
    Ok(Some(WindowSnapshot {
        identity: WindowIdentity {
            handle: value,
            process_id: pid,
            process_started_ticks,
        },
        title: String::from_utf16_lossy(&title[..title_length]),
        executable,
        client,
        desktop,
        dpi,
        foreground,
    }))
}

pub(super) fn windows() -> io::Result<Vec<WindowSnapshot>> {
    let mut handles: Vec<u64> = Vec::new();
    unsafe extern "system" fn collect(hwnd: HWND, parameter: LPARAM) -> i32 {
        // SAFETY: EnumWindows invokes this synchronously with our unique Vec.
        let handles = unsafe { &mut *(parameter as *mut Vec<u64>) };
        if handles.len() == 256 {
            return 0;
        }
        // SAFETY: this is a metadata-only query of the enumerated identifier.
        if unsafe { IsWindowVisible(hwnd) } != 0 {
            handles.push(hwnd as usize as u64);
        }
        1
    }
    // SAFETY: the pointer stays live and no other borrow is used during callbacks.
    let result = unsafe { EnumWindows(Some(collect), (&mut handles as *mut Vec<u64>) as LPARAM) };
    if result == 0 && handles.len() < 256 {
        return Err(super::error("EnumWindows"));
    }
    let mut windows = Vec::new();
    for handle in handles {
        // Inaccessible/protected windows are not valid automation targets.
        if let Ok(Some(snapshot)) = inspect(handle)
            && !snapshot.title.trim().is_empty()
        {
            windows.push(snapshot);
        }
    }
    Ok(windows)
}
