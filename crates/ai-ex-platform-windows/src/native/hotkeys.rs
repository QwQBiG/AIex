use std::io;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW, WM_HOTKEY};

use crate::hotkeys::{HotkeyBinding, HotkeyEvent, HotkeyRegistrations, HotkeySystem};

struct SystemHotkeys;
impl HotkeySystem for SystemHotkeys {
    fn register(&mut self, binding: HotkeyBinding) -> io::Result<()> {
        // SAFETY: NULL associates this validated binding with this owning thread.
        if unsafe {
            RegisterHotKey(
                ptr::null_mut(),
                binding.id,
                binding.modifiers | 0x4000,
                u32::from(binding.virtual_key),
            )
        } == 0
        {
            Err(super::error(
                "RegisterHotKey; binding may already be in use",
            ))
        } else {
            Ok(())
        }
    }
    fn unregister(&mut self, id: i32) -> io::Result<()> {
        // SAFETY: ids are only those registered by this thread's ownership set.
        if unsafe { UnregisterHotKey(ptr::null_mut(), id) } == 0 {
            Err(super::error("UnregisterHotKey"))
        } else {
            Ok(())
        }
    }
}

pub struct WindowsHotkeys {
    stop: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    events: mpsc::Receiver<HotkeyEvent>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl WindowsHotkeys {
    /// Registers only when explicitly called by the configured service. No
    /// polling or registration occurs when a WindowsDesktop is constructed.
    pub fn register(bindings: Vec<HotkeyBinding>, generation: u64) -> io::Result<Self> {
        for binding in &bindings {
            binding.validate()?;
        }
        if bindings.len() > 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "at most 16 hotkeys",
            ));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let overflow = Arc::new(AtomicBool::new(false));
        let (sender, events) = mpsc::sync_channel(32);
        let (ready, initialized) = mpsc::sync_channel(1);
        let thread_stop = stop.clone();
        let thread_overflow = overflow.clone();
        let thread = thread::spawn(move || {
            let mut registered =
                match HotkeyRegistrations::register(SystemHotkeys, &bindings, generation) {
                    Ok(registered) => {
                        let _ack = ready.send(Ok(()));
                        registered
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        let _ack = ready.send(Err(io::Error::new(error.kind(), detail)));
                        return Err(error);
                    }
                };
            while !thread_stop.load(Ordering::Acquire) {
                let mut message = MSG::default();
                // SAFETY: message is a live MSG; NULL reads this thread's queue.
                for _ in 0..64 {
                    if thread_stop.load(Ordering::Acquire)
                        || unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) }
                            == 0
                    {
                        break;
                    }
                    if message.message == WM_HOTKEY
                        && let Some(event) = registered.event(message.wParam as i32)
                    {
                        match sender.try_send(event) {
                            Ok(()) => {}
                            Err(mpsc::TrySendError::Full(_)) => {
                                thread_overflow.store(true, Ordering::Release);
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => {
                                thread_stop.store(true, Ordering::Release);
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
            registered.close()
        });
        let mut owner = Self {
            stop,
            overflow,
            events,
            thread: Some(thread),
        };
        match initialized.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(owner),
            result => {
                let detail = match result {
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => error.to_string(),
                    _ => unreachable!(),
                };
                let cleanup = owner.shutdown();
                Err(io::Error::other(format!(
                    "global hotkey initialization failed: {detail}; cleanup={cleanup:?}"
                )))
            }
        }
    }

    /// An overflow has an unknown intent count. The service must reconcile
    /// state instead of replaying toggles or assuming an emergency key arrived.
    pub fn poll(&mut self) -> io::Result<Vec<HotkeyEvent>> {
        if self.overflow.swap(false, Ordering::AcqRel) {
            self.stop.store(true, Ordering::Release);
            for _event in self.events.try_iter() {}
            return Err(io::Error::other(
                "global hotkey event overflow; reconcile state",
            ));
        }
        if !self.stop.load(Ordering::Acquire)
            && self
                .thread
                .as_ref()
                .is_some_and(|thread| thread.is_finished())
        {
            return Err(io::Error::other(
                "global hotkey thread stopped unexpectedly",
            ));
        }
        Ok(self.events.try_iter().collect())
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        let Some(thread) = &self.thread else {
            return Ok(());
        };
        let started = Instant::now();
        while !thread.is_finished() {
            if started.elapsed() >= Duration::from_secs(2) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "global hotkey thread is still stopping",
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
        self.thread
            .take()
            .expect("hotkey thread owned")
            .join()
            .map_err(|_| io::Error::other("global hotkey thread stopped unexpectedly"))?
    }
}

impl Drop for WindowsHotkeys {
    fn drop(&mut self) {
        let _cleanup = self.shutdown();
    }
}
