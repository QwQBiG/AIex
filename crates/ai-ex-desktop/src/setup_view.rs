use super::*;
use crate::ui::theme;

impl SetupApp {
    pub(super) fn show_window(&mut self, ui: &mut egui::Ui) {
        self.poll_probe();
        let status_height = if self.status.is_empty() {
            0.0
        } else {
            ui.painter()
                .layout(
                    self.status.clone(),
                    egui::TextStyle::Body.resolve(ui.style()),
                    theme::TEXT,
                    (ui.available_width() - 40.0).max(1.0),
                )
                .size()
                .y
                .min(70.0)
                + 6.0
        };
        egui::Panel::bottom("setup_navigation")
            .resizable(false)
            .exact_size(58.0 + status_height)
            .show(ui, |ui| self.show_actions(ui));
        egui::Frame::new().inner_margin(20.0).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("setup_form")
                .show(ui, |ui| {
                    ui.heading("让对话，从这里开始");
                    ui.weak("选择模型，给伙伴一个称呼。其他的可以慢慢来。");
                    ui.add_space(8.0);
                    self.show_identity(ui);
                    ui.add_space(12.0);
                    self.show_model(ui);
                    ui.add_space(12.0);
                    self.show_advanced(ui);
                });
        });
        if self.checking {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    fn show_identity(&mut self, ui: &mut egui::Ui) {
        theme::card().inner_margin(14.0).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("你的伙伴");
            ui.horizontal(|ui| {
                ui.label("角色名称");
                ui.add(
                    egui::TextEdit::singleline(&mut self.persona_name)
                        .desired_width((ui.available_width() - 150.0).max(100.0)),
                );
                ui.checkbox(&mut self.memory_enabled, "启用长期记忆");
            });
            ui.small("记忆保存在本机；参与对话的记录会发送给你选择的模型。\n关闭不会删除已有记录，保存后重启服务生效。");
        });
    }

    fn show_model(&mut self, ui: &mut egui::Ui) {
        theme::card().inner_margin(14.0).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("模型连接");
            ui.horizontal(|ui| {
                ui.add_sized([70.0, 30.0], egui::Label::new("模型来源"));
                let before = self.provider;
                egui::ComboBox::from_id_salt("provider")
                    .width(ui.available_width())
                    .selected_text(self.provider.label())
                    .show_ui(ui, |ui| {
                        for provider in [
                            ProviderChoice::DeepSeek,
                            ProviderChoice::KoboldCpp,
                            ProviderChoice::Ollama,
                        ] {
                            ui.selectable_value(&mut self.provider, provider, provider.label());
                        }
                    });
                if before != self.provider {
                    self.provider_changed();
                }
            });
            ui.horizontal(|ui| {
                ui.add_sized([70.0, 30.0], egui::Label::new("模型地址"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.endpoint)
                        .desired_width((ui.available_width() - 112.0).max(100.0)),
                );
                if ui
                    .add_enabled(!self.checking, egui::Button::new("检查连接"))
                    .clicked()
                {
                    self.check_connection();
                }
            });
            field(ui, "模型名称", &mut self.model, false);
            if self.provider == ProviderChoice::DeepSeek {
                field(ui, "API Key", &mut self.api_key, true);
                ui.small("密钥只在本次运行中使用，不会写入配置文件。");
            }
            egui::CollapsingHeader::new("如何填写模型信息")
                .id_salt("provider_help")
                .show(ui, |ui| {
                    ui.label(self.provider.description());
                    ui.label(self.provider.model_hint());
                    ui.weak("检查连接仅验证地址和端口；服务启动后再验证模型与密钥。");
                });
        });
    }

    fn show_advanced(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::CollapsingHeader::new("更多选项 · 启动与直播")
                .id_salt("setup_advanced")
                .show(ui, |ui| {
                    ui.checkbox(&mut self.start_service, "打开 AIex 时自动启动服务（推荐）");
                    ui.checkbox(
                        &mut self.bilibili_enabled,
                        "接收 Bilibili 直播事件（可稍后开启）",
                    );
                    if self.bilibili_enabled {
                        field(ui, "直播间号", &mut self.bilibili_room_id, false);
                        ui.label("Cookie 环境变量名");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.bilibili_cookie_env)
                                .desired_width(f32::INFINITY),
                        );
                        ui.small("只填环境变量名，不要粘贴 Cookie 内容。");
                    }
                    ui.add_space(6.0);
                    ui.small(format!("配置文件：{}", self.config_path.display()));
                    ui.weak("首次启动会自动准备本地连接。");
                });
        });
    }

    fn show_actions(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new().inner_margin(10.0).show(ui, |ui| {
            if !self.status.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt("setup_status")
                    .max_height(70.0)
                    .show(ui, |ui| {
                        let color = if self.status_error {
                            ui.visuals().error_fg_color
                        } else {
                            theme::TEXT
                        };
                        ui.colored_label(color, &self.status);
                    });
                ui.add_space(6.0);
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add(theme::primary_button("保存并进入 AIex")).clicked() {
                    self.save(ui.ctx());
                }
                if ui.button("返回首页（不保存修改）").clicked() {
                    self.navigation
                        .request(ui.ctx(), crate::navigation::Destination::Home);
                }
                if self.checking {
                    ui.spinner();
                }
            });
        });
    }
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String, password: bool) {
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 30.0], egui::Label::new(label));
        ui.add(
            egui::TextEdit::singleline(value)
                .password(password)
                .desired_width(ui.available_width()),
        );
    });
}
