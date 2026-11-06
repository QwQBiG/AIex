use super::*;
use ai_ex_domain::{NativeDesktopAction, NativePointerButton};

impl NativeDesktopPanel {
    pub(in crate::native_desktop_panel) fn show_proposal(
        &mut self,
        ui: &mut egui::Ui,
        requests: &mut Vec<NativeDesktopRequest>,
    ) {
        if self.proposal.idle() && (self.manual_mode || !self.connected) {
            return;
        }
        ui.separator();
        ui.strong("视觉建议 · 可确认执行");
        ui.weak(
            "准备建议会将所选窗口画面发送给配置的视觉模型（可能为远程）；准备阶段不会发送键鼠。",
        );
        if self.proposal.idle()
            && ui
                .add_enabled(
                    self.preview_available() && self.preview_arm.is_none(),
                    egui::Button::new("准备可确认的视觉建议"),
                )
                .clicked()
        {
            self.arm_proposal_preparation();
        }
        if self.proposal.arm.is_some() {
            ui.strong("请在约 3 秒内切回专用窗口。");
            if ui.button("取消倒计时").clicked() {
                self.cancel_proposal_countdown();
            }
        }
        if let Some(message) = self.proposal.message {
            ui.label(message);
        }
        if let Some((id, state)) = self
            .proposal
            .preparation
            .as_ref()
            .map(|preparation| (preparation.id, preparation.state))
        {
            ui.weak(format!("建议编号：{id}"));
            if state != NativeVisionPreparationState::Ready {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.query_preparation().is_some(),
                            egui::Button::new("查询建议状态"),
                        )
                        .clicked()
                        && let Some(request) = self.query_preparation()
                    {
                        requests.push(request);
                    }
                    if state != NativeVisionPreparationState::Preparing
                        && ui.button("停止跟踪此建议").clicked()
                    {
                        self.proposal.preparation = None;
                        self.proposal.review = None;
                        self.proposal.message =
                            Some("已停止跟踪；服务可能仍在处理，建议不会自动执行。");
                    }
                });
            }
        }
        if let Some(review) = &self.proposal.review {
            ui.strong("请完整审阅建议；尚未执行。");
            if let Some(session) = self
                .confirmed
                .as_ref()
                .and_then(|state| state.session.as_ref())
            {
                let window = &session.window;
                ui.label("目标窗口完整标题：");
                full_text(ui, "proposal_title", &window.title);
                ui.label(format!(
                    "PID {} · 启动身份 {} · 窗口标识 {}",
                    window.process_id, window.process_started_ticks, window.handle
                ));
                ui.label("目标程序完整授权路径：");
                full_text(ui, "proposal_scope", &window.scope);
            }
            ui.label(format!(
                "参考剩余时间：{} 秒；以服务核验为准。",
                review
                    .expires
                    .saturating_duration_since(Instant::now())
                    .as_secs()
            ));
            action_review(ui, &review.value.action);
            ui.weak("执行前会重新截取并比对画面、检查前台窗口及许可。动态画面可能被拒绝；检查与输入之间仍可能发生变化。");
            ui.strong("点击准备执行后，倒计时结束可能向所选窗口实际发送一次键鼠输入。");
            let can_confirm = self.proposal.arm.is_none()
                && self.proposal.pending.is_none()
                && self.proposal.operation.is_none()
                && self.context_matches(&review.context)
                && review.expires > Instant::now() + Duration::from_secs(3);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(can_confirm, egui::Button::new("准备执行此建议"))
                    .clicked()
                {
                    self.arm_proposal_confirmation();
                }
                if ui
                    .add_enabled(
                        self.proposal.arm.is_none()
                            && self.proposal.pending.is_none()
                            && self.discard_request().is_some(),
                        egui::Button::new("丢弃建议"),
                    )
                    .clicked()
                    && let Some(request) = self.discard_request()
                {
                    requests.push(request);
                }
            });
        }
        self.show_proposal_operation(ui, requests);
    }

    fn show_proposal_operation(
        &mut self,
        ui: &mut egui::Ui,
        requests: &mut Vec<NativeDesktopRequest>,
    ) {
        let Some((id, proposal_id, state)) = self
            .proposal
            .operation
            .as_ref()
            .map(|operation| (operation.id, operation.proposal_id, operation.state))
        else {
            return;
        };
        ui.separator();
        ui.strong("本次视觉操作");
        ui.label(format!("操作编号：{id} · 建议编号：{proposal_id}"));
        ui.weak(match state {
            NativeVisionOperationState::Pending => "服务正在处理；不要重复执行。",
            NativeVisionOperationState::Unknown => {
                "结果未知：输入可能已发生或仍在处理，请查询并核对目标；不会自动重试。"
            }
            NativeVisionOperationState::Completed => "服务报告已完成，请核对目标。",
            NativeVisionOperationState::Rejected => "服务报告未执行。",
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.query_operation().is_some(),
                    egui::Button::new("查询原操作状态"),
                )
                .clicked()
                && let Some(request) = self.query_operation()
            {
                requests.push(request);
            }
            match state {
                NativeVisionOperationState::Completed | NativeVisionOperationState::Rejected => {
                    if ui.button("结束本次跟踪").clicked() {
                        self.proposal.operation = None;
                        self.proposal.preparation = None;
                        self.proposal.message = None;
                    }
                }
                NativeVisionOperationState::Pending | NativeVisionOperationState::Unknown => {
                    if ui.button("已停止原服务并核对目标").clicked() {
                        self.proposal.confirm_unknown_end = true;
                    }
                }
            }
        });
        if self.query_operation().is_none()
            && matches!(
                state,
                NativeVisionOperationState::Pending | NativeVisionOperationState::Unknown
            )
        {
            ui.weak("当前不是原服务实例，无法查询原操作；请先停止原服务并核对目标。");
        }
        if self.proposal.confirm_unknown_end {
            let mut open = true;
            let mut cancel = false;
            let mut confirm = false;
            egui::Window::new("结束未知操作跟踪？").open(&mut open).collapsible(false).show(ui.ctx(), |ui| {
                ui.label("旧服务若仍在运行，操作可能稍后执行。请确认原服务已停止，并已核对目标窗口。");
                ui.horizontal_wrapped(|ui| {
                    cancel = ui.button("继续跟踪").clicked();
                    confirm = ui.button("确认已停止并核对").clicked();
                });
            });
            if confirm {
                self.cancel_proposal_context();
                self.proposal.operation = None;
                self.proposal.message = None;
            }
            if confirm || cancel || !open {
                self.proposal.confirm_unknown_end = false;
            }
        }
    }
}

fn button_name(button: NativePointerButton) -> &'static str {
    match button {
        NativePointerButton::Left => "左键",
        NativePointerButton::Right => "右键",
        NativePointerButton::Middle => "中键",
    }
}

pub(super) fn action_review(ui: &mut egui::Ui, action: &NativeDesktopAction) {
    match action {
        NativeDesktopAction::Click { x, y, button } => {
            ui.label(format!(
                "点击：{}，物理坐标 ({x}, {y})",
                button_name(*button)
            ));
        }
        NativeDesktopAction::Drag {
            from_x,
            from_y,
            to_x,
            to_y,
            button,
            steps,
            duration_ms,
        } => {
            ui.label(format!(
                "拖动：{}，({from_x}, {from_y}) → ({to_x}, {to_y})，{steps} 步，{duration_ms} 毫秒",
                button_name(*button)
            ));
        }
        NativeDesktopAction::Scroll { horizontal, delta } => {
            ui.label(format!(
                "{}滚动：{delta}",
                if *horizontal { "水平" } else { "垂直" }
            ));
            ui.weak("滚动作用于执行时鼠标所在位置，请将鼠标移到目标区域。");
        }
        NativeDesktopAction::Wait { milliseconds } => {
            ui.label(format!("等待：{milliseconds} 毫秒"));
        }
        NativeDesktopAction::Noop => {
            ui.label("不执行动作");
        }
        NativeDesktopAction::TypeText { text } => {
            ui.label(format!("将输入的完整文字（{} 字）：", text.chars().count()));
            full_text(ui, "proposal_text", text);
        }
        NativeDesktopAction::KeyChord { keys } => {
            ui.label("将发送的完整按键组合：");
            for (index, key) in keys.iter().enumerate() {
                ui.label(format!("第 {} 个键", index + 1));
                full_text(ui, ("proposal_key", index), key);
            }
        }
    }
}

fn full_text(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, text: &str) {
    let id = egui::Id::new(id);
    let escaped = super::super::review_text::escaped(text);
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(220.0)
        .show(ui, |ui| {
            ui.add(egui::Label::new(escaped).wrap());
        });
    ui.weak("空白、格式控制和组合字符已用 U+ 编号标出；执行内容由服务持有，不取自此处显示。");
    ui.push_id(id, |ui| {
        ui.collapsing("查看完整逐字符 U+ 码点", |ui| {
            egui::ScrollArea::vertical()
                .id_salt("all_codepoints")
                .max_height(180.0)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(super::super::review_text::codepoints(text)).wrap());
                });
        });
    });
}
