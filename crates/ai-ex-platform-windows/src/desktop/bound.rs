use std::collections::BTreeSet;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::*;

pub struct BoundDesktop<S: DesktopSystem> {
    pub(super) system: S,
    pub(super) ticket: Option<WindowTicket>,
    pub(super) revision: u64,
    pub(super) owned: BTreeSet<OwnedInput>,
}

impl<S: DesktopSystem> BoundDesktop<S> {
    pub fn new(system: S) -> Self {
        Self {
            system,
            ticket: None,
            revision: 0,
            owned: BTreeSet::new(),
        }
    }
    pub fn windows(&mut self) -> io::Result<Vec<WindowSnapshot>> {
        self.system.windows()
    }
    pub fn ticket(&self) -> Option<WindowTicket> {
        self.ticket
    }
    pub fn owned_inputs(&self) -> Vec<OwnedInput> {
        self.owned.iter().copied().collect()
    }
    pub fn shutdown(&mut self) -> io::Result<ReleaseReport> {
        let released = self.release_owned();
        let capture = self.system.shutdown();
        if !released.still_owned.is_empty() {
            return Err(io::Error::other(format!(
                "owned input release incomplete: {:?}; capture={capture:?}",
                released.still_owned
            )));
        }
        capture?;
        Ok(released)
    }

    /// Rebinding is explicit. Merely observing moved/resized windows never
    /// changes an existing ticket or admits a paused session.
    pub fn bind(&mut self, identity: WindowIdentity) -> io::Result<WindowTicket> {
        if !self.owned.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "release owned input before binding",
            ));
        }
        let snapshot = self.system.inspect(identity.handle)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "target window no longer exists")
        })?;
        if snapshot.identity != identity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "target window identity changed",
            ));
        }
        snapshot.client.validate()?;
        snapshot.desktop.validate()?;
        if snapshot.dpi == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "target DPI unavailable",
            ));
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("window geometry revision exhausted"))?;
        let ticket = WindowTicket {
            identity,
            client: snapshot.client,
            desktop: snapshot.desktop,
            dpi: snapshot.dpi,
            geometry_revision: self.revision,
        };
        self.ticket = Some(ticket);
        Ok(ticket)
    }

    pub fn check(&mut self, ticket: &WindowTicket) -> io::Result<TargetCheck> {
        let changed = |reason, observed| Ok(TargetCheck::NeedsConfirmation { reason, observed });
        if self.ticket != Some(*ticket) {
            return changed(TargetChange::TicketRetired, None);
        }
        let Some(current) = self.system.inspect(ticket.identity.handle)? else {
            return changed(TargetChange::WindowGone, None);
        };
        if current.identity != ticket.identity {
            return changed(TargetChange::IdentityChanged, Some(current));
        }
        if current.client != ticket.client
            || current.desktop != ticket.desktop
            || current.dpi != ticket.dpi
        {
            return changed(TargetChange::GeometryChanged, Some(current));
        }
        if !current.foreground {
            return changed(TargetChange::NotForeground, Some(current));
        }
        Ok(TargetCheck::Ready(current))
    }

    pub fn capture<G>(
        &mut self,
        ticket: &WindowTicket,
        _allowance: DispatchAllowance,
        timeout: Duration,
        cancelled: &AtomicBool,
        mut guard: G,
    ) -> Result<RgbaFrame, InputFailure>
    where
        G: FnMut() -> io::Result<()>,
    {
        let result = (|| {
            let snapshot = self.admit(ticket, cancelled, &mut guard)?;
            if timeout.is_zero() || timeout > Duration::from_secs(5) {
                return Err((
                    InputFailureKind::InvalidAction,
                    "capture timeout must be in 1..=5000 ms".into(),
                ));
            }
            let frame = match self.system.capture(&snapshot, timeout, cancelled) {
                Ok(frame) => frame,
                Err(error) => {
                    // Preserve a concurrent stop, permission revocation, or
                    // target change instead of reporting it as a GDI failure.
                    self.admit(ticket, cancelled, &mut guard)?;
                    return Err((InputFailureKind::SystemCall, error.to_string()));
                }
            };
            self.admit(ticket, cancelled, &mut guard)?;
            frame
                .validate()
                .map_err(|e| (InputFailureKind::SystemCall, e.to_string()))?;
            if frame.width != ticket.client.width || frame.height != ticket.client.height {
                return Err((
                    InputFailureKind::TargetChanged,
                    "capture dimensions changed".into(),
                ));
            }
            Ok(frame)
        })();
        result.map_err(|(kind, detail)| self.failure(kind, detail, InputReceipt::default()))
    }

    pub(super) fn admit<G>(
        &mut self,
        ticket: &WindowTicket,
        cancelled: &AtomicBool,
        guard: &mut G,
    ) -> Result<WindowSnapshot, (InputFailureKind, String)>
    where
        G: FnMut() -> io::Result<()>,
    {
        if cancelled.load(Ordering::Acquire) {
            return Err((
                InputFailureKind::Cancelled,
                "native action cancelled".into(),
            ));
        }
        guard().map_err(|e| (InputFailureKind::PermissionRevoked, e.to_string()))?;
        match self
            .check(ticket)
            .map_err(|e| (InputFailureKind::SystemCall, e.to_string()))?
        {
            TargetCheck::Ready(snapshot) => Ok(snapshot),
            TargetCheck::NeedsConfirmation { reason, .. } => Err((
                InputFailureKind::TargetChanged,
                format!("target needs confirmation: {reason:?}"),
            )),
        }
    }

    pub(super) fn failure(
        &mut self,
        kind: InputFailureKind,
        detail: String,
        receipt: InputReceipt,
    ) -> InputFailure {
        InputFailure {
            kind,
            detail,
            receipt,
            cleanup: self.release_owned(),
        }
    }
}

impl<S: DesktopSystem> Drop for BoundDesktop<S> {
    fn drop(&mut self) {
        let _cleanup = self.release_owned();
    }
}
