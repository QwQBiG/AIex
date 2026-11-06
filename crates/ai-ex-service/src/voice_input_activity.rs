//! Tracks active capture/transcription without changing dialogue or playback state.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use ai_ex_domain::{AppError, VoiceInputState};
use ai_ex_observability::EventHub;

#[derive(Clone)]
pub(crate) struct VoiceInputActivity {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    events: EventHub,
}

#[derive(Default)]
struct State {
    revision: u64,
    next_id: u64,
    leases: BTreeSet<u64>,
}

pub(crate) struct VoiceInputLease {
    inner: Arc<Inner>,
    id: u64,
}

impl VoiceInputActivity {
    pub(crate) fn new(events: EventHub) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                events,
            }),
        }
    }

    pub(crate) fn begin(&self) -> Result<VoiceInputLease, AppError> {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Reserve one revision for each live lease's eventual release as well.
        let needed = u64::try_from(state.leases.len())
            .ok()
            .and_then(|n| n.checked_add(2))
            .ok_or_else(exhausted)?;
        state.revision.checked_add(needed).ok_or_else(exhausted)?;
        let id = state.next_id.checked_add(1).ok_or_else(exhausted)?;
        state.next_id = id;
        state.revision += 1;
        state.leases.insert(id);
        self.inner.events.observe_voice_input(VoiceInputState {
            revision: state.revision,
            active: true,
        });
        Ok(VoiceInputLease {
            inner: self.inner.clone(),
            id,
        })
    }
}

impl Drop for VoiceInputLease {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.leases.remove(&self.id) {
            // begin() reserved a revision for every outstanding lease.
            state.revision += 1;
            self.inner.events.observe_voice_input(VoiceInputState {
                revision: state.revision,
                active: !state.leases.is_empty(),
            });
        }
    }
}

fn exhausted() -> AppError {
    AppError::unavailable("voice activity identity exhausted")
}

#[cfg(test)]
#[path = "voice_input_activity_tests.rs"]
mod tests;
