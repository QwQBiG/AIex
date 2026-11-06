use super::*;
use ai_ex_domain::ConversationState;
use ai_ex_ui_model::PresentationState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    Conversation,
    Character,
    Scenes,
    Memory,
    Settings,
    Native,
}

impl DesktopApp {
    pub(super) fn show_contents(&mut self, ui: &mut egui::Ui) {
        self.drain_events();
        self.handle_backup_close(
            ui.ctx(),
            ui.input(|input| input.viewport().close_requested()),
        );
        if let Some(error) = self
            .owned_service
            .as_mut()
            .and_then(|service| service.take_failure())
        {
            self.push_log(&error);
            self.last_error = Some(error);
        }
        self.poll_character(ui.ctx());
        self.poll_scene(ui.ctx());
        self.appearance.poll(ui.ctx());
        egui::Panel::top("app_header")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(super::theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(24, 14))
                    .stroke(egui::Stroke::new(1.0, super::theme::BORDER)),
            )
            .show(ui, |ui| self.show_header(ui));
        if self.page != Page::Native {
            self.native.cancel_drop();
            self.native.close_download_confirmation();
        } else if self.native.waiting_for_drop() {
            egui::Panel::top("native_drop_banner")
                .frame(
                    egui::Frame::new()
                        .fill(super::theme::ACCENT_SOFT)
                        .inner_margin(12),
                )
                .show(ui, |ui| self.native.show_drop_banner(ui));
        }
        match self.page {
            Page::Conversation => self.show_chat_page(ui),
            page => {
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .fill(super::theme::BACKGROUND)
                            .inner_margin(20),
                    )
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt(("app_page", page as u8))
                            .auto_shrink([false, false])
                            .show(ui, |ui| match page {
                                Page::Character => self.show_character_workspace(ui),
                                Page::Scenes => {
                                    ui.heading("场景组合");
                                    ui.weak(
                                        "保存喜爱的角色与外形，下次一起恢复，也可以分享给别人。",
                                    );
                                    ui.add_space(14.0);
                                    super::theme::card().show(ui, |ui| {
                                        ui.set_min_width(ui.available_width());
                                        self.show_scene_panel(ui);
                                    });
                                }
                                Page::Settings => self.show_settings_page(ui),
                                Page::Memory => self.show_memory_page(ui),
                                Page::Native => self.show_native_page(ui),
                                Page::Conversation => unreachable!(),
                            });
                    });
            }
        }
        self.show_persona_confirmation(ui.ctx());
        self.show_emergency_confirmation(ui.ctx());
        self.show_navigation_confirmation(ui.ctx());
        self.show_backup_close_confirmation(ui.ctx());
        self.show_retained_messages(ui.ctx());
    }

    fn show_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("AIex").size(26.0).strong());
            if ui.available_width() > 600.0 {
                ui.add_space(8.0);
                ui.colored_label(super::theme::MUTED, "留一点时间，慢慢相处");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("急停")
                    .on_hover_text("撤销自动化许可并停止当前输出")
                    .clicked()
                {
                    self.confirm_emergency_stop = true;
                }
                let active =
                    self.state.runtime.active_turn.is_some() || self.state.runtime.playback.active;
                if ui
                    .add_enabled(
                        active && self.state.connection == ConnectionState::Connected,
                        egui::Button::new("打断回复"),
                    )
                    .clicked()
                {
                    self.send(WorkerCommand::Interrupt);
                }
                let (label, color, fill) = match self.state.connection {
                    ConnectionState::Connected
                        if self.state.needs_resync || !self.persona_synced =>
                    {
                        ("同步中", super::theme::ACCENT, super::theme::ACCENT_SOFT)
                    }
                    ConnectionState::Connected => (
                        "已连接",
                        super::theme::SUCCESS,
                        egui::Color32::from_rgb(225, 241, 232),
                    ),
                    ConnectionState::Connecting => {
                        ("连接中", super::theme::ACCENT, super::theme::ACCENT_SOFT)
                    }
                    ConnectionState::Disconnected => {
                        ("未连接", super::theme::MUTED, super::theme::SURFACE_ALT)
                    }
                };
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(label).small().color(color).strong());
                    });
            });
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (page, label) in [
                (Page::Conversation, "聊天"),
                (Page::Character, "角色与外形"),
                (Page::Scenes, "场景组合"),
                (Page::Memory, "记忆"),
                (Page::Settings, "设置与诊断"),
                (Page::Native, "本地模型与声音"),
            ] {
                let selected = self.page == page;
                let button = egui::Button::new(
                    egui::RichText::new(label)
                        .color(if selected {
                            super::theme::ACCENT
                        } else {
                            super::theme::MUTED
                        })
                        .strong(),
                )
                .fill(if selected {
                    super::theme::ACCENT_SOFT
                } else {
                    super::theme::SURFACE
                })
                .stroke(egui::Stroke::NONE)
                .corner_radius(10);
                if ui.add(button).clicked() {
                    self.page = page;
                }
            }
        });
        if self.native_mode && self.native.microphone_restart_required() {
            ui.horizontal_wrapped(|ui| {
                ui.weak("麦克风开关重新进入对话后生效。");
                if self.owned_service.is_some() {
                    if ui.small_button("重新进入本地对话").clicked() {
                        self.request_navigation(
                            ui.ctx(),
                            crate::navigation::Destination::NativeChat,
                        );
                    }
                } else {
                    ui.weak("请由启动服务的窗口重启服务。");
                }
            });
        }
        if let Some(error) = self.last_error.clone() {
            ui.horizontal(|ui| {
                if ui.small_button("收起错误").clicked() {
                    self.last_error = None;
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&error).color(ui.visuals().error_fg_color),
                    )
                    .truncate(),
                )
                .on_hover_text(error);
            });
        }
        if let Some(notice) = self.notice.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.weak(notice);
                if !self.retained_messages.is_empty() && ui.small_button("查看保留的消息").clicked()
                {
                    self.show_retained = true;
                }
                if ui.small_button("收起提示").clicked() {
                    self.notice = None;
                }
            });
        }
    }

    fn show_character_workspace(&mut self, ui: &mut egui::Ui) {
        ui.heading("角色与外形");
        ui.weak("把喜欢的样子与相处方式，组合成你的伙伴。");
        ui.add_space(14.0);
        ui.add_enabled_ui(!self.scene_busy(), |ui| {
            if ui.available_width() >= 1020.0 {
                ui.columns(2, |columns| {
                    self.show_character_appearance(&mut columns[0]);
                    self.show_character_persona(&mut columns[1]);
                });
            } else {
                self.show_character_appearance(ui);
                ui.add_space(14.0);
                self.show_character_persona(ui);
            }
        });
    }

    fn show_character_appearance(&mut self, ui: &mut egui::Ui) {
        super::theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("外形工作室").strong().size(18.0));
            ui.weak("立绘、自己的图片，或只留下对话。随时都能更换。");
            ui.add_space(10.0);
            self.appearance.show_controls(ui);
            ui.add_space(12.0);
            egui::Frame::new()
                .fill(super::theme::STAGE)
                .corner_radius(14)
                .inner_margin(14)
                .show(ui, |ui| {
                    let state = PresentationState::from_ui(&self.state);
                    self.appearance
                        .show_portrait(ui, state, &self.active_persona.name, 370.0);
                });
        });
    }

    fn show_character_persona(&mut self, ui: &mut egui::Ui) {
        super::theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("怎样与你相处").strong().size(18.0));
            ui.add_space(8.0);
            self.show_persona_panel(ui);
        });
    }

    fn activity_label(&self) -> &'static str {
        if self.memory.backup_pending() {
            return "正在备份记忆";
        }
        if self.memory.mutation_pending() {
            return "正在更新记忆";
        }
        match self.state.connection {
            ConnectionState::Disconnected => return "尚未连接，草稿可以先写",
            ConnectionState::Connecting => return "正在连接，草稿可以先写",
            ConnectionState::Connected => {}
        }
        if self.state.needs_resync || !self.persona_synced {
            return "正在同步角色与对话";
        }
        match self.state.runtime.state {
            ConversationState::Idle => "在这里，随时可以聊聊",
            ConversationState::Listening => "正在听你说",
            ConversationState::Thinking => "正在思考",
            ConversationState::Speaking => "正在回应你",
            ConversationState::Interrupted => "已停下，可以继续聊",
            ConversationState::Failed => "回复遇到问题",
            ConversationState::Stopped => "对话已停止",
        }
    }

    fn show_chat_page(&mut self, ui: &mut egui::Ui) {
        let wide = ui.available_width() >= 940.0;
        if wide {
            let width = (ui.available_width() * 0.35).clamp(310.0, 430.0);
            egui::Panel::right("conversation_character")
                .exact_size(width)
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(super::theme::BACKGROUND)
                        .inner_margin(egui::Margin {
                            left: 0,
                            right: 20,
                            top: 20,
                            bottom: 20,
                        }),
                )
                .show(ui, |ui| {
                    super::theme::card()
                        .fill(super::theme::STAGE)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                egui::RichText::new("陪在这里")
                                    .color(super::theme::MUTED)
                                    .size(12.0),
                            );
                            let state = PresentationState::from_ui(&self.state);
                            self.appearance.show_portrait(
                                ui,
                                state,
                                &self.active_persona.name,
                                (ui.available_height() - 124.0).max(120.0),
                            );
                            ui.add_space(14.0);
                            ui.vertical_centered(|ui| {
                                if ui.button("自定义角色与外形").clicked() {
                                    self.page = Page::Character;
                                }
                            });
                        });
                });
        }
        let composer_height = if ui.available_height() < 540.0 {
            140.0
        } else {
            168.0
        };
        let status_height = self.composer_status_height(ui, ui.available_width() - 70.0);
        egui::Panel::bottom("conversation_composer")
            .exact_size(composer_height + status_height)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(super::theme::BACKGROUND)
                    .inner_margin(egui::Margin {
                        left: 20,
                        right: 20,
                        top: 0,
                        bottom: 16,
                    }),
            )
            .show(ui, |ui| {
                super::theme::card().inner_margin(14).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    self.show_composer(ui);
                });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(super::theme::BACKGROUND)
                    .inner_margin(if wide { 20 } else { 12 }),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("此刻的对话");
                    ui.add_space(4.0);
                    ui.colored_label(super::theme::MUTED, self.activity_label());
                });
                ui.add_space(8.0);
                if !wide {
                    egui::CollapsingHeader::new("查看伙伴")
                        .default_open(false)
                        .show(ui, |ui| {
                            let state = PresentationState::from_ui(&self.state);
                            self.appearance.show_portrait(
                                ui,
                                state,
                                &self.active_persona.name,
                                140.0,
                            );
                        });
                }
                crate::speech_panel::show(ui, &self.state);
                super::theme::card()
                    .inner_margin(if wide { 20 } else { 12 })
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.set_min_height(ui.available_height());
                        self.show_conversation(ui);
                    });
            });
    }

    fn show_settings_page(&mut self, ui: &mut egui::Ui) {
        use crate::navigation::Destination;
        ui.heading("设置与诊断");
        ui.weak("调整连接、查看运行情况，或回到首页体验人物。");
        ui.horizontal_wrapped(|ui| {
            if ui.button("连接设置").clicked() {
                self.request_navigation(ui.ctx(), Destination::Setup);
            }
            if ui.button("首页").clicked() {
                self.request_navigation(ui.ctx(), Destination::Home);
            }
            if !self.retained_messages.is_empty() && ui.button("保留的消息").clicked() {
                self.show_retained = true;
            }
        });
        if let Some(error) = self.last_error.clone() {
            ui.colored_label(ui.visuals().error_fg_color, &error);
            if ui.button("复制错误信息").clicked() {
                ui.ctx().copy_text(error);
            }
        }
        ui.separator();
        self.show_health(ui);
        self.show_model_panel(ui);
        self.show_policy_panel(ui);
        self.synchronize_desktop_controls();
        for command in self.desktop_runtime.show(ui) {
            self.send_desktop_control(command);
        }
        self.subtitle_overlay.show_controls(ui);
        for command in self.native_desktop.show(ui) {
            self.send_desktop_control(command);
        }
        self.show_automation_panel(ui);
        ui.separator();
        ui.checkbox(&mut self.show_developer, "显示详细诊断");
        if self.show_developer {
            ui.weak(format!(
                "完成 {} · 打断 {} · 故障 {} · 事件 #{}",
                self.state.runtime.turns_completed,
                self.state.runtime.turns_interrupted,
                self.state.runtime.faults,
                self.state.runtime.last_sequence
            ));
            self.show_developer_panel(ui);
            self.show_stage_panel(ui);
        }
        ui.add_space(12.0);
        ui.weak(format!("版本 {}", env!("CARGO_PKG_VERSION")));
    }

    fn show_memory_page(&mut self, ui: &mut egui::Ui) {
        self.memory.set_profile(&self.active_persona.profile_id);
        self.memory
            .synchronize_backup_instance(self.state.runtime.instance_id);
        let connected = self.state.connection == ConnectionState::Connected
            && self.persona_synced
            && !self.state.needs_resync;
        let idle = self.state.runtime.active_turn.is_none()
            && !self.state.runtime.playback.active
            && !self.persona_apply_pending
            && !self.confirm_persona
            && !self.scene_busy();
        if let Some(request) = self
            .memory
            .show(ui, &self.active_persona.name, connected, idle)
            && let Some(command) = self.memory.begin(request)
            && let Err(error) = self.worker.commands.send(command)
            && let WorkerCommand::Memory { request_id, .. } = error.0
        {
            self.memory.receive(
                request_id,
                Err(ai_ex_domain::AppError::unavailable(
                    "桌面网络工作线程已停止。",
                )),
            );
        }
    }

    fn show_native_page(&mut self, ui: &mut egui::Ui) {
        if !self.native_mode {
            ui.heading("本地模型与声音");
            ui.label("当前使用外部连接。可以打开独立的本地准备流程，原连接设置会保留。");
            if ui.button("准备本地伙伴").clicked() {
                self.request_navigation(ui.ctx(), crate::navigation::Destination::Native);
            }
            return;
        }
        let connected = self.state.connection == ConnectionState::Connected;
        if !connected && ui.button("返回首页重试").clicked() {
            self.request_navigation(ui.ctx(), crate::navigation::Destination::Home);
        }
        for command in self
            .native
            .show(ui, &self.active_persona.profile_id, connected)
        {
            if let Err(error) = self.worker.commands.send(command)
                && let WorkerCommand::Native { request_id, .. } = error.0
            {
                self.native.receive(
                    request_id,
                    Err(ai_ex_domain::AppError::unavailable(
                        "桌面网络工作线程已停止。",
                    )),
                );
            }
        }
        if self.native.take_open_chat() {
            self.native.cancel_drop();
            if self.native.microphone_restart_required() && self.owned_service.is_some() {
                self.request_navigation(ui.ctx(), crate::navigation::Destination::NativeChat);
            } else {
                if self.native.microphone_restart_required() {
                    self.notice =
                        Some("麦克风开关重新进入对话后生效，请由启动服务的窗口重启服务。".into());
                }
                self.page = Page::Conversation;
            }
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
}
