//! Native sessions remain separate from synthetic template replay and permissions.
use crate::worker::WorkerCommand;
use ai_ex_domain::{
    AppError, NativeDesktopReply, NativeDesktopRequest, NativeDesktopSnapshot, NativeWindow,
};
use eframe::egui;
use std::time::{Duration, Instant};
use uuid::Uuid;

mod action;
#[cfg(test)]
mod tests;
mod view;

pub struct NativeDesktopPanel {
    instance: Option<Uuid>,
    connected: bool,
    confirmed: Option<NativeDesktopSnapshot>,
    windows: Vec<NativeWindow>,
    selection: Option<usize>,
    sequence: u64,
    pending: std::collections::BTreeMap<u64, NativeDesktopRequest>,
    activated: bool,
    poll_at: Option<Instant>,
    fresh: bool,
    goal: String,
    manual_mode: bool,
    action: action::ActionDraft,
    max_frames: u32,
    max_actions: u32,
    error: Option<String>,
}

impl Default for NativeDesktopPanel {
    fn default() -> Self {
        Self {
            instance: None,
            connected: false,
            confirmed: None,
            windows: Vec::new(),
            selection: None,
            sequence: 0,
            pending: Default::default(),
            activated: false,
            poll_at: None,
            fresh: false,
            goal: String::new(),
            manual_mode: true,
            action: Default::default(),
            max_frames: 32,
            max_actions: 64,
            error: None,
        }
    }
}

impl NativeDesktopPanel {
    pub fn synchronize(&mut self, instance: Option<Uuid>, connected: bool) {
        if self.instance != instance {
            self.instance = instance;
            self.confirmed = None;
            self.windows.clear();
            self.selection = None;
            self.pending.clear();
            self.fresh = false;
        }
        if !connected {
            self.fresh = false;
            self.pending.clear();
            self.poll_at = None;
        }
        self.connected = connected && instance.is_some();
    }

    fn begin(&mut self, request: NativeDesktopRequest) -> Option<WorkerCommand> {
        if !self.connected || self.pending.len() >= 8 {
            return None;
        }
        let lane = lane(&request);
        if self.pending.values().any(|old| lane == self::lane(old)) {
            return None;
        }
        self.sequence = self.sequence.checked_add(1)?;
        self.pending.insert(self.sequence, request.clone());
        self.activated = true;
        Some(WorkerCommand::NativeDesktop {
            request_id: self.sequence,
            request,
        })
    }

    pub fn poll(&mut self) -> Option<WorkerCommand> {
        if !self.activated
            || self
                .poll_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return None;
        }
        self.poll_at = Some(Instant::now());
        self.begin(NativeDesktopRequest::Status)
    }

    pub fn receive(&mut self, id: u64, result: Result<NativeDesktopReply, AppError>) {
        let Some(request) = self.pending.remove(&id) else {
            return;
        };
        match result {
            Ok(reply) if self.connected && Some(reply.snapshot.instance_id) == self.instance => {
                if self
                    .confirmed
                    .as_ref()
                    .is_some_and(|old| old.revision > reply.snapshot.revision)
                {
                    return;
                }
                if matches!(request, NativeDesktopRequest::Windows) {
                    self.windows = reply.windows;
                    self.selection = None;
                }
                let previous = self
                    .confirmed
                    .as_ref()
                    .and_then(|state| state.session.as_ref());
                let next = reply.snapshot.session.as_ref();
                if previous.map(|session| session.session_id)
                    != next.map(|session| session.session_id)
                    || matches!(
                        request,
                        NativeDesktopRequest::SetGoal { .. }
                            | NativeDesktopRequest::SetAction { .. }
                    )
                {
                    self.goal = next.map(|session| session.goal.clone()).unwrap_or_default();
                    if let Some(session) = next {
                        self.manual_mode =
                            session.armed_action.is_some() || session.goal.is_empty();
                        self.action = session
                            .armed_action
                            .as_ref()
                            .map(action::ActionDraft::from_action)
                            .unwrap_or_else(|| action::ActionDraft::centered(&session.window));
                    }
                }
                self.confirmed = Some(reply.snapshot);
                self.fresh = true;
                self.error = None;
            }
            Ok(_) => {}
            Err(error) => {
                self.fresh = false;
                self.error = Some(format!("操作结果待核对，请刷新；不会自动重试：{error}"));
            }
        }
    }

    fn start_request(&self) -> Option<NativeDesktopRequest> {
        let state = self.confirmed.as_ref()?;
        let window = self.windows.get(self.selection?)?.clone();
        if !self.connected
            || !self.fresh
            || !state.available
            || state.session.as_ref().is_some_and(|session| {
                !matches!(
                    session.phase,
                    ai_ex_domain::NativeDesktopPhase::Stopped
                        | ai_ex_domain::NativeDesktopPhase::Failed
                )
            })
        {
            return None;
        }
        Some(NativeDesktopRequest::StartNative {
            instance_id: state.instance_id,
            expected_revision: state.revision,
            operation_id: Uuid::new_v4(),
            window,
            max_frames: self.max_frames.clamp(1, 256),
            max_actions: self.max_actions.clamp(1, 4096),
        })
    }
}

fn lane(request: &NativeDesktopRequest) -> u8 {
    match request {
        NativeDesktopRequest::Status | NativeDesktopRequest::Windows => 0,
        NativeDesktopRequest::Pause { .. } => 2,
        NativeDesktopRequest::Stop { .. } => 3,
        _ => 1,
    }
}
