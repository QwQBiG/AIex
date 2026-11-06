use super::activity::{bump, validate_cursor};
use super::*;
use ai_ex_domain::{NativeDesktopPhase, NativeDesktopSession, NativeWindow};
use ai_ex_platform_windows::desktop::{TargetCheck, WindowIdentity, WindowSnapshot};
use ai_ex_safety::{ActionRequest, Capability};
use std::sync::atomic::Ordering;

impl<S: DesktopSystem + Send + 'static> NativeHost<S> {
    pub(super) async fn execute_owned(
        &self,
        request: NativeDesktopRequest,
    ) -> Result<NativeDesktopReply, AppError> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("native host closed"));
        }
        let mut state = self.state.lock().await;
        match &request {
            NativeDesktopRequest::Windows => {
                let windows = state
                    .desktop
                    .lock()
                    .map_err(|_| AppError::unavailable("adapter lock poisoned"))?
                    .windows()
                    .map_err(io_error)?;
                let mut reply = self.reply(Vec::new())?;
                // Long titles and many windows must still fit the control reply.
                for window in windows {
                    let Ok(summary) = window_summary(window) else {
                        continue;
                    };
                    if reply.windows.len() == 256 {
                        break;
                    }
                    reply.windows.push(summary);
                    if serde_json::to_vec(&reply)
                        .map_err(|error| AppError::protocol(error.to_string()))?
                        .len()
                        > 32 * 1024
                    {
                        reply.windows.pop();
                        break;
                    }
                }
                return Ok(reply);
            }
            NativeDesktopRequest::StartNative {
                instance_id,
                expected_revision,
                operation_id,
                window,
                max_frames,
                max_actions,
            } => {
                if state.start.as_ref().is_some_and(|prior| prior == &request) {
                    return self.reply(Vec::new());
                }
                if state.start.as_ref().is_some_and(|prior| matches!(prior,
                    NativeDesktopRequest::StartNative { operation_id: previous, .. } if previous == operation_id)) {
                    return Err(AppError::protocol("native start operation id was already used by another intent"));
                }
                if operation_id.is_nil()
                    || !(1..=256).contains(max_frames)
                    || !(1..=4096).contains(max_actions)
                {
                    return Err(AppError::configuration(
                        "native start needs bounded budgets and an operation id",
                    ));
                }
                if window.scope.len() > 4096 {
                    return Err(AppError::configuration(
                        "native executable scope exceeds 4096 bytes",
                    ));
                }
                let previous = self.snapshot()?;
                validate_cursor(&previous, *instance_id, *expected_revision, None, false)?;
                if previous.session.as_ref().is_some_and(|s| {
                    !matches!(
                        s.phase,
                        NativeDesktopPhase::Stopped | NativeDesktopPhase::Failed
                    )
                }) {
                    return Err(AppError::invalid_transition(
                        "stop the current native session before replacing it",
                    ));
                }
                self.safety.authorize(ActionRequest {
                    capability: Capability::ScreenRead,
                    target: window.scope.clone(),
                    rationale: "prepare explicitly selected native window".into(),
                })?;
                let audit = JsonlAuditLog::open(&self.audit_path).await?;
                let identity = WindowIdentity {
                    handle: window.handle,
                    process_id: window.process_id,
                    process_started_ticks: window.process_started_ticks,
                };
                let (ticket, current) = {
                    let mut desktop = state
                        .desktop
                        .lock()
                        .map_err(|_| AppError::unavailable("adapter lock poisoned"))?;
                    let released = desktop.release_owned();
                    if !released.still_owned.is_empty() {
                        return Err(AppError::safety("owned input release incomplete"));
                    }
                    let ticket = desktop.bind(identity).map_err(io_error)?;
                    let current = match desktop.check(&ticket).map_err(io_error)? {
                        TargetCheck::Ready(current) => current,
                        TargetCheck::NeedsConfirmation {
                            observed: Some(current),
                            ..
                        } => current,
                        _ => {
                            return Err(AppError::invalid_transition(
                                "selected window disappeared",
                            ));
                        }
                    };
                    (ticket, current)
                };
                if current.executable != window.scope {
                    return Err(AppError::safety("selected executable changed"));
                }
                let summary = window_summary(current)?;
                let mut projection = self
                    .shared
                    .snapshot
                    .lock()
                    .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
                validate_cursor(&projection, *instance_id, *expected_revision, None, false)?;
                self.shared.revoke(&mut projection)?;
                projection.session = Some(NativeDesktopSession {
                    session_id: Uuid::new_v4(),
                    operation_id: *operation_id,
                    window: summary,
                    geometry_revision: ticket.geometry_revision,
                    phase: NativeDesktopPhase::Prepared,
                    frames: 0,
                    actions: 0,
                    max_frames: *max_frames,
                    max_actions: *max_actions,
                    frame_id: None,
                    last_operation: None,
                    goal: String::new(),
                    armed_action: None,
                    last_outcome: Some("switch to the selected window, then confirm resume".into()),
                });
                state.ticket = Some(ticket);
                state.frame = None;
                state.start = Some(request.clone());
                state.seen.clear();
                let generation = self.shared.activity.load(Ordering::Acquire);
                state.coordinator = Some(ai_ex_automation::AutomationCoordinator::new(
                    self.safety.clone(),
                    NativeDesktopPort::new(
                        state.desktop.clone(),
                        ticket,
                        self.shared.cancelled.clone(),
                        self.shared.activity.clone(),
                        generation,
                    ),
                    native_audit(audit),
                ));
            }
            NativeDesktopRequest::Resume {
                instance_id,
                expected_revision,
                session_id,
            } => {
                self.resume(&mut state, *instance_id, *expected_revision, *session_id)
                    .await?;
            }
            NativeDesktopRequest::Capture {
                instance_id,
                expected_revision,
                session_id,
            } => {
                self.check_running(*instance_id, *expected_revision, *session_id)?;
                self.capture(&mut state).await?;
            }
            NativeDesktopRequest::SetGoal {
                instance_id,
                expected_revision,
                session_id,
                goal,
            } => {
                if goal.trim().is_empty() || goal.chars().count() > 1024 {
                    return Err(AppError::configuration(
                        "native goal needs 1..=1024 characters",
                    ));
                }
                let mut snapshot = self
                    .shared
                    .snapshot
                    .lock()
                    .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
                validate_cursor(
                    &snapshot,
                    *instance_id,
                    *expected_revision,
                    Some(*session_id),
                    false,
                )?;
                let session = snapshot.session.as_mut().expect("validated session");
                session.goal = goal.clone();
                session.armed_action = None;
                bump(&mut snapshot)?;
            }
            NativeDesktopRequest::SetAction {
                instance_id,
                expected_revision,
                session_id,
                action,
            } => {
                action.validate()?;
                let mut snapshot = self
                    .shared
                    .snapshot
                    .lock()
                    .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
                validate_cursor(
                    &snapshot,
                    *instance_id,
                    *expected_revision,
                    Some(*session_id),
                    false,
                )?;
                let session = snapshot.session.as_mut().expect("validated session");
                session.armed_action = Some(action.clone());
                session.goal.clear();
                bump(&mut snapshot)?;
            }
            NativeDesktopRequest::Execute { operation_id, .. }
            | NativeDesktopRequest::VisionStep { operation_id, .. } => {
                if let Some(prior) = state.seen.get(operation_id) {
                    if prior == &request {
                        return self.reply(Vec::new());
                    }
                    return Err(AppError::protocol(
                        "native operation id was already used by another intent",
                    ));
                }
                self.step(&mut state, request.clone()).await?;
            }
            NativeDesktopRequest::PreviewVision { .. } => {
                return self.preview_vision(&mut state, &request).await;
            }
            NativeDesktopRequest::PrepareVisionProposal { .. } => {
                return self.preview_vision(&mut state, &request).await;
            }
            NativeDesktopRequest::DiscardVisionProposal { .. } => {
                return self.discard_vision_proposal(&request);
            }
            NativeDesktopRequest::ConfirmVisionProposal { .. } => {
                return self.confirm_vision_proposal(&mut state, &request).await;
            }
            _ => return Err(AppError::protocol("native request was not admitted")),
        }
        self.reply(Vec::new())
    }

    async fn resume(
        &self,
        state: &mut State<S>,
        instance: Uuid,
        revision: u64,
        session_id: Uuid,
    ) -> Result<(), AppError> {
        let snapshot = self.snapshot()?;
        validate_cursor(&snapshot, instance, revision, Some(session_id), false)?;
        let previous = snapshot.session.as_ref().expect("validated session");
        if matches!(
            previous.phase,
            NativeDesktopPhase::Stopped | NativeDesktopPhase::Failed
        ) {
            return Err(AppError::invalid_transition(
                "terminal native sessions cannot resume",
            ));
        }
        if self.shared.pending.load(Ordering::Acquire) != 0
            || self.speech.snapshot() != ai_ex_audio::SpeechActivitySnapshot::Idle
        {
            return Err(AppError::invalid_transition(
                "wait for conversation and playback to become idle",
            ));
        }
        let (ticket, observed, ready) = {
            let mut desktop = state
                .desktop
                .lock()
                .map_err(|_| AppError::unavailable("adapter lock poisoned"))?;
            let release = desktop.release_owned();
            if !release.still_owned.is_empty() {
                return Err(AppError::safety("owned input release incomplete"));
            }
            let ticket = desktop
                .bind(
                    state
                        .ticket
                        .ok_or_else(|| AppError::invalid_transition("no native binding"))?
                        .identity,
                )
                .map_err(io_error)?;
            match desktop.check(&ticket).map_err(io_error)? {
                TargetCheck::Ready(observed) => (ticket, observed, true),
                TargetCheck::NeedsConfirmation {
                    observed: Some(observed),
                    ..
                } => (ticket, observed, false),
                _ => {
                    return Err(AppError::invalid_transition(
                        "native window identity changed",
                    ));
                }
            }
        };
        if observed.executable != previous.window.scope {
            return Err(AppError::safety("native executable scope changed"));
        }
        let summary = window_summary(observed)?;
        let geometry_matches = previous.window.left == summary.left
            && previous.window.top == summary.top
            && previous.window.width == summary.width
            && previous.window.height == summary.height
            && previous.window.dpi == summary.dpi;
        let audit = JsonlAuditLog::open(&self.audit_path).await?;
        let mut projection = self
            .shared
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
        validate_cursor(&projection, instance, revision, Some(session_id), false)?;
        self.shared.revoke(&mut projection)?;
        let current = projection.session.as_mut().expect("validated session");
        current.window = summary;
        current.geometry_revision = ticket.geometry_revision;
        current.frame_id = None;
        current.phase = if ready && geometry_matches {
            NativeDesktopPhase::Running
        } else {
            NativeDesktopPhase::Prepared
        };
        current.last_outcome = Some(
            if ready && geometry_matches {
                "ready"
            } else {
                "confirm changed geometry or switch to target window"
            }
            .into(),
        );
        self.shared
            .cancelled
            .store(!(ready && geometry_matches), Ordering::Release);
        state.ticket = Some(ticket);
        state.frame = None;
        state.coordinator = Some(AutomationCoordinator::new(
            self.safety.clone(),
            NativeDesktopPort::new(
                state.desktop.clone(),
                ticket,
                self.shared.cancelled.clone(),
                self.shared.activity.clone(),
                self.shared.activity.load(Ordering::Acquire),
            ),
            native_audit(audit),
        ));
        bump(&mut projection)
    }
}

fn window_summary(window: WindowSnapshot) -> Result<NativeWindow, AppError> {
    if window.executable.len() > 4096 || window.executable.trim().is_empty() {
        return Err(AppError::configuration(
            "native executable scope is empty or exceeds 4096 bytes",
        ));
    }
    Ok(NativeWindow {
        handle: window.identity.handle,
        process_id: window.identity.process_id,
        process_started_ticks: window.identity.process_started_ticks,
        title: window.title.chars().take(256).collect(),
        scope: window.executable,
        left: window.client.left,
        top: window.client.top,
        width: window.client.width,
        height: window.client.height,
        dpi: window.dpi,
        foreground: window.foreground,
    })
}
