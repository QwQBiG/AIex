use std::io;
use std::mem::size_of;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Storage::Xps::{PW_CLIENTONLY, PrintWindow};

use crate::desktop::{RgbaFrame, WindowSnapshot};

struct WindowDc(HWND, HDC);
impl Drop for WindowDc {
    fn drop(&mut self) {
        // SAFETY: this DC came from GetDC for this exact window.
        unsafe {
            ReleaseDC(self.0, self.1);
        }
    }
}
struct MemoryDc(HDC);
impl Drop for MemoryDc {
    fn drop(&mut self) {
        // SAFETY: the compatible DC is exclusively owned by this object.
        unsafe {
            DeleteDC(self.0);
        }
    }
}
struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        // SAFETY: the bitmap is owned and deselected before this drop.
        unsafe {
            DeleteObject(self.0);
        }
    }
}
struct Selected(HDC, HGDIOBJ);
impl Drop for Selected {
    fn drop(&mut self) {
        // SAFETY: restores the previous object while the DC is still live.
        unsafe {
            SelectObject(self.0, self.1);
        }
    }
}

pub(super) fn capture(target: &WindowSnapshot) -> io::Result<RgbaFrame> {
    target.client.validate()?;
    let width = target.client.width;
    let height = target.client.height;
    let bytes = u64::from(width) * u64::from(height) * 4;
    if bytes > 64 * 1024 * 1024 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture exceeds 64 MiB",
        ));
    }
    let hwnd = super::super::inspect::handle(target.identity.handle)?;
    let _dpi = super::super::inspect::DpiContext::enter()?;
    // SAFETY: GetDC/compatible objects are checked and immediately given owners.
    let source = unsafe { GetDC(hwnd) };
    if source.is_null() {
        return Err(super::super::error("GetDC target window"));
    }
    let source = WindowDc(hwnd, source);
    let memory = unsafe { CreateCompatibleDC(source.1) };
    if memory.is_null() {
        return Err(super::super::error("CreateCompatibleDC"));
    }
    let memory = MemoryDc(memory);
    let bitmap = unsafe { CreateCompatibleBitmap(source.1, width as i32, height as i32) };
    if bitmap.is_null() {
        return Err(super::super::error("CreateCompatibleBitmap"));
    }
    let bitmap = Bitmap(bitmap);
    let previous = unsafe { SelectObject(memory.0, bitmap.0) };
    if previous.is_null() || previous == (-1_isize) as HGDIOBJ {
        return Err(super::super::error("SelectObject capture bitmap"));
    }
    let selected = Selected(memory.0, previous);
    // SAFETY: PrintWindow is confined to the kill-on-close worker, not the host.
    // Only this window's client content is requested, with no desktop fallback.
    if unsafe { PrintWindow(hwnd, memory.0, PW_CLIENTONLY | 2) } == 0 {
        return Err(super::super::error(
            "PrintWindow client; this window may not support GDI capture",
        ));
    }
    drop(selected);
    let mut rgba = vec![0_u8; bytes as usize];
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: the deselected bitmap matches the positive dimensions and exact
    // writable buffer size; negative height selects top-down BGRA rows.
    let copied = unsafe {
        GetDIBits(
            memory.0,
            bitmap.0,
            0,
            height,
            rgba.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if copied != height as i32 {
        return Err(super::super::error("GetDIBits target frame"));
    }
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    let frame = RgbaFrame {
        width,
        height,
        rgba,
    };
    frame.validate()?;
    Ok(frame)
}
