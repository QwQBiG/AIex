//! Explicit visual inspection. Suggestions from this path are never dispatched.

use super::activity::{bump, validate_cursor};
use super::*;
use ai_ex_domain::{NativeDesktopPhase, NativeVisionPreview};
use ai_ex_platform_windows::desktop::TargetCheck;
use std::time::Duration;

impl<S: DesktopSystem + Send + 'static> NativeHost<S> {
    pub(super) async fn preview_vision(
        &self,
        state: &mut State<S>,
        request: &NativeDesktopRequest,
    ) -> Result<NativeDesktopReply, AppError> {
        let NativeDesktopRequest::PreviewVision {
            instance_id,
            expected_revision,
            session_id,
            operation_id,
            goal,
        } = request
        else {
            return Err(AppError::protocol("vision preview intent required"));
        };
        if operation_id.is_nil() || goal.trim().is_empty() || goal.chars().count() > 1024 {
            return Err(AppError::configuration("invalid vision preview intent"));
        }
        let generation = self.check_running(*instance_id, *expected_revision, *session_id)?;
        let before = self.snapshot()?;
        validate_cursor(
            &before,
            *instance_id,
            *expected_revision,
            Some(*session_id),
            false,
        )?;
        let session = before.session.as_ref().expect("validated session");
        if session.goal != *goal || session.armed_action.is_some() {
            return Err(AppError::invalid_transition(
                "save the vision goal before previewing",
            ));
        }
        if state.seen.contains_key(operation_id) {
            return Err(AppError::invalid_transition(
                "native operation id was already used",
            ));
        }
        if state.seen.len() >= 4096 {
            return Err(AppError::unavailable("native operation capacity exhausted"));
        }
        let scope = session.window.scope.clone();
        state.seen.insert(*operation_id, request.clone());

        let analyzed = self.vision_action(state, request, generation).await;
        let checked = analyzed.and_then(|action| {
            self.guard(generation)?;
            let (frame_id, captured, _) = state
                .frame
                .as_ref()
                .ok_or_else(|| AppError::invalid_transition("preview frame unavailable"))?;
            if captured.elapsed() > Duration::from_secs(5) {
                return Err(AppError::invalid_transition("preview frame expired"));
            }
            let ticket = state
                .ticket
                .ok_or_else(|| AppError::invalid_transition("native target unavailable"))?;
            let observed = state
                .desktop
                .lock()
                .map_err(|_| AppError::unavailable("native adapter lock poisoned"))?
                .check(&ticket)
                .map_err(io_error)?;
            if !matches!(observed, TargetCheck::Ready(ref target) if target.executable == scope) {
                return Err(AppError::invalid_transition(
                    "native target changed during preview",
                ));
            }
            self.guard(generation)?;
            Ok((action, *frame_id, ticket))
        });

        // A preview never leaves a frame that can be passed to Execute.
        state.frame = None;
        let mut snapshot = self
            .shared
            .snapshot
            .lock()
            .map_err(|_| AppError::unavailable("projection lock poisoned"))?;
        let previous_frame = snapshot
            .session
            .as_mut()
            .filter(|session| session.session_id == *session_id)
            .and_then(|session| session.frame_id.take());
        if previous_frame.is_some() {
            bump(&mut snapshot)?;
        }
        let (action, frame_id, ticket) = checked?;
        self.guard(generation)?;
        let session = snapshot
            .session
            .as_ref()
            .ok_or_else(|| AppError::invalid_transition("native session disappeared"))?;
        if snapshot.instance_id != *instance_id
            || session.session_id != *session_id
            || session.phase != NativeDesktopPhase::Running
            || session.geometry_revision != ticket.geometry_revision
            || session.window.scope != scope
            || session.goal != *goal
            || session.armed_action.is_some()
            || previous_frame != Some(frame_id)
        {
            return Err(AppError::invalid_transition(
                "native preview context changed",
            ));
        }
        Ok(NativeDesktopReply {
            snapshot: snapshot.clone(),
            windows: Vec::new(),
            vision_preview: Some(NativeVisionPreview {
                operation_id: *operation_id,
                action,
            }),
        })
    }
}
