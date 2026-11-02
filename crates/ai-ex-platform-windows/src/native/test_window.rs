use std::io;
use std::ptr;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Default)]
struct State {
    clicks: u64,
    mouse_ups: u64,
    keys: u64,
    key_ups: u64,
    scroll: i64,
    cursor: (i16, i16),
    text: Vec<u16>,
    seen_keys: std::collections::BTreeSet<u16>,
}

pub(super) fn run() -> io::Result<()> {
    let _dpi = super::inspect::DpiContext::enter()?;
    let class: Vec<_> = "LocalCompanionInputTest\0".encode_utf16().collect();
    let title: Vec<_> = "原生交互专用测试窗口\0".encode_utf16().collect();
    // SAFETY: NULL obtains the current module, the cursor is a system resource.
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    let cursor = unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) };
    let definition = WNDCLASSW {
        lpfnWndProc: Some(procedure),
        hInstance: module,
        lpszClassName: class.as_ptr(),
        hCursor: cursor,
        style: CS_HREDRAW | CS_VREDRAW,
        ..Default::default()
    };
    // SAFETY: class/title and callback are valid for the entire window lifetime.
    if unsafe { RegisterClassW(&definition) } == 0 {
        return Err(super::error("RegisterClassW test canvas"));
    }
    let mut state = Box::<State>::default();
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            760,
            540,
            ptr::null_mut(),
            ptr::null_mut(),
            module,
            ptr::from_mut(state.as_mut()).cast(),
        )
    };
    if hwnd.is_null() {
        return Err(super::error("CreateWindowExW test canvas"));
    }
    // SAFETY: this function owns the window and only displays this test canvas.
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
    }
    let result = message_loop();
    // SAFETY: remove the borrowed pointer before the Rust state may be dropped,
    // including a message-loop failure; no pointer ownership crosses Win32.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        if IsWindow(hwnd) != 0 {
            DestroyWindow(hwnd);
        }
    }
    result
}

fn message_loop() -> io::Result<()> {
    loop {
        let mut message = MSG::default();
        // SAFETY: the live MSG receives this thread's messages.
        let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        if result == 0 {
            return Ok(());
        }
        if result == -1 {
            return Err(super::error("GetMessageW test canvas"));
        }
        // SAFETY: message was initialized by GetMessageW and its window owns the callback.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: WM_NCCREATE documents a CREATESTRUCTW valid for this callback.
        let created = unsafe { &*(lparam as *const CREATESTRUCTW) };
        // SAFETY: this borrows the outer Box, which outlives the message loop.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, created.lpCreateParams as isize);
        }
    }
    if matches!(
        message,
        WM_LBUTTONDOWN
            | WM_RBUTTONDOWN
            | WM_MBUTTONDOWN
            | WM_LBUTTONUP
            | WM_RBUTTONUP
            | WM_MBUTTONUP
            | WM_MOUSEMOVE
            | WM_KEYDOWN
            | WM_SYSKEYDOWN
            | WM_KEYUP
            | WM_SYSKEYUP
            | WM_CHAR
            | WM_MOUSEWHEEL
            | WM_MOUSEHWHEEL
    ) {
        // SAFETY: no Win32 calls occur while the state borrow is live, so nested
        // window messages cannot create a second Rust mutable reference.
        let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
        if !pointer.is_null() {
            let state = unsafe { &mut *pointer };
            match message {
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                    state.clicks = state.clicks.saturating_add(1)
                }
                WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                    state.mouse_ups = state.mouse_ups.saturating_add(1)
                }
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    state.keys = state.keys.saturating_add(1);
                    if wparam < 256 {
                        state.seen_keys.insert(wparam as u16);
                    }
                }
                WM_KEYUP | WM_SYSKEYUP => state.key_ups = state.key_ups.saturating_add(1),
                WM_MOUSEMOVE => state.cursor = (lparam as u16 as i16, (lparam >> 16) as u16 as i16),
                WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                    state.scroll = state
                        .scroll
                        .saturating_add(i64::from((wparam >> 16) as u16 as i16))
                }
                WM_CHAR => {
                    if state.text.len() == 256 {
                        state.text.remove(0);
                    }
                    state.text.push(wparam as u16);
                }
                _ => {}
            }
        }
        // SAFETY: only this dedicated window is invalidated; no other window moves.
        unsafe {
            InvalidateRect(hwnd, ptr::null(), 0);
        }
    }
    match message {
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: BeginPaint/EndPaint are paired, with no borrowed Rust state in GDI calls.
            let dc = unsafe { BeginPaint(hwnd, &mut paint) };
            draw(hwnd, dc);
            unsafe {
                EndPaint(hwnd, &paint);
            }
            0
        }
        WM_PRINTCLIENT => {
            draw(hwnd, wparam as HDC);
            0
        }
        WM_NCDESTROY => {
            // SAFETY: clears the non-owning pointer before window destruction completes.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
        }
        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn draw(hwnd: HWND, dc: HDC) {
    if dc.is_null() {
        return;
    }
    // SAFETY: the window-owned pointer is live; cloning completes before any
    // drawing call that might cause nested window messages.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
    let state = if pointer.is_null() {
        State::default()
    } else {
        unsafe { (&*pointer).clone() }
    };
    let mut bounds = RECT::default();
    // SAFETY: bounds is a live RECT and all drawing is confined to our own DC.
    unsafe {
        GetClientRect(hwnd, &mut bounds);
    }
    fill(dc, bounds, 0x00231D1D);
    // SAFETY: these are valid GDI color/mode integer values.
    unsafe {
        SetTextColor(dc, 0x00F5EBE6);
        SetBkMode(dc, TRANSPARENT as i32);
    }
    line(
        dc,
        24,
        20,
        "仅此专用窗口接收测试输入；关闭窗口会使绑定失效。",
    );
    line(
        dc,
        24,
        50,
        "可移动、缩放或拖到另一显示器，回到工作室核对后再恢复。",
    );
    line(
        dc,
        24,
        85,
        &format!(
            "鼠标 Down / Up：{} / {}    键盘 Down / Up：{} / {}",
            state.clicks, state.mouse_ups, state.keys, state.key_ups
        ),
    );
    line(
        dc,
        24,
        115,
        &format!(
            "窗口内鼠标：{}, {}    滚动增量：{}",
            state.cursor.0, state.cursor.1, state.scroll
        ),
    );
    for row in 0..8 {
        for column in 0..8 {
            fill(
                dc,
                RECT {
                    left: 160 + column * 4,
                    top: 160 + row * 4,
                    right: 164 + column * 4,
                    bottom: 164 + row * 4,
                },
                if (row + column) % 2 == 0 {
                    0x00DB6B9D
                } else {
                    0x00FFD5E8
                },
            );
        }
    }
    line(dc, 216, 164, "32 × 32 物理像素棋盘，便于核对点击与拖动位置");
    let down: Vec<_> = state
        .seen_keys
        .iter()
        .filter(|&&key| {
            // SAFETY: only bounded keys previously received by this test canvas are queried.
            (unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(i32::from(key))
            }) < 0
        })
        .map(|key| format!("0x{key:02X}"))
        .collect();
    line(
        dc,
        24,
        230,
        &format!(
            "这些已见按键当前仍按下：{}",
            if down.is_empty() {
                "无".into()
            } else {
                down.join(" ")
            }
        ),
    );
    line(
        dc,
        24,
        268,
        "停止或急停后检查 Up 与仍按下状态；不会替你释放手动按住的键。",
    );
    line(
        dc,
        24,
        310,
        &format!("最近输入：{}", String::from_utf16_lossy(&state.text)),
    );
}

fn fill(dc: HDC, rectangle: RECT, color: u32) {
    // SAFETY: the brush is temporary and owned, the RECT is live for FillRect.
    let brush = unsafe { CreateSolidBrush(color) };
    if !brush.is_null() {
        unsafe {
            FillRect(dc, &rectangle, brush);
            DeleteObject(brush);
        }
    }
}

fn line(dc: HDC, x: i32, y: i32, text: &str) {
    let units: Vec<_> = text.encode_utf16().collect();
    // SAFETY: units is live UTF16 storage, with the explicit element count.
    unsafe {
        TextOutW(dc, x, y, units.as_ptr(), units.len() as i32);
    }
}
