use super::*;

impl NativeDesktopPanel {
    pub fn show(&mut self, ui: &mut egui::Ui) -> Vec<WorkerCommand> {
        let mut requests = Vec::new();
        let mut emergency = false;
        ui.group(|ui| {
            ui.strong("专用窗口操作");
            ui.weak("只操作你选定的窗口。请先准备无私人内容的专用测试窗口，并在服务配置中明确授权。");
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(self.connected, egui::Button::new("扫描可选窗口")).clicked() { requests.push(NativeDesktopRequest::Windows); }
                if ui.add_enabled(self.connected, egui::Button::new("核对运行状态")).clicked() { requests.push(NativeDesktopRequest::Status); }
                if ui.add_enabled(self.connected, egui::Button::new("全局急停")).clicked() { emergency = true; }
            });
            for (index, window) in self.windows.iter().enumerate() {
                ui.selectable_value(&mut self.selection, Some(index), format!("{} · PID {} · {}×{} · DPI {}",
                    window.title, window.process_id, window.width, window.height, window.dpi));
            }
            if let Some(window) = self.selection.and_then(|index| self.windows.get(index)) {
                ui.label(format!("授权范围：{}", window.scope));
                ui.weak(format!("物理位置 ({}, {})；启动身份 {}", window.left, window.top, window.process_started_ticks));
            }
            ui.weak("权限默认关闭。请在 safety 配置中为目标程序完整路径设置真实许可并重启服务；开始操作不会自动授权。");
            self.max_frames = self.max_frames.clamp(1, 256);
            self.max_actions = self.max_actions.clamp(1, 4096);
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::DragValue::new(&mut self.max_frames).range(1..=256).prefix("最多截图 "));
                ui.add(egui::DragValue::new(&mut self.max_actions).range(1..=4096).prefix("最多动作 "));
                let start = self.start_request();
                if ui.add_enabled(start.is_some(), egui::Button::new("准备所选窗口")).clicked()
                    && let Some(request) = start { requests.push(request); }
            });
            if let Some(state) = self.confirmed.as_ref() {
                ui.label(format!("原生执行 · 已确认 #{}{}", state.revision,
                    if state.available { "" } else { " · 当前不可用" }));
                if let Some(session) = &state.session {
                    ui.label(format!("{} · {:?} · 画面 {}/{} · 动作 {}/{}", session.window.title,
                        session.phase, session.frames, session.max_frames, session.actions, session.max_actions));
                    ui.weak(format!("窗口几何代次 {}", session.geometry_revision));
                    if let Some(outcome) = &session.last_outcome { ui.label(outcome); }
                    ui.horizontal_wrapped(|ui| {
                        ui.selectable_value(&mut self.manual_mode, true, "手动单步");
                        ui.selectable_value(&mut self.manual_mode, false, "视觉单步（需要视觉模型）");
                    });
                    if self.manual_mode { self.action.show(ui); }
                    else { ui.add(egui::TextEdit::multiline(&mut self.goal).hint_text("为视觉单步设置清晰目标").desired_rows(2)); }
                    let saved = if session.armed_action.is_some() { "手动动作" }
                        else if !session.goal.is_empty() { "视觉目标" } else { "尚未保存单步内容" };
                    ui.label(format!("服务已确认：{saved}。编辑中的内容需要再次保存。"));
                    ui.horizontal_wrapped(|ui| {
                        let content_valid = if self.manual_mode { self.action.action().is_some() } else { !self.goal.trim().is_empty() };
                        if ui.add_enabled(self.connected && self.fresh && content_valid, egui::Button::new("保存单步内容")).clicked() {
                            if self.manual_mode {
                                if let Some(action) = self.action.action() { requests.push(NativeDesktopRequest::SetAction {
                                    instance_id: state.instance_id, expected_revision: state.revision,
                                    session_id: session.session_id, action }); }
                            } else { requests.push(NativeDesktopRequest::SetGoal { instance_id: state.instance_id,
                                expected_revision: state.revision, session_id: session.session_id, goal: self.goal.clone() }); }
                        }
                        if ui.add_enabled(self.connected, egui::Button::new("暂停核对")).clicked() {
                            requests.push(NativeDesktopRequest::Pause { instance_id: state.instance_id,
                                expected_revision: state.revision, session_id: session.session_id });
                        }
                        if ui.add_enabled(self.connected, egui::Button::new("停止本次操作")).clicked() {
                            requests.push(NativeDesktopRequest::Stop { instance_id: state.instance_id,
                                expected_revision: state.revision, session_id: session.session_id });
                        }
                    });
                    ui.weak("保存后切回专用窗口。启用快捷键后，用 Ctrl + Alt + F9 恢复，F11 执行已保存单步，F8 暂停。每次单步都获取新画面，窗口移动或前台变化时先核对，再恢复。");
                }
            }
            if let Some(error) = &self.error { ui.colored_label(ui.visuals().warn_fg_color, error); }
        });
        let mut commands = requests
            .into_iter()
            .filter_map(|request| self.begin(request))
            .collect::<Vec<_>>();
        if emergency {
            commands.push(WorkerCommand::EmergencyStop);
        }
        commands
    }
}
