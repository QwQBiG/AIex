use std::io;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use super::actions::{self, Step};
use super::*;

impl<S: DesktopSystem> BoundDesktop<S> {
    pub fn execute<G>(
        &mut self,
        ticket: &WindowTicket,
        _allowance: DispatchAllowance,
        action: &NativeAction,
        cancelled: &AtomicBool,
        mut guard: G,
    ) -> Result<InputReceipt, InputFailure>
    where
        G: FnMut() -> io::Result<()>,
    {
        let mut receipt = InputReceipt::default();
        let result = (|| {
            if !self.owned.is_empty() {
                return Err((
                    InputFailureKind::ReleaseIncomplete,
                    "previous owned input is not released".into(),
                ));
            }
            self.admit(ticket, cancelled, &mut guard)?;
            let plan =
                actions::plan(ticket, action).map_err(|e| (InputFailureKind::InvalidAction, e))?;
            for step in plan {
                self.admit(ticket, cancelled, &mut guard)?;
                match step {
                    Step::Batch(events) => {
                        self.batch(&events, &mut receipt, ticket, cancelled, &mut guard)?
                    }
                    Step::Delay(duration) => {
                        let started = Instant::now();
                        while started.elapsed() < duration {
                            self.admit(ticket, cancelled, &mut guard)?;
                            std::thread::sleep(
                                duration
                                    .saturating_sub(started.elapsed())
                                    .min(Duration::from_millis(10)),
                            );
                        }
                    }
                    Step::CursorInside => {
                        let (x, y) = self.system.cursor().map_err(system_error)?;
                        if !ticket.client.contains(x, y) || !ticket.desktop.contains(x, y) {
                            return Err((
                                InputFailureKind::TargetChanged,
                                "cursor outside bound client".into(),
                            ));
                        }
                    }
                }
            }
            self.admit(ticket, cancelled, &mut guard)?;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(receipt),
            Err((kind, detail)) => Err(self.failure(kind, detail, receipt)),
        }
    }

    fn batch<G>(
        &mut self,
        events: &[InputEvent],
        receipt: &mut InputReceipt,
        ticket: &WindowTicket,
        cancelled: &AtomicBool,
        guard: &mut G,
    ) -> Result<(), (InputFailureKind, String)>
    where
        G: FnMut() -> io::Result<()>,
    {
        // User-held modifiers can also turn click/scroll into a different OS
        // gesture, so they are checked for every input batch.
        if !events.is_empty() {
            for value in [0x10, 0x11, 0x12, 0x5B, 0x5C] {
                let key = KeyCode::new(value).expect("constant virtual key");
                let owned = self.owned.iter().any(|input| {
                    matches!(input,
                    OwnedInput::Key(held) if held.family() == key.family())
                });
                if !owned
                    && self
                        .system
                        .is_pressed(OwnedInput::Key(key))
                        .map_err(system_error)?
                {
                    return Err((
                        InputFailureKind::AlreadyPressed,
                        "user modifier is already held".into(),
                    ));
                }
            }
        }
        for event in events {
            if let Some((input, true)) = held_event(*event)
                && (self.owned.contains(&input)
                    || self.system.is_pressed(input).map_err(system_error)?)
            {
                return Err((
                    InputFailureKind::AlreadyPressed,
                    "requested input is already held".into(),
                ));
            }
        }
        // Physical-state queries do not authorize injection. Recheck the live
        // target and permission after them, immediately before SendInput.
        self.admit(ticket, cancelled, guard)?;
        receipt.attempted_events += events.len();
        let count = self.system.send_input(events).map_err(system_error)?;
        let accepted = count.min(events.len());
        receipt.accepted_events += accepted;
        for event in &events[..accepted] {
            if let Some((input, down)) = held_event(*event) {
                if down {
                    self.owned.insert(input);
                } else {
                    self.owned.remove(&input);
                }
            }
        }
        if count != events.len() {
            return Err((
                InputFailureKind::PartialInsertion,
                format!(
                    "input batch accepted {accepted}/{}; action is not replayed",
                    events.len()
                ),
            ));
        }
        Ok(())
    }

    /// Cleanup is independent of focus and permission revocation, but can only
    /// release input whose Down was accepted from this adapter's own batch.
    pub fn release_owned(&mut self) -> ReleaseReport {
        let inputs: Vec<_> = self.owned.iter().rev().copied().collect();
        let events: Vec<_> = inputs.iter().map(|input| release_event(*input)).collect();
        let count = if events.is_empty() {
            0
        } else {
            self.system
                .send_input(&events)
                .unwrap_or(0)
                .min(events.len())
        };
        for input in &inputs[..count] {
            self.owned.remove(input);
        }
        ReleaseReport {
            attempted: events.len(),
            released: count,
            still_owned: self.owned_inputs(),
        }
    }
}

fn system_error(error: io::Error) -> (InputFailureKind, String) {
    (InputFailureKind::SystemCall, error.to_string())
}

fn held_event(event: InputEvent) -> Option<(OwnedInput, bool)> {
    match event {
        InputEvent::Button { button, down } => Some((OwnedInput::Mouse(button), down)),
        InputEvent::Key { key, down } => Some((OwnedInput::Key(key), down)),
        InputEvent::Unicode { unit, down } => Some((OwnedInput::Unicode(unit), down)),
        _ => None,
    }
}

fn release_event(input: OwnedInput) -> InputEvent {
    match input {
        OwnedInput::Mouse(button) => InputEvent::Button {
            button,
            down: false,
        },
        OwnedInput::Key(key) => InputEvent::Key { key, down: false },
        OwnedInput::Unicode(unit) => InputEvent::Unicode { unit, down: false },
    }
}
