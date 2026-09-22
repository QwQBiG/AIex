use std::collections::VecDeque;

use ai_ex_domain::{ComponentHealth, PersonaSnapshot, StageSnapshot, SystemEvent};
use ai_ex_ui_model::{ApplyOutcome, ConnectionState, UiState};
use eframe::egui;

use crate::appearance::AppearancePanel;
use crate::ui::theme::{self, configure_appearance};
use crate::worker::{WorkerCommand, WorkerEvent, WorkerHandle};

#[path = "app_chat.rs"]
mod chat;
#[path = "app_layout.rs"]
mod layout;
#[path = "app_navigation.rs"]
mod navigation;

#[cfg(test)]
#[path = "app_ui_tests.rs"]
mod ui_tests;

#[cfg(test)]
#[path = "character_app_tests.rs"]
mod character_tests;

#[path = "scene_app.rs"]
mod scene;

#[cfg(test)]
#[path = "scene_app_tests.rs"]
mod scene_tests;

#[path = "resume_app.rs"]
mod resume;

#[cfg(test)]
#[path = "resume_app_tests.rs"]
mod resume_tests;

#[path = "library_app.rs"]
mod library;

#[cfg(test)]
#[path = "library_app_tests.rs"]
mod library_tests;

pub struct DesktopApp {
    state: UiState,
    worker: WorkerHandle,
    input: String,
    last_error: Option<String>,
    confirm_emergency_stop: bool,
    health: Vec<ComponentHealth>,
    show_developer: bool,
    logs: VecDeque<String>,
    log_filter: String,
    export_feedback: Option<String>,
    persona: PersonaSnapshot,
    active_persona: PersonaSnapshot,
    persona_dirty: bool,
    pending_persona: Option<PersonaSnapshot>,
    confirm_persona: bool,
    persona_apply_pending: bool,
    taboos_editor: String,
    stage: StageSnapshot,
    appearance: AppearancePanel,
    character_files: crate::character_files::CharacterFiles,
    active_character: ai_ex_config::character::CharacterManifest,
    scene_files: crate::scene_files::SceneFiles,
    pending_scene: Option<scene::PendingScene>,
    applying_scene: Option<scene::PendingScene>,
    resume: crate::scene_resume::SceneResume,
    persona_synced: bool,
    character_library: crate::character_library::CharacterLibrary,
    active_source: String,
    page: layout::Page,
    composer_ime_active: bool,
    navigation: crate::navigation::Navigation,
    pending_navigation: Option<crate::navigation::Destination>,
    owned_service: Option<crate::service_process::ManagedService>,
    notice: Option<String>,
    retained_messages: VecDeque<navigation::RetainedMessage>,
    show_retained: bool,
    memory: crate::memory_panel::MemoryPanel,
}

impl DesktopApp {
    pub fn new(
        context: &eframe::CreationContext<'_>,
        worker: WorkerHandle,
        developer_mode: bool,
        launch: crate::scene_resume::ResumeLaunch,
    ) -> Self {
        configure_appearance(&context.egui_ctx);
        let mut app = Self::with_storage(worker, developer_mode, context.storage);
        app.resume = crate::scene_resume::SceneResume::load(context.storage, launch);
        app
    }

    fn with_storage(
        worker: WorkerHandle,
        developer_mode: bool,
        storage: Option<&dyn eframe::Storage>,
    ) -> Self {
        Self {
            state: UiState::new(200).expect("valid UI capacity"),
            worker,
            input: String::new(),
            last_error: None,
            confirm_emergency_stop: false,
            health: Vec::new(),
            show_developer: developer_mode,
            logs: VecDeque::with_capacity(200),
            log_filter: String::new(),
            export_feedback: None,
            persona: PersonaSnapshot::default(),
            active_persona: PersonaSnapshot::default(),
            persona_dirty: false,
            pending_persona: None,
            confirm_persona: false,
            persona_apply_pending: false,
            taboos_editor: String::new(),
            stage: StageSnapshot::default(),
            appearance: AppearancePanel::load(storage),
            character_files: Default::default(),
            active_character: ai_ex_config::character::CharacterManifest::from_persona(
                Default::default(),
            ),
            scene_files: Default::default(),
            pending_scene: None,
            applying_scene: None,
            resume: Default::default(),
            persona_synced: false,
            character_library: crate::character_library::CharacterLibrary::load(storage),
            active_source: String::new(),
            page: layout::Page::Conversation,
            composer_ime_active: false,
            navigation: Default::default(),
            pending_navigation: None,
            owned_service: None,
            notice: None,
            retained_messages: VecDeque::new(),
            show_retained: false,
            memory: Default::default(),
        }
    }

    pub fn with_navigation(mut self, navigation: crate::navigation::Navigation) -> Self {
        self.navigation = navigation;
        self
    }

    pub fn with_service(mut self, service: Option<crate::service_process::ManagedService>) -> Self {
        self.owned_service = service;
        self
    }

    fn push_log(&mut self, message: impl Into<String>) {
        if self.logs.len() >= 200 {
            self.logs.pop_front();
        }
        self.logs.push_back(message.into());
    }

    fn active_model_health(&self) -> Option<&ComponentHealth> {
        self.health
            .iter()
            .find(|item| matches!(item.component.as_str(), "deepseek" | "koboldcpp" | "ollama"))
    }

    fn export_diagnostics(&mut self) {
        let path = std::env::current_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("aiex-desktop-diagnostics.log");
        let content = self.logs.iter().cloned().collect::<Vec<_>>().join("\n");
        match std::fs::write(&path, content) {
            Ok(()) => {
                let message = format!("诊断日志已导出：{}", path.display());
                self.export_feedback = Some(message.clone());
                self.push_log(message);
            }
            Err(error) => {
                let message = format!("诊断日志导出失败：{error}");
                self.export_feedback = Some(message.clone());
                self.push_log(message);
            }
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.worker.events.try_recv() {
            match event {
                WorkerEvent::Connection(connected) => {
                    if !connected {
                        self.persona_synced = false;
                    }
                    self.push_log(if connected {
                        "control connected"
                    } else {
                        "control disconnected"
                    });
                    self.state.connection = if connected {
                        ConnectionState::Connected
                    } else {
                        ConnectionState::Disconnected
                    };
                }
                WorkerEvent::Snapshot(snapshot) => {
                    if (self.state.runtime.instance_id.is_none()
                        || snapshot.instance_id == self.state.runtime.instance_id)
                        && snapshot.last_sequence >= self.state.runtime.last_sequence
                    {
                        self.state.apply_snapshot(snapshot);
                    }
                }
                WorkerEvent::Memory { request_id, result } => {
                    self.memory.set_profile(&self.active_persona.profile_id);
                    let (result, snapshot) = match result {
                        Ok(reply)
                            if reply.snapshot.instance_id == self.state.runtime.instance_id =>
                        {
                            (Ok(reply.response), Some(reply.snapshot))
                        }
                        Ok(_) => (
                            Err(ai_ex_domain::AppError::connectivity(
                                "服务实例已变化，请刷新记忆后核对。",
                            )),
                            None,
                        ),
                        Err(error) => (Err(error), None),
                    };
                    if self.memory.receive(request_id, result) {
                        self.state.turns.clear();
                        let snapshot = snapshot
                            .filter(|snapshot| {
                                snapshot.last_sequence >= self.state.runtime.last_sequence
                            })
                            .unwrap_or_else(|| self.state.runtime.clone());
                        self.state.recover_snapshot(snapshot);
                        self.notice =
                            Some("记忆已更新，已开始新的对话上下文；输入草稿保留。".to_owned());
                    }
                }
                WorkerEvent::HistoryGap(snapshot) => {
                    if snapshot.instance_id == self.state.runtime.instance_id
                        && snapshot.last_sequence < self.state.runtime.last_sequence
                    {
                        continue;
                    }
                    self.state.recover_snapshot(snapshot);
                    let message = "连接已恢复，部分历史无法补齐；可以继续对话。";
                    self.notice = Some(message.to_owned());
                    self.push_log(message);
                }
                WorkerEvent::SubmitRejected { text, error } => {
                    if self.input.is_empty() {
                        self.input = text;
                    } else {
                        self.retained_messages
                            .push_back(navigation::RetainedMessage {
                                text,
                                state: navigation::DeliveryState::NotDelivered,
                            });
                    }
                    self.push_log(&error);
                    self.last_error = Some(error);
                    self.notice = Some("有消息未送达，草稿已保留。".to_owned());
                }
                WorkerEvent::SubmitUncertain { text, error } => {
                    self.retained_messages
                        .push_back(navigation::RetainedMessage {
                            text,
                            state: navigation::DeliveryState::Uncertain,
                        });
                    self.push_log(&error);
                    self.last_error = Some(error);
                    self.notice =
                        Some("有消息的发送结果待核对，原文已保留；请先检查聊天记录。".to_owned());
                }
                WorkerEvent::Persona(profile) => {
                    self.persona_synced = self.state.connection == ConnectionState::Connected;
                    if !self.persona_apply_pending {
                        if self.active_persona != profile {
                            self.active_character =
                                ai_ex_config::character::CharacterManifest::from_persona(
                                    profile.clone(),
                                );
                            self.active_source = "服务同步（未提供包来源）".to_owned();
                        }
                        if self.active_source.is_empty() {
                            self.active_source = "服务同步（未提供包来源）".to_owned();
                        }
                        self.update_active_persona(&profile);
                    }
                    if !self.persona_apply_pending
                        && !self.persona_dirty
                        && self.pending_persona.is_none()
                        && !self.character_files.is_loading()
                    {
                        self.taboos_editor = profile.taboos.join("\n");
                        self.persona = profile;
                        self.persona_dirty = false;
                        self.persona_apply_pending = false;
                        self.sync_character_draft_metadata();
                    } else {
                        self.push_log(format!(
                            "persona update received while editing: {}@{}",
                            profile.profile_id, profile.revision
                        ));
                    }
                }
                WorkerEvent::PersonaApplied(profile) => {
                    self.persona_synced = true;
                    self.finish_scene(&profile);
                    self.update_active_persona(&profile);
                    self.taboos_editor = profile.taboos.join("\n");
                    self.persona = profile;
                    self.persona_dirty = false;
                    self.persona_apply_pending = false;
                    self.sync_character_draft_metadata();
                }
                WorkerEvent::PersonaApplyFailed(error) => {
                    if self.applying_scene.take().is_some() {
                        self.scene_files.feedback = Some(
                            "场景未获服务确认，外形保持原样；请检查连接与当前角色后重试。"
                                .to_owned(),
                        );
                    }
                    self.persona_apply_pending = false;
                    self.last_error = Some(error);
                }
                WorkerEvent::Stage(snapshot) => {
                    self.push_log(format!(
                        "stage snapshot received: {} action(s)",
                        snapshot.actions.len()
                    ));
                    self.stage = snapshot;
                }
                WorkerEvent::Health(health) => {
                    let details = health
                        .iter()
                        .map(|item| {
                            format!(
                                "health {} ready={} {}",
                                item.component, item.ready, item.detail
                            )
                        })
                        .collect::<Vec<_>>();
                    self.push_log(format!(
                        "health snapshot received: {} component(s)",
                        health.len()
                    ));
                    self.health = health;
                    for detail in details {
                        self.push_log(detail);
                    }
                }
                WorkerEvent::Events(events) => {
                    for event in events {
                        match &event.event {
                            ai_ex_domain::SystemEvent::LiveEventReceived {
                                event_type,
                                summary,
                                ..
                            } => self.push_log(format!("live event {event_type}: {summary}")),
                            ai_ex_domain::SystemEvent::LiveResponseSuggested {
                                automatic, ..
                            } => self.push_log(format!(
                                "live reaction suggested (automatic={automatic})",
                            )),
                            ai_ex_domain::SystemEvent::SentenceReady { text, .. } => {
                                let preview = text.replace("\r", " ").replace("\n", " ");
                                self.push_log(format!(
                                    "stage speech queued: {}",
                                    preview.chars().take(120).collect::<String>(),
                                ));
                            }
                            ai_ex_domain::SystemEvent::EmotionChanged { emotion, .. } => {
                                self.push_log(format!("stage expression: {emotion:?}"));
                            }
                            SystemEvent::PersonaChanged {
                                profile_id,
                                revision,
                            } => {
                                self.push_log(format!("persona changed: {profile_id}@{revision}"));
                            }
                            SystemEvent::ComponentHealthChanged {
                                component,
                                ready,
                                detail,
                            } => {
                                let state = if *ready { "ready" } else { "unavailable" };
                                self.push_log(format!(
                                    "health transition {component}={state}: {detail}"
                                ));
                            }
                            _ => {}
                        }
                        if self.state.apply_event(event) == ApplyOutcome::GapDetected {
                            self.last_error =
                                Some("事件序号出现缺口，正在等待状态重新同步。".to_owned());
                            break;
                        }
                    }
                }
                WorkerEvent::Failure(error) => {
                    self.push_log(format!("failure: {error}"));
                    self.last_error = Some(error);
                }
                WorkerEvent::Log(message) => self.push_log(message),
            }
        }
    }

    fn send(&mut self, command: WorkerCommand) -> bool {
        if self.worker.commands.send(command).is_err() {
            self.last_error = Some("桌面网络工作线程已停止。".to_owned());
            return false;
        }
        true
    }

    fn update_active_persona(&mut self, profile: &PersonaSnapshot) {
        if self.active_persona.profile_id != profile.profile_id {
            self.state.turns.clear();
            self.state.runtime.current_emotion = None;
            self.state.runtime.playback = Default::default();
        }
        self.active_persona = profile.clone();
        self.memory.set_profile(&profile.profile_id);
    }

    fn can_submit(&self) -> bool {
        self.state.connection == ConnectionState::Connected
            && !self.state.needs_resync
            && self.persona_synced
            && !self.persona_apply_pending
            && !self.confirm_persona
            && !self.memory.mutation_pending()
            && self.resume.phase == crate::scene_resume::ResumePhase::Idle
    }

    fn submit(&mut self) {
        if !self.can_submit() {
            return;
        }
        let text = self.input.trim();
        if text.is_empty() {
            return;
        }
        let text = text.to_owned();
        if self.send(WorkerCommand::Submit(text)) {
            self.input.clear();
        }
    }

    fn poll_character(&mut self, context: &egui::Context) {
        if let Some(manifest) = self.character_files.poll(context) {
            self.persona = manifest.persona;
            self.taboos_editor = self.persona.taboos.join("\n");
            self.persona_dirty = true;
            self.pending_persona = None;
            self.confirm_persona = false;
        }
    }

    fn show_persona_panel(&mut self, ui: &mut egui::Ui) {
        self.poll_character(ui.ctx());
        let mut changed = false;
        let mut request_confirm = false;
        egui::CollapsingHeader::new("角色设定与收藏")
            .default_open(true)
            .show(ui, |ui| {
                self.show_character_library(ui);
                ui.add_enabled_ui(!self.persona_apply_pending && !self.confirm_persona, |ui| {
                    if self.character_files.show(ui, &self.persona) {
                        self.persona_dirty = true;
                    }
                });
                ui.add_enabled_ui(
                    !self.character_files.is_loading()
                        && !self.persona_apply_pending
                        && !self.confirm_persona,
                    |ui| {
                        ui.weak("修改后先预览，再确认应用到当前对话。正在回复时请先打断。");
                        ui.horizontal_wrapped(|ui| {
                            ui.label("档案 ID");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.persona.profile_id)
                                        .desired_width(180.0),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                            ui.label(format!("版本 {}", self.persona.revision));
                            if ui.button("版本 +1").clicked() {
                                self.persona.revision = self.persona.revision.saturating_add(1);
                                changed = true;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("名称");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.persona.name)
                                        .desired_width(140.0),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                            ui.label("语气");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.persona.tone)
                                        .desired_width(140.0),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        });
                        ui.label("系统提示词");
                        if ui
                            .add(
                                egui::TextEdit::multiline(&mut self.persona.system_prompt)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(3),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        ui.label("禁忌（每行一项）");
                        if ui
                            .add(
                                egui::TextEdit::multiline(&mut self.taboos_editor)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(2),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label("直播模式");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.persona.live_mode)
                                        .desired_width(140.0),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                            if ui.button("预览并请求确认").clicked() {
                                request_confirm = true;
                            }
                        });
                        if self.persona_dirty {
                            ui.colored_label(ui.visuals().warn_fg_color, "有未确认的角色修改");
                        }
                        if self.persona_apply_pending {
                            ui.weak("正在等待服务确认角色切换……");
                        }
                    },
                );
            });
        if changed {
            self.persona_dirty = true;
            self.persona.taboos = self
                .taboos_editor
                .lines()
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect();
        }
        if request_confirm {
            self.persona.taboos = self
                .taboos_editor
                .lines()
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect();
            match self.persona.validate() {
                Ok(()) => {
                    self.pending_persona = Some(self.persona.clone());
                    self.confirm_persona = true;
                    self.push_log("persona draft is ready for confirmation");
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
    }

    fn show_health(&self, ui: &mut egui::Ui) {
        ui.collapsing("组件健康状态（实时刷新）", |ui| {
            if self.health.is_empty() {
                ui.weak("等待服务健康信息……");
                return;
            }
            ui.horizontal_wrapped(|ui| {
                for item in &self.health {
                    let color = if item.ready {
                        theme::SUCCESS
                    } else {
                        ui.visuals().error_fg_color
                    };
                    let label = if item.ready { "就绪" } else { "不可用" };
                    ui.colored_label(color, format!("{}：{}", item.component, label))
                        .on_hover_text(&item.detail);
                }
            });
        });
    }

    fn show_model_panel(&self, ui: &mut egui::Ui) {
        ui.collapsing("模型 Provider 诊断", |ui|
        {
            let Some(item) = self.active_model_health() else
            {
                ui.weak("尚未收到 DeepSeek、KoboldCpp 或 Ollama 的健康信息。");
                return;
            };
            let color = if item.ready
            {
                theme::SUCCESS
            }
            else
            {
                ui.visuals().error_fg_color
            };
            let state = if item.ready { "就绪" } else { "不可用" };
            ui.colored_label(color, format!("{}：{}", item.component, state));
            ui.label(&item.detail);
            ui.small("切换 Provider 或模型名需要重新运行首次设置，或修改配置后重启服务；桌面端不会偷偷替换当前模型。");
        });
    }

    fn show_policy_panel(&self, ui: &mut egui::Ui) {
        ui.collapsing("人格、记忆与自动化策略", |ui| {
            ui.label(format!(
                "当前人格：{} @ revision {} · 直播模式：{}",
                self.active_persona.name,
                self.active_persona.revision,
                self.active_persona.live_mode,
            ));
            if let Some(memory) = self.health.iter().find(|item| item.component == "memory") {
                let state = if memory.ready { "可用" } else { "不可用" };
                ui.label(format!("记忆：{}（{}）", state, memory.detail));
            } else {
                ui.weak("记忆状态尚未同步。");
            }
            if let Some(safety) = self.health.iter().find(|item| item.component == "safety") {
                let emergency = safety.detail.contains("emergency stop");
                let color = if emergency {
                    ui.visuals().error_fg_color
                } else {
                    theme::SUCCESS
                };
                ui.colored_label(color, format!("自动化安全门：{}", safety.detail));
            } else {
                ui.weak("自动化安全状态尚未同步。");
            }
            ui.small("人格修改必须确认；记忆默认本地保存；外部动作仍受权限、冷却和急停约束。");
        });
    }

    fn show_automation_panel(&self, ui: &mut egui::Ui) {
        ui.collapsing("视觉与游戏安全状态", |ui|
        {
            ui.horizontal(|ui|
            {
                ui.label("执行模式：");
                ui.colored_label(theme::SUCCESS, "dry-run（无副作用）");
            });
            ui.small("真实鼠标、键盘和进程启动不会从桌面界面直接触发。动作必须经过独立插件、白名单、审计和急停。");
            let relevant = self
                .health
                .iter()
                .filter(|item| {
                    let component = item.component.to_ascii_lowercase();
                    component.contains("automation")
                        || component.contains("vision")
                        || component.contains("plugin")
                        || component.contains("stage")
                        || component.contains("obs")
                })
                .collect::<Vec<_>>();
            if relevant.is_empty()
            {
                ui.weak("等待自动化/插件健康信息……");
                return;
            }
            for item in relevant
            {
                let color = if item.ready
                {
                    theme::SUCCESS
                }
                else
                {
                    ui.visuals().error_fg_color
                };
                let state = if item.ready { "就绪" } else { "不可用" };
                ui.horizontal(|ui|
                {
                    ui.colored_label(color, format!("{}：{}", item.component, state));
                    if !item.detail.is_empty()
                    {
                        ui.small(&item.detail);
                    }
                });
            }
        });
    }
    fn show_stage_panel(&self, ui: &mut egui::Ui) {
        if !self.show_developer {
            return;
        }
        ui.collapsing("舞台/OBS 动作遥测", |ui| {
            ui.small(format!(
                "schema={}，最近 {} 个动作",
                self.stage.schema_version,
                self.stage.actions.len()
            ));
            if self.stage.capabilities.is_empty() {
                ui.weak("舞台能力尚未同步；旧服务快照可能没有能力字段。");
            } else {
                ui.small(format!("当前能力：{}", self.stage.capabilities.join("、")));
            }
            if self.stage.actions.is_empty() {
                ui.weak("尚未收到舞台动作；可发送一条对话或运行 dry-run 回放后刷新。");
                return;
            }
            for action in self.stage.actions.iter().rev().take(24) {
                ui.monospace(format!(
                    "#{} [{}] {}",
                    action.sequence, action.kind, action.detail
                ));
            }
        });
    }

    fn show_developer_panel(&mut self, ui: &mut egui::Ui) {
        if !self.show_developer {
            return;
        }
        ui.collapsing("开发者诊断日志", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.small("桌面控制协议与事件流日志；服务端原始日志继续输出到启动终端。");
                if ui.button("导出日志").clicked() {
                    self.export_diagnostics();
                }
                if ui.button("清空").clicked() {
                    self.logs.clear();
                    self.export_feedback = None;
                }
            });
            ui.horizontal(|ui| {
                ui.label("筛选");
                ui.text_edit_singleline(&mut self.log_filter);
                if ui.button("清除筛选").clicked() {
                    self.log_filter.clear();
                }
            });
            let filtered = self
                .logs
                .iter()
                .filter(|line| self.log_filter.is_empty() || line.contains(&self.log_filter))
                .collect::<Vec<_>>();
            ui.small(format!(
                "显示 {} / {} 条；日志最多保留 200 条。",
                filtered.len(),
                self.logs.len()
            ));
            if let Some(feedback) = &self.export_feedback {
                ui.weak(feedback);
            }
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in filtered {
                        ui.monospace(line);
                    }
                });
        });
    }

    fn show_persona_confirmation(&mut self, context: &egui::Context) {
        if !self.confirm_persona {
            return;
        }
        let Some(profile) = self.pending_persona.clone() else {
            self.confirm_persona = false;
            return;
        };
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("确认角色切换")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui|
            {
                ui.heading(format!("{} @ revision {}", profile.name, profile.revision));
                ui.label(format!("档案：{}", profile.profile_id));
                self.show_scene_confirmation(ui);
                ui.label("相同档案 ID 保留经历；换档案会切换记忆范围并清空短期对话。活动回复期间会拒绝应用。");
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui|
                {
                    ui.label(profile.compiled_system_prompt());
                });
                ui.horizontal(|ui|
                {
                    if ui.button("取消").clicked()
                    {
                        cancel = true;
                    }
                    if ui.button("确认应用").clicked()
                    {
                        apply = true;
                    }
                });
            });
        if apply {
            self.apply_pending_persona(profile);
        } else if cancel {
            self.cancel_pending_persona();
        }
    }

    fn apply_pending_persona(&mut self, profile: PersonaSnapshot) {
        if self.memory.mutation_pending() {
            self.last_error = Some("记忆正在保存，请等操作结束后再切换角色。".to_owned());
            return;
        }
        if self.state.connection != ConnectionState::Connected {
            self.last_error = Some("服务未连接，无法应用角色。".to_owned());
            return;
        }
        self.resume.phase = crate::scene_resume::ResumePhase::Idle;
        self.persona_apply_pending = true;
        if self
            .worker
            .commands
            .send(WorkerCommand::SetPersona(profile))
            .is_err()
        {
            self.persona_apply_pending = false;
            self.last_error = Some("桌面网络工作线程已停止。".to_owned());
            return;
        }
        self.push_log("persona apply requested");
        self.applying_scene = self.pending_scene.take();
        self.confirm_persona = false;
        self.pending_persona = None;
    }

    fn cancel_pending_persona(&mut self) {
        self.resume.phase = crate::scene_resume::ResumePhase::Idle;
        self.confirm_persona = false;
        self.pending_persona = None;
        if self.pending_scene.take().is_some() {
            self.scene_files.feedback = Some("已取消场景切换，当前组合与草稿保留。".to_owned());
        }
        self.push_log("persona draft discarded");
    }

    fn show_emergency_confirmation(&mut self, context: &egui::Context) {
        if !self.confirm_emergency_stop {
            return;
        }
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("确认急停")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui| {
                ui.label("急停会撤销全部自动化许可，并尝试立即打断当前输出。");
                ui.label("本次服务运行期间不能从桌面界面恢复。");
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                    if ui
                        .add(egui::Button::new("确认急停").fill(egui::Color32::DARK_RED))
                        .clicked()
                    {
                        confirm = true;
                    }
                });
            });
        if confirm {
            self.send(WorkerCommand::EmergencyStop);
            self.confirm_emergency_stop = false;
        } else if cancel {
            self.confirm_emergency_stop = false;
        }
    }
}

impl eframe::App for DesktopApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.show_contents(ui);
        if (self.resume.dirty || self.character_library.dirty)
            && let Some(storage) = frame.storage_mut()
        {
            self.save_resume(storage);
            self.save_library(storage);
            storage.flush();
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.appearance.save(storage);
        self.save_resume(storage);
        self.save_library(storage);
    }
}
