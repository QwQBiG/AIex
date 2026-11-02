use super::*;

impl DesktopRuntimePanel {
    pub fn show(&mut self, ui: &mut egui::Ui) -> Vec<WorkerCommand> {
        let mut requests = Vec::new();
        ui.group(|ui| {
            ui.strong("陪伴方式与快捷键");
            ui.weak("修改后点击应用。这里只调整陪伴行为，不重载模型或设备。");
            ui.add_enabled_ui(self.connected, |ui| {
                ui.checkbox(
                    &mut self.draft.proactive_enabled,
                    "允许伙伴在空闲时主动说话",
                );
                ui.add(
                    egui::Slider::new(&mut self.draft.cooldown_seconds, 10..=86400)
                        .logarithmic(true)
                        .text("两次主动说话至少间隔（秒）"),
                );
                ui.checkbox(&mut self.draft.do_not_disturb, "免打扰（停止主动说话）");
                let mut quiet = self.draft.quiet_hours.is_some();
                if ui.checkbox(&mut quiet, "定时安静时段").changed() {
                    self.draft.quiet_hours = quiet.then_some(ai_ex_domain::QuietHours {
                        start_minute: 1320,
                        end_minute: 480,
                    });
                }
                if let Some(hours) = self.draft.quiet_hours.as_mut() {
                    ui.horizontal(|ui| {
                        minute_editor(ui, "从", &mut hours.start_minute);
                        minute_editor(ui, "到", &mut hours.end_minute);
                        ui.weak("本地时间，支持跨午夜");
                    });
                }
                ui.checkbox(&mut self.draft.captions_visible, "显示独立悬浮字幕");
                ui.checkbox(
                    &mut self.draft.hotkeys_enabled,
                    "启用全局快捷键（本次启动需重新应用）",
                );
                ui.weak(
                    "Ctrl + Alt + F6 打断 · F7 急停 · F8 暂停 · F9 恢复 · F10 字幕 · F11 已保存单步",
                );
            });
            if let Some(state) = self.confirmed.as_ref() {
                ui.label(format!(
                    "已确认设置 #{} · 快捷键{}",
                    state.revision,
                    if state.hotkeys_registered {
                        "已注册"
                    } else {
                        "未注册"
                    }
                ));
                if state.hotkeys_error {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "快捷键未就绪，可能与其他程序冲突；处理后重新应用。",
                    );
                }
                if state.revision != self.draft_revision {
                    ui.weak("设置已在其他位置变化；编辑草稿已保留，请先核对。 ");
                }
            } else {
                ui.weak("等待服务确认当前设置。");
            }
            if self.draft.validate().is_err() {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "请设置不同的安静时段起止时间，再应用。",
                );
            }
            ui.horizontal_wrapped(|ui| {
                let apply = self.mutation(false);
                if ui
                    .add_enabled(apply.is_some(), egui::Button::new("应用设置"))
                    .clicked()
                    && let Some(request) = apply
                {
                    requests.push(request);
                }
                let reload = self.mutation(true);
                if ui
                    .add_enabled(reload.is_some(), egui::Button::new("重新读取行为设置文件"))
                    .clicked()
                    && let Some(request) = reload
                {
                    requests.push(request);
                }
                if ui.button("采用已确认设置").clicked()
                    && let Some(state) = &self.confirmed
                {
                    self.draft = state.settings.clone();
                    self.draft_revision = state.revision;
                }
                if ui
                    .add_enabled(self.connected, egui::Button::new("刷新"))
                    .clicked()
                {
                    requests.push(DesktopRuntimeRequest::State);
                }
            });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(self.connected, egui::Button::new("导出脱敏诊断"))
                    .clicked()
                {
                    requests.push(DesktopRuntimeRequest::Diagnostics);
                }
                let vts = self.connected
                    && self
                        .confirmed
                        .as_ref()
                        .is_some_and(|state| state.vts_discovery_available);
                if ui
                    .add_enabled(vts, egui::Button::new("发现 VTS 人物热键"))
                    .clicked()
                {
                    requests.push(DesktopRuntimeRequest::DiscoverVts);
                }
            });
            ui.weak(
                "诊断只包含版本、运行时长和已确认行为设置，不包含对话、原始日志、私人音频或路径。",
            );
            for key in &self.vts {
                ui.label(format!("{} · {}", key.name, key.id));
            }
            if let Some(message) = &self.feedback {
                ui.label(message);
            }
        });
        requests
            .into_iter()
            .filter_map(|request| self.begin(request))
            .collect()
    }
}

fn minute_editor(ui: &mut egui::Ui, label: &str, minute: &mut u16) {
    let mut hour = *minute / 60;
    let mut minutes = *minute % 60;
    ui.label(label);
    ui.add(egui::DragValue::new(&mut hour).range(0..=23).suffix("时"));
    ui.add(
        egui::DragValue::new(&mut minutes)
            .range(0..=59)
            .suffix("分"),
    );
    *minute = hour * 60 + minutes;
}
