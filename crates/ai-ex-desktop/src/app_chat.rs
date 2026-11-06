use super::*;
use ai_ex_ui_model::TurnStatus;

pub(super) const COMPOSER_ID: &str = "conversation_input";

impl DesktopApp {
    pub(super) fn show_conversation(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("conversation_history")
            .max_height(ui.available_height())
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.state.turns.is_empty() {
                    ui.add_space(36.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("从一句话开始")
                                .small()
                                .color(super::theme::ACCENT),
                        );
                        ui.add_space(12.0);
                        ui.heading(format!("和 {} 聊聊", self.active_persona.name));
                        ui.add_space(8.0);
                        ui.weak("今天发生了什么，或是想一起做点什么？");
                        if !self.can_submit() {
                            ui.weak("连接准备好后就能发送，也可以先写下想说的话。");
                        }
                        if self.input.is_empty() {
                            ui.add_space(16.0);
                            ui.weak("不知道说什么？可以从这里开始");
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                for suggestion in [
                                    "你好，今天我们来聊聊吧。",
                                    "你愿意介绍一下自己吗？",
                                    "陪我聊聊今天发生的事。",
                                ] {
                                    if ui
                                        .add(
                                            egui::Button::new(suggestion)
                                                .fill(super::theme::SURFACE_ALT)
                                                .stroke(egui::Stroke::NONE)
                                                .corner_radius(9),
                                        )
                                        .clicked()
                                    {
                                        self.input = suggestion.to_owned();
                                        ui.ctx().memory_mut(|memory| {
                                            memory.request_focus(egui::Id::new(COMPOSER_ID));
                                        });
                                    }
                                }
                            });
                        }
                    });
                }
                for turn in &self.state.turns {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        egui::Frame::new()
                            .fill(super::theme::ACCENT_SOFT)
                            .corner_radius(egui::CornerRadius {
                                nw: 16,
                                ne: 4,
                                sw: 16,
                                se: 16,
                            })
                            .inner_margin(16)
                            .show(ui, |ui| {
                                ui.set_max_width((ui.available_width() * 0.88).max(120.0));
                                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                    ui.label(
                                        egui::RichText::new("你")
                                            .small()
                                            .color(super::theme::ACCENT),
                                    );
                                    ui.add(egui::Label::new(&turn.user_text).wrap());
                                });
                            });
                    });
                    ui.add_space(12.0);
                    egui::Frame::new()
                        .fill(super::theme::SURFACE_ALT.gamma_multiply(0.42))
                        .corner_radius(egui::CornerRadius {
                            nw: 4,
                            ne: 16,
                            sw: 16,
                            se: 16,
                        })
                        .inner_margin(16)
                        .show(ui, |ui| {
                            ui.set_max_width((ui.available_width() * 0.95).max(120.0));
                            ui.label(
                                egui::RichText::new(&self.active_persona.name)
                                    .color(super::theme::ACCENT)
                                    .strong(),
                            );
                            if turn.assistant_text.is_empty()
                                && turn.status == TurnStatus::Streaming
                            {
                                ui.weak("正在想怎么回应你……");
                            } else {
                                ui.add(egui::Label::new(&turn.assistant_text).wrap());
                            }
                            match turn.status {
                                TurnStatus::Streaming => {
                                    ui.weak("正在回复……");
                                }
                                TurnStatus::Interrupted => {
                                    ui.weak("这次回复已打断");
                                }
                                TurnStatus::Failed => {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        "回复未完成，请检查连接后重试",
                                    );
                                }
                                TurnStatus::Completed => {}
                            }
                        });
                    ui.add_space(16.0);
                }
            });
    }

    pub(super) fn show_composer(&mut self, ui: &mut egui::Ui) {
        let ime_event = ui.input(|input| {
            let mut seen = false;
            for event in &input.events {
                if let egui::Event::Ime(event) = event {
                    seen = true;
                    match event {
                        egui::ImeEvent::Preedit { text, .. } => {
                            self.composer_ime_active = !text.is_empty()
                        }
                        egui::ImeEvent::Commit(_) => self.composer_ime_active = false,
                        _ => {}
                    }
                }
            }
            seen
        });
        let id = egui::Id::new(COMPOSER_ID);
        let focused = ui.memory(|memory| memory.has_focus(id));
        let keyboard_submit = focused
            && !ime_event
            && !self.composer_ime_active
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter));
        let status_height = self.composer_status_height(ui, ui.available_width());
        let draft_height = (ui.available_height() - 56.0 - status_height).clamp(32.0, 58.0);
        let response = egui::ScrollArea::vertical()
            .id_salt("conversation_draft")
            .min_scrolled_height(0.0)
            .max_height(draft_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.input)
                        .id(id)
                        .frame(egui::Frame::NONE)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .hint_text("写下想说的话……"),
                )
            })
            .inner;
        if !response.has_focus() {
            self.composer_ime_active = false;
        }
        ui.separator();
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 36.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let enabled = self.can_submit()
                    && !self.input.trim().is_empty()
                    && !ime_event
                    && !self.composer_ime_active;
                let send = ui.add_enabled(enabled, super::theme::primary_button("发送"));
                if enabled && (send.clicked() || keyboard_submit) {
                    self.submit();
                    response.request_focus();
                }
                ui.label(
                    egui::RichText::new("Enter 换行 · Ctrl + Enter 发送")
                        .small()
                        .color(super::theme::MUTED),
                );
            },
        );
        if let Some(status) = self.composer_status() {
            ui.weak(status);
        }
    }

    pub(super) fn composer_status_height(&self, ui: &egui::Ui, width: f32) -> f32 {
        self.composer_status().map_or(0.0, |status| {
            ui.painter()
                .layout(
                    status.to_owned(),
                    egui::TextStyle::Body.resolve(ui.style()),
                    super::theme::MUTED,
                    width.max(1.0),
                )
                .size()
                .y
                + ui.spacing().item_spacing.y
        })
    }

    fn composer_status(&self) -> Option<&'static str> {
        if self.can_submit() {
            None
        } else if self.memory.backup_pending() {
            Some("正在备份记忆，请等待完成；聊天草稿会保留。")
        } else if self.memory.mutation_pending() {
            Some("正在更新记忆，草稿会保留。")
        } else if self.persona_apply_pending || self.confirm_persona {
            Some("角色切换确认完成后可以发送；草稿会保留。")
        } else if self.state.connection == ConnectionState::Connected {
            Some("正在同步角色与对话状态，草稿会保留。")
        } else {
            Some("等待连接，可以先写草稿；连接设置位于“设置与诊断”。")
        }
    }
}
