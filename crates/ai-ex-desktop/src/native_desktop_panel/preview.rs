use super::*;
use ai_ex_domain::{NativeDesktopAction, NativeDesktopPhase, NativeVisionPreview};
use std::sync::atomic::Ordering;

pub(super) struct ArmedPreview {
    instance: Uuid,
    revision: u64,
    session: Uuid,
    goal: String,
    pub(super) send_at: Instant,
}

pub(super) struct DisplayedPreview {
    pub(super) value: NativeVisionPreview,
    pub(super) revision: u64,
    pub(super) session: Uuid,
}

impl NativeDesktopPanel {
    pub(super) fn preview_available(&self) -> bool {
        let Some(state) = &self.confirmed else {
            return false;
        };
        let Some(session) = &state.session else {
            return false;
        };
        self.connected
            && self.fresh
            && state.available
            && !self.manual_mode
            && session.phase == NativeDesktopPhase::Running
            && session.armed_action.is_none()
            && !session.goal.trim().is_empty()
            && session.goal == self.goal
            && !self.pending.values().any(|request| lane(request) == 1)
            && self.preview_guard.load(Ordering::Acquire) < u64::MAX
    }

    pub(super) fn arm_preview(&mut self) {
        if !self.preview_available() || self.preview_arm.is_some() {
            return;
        }
        let Some(state) = &self.confirmed else { return };
        let Some(session) = &state.session else {
            return;
        };
        self.preview = None;
        self.preview_arm = Some(ArmedPreview {
            instance: state.instance_id,
            revision: state.revision,
            session: session.session_id,
            goal: session.goal.clone(),
            send_at: Instant::now() + Duration::from_secs(3),
        });
        self.preview_message = Some("请在倒计时结束前切回专用窗口；仅分析，不发送键鼠。");
    }

    pub fn cancel_preview(&mut self) {
        if self.preview_arm.is_none() && self.preview.is_none() && self.preview_pending.is_none() {
            return;
        }
        let was_sent = self.preview_pending.is_some();
        let _ = self
            .preview_guard
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            });
        self.preview_arm = None;
        self.preview = None;
        self.preview_pending = None;
        self.preview_message =
            was_sent.then_some("预览请求已提交，可能仍在处理；结果将被丢弃，不会自动重试。");
    }

    pub(super) fn dispatch_preview(&mut self) -> Option<WorkerCommand> {
        let arm = self.preview_arm.take()?;
        let send_before = arm.send_at + Duration::from_secs(2);
        if Instant::now() >= send_before {
            self.preview_message = Some("预览等待过久，未发送；请重新准备。");
            return None;
        }
        let valid = self.preview_available()
            && self.confirmed.as_ref().is_some_and(|state| {
                state.instance_id == arm.instance
                    && state.revision == arm.revision
                    && state.session.as_ref().is_some_and(|session| {
                        session.session_id == arm.session && session.goal == arm.goal
                    })
            });
        if !valid {
            self.preview_message = Some("状态已变化，预览未发送；请重新核对。");
            return None;
        }
        let request = NativeDesktopRequest::PreviewVision {
            instance_id: arm.instance,
            expected_revision: arm.revision,
            session_id: arm.session,
            operation_id: Uuid::new_v4(),
            goal: arm.goal,
        };
        self.sequence = self.sequence.checked_add(1)?;
        let generation = self.preview_guard.load(Ordering::Acquire);
        self.pending.insert(self.sequence, request.clone());
        self.preview_pending = Some((self.sequence, generation));
        self.preview_message = Some("预览请求已提交，可能仍在处理；不会自动重试。");
        Some(WorkerCommand::NativeDesktopPreview {
            request_id: self.sequence,
            request,
            generation,
            guard: Arc::clone(&self.preview_guard),
            send_before,
        })
    }

    pub(super) fn preview_matches(
        &self,
        request: &NativeDesktopRequest,
        reply: &NativeDesktopReply,
    ) -> bool {
        let NativeDesktopRequest::PreviewVision {
            instance_id,
            session_id,
            operation_id,
            goal,
            ..
        } = request
        else {
            return false;
        };
        reply.snapshot.instance_id == *instance_id
            && self.goal == *goal
            && !self.manual_mode
            && reply.snapshot.session.as_ref().is_some_and(|session| {
                session.session_id == *session_id
                    && session.goal == *goal
                    && session.phase == NativeDesktopPhase::Running
                    && session.armed_action.is_none()
            })
            && reply.vision_preview.as_ref().is_some_and(|preview| {
                preview.operation_id == *operation_id && preview.action.validate().is_ok()
            })
    }

    pub(super) fn observe_preview_snapshot(&mut self, reply: &NativeDesktopReply) {
        let session = reply.snapshot.session.as_ref();
        let armed_changed = self.preview_arm.as_ref().is_some_and(|arm| {
            arm.instance != reply.snapshot.instance_id
                || arm.revision != reply.snapshot.revision
                || !session.is_some_and(|current| {
                    current.session_id == arm.session
                        && current.goal == arm.goal
                        && current.phase == NativeDesktopPhase::Running
                })
        });
        let shown_changed = self.preview.as_ref().is_some_and(|preview| {
            preview.revision != reply.snapshot.revision
                || !session.is_some_and(|current| {
                    current.session_id == preview.session
                        && current.phase == NativeDesktopPhase::Running
                })
        });
        let pending_changed = self.preview_pending.is_some()
            && !session.is_some_and(|current| {
                current.phase == NativeDesktopPhase::Running
                    && current.goal == self.goal
                    && !self.manual_mode
            });
        if armed_changed || shown_changed || pending_changed {
            self.cancel_preview();
        }
    }
}

pub(super) fn summary(action: &NativeDesktopAction) -> String {
    use NativeDesktopAction::*;
    match action {
        Click { x, y, button } => format!("建议点击 ({x}, {y}) · {button:?}"),
        KeyChord { keys } => format!("建议按键：{}", keys.join(" + ")),
        TypeText { text } => format!("建议输入文字（{} 字，内容未显示）", text.chars().count()),
        Drag {
            from_x,
            from_y,
            to_x,
            to_y,
            ..
        } => format!("建议拖动 ({from_x}, {from_y}) → ({to_x}, {to_y})"),
        Scroll { horizontal, delta } => format!(
            "建议{}滚动 {delta}",
            if *horizontal { "水平" } else { "垂直" }
        ),
        Wait { milliseconds } => format!("建议等待 {milliseconds} 毫秒"),
        Noop => "建议不执行动作".into(),
    }
}
