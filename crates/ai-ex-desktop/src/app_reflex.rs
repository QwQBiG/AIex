//! Server-confirmed controls for bounded, synthetic template replay only.
//! Closing the desktop does not cancel an action already issued by the host.

use ai_ex_domain::{
    AppError, AutomationControlMode, AutomationPauseReason, AutomationRequest,
    AutomationResumeEligibility, AutomationSessionPhase, AutomationSnapshot, AutomationStepOutcome,
    ErrorKind,
};
use ai_ex_ui_model::ConnectionState;
use eframe::egui;
use uuid::Uuid;

use super::DesktopApp;
use crate::worker::WorkerCommand;

#[cfg(test)]
#[path = "app_reflex_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
enum Intent {
    Status,
    Start,
    Pause,
    Resume,
    Stop,
}

impl Intent {
    fn lane(self) -> usize {
        match self {
            Self::Status => 0,
            Self::Start | Self::Resume => 1,
            Self::Pause => 2,
            Self::Stop => 3,
        }
    }
}

struct Pending {
    id: u64,
    request: AutomationRequest,
    uncertain: bool,
}

#[derive(Default)]
pub(super) struct ReflexPanel {
    manifest: String,
    confirmed: Option<AutomationSnapshot>,
    instance: Option<Uuid>,
    pending: [Option<Pending>; 4],
    next_id: u64,
    activated: bool,
    fresh: bool,
    ready: bool,
    error: Option<String>,
}

impl ReflexPanel {
    fn synchronize(&mut self, instance: Option<Uuid>, connected: bool) {
        if self.instance != instance {
            self.instance = instance;
            self.confirmed = None;
            self.pending = Default::default();
            self.activated = false;
            self.fresh = false;
            self.error = None;
        }
        self.ready = connected;
        if !connected {
            self.fresh = false;
        }
    }

    pub(super) fn disconnected(&mut self) {
        self.fresh = false;
        self.ready = false;
        // A disconnected transport says nothing about server-side completion.
        for pending in self.pending.iter_mut().flatten() {
            pending.uncertain = true;
        }
    }

    fn mutation_pending(&self) -> bool {
        self.pending[1..].iter().any(Option::is_some)
    }

    fn request(&self, intent: Intent, connected: bool) -> Option<AutomationRequest> {
        if !connected
            || !self.ready
            || self.instance.is_none()
            || self.pending[intent.lane()].is_some()
        {
            return None;
        }
        if matches!(intent, Intent::Status) {
            return Some(AutomationRequest::Status);
        }
        let snapshot = self.confirmed.as_ref()?;
        if !snapshot.available || snapshot.instance_id != self.instance? {
            return None;
        }
        // Priority Stop remains available with a known cursor after an ambiguous
        // result. It binds the same session and cannot stop a replacement session.
        if !self.fresh && !matches!(intent, Intent::Stop) {
            return None;
        }
        let instance_id = snapshot.instance_id;
        let expected_revision = snapshot.revision;
        if matches!(intent, Intent::Start) {
            if self.mutation_pending()
                || snapshot
                    .session
                    .as_ref()
                    .is_some_and(|session| session.phase != AutomationSessionPhase::Finished)
            {
                return None;
            }
            let request = AutomationRequest::StartTemplate {
                instance_id,
                expected_revision,
                operation_id: Uuid::from_u128(1),
                manifest: self.manifest.clone(),
            };
            return request.validate().ok().map(|()| request);
        }
        let session = snapshot.session.as_ref()?;
        if session.phase == AutomationSessionPhase::Finished
            || session.control_mode == AutomationControlMode::Stopped
        {
            return None;
        }
        let session_id = session.session_id;
        match intent {
            Intent::Pause => Some(AutomationRequest::Pause {
                instance_id,
                session_id,
                expected_revision,
            }),
            Intent::Resume
                if !self.mutation_pending()
                    && session.phase == AutomationSessionPhase::Active
                    && session.control_mode == AutomationControlMode::PausedForChat
                    && session.resume_eligibility == AutomationResumeEligibility::Idle =>
            {
                Some(AutomationRequest::Resume {
                    instance_id,
                    session_id,
                    expected_revision,
                })
            }
            Intent::Stop => Some(AutomationRequest::Stop {
                instance_id,
                session_id,
                expected_revision,
            }),
            _ => None,
        }
    }

    fn enqueue(
        &mut self,
        intent: Intent,
        connected: bool,
        commands: &tokio::sync::mpsc::UnboundedSender<WorkerCommand>,
    ) {
        let Some(mut request) = self.request(intent, connected) else {
            return;
        };
        if let AutomationRequest::StartTemplate { operation_id, .. } = &mut request {
            *operation_id = Uuid::new_v4();
        }
        let Some(id) = self.next_id.checked_add(1) else {
            self.error = Some("操作编号已耗尽，请重新打开桌面。".to_owned());
            return;
        };
        self.next_id = id;
        let lane = intent.lane();
        if commands
            .send(WorkerCommand::Automation {
                request_id: id,
                request: request.clone(),
            })
            .is_err()
        {
            self.fresh = false;
            self.error = Some("自动化控制通道已关闭；草稿保留。".to_owned());
            return;
        }
        self.pending[lane] = Some(Pending {
            id,
            request,
            uncertain: false,
        });
        if matches!(intent, Intent::Status) {
            self.activated = true;
        }
    }

    fn resolved(pending: &Pending, snapshot: &AutomationSnapshot) -> bool {
        match &pending.request {
            AutomationRequest::Status => true,
            AutomationRequest::StartTemplate {
                instance_id,
                expected_revision,
                operation_id,
                ..
            } => {
                snapshot.instance_id != *instance_id
                    || snapshot.revision > *expected_revision
                    || snapshot
                        .session
                        .as_ref()
                        .is_some_and(|session| session.operation_id == *operation_id)
            }
            AutomationRequest::Pause {
                instance_id,
                expected_revision,
                ..
            }
            | AutomationRequest::Resume {
                instance_id,
                expected_revision,
                ..
            } => snapshot.instance_id != *instance_id || snapshot.revision > *expected_revision,
            // Stop accepts an older cursor. A revision bump alone cannot prove
            // that a timed-out Stop will not still run against this session.
            AutomationRequest::Stop {
                instance_id,
                session_id,
                ..
            } => {
                snapshot.instance_id != *instance_id
                    || snapshot.session.as_ref().is_none_or(|session| {
                        session.session_id != *session_id
                            || session.control_mode == AutomationControlMode::Stopped
                            || session.phase == AutomationSessionPhase::Finished
                    })
            }
        }
    }

    fn accept(&mut self, snapshot: AutomationSnapshot) -> Result<bool, AppError> {
        snapshot.validate()?;
        if !self.ready || Some(snapshot.instance_id) != self.instance {
            return Ok(false);
        }
        if let Some(current) = &self.confirmed {
            if snapshot.projection_sequence < current.projection_sequence
                || snapshot.revision < current.revision
            {
                return Ok(false);
            }
            if snapshot.projection_sequence == current.projection_sequence && &snapshot != current {
                return Err(AppError::protocol(
                    "automation snapshot changed without advancing projection sequence",
                ));
            }
        }
        for pending in &mut self.pending {
            if pending
                .as_ref()
                .is_some_and(|pending| pending.uncertain && Self::resolved(pending, &snapshot))
            {
                *pending = None;
            }
        }
        self.confirmed = Some(snapshot);
        self.fresh = !self
            .pending
            .iter()
            .flatten()
            .any(|pending| pending.uncertain);
        if self.fresh {
            self.error = None;
        }
        Ok(true)
    }

    pub(super) fn receive(
        &mut self,
        id: Option<u64>,
        result: Result<AutomationSnapshot, AppError>,
    ) {
        if !self.ready {
            return;
        }
        let lane = id.and_then(|id| {
            self.pending
                .iter()
                .position(|pending| pending.as_ref().is_some_and(|pending| pending.id == id))
        });
        // A late correlated reply cannot claim authority after instance reset.
        if id.is_some() && lane.is_none() {
            return;
        }
        match result {
            Ok(snapshot) => {
                let reply_instance = snapshot.instance_id;
                match self.accept(snapshot) {
                    Ok(true) => {
                        if let Some(lane) = lane {
                            self.pending[lane] = None;
                        }
                        self.fresh = !self
                            .pending
                            .iter()
                            .flatten()
                            .any(|pending| pending.uncertain);
                    }
                    Ok(false) => {
                        // A correlated successful response settles its request,
                        // even when a newer same-instance projection arrived first.
                        // Its older payload still cannot replace current state.
                        if let Some(lane) = lane {
                            if Some(reply_instance) == self.instance {
                                self.pending[lane] = None;
                            } else if let Some(pending) = &mut self.pending[lane] {
                                pending.uncertain = true;
                            }
                        }
                        self.fresh = Some(reply_instance) == self.instance
                            && !self
                                .pending
                                .iter()
                                .flatten()
                                .any(|pending| pending.uncertain);
                    }
                    Err(error) => {
                        self.fresh = false;
                        self.error = Some(error.to_string());
                        if let Some(lane) = lane
                            && let Some(pending) = &mut self.pending[lane]
                        {
                            pending.uncertain = true;
                        }
                    }
                }
            }
            Err(error) => {
                self.fresh = false;
                self.error = Some(error.to_string());
                if let Some(lane) = lane {
                    let definite = matches!(
                        error.kind,
                        ErrorKind::Configuration
                            | ErrorKind::InvalidTransition
                            | ErrorKind::Safety
                            | ErrorKind::Unavailable
                    );
                    if lane == 0 || definite {
                        self.pending[lane] = None;
                    } else if let Some(pending) = &mut self.pending[lane] {
                        pending.uncertain = true;
                    }
                }
            }
        }
    }
}

impl DesktopApp {
    pub(super) fn sync_reflex_identity(&mut self) {
        self.reflex.synchronize(
            self.state.runtime.instance_id,
            self.state.connection == ConnectionState::Connected
                && !self.state.needs_resync
                && self.reflex_runtime_ready,
        );
    }

    pub(super) fn show_reflex_panel(&mut self, ui: &mut egui::Ui) {
        self.sync_reflex_identity();
        let connected = self.state.connection == ConnectionState::Connected
            && !self.state.needs_resync
            && self.reflex_runtime_ready;
        if connected && self.state.runtime.instance_id.is_some() && !self.reflex.activated {
            self.reflex
                .enqueue(Intent::Status, connected, &self.worker.commands);
        }
        ui.group(|ui| {
            ui.strong("合成模板回放");
            ui.small("只读取显式回放清单，在 dry-run 主机中运行；不会读取真实屏幕或发送真实输入。");
            ui.label("回放清单路径");
            ui.add(
                egui::TextEdit::singleline(&mut self.reflex.manifest)
                    .hint_text("synthetic-replay.json"),
            );
            let pause_label = if self
                .reflex
                .confirmed
                .as_ref()
                .and_then(|snapshot| snapshot.session.as_ref())
                .is_some_and(|session| session.control_mode == AutomationControlMode::PausedForChat)
            {
                "保持暂停"
            } else {
                "暂停回放"
            };
            ui.horizontal_wrapped(|ui| {
                for (intent, label) in [
                    (Intent::Start, "开始回放"),
                    (Intent::Pause, pause_label),
                    (Intent::Resume, "恢复回放"),
                    (Intent::Stop, "停止回放"),
                    (Intent::Status, "刷新回放状态"),
                ] {
                    if ui
                        .add_enabled(
                            self.reflex.request(intent, connected).is_some(),
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        self.reflex
                            .enqueue(intent, connected, &self.worker.commands);
                    }
                }
            });
            if !self.reflex.fresh {
                ui.weak("状态待核对；动作结果不确定时只查询状态，不会自动重发。");
            }
            if self.reflex.mutation_pending() {
                ui.weak("等待服务确认操作结果……");
            }
            if let Some(snapshot) = &self.reflex.confirmed {
                if !snapshot.available {
                    ui.weak("当前服务未提供合成回放主机。");
                }
                if let Some(session) = &snapshot.session {
                    ui.label(format!(
                        "阶段：{} · 控制：{}",
                        phase_label(session.phase),
                        control_label(session.control_mode, session.pause_reason)
                    ));
                    ui.label(format!(
                        "帧：{}/{} · 动作：{}/{} · 跳过：{}",
                        session.frames,
                        session.max_frames,
                        session.actions,
                        session.max_actions,
                        session.skipped_frames
                    ));
                    ui.small(format!(
                        "暂停原因：{} · 恢复条件：{} · 结果：{}",
                        reason_label(session.pause_reason),
                        eligibility_label(session.resume_eligibility),
                        outcome_label(session.latest_outcome)
                    ));
                    if let Some(search) = &session.search {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            "搜索未完成：计算预算不足，尚未认证目标；本步未移动或点击。",
                        );
                        ui.label(format!(
                            "已核验：{}/{} 个位置 · 计算：{}/{}",
                            search.verified, search.candidates, search.work, search.max_work
                        ));
                        ui.small(format!(
                            "精确计算：{}（{} 个位置） · 上界计算：{}（{} 次）",
                            search.exact_pixel_pairs,
                            search.exact_evaluations,
                            search.bound_work,
                            search.bound_queries
                        ));
                        ui.small(format!(
                            "搜索区域（帧内像素）：x={}，y={}，宽={}，高={}",
                            search.region.x,
                            search.region.y,
                            search.region.width,
                            search.region.height
                        ));
                    }
                    if let Some(error) = &session.last_error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                } else {
                    ui.weak("尚无回放会话。");
                }
            }
            if let Some(error) = &self.reflex.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
    }
}

fn phase_label(phase: AutomationSessionPhase) -> &'static str {
    match phase {
        AutomationSessionPhase::Loading => "正在载入",
        AutomationSessionPhase::Active => "运行中",
        AutomationSessionPhase::Stopping => "正在停止",
        AutomationSessionPhase::Finished => "已结束",
    }
}

fn control_label(
    control: AutomationControlMode,
    reason: Option<AutomationPauseReason>,
) -> &'static str {
    match control {
        AutomationControlMode::Running => "正在执行",
        AutomationControlMode::PausedForChat => match reason {
            Some(AutomationPauseReason::Chat) => "聊天暂停",
            Some(AutomationPauseReason::Manual) => "手动保持暂停",
            _ => "暂停（原因待核对）",
        },
        AutomationControlMode::Stopped => "已停止",
    }
}

fn reason_label(reason: Option<AutomationPauseReason>) -> &'static str {
    match reason {
        Some(AutomationPauseReason::Chat) => "聊天",
        Some(AutomationPauseReason::Manual) => "手动保持暂停",
        Some(AutomationPauseReason::Unknown) => "状态未知",
        None => "无",
    }
}

fn eligibility_label(eligibility: AutomationResumeEligibility) -> &'static str {
    match eligibility {
        AutomationResumeEligibility::Idle => "可恢复",
        AutomationResumeEligibility::Busy => "等待语音或生成结束",
        AutomationResumeEligibility::Unknown => "状态未知",
    }
}

fn outcome_label(outcome: Option<AutomationStepOutcome>) -> &'static str {
    match outcome {
        Some(AutomationStepOutcome::Waiting) => "等待下一帧",
        Some(AutomationStepOutcome::Matched) => "已匹配",
        Some(AutomationStepOutcome::Missed) => "未匹配",
        Some(AutomationStepOutcome::Completed) => "已完成",
        Some(AutomationStepOutcome::BudgetExhausted) => "帧或动作预算已用尽",
        Some(AutomationStepOutcome::SearchIncomplete) => "搜索未完成",
        Some(AutomationStepOutcome::Inactive) => "已暂停或停止",
        Some(AutomationStepOutcome::Interrupted) => "本步已中断",
        Some(AutomationStepOutcome::Failed) => "执行失败",
        Some(AutomationStepOutcome::InputExhausted) => "回放帧已用完",
        Some(AutomationStepOutcome::Stopped) => "已停止",
        None => "等待首帧",
    }
}
