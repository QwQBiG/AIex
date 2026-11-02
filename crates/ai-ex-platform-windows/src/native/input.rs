use std::io;
use std::mem::size_of;

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::desktop::{InputEvent, OwnedInput, PointerButton};

pub(super) fn send(events: &[InputEvent]) -> io::Result<usize> {
    if events.is_empty() || events.len() > 128 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "input batch must contain 1..=128 events",
        ));
    }
    let inputs: Vec<_> = events.iter().map(convert).collect();
    // SAFETY: INPUT owns only integer fields; the array length/layout are exact.
    let count = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    // Zero is a known zero-insertion result. Windows does not identify UIPI
    // blocking reliably through GetLastError, so the caller does not retry it.
    Ok(count as usize)
}

fn convert(event: &InputEvent) -> INPUT {
    match *event {
        InputEvent::MoveAbsolute { x, y } => mouse(
            x,
            y,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        ),
        InputEvent::Button { button, down } => mouse(
            0,
            0,
            0,
            match (button, down) {
                (PointerButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                (PointerButton::Left, false) => MOUSEEVENTF_LEFTUP,
                (PointerButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                (PointerButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                (PointerButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                (PointerButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
            },
        ),
        InputEvent::Scroll { horizontal, delta } => mouse(
            0,
            0,
            delta as u32,
            if horizontal {
                MOUSEEVENTF_HWHEEL
            } else {
                MOUSEEVENTF_WHEEL
            },
        ),
        InputEvent::Key { key, down } => keyboard(
            key.value(),
            0,
            if key.extended() {
                KEYEVENTF_EXTENDEDKEY
            } else {
                0
            } | if down { 0 } else { KEYEVENTF_KEYUP },
        ),
        InputEvent::Unicode { unit, down } => keyboard(
            0,
            unit,
            KEYEVENTF_UNICODE | if down { 0 } else { KEYEVENTF_KEYUP },
        ),
    }
}

fn mouse(x: i32, y: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: x,
                dy: y,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn keyboard(key: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub(super) fn pressed(input: OwnedInput) -> io::Result<bool> {
    let key = match input {
        OwnedInput::Key(key) => i32::from(key.value()),
        OwnedInput::Mouse(PointerButton::Left) => i32::from(VK_LBUTTON),
        OwnedInput::Mouse(PointerButton::Right) => i32::from(VK_RBUTTON),
        OwnedInput::Mouse(PointerButton::Middle) => i32::from(VK_MBUTTON),
        OwnedInput::Unicode(_) => return Ok(false),
    };
    // SAFETY: the bounded virtual-key code is an integer, not a pointer.
    Ok((unsafe { GetAsyncKeyState(key) }) < 0)
}

pub(super) fn cursor() -> io::Result<(i32, i32)> {
    let _dpi = super::inspect::DpiContext::enter()?;
    let mut point = POINT::default();
    // SAFETY: point is a live writable POINT.
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return Err(super::error("GetCursorPos"));
    }
    Ok((point.x, point.y))
}
