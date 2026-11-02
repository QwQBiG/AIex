//! Registration ownership is independent of the service's intent generation.
//! The service discards events from retired generations before submitting work.
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyBinding {
    pub id: i32,
    pub modifiers: u32,
    pub virtual_key: u16,
}

impl HotkeyBinding {
    pub fn validate(self) -> io::Result<()> {
        if !(1..=0xBFFF).contains(&self.id)
            || !(8..=254).contains(&self.virtual_key)
            || self.virtual_key == 0x7B
            || self.modifiers & !0x400F != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid or reserved global hotkey",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyEvent {
    pub id: i32,
    pub generation: u64,
}

pub trait HotkeySystem {
    fn register(&mut self, binding: HotkeyBinding) -> io::Result<()>;
    fn unregister(&mut self, id: i32) -> io::Result<()>;
}

pub struct HotkeyRegistrations<S: HotkeySystem> {
    system: S,
    ids: BTreeSet<i32>,
    generation: u64,
}

impl<S: HotkeySystem> HotkeyRegistrations<S> {
    pub fn register(system: S, bindings: &[HotkeyBinding], generation: u64) -> io::Result<Self> {
        if bindings.len() > 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "at most 16 global hotkeys are supported",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut chords = BTreeSet::new();
        for binding in bindings {
            binding.validate()?;
            if !ids.insert(binding.id)
                || !chords.insert((binding.modifiers & 0xF, binding.virtual_key))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate hotkey identifier or chord",
                ));
            }
        }
        let mut registered = Self {
            system,
            ids: BTreeSet::new(),
            generation,
        };
        for binding in bindings {
            if let Err(error) = registered.system.register(*binding) {
                let cleanup = registered.close();
                return Err(io::Error::new(
                    error.kind(),
                    format!("hotkey registration failed: {error}; rollback={cleanup:?}"),
                ));
            }
            registered.ids.insert(binding.id);
        }
        Ok(registered)
    }

    pub fn event(&self, id: i32) -> Option<HotkeyEvent> {
        self.ids.contains(&id).then_some(HotkeyEvent {
            id,
            generation: self.generation,
        })
    }

    pub fn close(&mut self) -> io::Result<()> {
        let ids: Vec<_> = self.ids.iter().copied().collect();
        let mut failed = Vec::new();
        for id in ids {
            if self.system.unregister(id).is_ok() {
                self.ids.remove(&id);
            } else {
                failed.push(id);
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "global hotkeys still owned: {failed:?}"
            )))
        }
    }
}

impl<S: HotkeySystem> Drop for HotkeyRegistrations<S> {
    fn drop(&mut self) {
        let _cleanup = self.close();
    }
}

#[cfg(test)]
mod tests;
