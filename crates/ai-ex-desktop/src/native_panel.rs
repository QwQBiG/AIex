mod models;
#[cfg(test)]
mod tests;
mod voices;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ai_ex_config::native::NativeConfig;
use ai_ex_domain::{
    AppError, NativeOperation, NativePackage, NativeReply, NativeRequest, NativeState,
};
use eframe::egui;
use uuid::Uuid;

use crate::worker::WorkerCommand;

pub struct NativePanel {
    pub(super) state: Option<NativeState>,
    pub(super) packages: Vec<NativePackage>,
    pub(super) config: Option<NativeConfig>,
    pub(super) operations: BTreeMap<Uuid, NativeOperation>,
    pending: BTreeMap<u64, NativeRequest>,
    next_request: u64,
    last_poll: Option<Instant>,
    refresh: bool,
    pub(super) status: String,
    pub(super) uncertain: bool,
    unconfirmed: Option<NativeRequest>,
    pub(super) download_confirmation: Option<String>,
    pub(super) remove_confirmation: Option<String>,
    pub(super) import_paths: BTreeMap<String, String>,
    pub(super) selected_voice: String,
    profile_id: String,
    pub(super) voice_name: String,
    pub(super) reference_path: String,
    pub(super) reference_text: String,
    pub(super) reference_english: bool,
    pub(super) preview_text: String,
    pub(super) step: usize,
    pub(super) device_draft: Option<ai_ex_config::native::NativeDevices>,
    pub(super) preview_operation: Option<Uuid>,
    voice_imports: BTreeMap<Uuid, voices::VoiceImport>,
    completed_voice_import: Option<voices::VoiceImport>,
    applied_microphone: Option<bool>,
    microphone_restart_required: bool,
    pub(super) open_chat: bool,
}

impl Default for NativePanel {
    fn default() -> Self {
        Self {
            state: None,
            packages: Vec::new(),
            config: None,
            operations: BTreeMap::new(),
            pending: BTreeMap::new(),
            next_request: 0,
            last_poll: None,
            refresh: true,
            status: String::new(),
            uncertain: false,
            unconfirmed: None,
            download_confirmation: None,
            remove_confirmation: None,
            import_paths: BTreeMap::new(),
            selected_voice: String::new(),
            profile_id: String::new(),
            voice_name: "我的声音".into(),
            reference_path: String::new(),
            reference_text: String::new(),
            reference_english: false,
            preview_text: "你好，很高兴认识你。今天过得怎么样？".into(),
            step: 0,
            device_draft: None,
            preview_operation: None,
            voice_imports: BTreeMap::new(),
            completed_voice_import: None,
            applied_microphone: None,
            microphone_restart_required: false,
            open_chat: false,
        }
    }
}

impl NativePanel {
    pub fn receive(&mut self, request_id: u64, result: Result<NativeReply, AppError>) {
        let Some(request) = self.pending.remove(&request_id) else {
            return;
        };
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                if mutating(&request) {
                    self.uncertain = matches!(
                        error.kind,
                        ai_ex_domain::ErrorKind::Connectivity | ai_ex_domain::ErrorKind::Protocol
                    );
                    if self.uncertain {
                        self.unconfirmed = Some(request.clone());
                    }
                    self.refresh = true;
                }
                self.status = if self.uncertain {
                    format!("结果待核对，请先刷新状态；操作不会自动重发。{error}")
                } else {
                    format!("操作未完成：{error}")
                };
                return;
            }
        };
        match reply {
            NativeReply::State(state) => {
                if !state.configured {
                    self.config = None;
                }
                if self
                    .state
                    .as_ref()
                    .is_none_or(|old| old.configured != state.configured)
                {
                    self.refresh = true;
                }
                if matches!(
                    &self.unconfirmed,
                    Some(NativeRequest::PreviewVoice { .. } | NativeRequest::StopPreview)
                ) || matches!(
                    &self.unconfirmed,
                    Some(NativeRequest::CreateDefaults | NativeRequest::Settings)
                ) && !state.configured
                {
                    self.confirm_outcome();
                }
                self.state = Some(state);
            }
            NativeReply::Packages(packages) => {
                self.packages = packages;
                if matches!(
                    &self.unconfirmed,
                    Some(
                        NativeRequest::Download { .. }
                            | NativeRequest::Import { .. }
                            | NativeRequest::Remove { .. }
                            | NativeRequest::Cancel { .. }
                    )
                ) {
                    self.confirm_outcome();
                }
            }
            NativeReply::Settings { revision, document } => match NativeConfig::parse(&document) {
                Ok(config) => {
                    if config.revision != revision {
                        self.status = "服务返回的设置版本不一致，请刷新状态。".into();
                        self.refresh = true;
                        return;
                    }
                    if self.selected_voice.is_empty()
                        || !config
                            .voices
                            .iter()
                            .any(|voice| voice.id == self.selected_voice)
                    {
                        self.selected_voice = config
                            .voice_for_profile(&self.profile_id)
                            .map(|voice| voice.id.clone())
                            .unwrap_or_default();
                    }
                    let baseline = *self
                        .applied_microphone
                        .get_or_insert(config.devices.microphone_enabled);
                    self.microphone_restart_required =
                        baseline != config.devices.microphone_enabled;
                    if let (Some(previous), Some(mut draft)) = (&self.config, self.device_draft) {
                        if draft.microphone_enabled == previous.devices.microphone_enabled {
                            draft.microphone_enabled = config.devices.microphone_enabled;
                        }
                        if draft.playback_enabled == previous.devices.playback_enabled {
                            draft.playback_enabled = config.devices.playback_enabled;
                        }
                        self.device_draft = (draft != config.devices).then_some(draft);
                    }
                    if self
                        .device_draft
                        .is_some_and(|draft| draft == config.devices)
                    {
                        self.device_draft = None;
                    }
                    self.confirm_voice_import(&config);
                    self.config = Some(config);
                    if matches!(request, NativeRequest::ImportVoice { .. }) {
                        self.reference_path.clear();
                        self.reference_text.clear();
                        if let Some(voice) =
                            self.config.as_ref().and_then(|config| config.voices.last())
                        {
                            self.selected_voice = voice.id.clone();
                        }
                    }
                    if matches!(
                        &self.unconfirmed,
                        Some(
                            NativeRequest::SaveSettings { .. }
                                | NativeRequest::ImportVoice { .. }
                                | NativeRequest::BindVoice { .. }
                                | NativeRequest::CreateDefaults
                                | NativeRequest::Settings
                        )
                    ) {
                        self.confirm_outcome();
                    }
                    if mutating(&request) {
                        self.status = if self.microphone_restart_required {
                            "设置已保存。麦克风开关重新进入对话后生效。".into()
                        } else {
                            "设置已保存。".into()
                        };
                    }
                }
                Err(error) => self.status = format!("无法读取原生设置：{error}"),
            },
            NativeReply::Operation(operation) => {
                self.track_voice_import(&request, &operation);
                if matches!(request, NativeRequest::PreviewVoice { .. }) {
                    self.preview_operation = Some(operation.id);
                }
                if let Some(error) = &operation.error {
                    self.status = format!("操作未完成：{error}");
                }
                if operation.phase.is_terminal() {
                    if self.preview_operation == Some(operation.id) {
                        self.preview_operation = None;
                    }
                    self.refresh = true;
                    self.operations.remove(&operation.id);
                    if operation.error.is_none() {
                        self.status = format!(
                            "{}：{}",
                            self.packages
                                .iter()
                                .find(|package| package.id == operation.package_id)
                                .map_or("操作", |package| package.name.as_str()),
                            if operation.phase == ai_ex_domain::NativeOperationPhase::Cancelled {
                                "已取消"
                            } else {
                                "已完成"
                            }
                        );
                    }
                } else {
                    self.operations.insert(operation.id, operation);
                }
            }
            NativeReply::Accepted => {
                self.refresh = true;
            }
        }
    }

    fn begin(&mut self, request: NativeRequest) -> Option<WorkerCommand> {
        if self.pending.len() >= 16
            || self.pending.values().any(|pending| pending == &request)
            || self.uncertain
                && mutating(&request)
                && !matches!(
                    request,
                    NativeRequest::StopPreview | NativeRequest::Cancel { .. }
                )
        {
            return None;
        }
        self.next_request = self.next_request.checked_add(1)?;
        self.pending.insert(self.next_request, request.clone());
        Some(WorkerCommand::Native {
            request_id: self.next_request,
            request,
        })
    }

    pub fn has_pending(&self) -> bool {
        self.pending.values().any(mutating)
    }
    pub fn has_unsaved(&self) -> bool {
        self.has_pending()
            || self.uncertain
            || !self.operations.is_empty()
            || self
                .state
                .as_ref()
                .is_some_and(|state| state.preview_active)
            || self.device_draft.is_some()
            || !self.reference_path.is_empty()
            || !self.reference_text.is_empty()
    }

    fn confirm_outcome(&mut self) {
        self.uncertain = false;
        self.unconfirmed = None;
        self.status = "状态已重新核对，可继续操作。".into();
    }

    pub fn disconnected(&mut self) {
        let initialized = self.state.is_some()
            || self.config.is_some()
            || !self.pending.is_empty()
            || !self.operations.is_empty();
        self.pending.clear();
        self.operations.clear();
        self.preview_operation = None;
        self.voice_imports.clear();
        self.completed_voice_import = None;
        self.state = None;
        self.config = None;
        self.packages.clear();
        self.refresh = true;
        self.last_poll = None;
        self.uncertain = initialized;
        self.unconfirmed = initialized.then_some(NativeRequest::Settings);
        if initialized {
            self.status = "连接已断开。恢复后会重新核对设置和模型，操作不会重发。".into();
        }
    }

    pub fn runtime_restarted(&mut self) {
        self.disconnected();
        self.applied_microphone = None;
        self.microphone_restart_required = false;
    }

    pub fn microphone_restart_required(&self) -> bool {
        self.microphone_restart_required
    }

    pub fn take_open_chat(&mut self) -> bool {
        std::mem::take(&mut self.open_chat)
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        profile_id: &str,
        connected: bool,
    ) -> Vec<WorkerCommand> {
        let mut requests = Vec::new();
        ui.heading("准备本地伙伴");
        ui.weak("准备完成后可以离线对话。人物外形与其他设置随时可用。");
        if !connected {
            ui.label("正在连接本地服务，模型尚未准备也能打开此页面。");
            return Vec::new();
        }
        if self.profile_id != profile_id {
            self.profile_id = profile_id.into();
            if let Some(voice) = self
                .config
                .as_ref()
                .and_then(|config| config.voice_for_profile(profile_id))
            {
                self.selected_voice = voice.id.clone();
            }
        }
        if self.refresh
            || self
                .last_poll
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
        {
            let reload = self.refresh;
            self.refresh = false;
            self.last_poll = Some(Instant::now());
            requests.push(NativeRequest::State);
            requests.push(NativeRequest::Packages);
            if self.state.as_ref().is_some_and(|state| state.configured)
                && (self.config.is_none() || reload)
            {
                requests.push(NativeRequest::Settings);
            }
            for operation in self
                .operations
                .values()
                .filter(|operation| !operation.phase.is_terminal())
            {
                requests.push(NativeRequest::Operation { id: operation.id });
            }
            for id in self
                .packages
                .iter()
                .filter_map(|package| package.active_operation)
            {
                requests.push(NativeRequest::Operation { id });
            }
        }
        self.contents(ui, profile_id, &mut requests);
        requests
            .into_iter()
            .filter_map(|request| self.begin(request))
            .collect()
    }
}

fn mutating(request: &NativeRequest) -> bool {
    !matches!(
        request,
        NativeRequest::State
            | NativeRequest::Packages
            | NativeRequest::Settings
            | NativeRequest::Operation { .. }
    )
}
