use ai_ex_domain::AppError;
use eframe::egui;
use uuid::Uuid;

use crate::worker::{SpeechSplitRequest, SpeechSplitStatus, WorkerCommand};

#[cfg(test)]
#[path = "speech_split_panel_tests.rs"]
mod tests;

pub struct SpeechSplitPanel {
    instance: Option<Uuid>,
    connected: bool,
    confirmed: Option<SpeechSplitStatus>,
    fresh: bool,
    draft_enabled: bool,
    draft_chars: usize,
    draft_base: Option<(Uuid, u64)>,
    pending: Option<(u64, SpeechSplitRequest)>,
    uncertain: Option<(Uuid, u64)>,
    next_id: u64,
    query_on_connect: bool,
    feedback: Option<String>,
}

impl Default for SpeechSplitPanel {
    fn default() -> Self {
        Self {
            instance: None,
            connected: false,
            confirmed: None,
            fresh: false,
            draft_enabled: false,
            draft_chars: ai_ex_domain::SOFT_SPLIT_DEFAULT_CHARS,
            draft_base: None,
            pending: None,
            uncertain: None,
            next_id: 0,
            query_on_connect: false,
            feedback: None,
        }
    }
}

impl SpeechSplitPanel {
    pub fn synchronize(&mut self, instance: Option<Uuid>, connected: bool) {
        let connected = connected && instance.is_some();
        let instance_changed = self.instance != instance;
        if instance_changed || (self.connected && !connected) {
            if let Some((_, request)) = self.pending {
                self.mark_uncertain(request);
            }
            if self.uncertain.is_some() {
                self.feedback = Some("设置结果尚未确认，请重新读取服务状态；不会自动重发。".into());
            }
            if instance_changed {
                if self.is_clean() {
                    self.draft_base = None;
                }
                self.instance = instance;
                self.confirmed = None;
                if instance
                    .is_some_and(|current| self.uncertain.is_some_and(|(old, _)| old != current))
                {
                    self.uncertain = None;
                }
            }
            self.pending = None;
            self.fresh = false;
        }
        if connected && (!self.connected || instance_changed) {
            self.query_on_connect = true;
        }
        self.connected = connected;
    }

    pub fn poll(&mut self) -> Option<WorkerCommand> {
        if !self.connected || !self.query_on_connect || self.pending.is_some() {
            return None;
        }
        self.query_on_connect = false;
        self.begin(SpeechSplitRequest::Query)
    }

    fn desired(&self) -> Option<usize> {
        self.draft_enabled.then_some(self.draft_chars)
    }

    fn is_clean(&self) -> bool {
        self.draft_base.is_none()
            || self.confirmed.as_ref().is_some_and(|state| {
                self.draft_base == Some((state.instance_id, state.revision))
                    && self.desired() == state.minimum
            })
    }

    fn adopt(&mut self, state: &SpeechSplitStatus) {
        self.draft_enabled = state.minimum.is_some();
        if let Some(minimum) = state.minimum {
            self.draft_chars = minimum;
        }
        self.draft_base = Some((state.instance_id, state.revision));
    }

    fn mark_uncertain(&mut self, request: SpeechSplitRequest) {
        if let SpeechSplitRequest::Set {
            instance_id,
            expected_revision,
            ..
        }
        | SpeechSplitRequest::Fence {
            instance_id,
            expected_revision,
            ..
        } = request
        {
            let floor = self
                .uncertain
                .filter(|(instance, _)| *instance == instance_id)
                .map_or(expected_revision, |(_, revision)| {
                    revision.max(expected_revision)
                });
            self.uncertain = Some((instance_id, floor));
        }
    }

    fn begin(&mut self, request: SpeechSplitRequest) -> Option<WorkerCommand> {
        if !self.connected || self.pending.is_some() {
            return None;
        }
        self.next_id = self.next_id.checked_add(1)?;
        if matches!(
            request,
            SpeechSplitRequest::Set { .. } | SpeechSplitRequest::Fence { .. }
        ) {
            self.fresh = false;
        }
        self.pending = Some((self.next_id, request));
        Some(WorkerCommand::SpeechSplit {
            request_id: self.next_id,
            request,
        })
    }

    pub fn receive(&mut self, request_id: u64, result: Result<SpeechSplitStatus, AppError>) {
        if self
            .pending
            .as_ref()
            .is_none_or(|(id, _)| *id != request_id)
        {
            return;
        }
        let (_, request) = self.pending.take().expect("matching request");
        let status = match result {
            Ok(status) => status,
            Err(_) => {
                self.mark_uncertain(request);
                self.fresh = false;
                self.feedback = Some(
                    match request {
                        SpeechSplitRequest::Set { .. } => {
                            "设置结果尚未确认，请先读取服务状态；草稿已保留。"
                        }
                        SpeechSplitRequest::Fence { .. } => {
                            "核对尚未确认，请重新读取状态；原设置不会自动重发。"
                        }
                        SpeechSplitRequest::Query => {
                            "暂时无法读取朗读设置，请稍后重试；草稿已保留。"
                        }
                    }
                    .into(),
                );
                return;
            }
        };
        let valid_minimum = status.minimum.is_none_or(|minimum| {
            (ai_ex_domain::SOFT_SPLIT_MIN_CHARS..=ai_ex_domain::SOFT_SPLIT_MAX_CHARS)
                .contains(&minimum)
        });
        let matches_request = match request {
            SpeechSplitRequest::Query => true,
            SpeechSplitRequest::Set {
                instance_id,
                expected_revision,
                minimum,
            } => {
                self.uncertain.is_none()
                    && status.instance_id == instance_id
                    && expected_revision.checked_add(1) == Some(status.revision)
                    && status.minimum == minimum
            }
            SpeechSplitRequest::Fence {
                instance_id,
                expected_revision,
                minimum,
            } => {
                self.uncertain.is_some_and(|(instance, floor)| {
                    instance == instance_id && expected_revision >= floor
                }) && status.instance_id == instance_id
                    && expected_revision.checked_add(1) == Some(status.revision)
                    && status.minimum == minimum
            }
        };
        if !self.connected
            || Some(status.instance_id) != self.instance
            || !valid_minimum
            || !matches_request
            || self.uncertain.is_some_and(|(instance, floor)| {
                instance == status.instance_id && status.revision < floor
            })
            || self.confirmed.as_ref().is_some_and(|old| {
                old.revision > status.revision
                    || (old.revision == status.revision && old.minimum != status.minimum)
            })
        {
            self.mark_uncertain(request);
            self.fresh = false;
            self.feedback = Some("服务状态不匹配，请重新读取；草稿已保留。".into());
            return;
        }
        let had_uncertain = self.uncertain.is_some();
        if matches!(request, SpeechSplitRequest::Fence { .. }) {
            self.uncertain = None;
        }
        let replace = !had_uncertain
            && (self.is_clean()
                || matches!(request, SpeechSplitRequest::Set { minimum, .. } if self.desired() == minimum));
        if replace {
            self.adopt(&status);
        }
        self.confirmed = Some(status);
        self.fresh = true;
        self.feedback = if self.uncertain.is_some() {
            Some("这只是最近读取的状态；原请求可能仍会到达，请先确认当前服务值。".into())
        } else if matches!(request, SpeechSplitRequest::Fence { .. }) {
            Some("服务状态已核对；草稿未自动应用。".into())
        } else {
            None
        };
    }

    pub fn has_unsaved(&self) -> bool {
        self.uncertain.is_some()
            || !self.is_clean()
            || self.pending.as_ref().is_some_and(|(_, request)| {
                matches!(
                    request,
                    SpeechSplitRequest::Set { .. } | SpeechSplitRequest::Fence { .. }
                )
            })
    }

    pub fn show(&mut self, ui: &mut egui::Ui, idle: bool) -> Vec<WorkerCommand> {
        let mut commands = Vec::new();
        egui::CollapsingHeader::new("朗读分段（本次会话）")
            .id_salt("speech_split_session")
            .show(ui, |ui| {
            ui.weak("只影响当前服务会话；服务重启后恢复启动配置。开启后可能在中文逗号或顿号提前分段，停顿会改变；跨段的成对格式标记或引号可能影响朗读。");
            if self.connected && self.fresh {
                if let Some(state) = &self.confirmed {
                    let label = if self.uncertain.is_some() { "最近读取" } else { "服务已确认" };
                    match state.minimum {
                        Some(minimum) => { ui.label(format!("{label}：开启，至少 {minimum} 字")); }
                        None => { ui.label(format!("{label}：关闭")); }
                    }
                }
            } else {
                ui.weak("服务状态尚未确认。");
            }
            let can_query = self.connected && self.pending.is_none();
            let editing = can_query && self.confirmed.is_some();
            ui.add_enabled_ui(editing, |ui| {
                ui.checkbox(&mut self.draft_enabled, "在中文逗号和顿号处分段朗读");
                ui.add_enabled(
                    self.draft_enabled,
                    egui::Slider::new(
                        &mut self.draft_chars,
                        ai_ex_domain::SOFT_SPLIT_MIN_CHARS..=ai_ex_domain::SOFT_SPLIT_MAX_CHARS,
                    ).text("每段至少字数"),
                );
            });
            let conflict = self.fresh && self.uncertain.is_none() && self.confirmed.as_ref().is_some_and(|state| {
                self.draft_base != Some((state.instance_id, state.revision))
            });
            if conflict {
                ui.weak("服务设置已变化，草稿已保留；请先核对再应用。");
                ui.horizontal_wrapped(|ui| {
                    if ui.button("采用服务设置").clicked()
                        && let Some(state) = self.confirmed.clone()
                    {
                        self.adopt(&state);
                    }
                    if ui.button("保留草稿并继续编辑").clicked()
                        && let Some(state) = &self.confirmed
                    {
                        self.draft_base = Some((state.instance_id, state.revision));
                    }
                });
            }
            if !idle {
                ui.weak("请等待当前回复结束后再应用。");
            }
            if self.pending.is_some() {
                ui.weak("正在等待服务回复；尚未确认当前状态。");
            }
            if let Some(feedback) = &self.feedback {
                ui.colored_label(ui.visuals().warn_fg_color, feedback);
            }
            if self.uncertain.is_some() {
                ui.weak("旧请求仍可能到达；请先读取服务状态，再确认当前值。确认只用于核对，不会应用草稿。");
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(can_query, egui::Button::new("读取服务朗读设置")).clicked()
                    && let Some(command) = self.begin(SpeechSplitRequest::Query)
                {
                    commands.push(command);
                }
                let fence = self.confirmed.as_ref().and_then(|state| {
                    self.uncertain.filter(|(instance, floor)| {
                        *instance == state.instance_id && state.revision >= *floor
                    }).filter(|_| can_query && self.fresh && idle)
                        .map(|_| SpeechSplitRequest::Fence {
                            instance_id: state.instance_id,
                            expected_revision: state.revision,
                            minimum: state.minimum,
                        })
                });
                if ui.add_enabled(fence.is_some(), egui::Button::new("确认当前服务值")).clicked()
                    && let Some(request) = fence
                    && let Some(command) = self.begin(request)
                {
                    commands.push(command);
                }
                let apply = self.confirmed.as_ref().filter(|state| {
                    editing && idle && self.fresh && !conflict && self.uncertain.is_none()
                        && self.draft_base == Some((state.instance_id, state.revision))
                        && self.desired() != state.minimum
                }).map(|state| SpeechSplitRequest::Set {
                    instance_id: state.instance_id,
                    expected_revision: state.revision,
                    minimum: self.desired(),
                });
                if ui.add_enabled(apply.is_some(), egui::Button::new("应用到本次会话")).clicked()
                    && let Some(request) = apply
                    && let Some(command) = self.begin(request)
                {
                    commands.push(command);
                }
            });
        });
        commands
    }
}
