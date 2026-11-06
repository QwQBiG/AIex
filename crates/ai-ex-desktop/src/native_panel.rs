mod models;
mod speech_cleanup;
#[cfg(test)]
mod tests;
mod voices;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ai_ex_config::native::{NativeConfig, NativeMicrophoneInput, NativeSpeechCleanup};
use ai_ex_domain::{
    AppError, NativeInputDevice, NativeOperation, NativePackage, NativeReply, NativeRequest,
    NativeState,
};
use eframe::egui;
use uuid::Uuid;

use crate::worker::WorkerCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
enum DropTarget {
    Package(String),
    ReferenceWav,
}

struct StorageSpaceCheck {
    package_id: String,
    revision: u64,
    available_bytes: Option<u64>,
}

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
    unconfirmed_since: Option<u64>,
    package_unconfirmed: BTreeMap<u64, NativeRequest>,
    retained_package_operations: BTreeMap<Uuid, String>,
    retained_package_terminal: BTreeMap<Uuid, ai_ex_domain::NativeOperationPhase>,
    pub(super) download_confirmation: Option<String>,
    storage_space: Option<StorageSpaceCheck>,
    pub(super) remove_confirmation: Option<String>,
    pub(super) import_paths: BTreeMap<String, String>,
    drop_target: Option<DropTarget>,
    pub(super) selected_voice: String,
    voice_edit: Option<voices::VoiceEdit>,
    profile_id: String,
    pub(super) voice_name: String,
    pub(super) reference_path: String,
    pub(super) reference_text: String,
    pub(super) reference_english: bool,
    pub(super) preview_text: String,
    pub(super) step: usize,
    pub(super) device_draft: Option<ai_ex_config::native::NativeDevices>,
    pub(super) microphone_input_draft: Option<Option<NativeMicrophoneInput>>,
    pub(super) speech_cleanup_draft: Option<Option<NativeSpeechCleanup>>,
    pub(super) input_devices: Option<Vec<NativeInputDevice>>,
    pub(super) preview_operation: Option<Uuid>,
    voice_imports: BTreeMap<Uuid, voices::VoiceImport>,
    completed_voice_import: Option<voices::VoiceImport>,
    applied_microphone: Option<bool>,
    applied_microphone_input: Option<Option<NativeMicrophoneInput>>,
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
            unconfirmed_since: None,
            package_unconfirmed: BTreeMap::new(),
            retained_package_operations: BTreeMap::new(),
            retained_package_terminal: BTreeMap::new(),
            download_confirmation: None,
            storage_space: None,
            remove_confirmation: None,
            import_paths: BTreeMap::new(),
            drop_target: None,
            selected_voice: String::new(),
            voice_edit: None,
            profile_id: String::new(),
            voice_name: "我的声音".into(),
            reference_path: String::new(),
            reference_text: String::new(),
            reference_english: false,
            preview_text: "你好，很高兴认识你。今天过得怎么样？".into(),
            step: 0,
            device_draft: None,
            microphone_input_draft: None,
            speech_cleanup_draft: None,
            input_devices: None,
            preview_operation: None,
            voice_imports: BTreeMap::new(),
            completed_voice_import: None,
            applied_microphone: None,
            applied_microphone_input: None,
            microphone_restart_required: false,
            open_chat: false,
        }
    }
}

impl NativePanel {
    fn clear_storage_space(&mut self) {
        self.storage_space = None;
        self.pending
            .retain(|_, request| !matches!(request, NativeRequest::StorageSpace { .. }));
    }

    pub fn close_download_confirmation(&mut self) {
        self.download_confirmation = None;
        self.clear_storage_space();
    }

    fn request_storage_space(&mut self, requests: &mut Vec<NativeRequest>) {
        self.clear_storage_space();
        if let (Some(package_id), Some(config)) = (&self.download_confirmation, &self.config) {
            self.storage_space = Some(StorageSpaceCheck {
                package_id: package_id.clone(),
                revision: config.revision,
                available_bytes: None,
            });
            requests.push(NativeRequest::StorageSpace {
                expected_revision: config.revision,
            });
        }
    }

    fn arm_drop(&mut self, target: DropTarget) {
        self.drop_target = Some(target);
    }

    pub fn cancel_drop(&mut self) {
        self.drop_target = None;
    }

    pub fn waiting_for_drop(&self) -> bool {
        self.drop_target.is_some()
    }

    pub fn show_drop_banner(&mut self, ui: &mut egui::Ui) {
        let Some(target) = &self.drop_target else {
            return;
        };
        let name = match target {
            DropTarget::Package(id) => self
                .packages
                .iter()
                .find(|package| package.id == *id)
                .map_or_else(|| id.clone(), |package| package.name.clone()),
            DropTarget::ReferenceWav => "参考音频".to_owned(),
        };
        ui.horizontal_wrapped(|ui| {
            ui.strong(format!("等待拖入：{name}"));
            ui.label("将一个文件或文件夹拖到本窗口任意位置，只填写路径，不自动导入。");
            if ui.button("取消拖入").clicked() {
                self.cancel_drop();
                self.status = "已取消拖入。".into();
            }
        });
    }

    fn receive_drop(&mut self, ui: &mut egui::Ui) {
        if self.drop_target.is_none() {
            return;
        }
        let files = ui.input_mut(|input| std::mem::take(&mut input.raw.dropped_files));
        if files.is_empty() {
            return;
        }
        let target = self.drop_target.take().expect("drop target was present");
        let path = if files.len() != 1 {
            Err("请一次拖入一个文件或文件夹。")
        } else if let Some(path) = files[0].path.as_ref().and_then(|path| path.to_str()) {
            if path.is_empty() || path.chars().count() > 1024 {
                Err("路径为空或过长，请手动填写。")
            } else {
                Ok(path.to_owned())
            }
        } else {
            Err("系统未提供可用的本地路径，请手动填写。")
        };
        match (target, path) {
            (DropTarget::Package(id), Ok(path)) => {
                self.import_paths.insert(id, path);
                self.status = "已填入路径，请核对后点击“从本地导入”。".into();
            }
            (DropTarget::ReferenceWav, Ok(path)) => {
                self.reference_path = path;
                self.status = "已填入参考音频路径，请核对后点击“导入并保存专属音色”。".into();
            }
            (_, Err(message)) => self.status = message.into(),
        }
        ui.ctx().request_repaint();
    }

    pub fn receive(&mut self, request_id: u64, result: Result<NativeReply, AppError>) {
        let Some(request) = self.pending.remove(&request_id) else {
            return;
        };
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                if matches!(request, NativeRequest::StorageSpace { .. }) {
                    if let Some(space) = &mut self.storage_space {
                        space.available_bytes = None;
                    }
                    return;
                }
                if mutating(&request) {
                    let ambiguous = matches!(
                        error.kind,
                        ai_ex_domain::ErrorKind::Connectivity | ai_ex_domain::ErrorKind::Protocol
                    );
                    if ambiguous {
                        if package_mutation(&request) {
                            self.retain_cancel_id(&request);
                            self.package_unconfirmed.insert(request_id, request.clone());
                        } else if !matches!(
                            self.unconfirmed,
                            Some(NativeRequest::SaveSettings { .. })
                        ) || matches!(request, NativeRequest::SaveSettings { .. })
                        {
                            self.unconfirmed = Some(request.clone());
                            self.unconfirmed_since = Some(self.next_request);
                        }
                        self.uncertain = true;
                    } else if self.unconfirmed.is_none() && self.package_unconfirmed.is_empty() {
                        self.uncertain = false;
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
                    self.clear_storage_space();
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
                if !self.package_unconfirmed.is_empty() {
                    for package in &packages {
                        if let Some(id) = package.active_operation {
                            self.retained_package_operations
                                .entry(id)
                                .or_insert_with(|| package.id.clone());
                        }
                    }
                }
                self.packages = packages;
            }
            NativeReply::Settings { revision, document } => {
                if matches!(request, NativeRequest::Settings) {
                    if self
                        .pending
                        .values()
                        .any(|pending| matches!(pending, NativeRequest::SaveSettings { .. }))
                    {
                        return;
                    }
                    if let Some(NativeRequest::SaveSettings {
                        expected_revision, ..
                    }) = &self.unconfirmed
                    {
                        if self
                            .unconfirmed_since
                            .is_some_and(|barrier| request_id <= barrier)
                        {
                            self.refresh = true;
                            return;
                        }
                        if revision <= *expected_revision {
                            self.status =
                                "保存结果仍未确认，请稍后刷新，或重启服务后核对；不会自动重发。"
                                    .into();
                            return;
                        }
                    }
                }
                self.clear_storage_space();
                match NativeConfig::parse(&document) {
                    Ok(config) => {
                        if config.revision != revision {
                            self.status = "服务返回的设置版本不一致，请刷新状态。".into();
                            self.refresh = true;
                            return;
                        }
                        if self
                            .config
                            .as_ref()
                            .is_some_and(|current| current.revision > revision)
                        {
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
                        let input_baseline = self
                            .applied_microphone_input
                            .get_or_insert_with(|| config.microphone_input.clone());
                        self.microphone_restart_required = baseline
                            != config.devices.microphone_enabled
                            || *input_baseline != config.microphone_input;
                        if self.microphone_input_draft.as_ref() == Some(&config.microphone_input) {
                            self.microphone_input_draft = None;
                        }
                        if self.speech_cleanup_draft.as_ref() == Some(&config.speech_cleanup) {
                            self.speech_cleanup_draft = None;
                        }
                        if let (Some(previous), Some(mut draft)) = (&self.config, self.device_draft)
                        {
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
                        self.confirm_voice_edit(&config);
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
                                "设置已保存。麦克风设置重启服务后生效。".into()
                            } else {
                                "设置已保存。".into()
                            };
                        }
                    }
                    Err(error) => self.status = format!("无法读取原生设置：{error}"),
                }
            }
            NativeReply::Operation(operation) => {
                self.track_voice_import(&request, &operation);
                if !self.package_unconfirmed.is_empty() {
                    self.retained_package_operations
                        .entry(operation.id)
                        .or_insert_with(|| operation.package_id.clone());
                    if operation.phase.is_terminal() {
                        self.retained_package_terminal
                            .entry(operation.id)
                            .or_insert(operation.phase);
                    }
                }
                if matches!(request, NativeRequest::PreviewVoice { .. }) {
                    self.preview_operation = Some(operation.id);
                }
                if let Some(error) = &operation.error
                    && self.package_unconfirmed.is_empty()
                {
                    self.status = format!("操作未完成：{error}");
                }
                if operation.phase.is_terminal() {
                    if self.package_unconfirmed.is_empty() {
                        self.retained_package_operations.remove(&operation.id);
                        self.retained_package_terminal.remove(&operation.id);
                    }
                    if self.preview_operation == Some(operation.id) {
                        self.preview_operation = None;
                    }
                    self.refresh = true;
                    self.operations.remove(&operation.id);
                    if operation.error.is_none() && self.package_unconfirmed.is_empty() {
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
            NativeReply::StorageSpace {
                revision,
                available_bytes,
            } => {
                if matches!(request, NativeRequest::StorageSpace { expected_revision } if expected_revision == revision)
                    && self
                        .config
                        .as_ref()
                        .is_some_and(|config| config.revision == revision)
                    && let Some(space) = &mut self.storage_space
                    && space.revision == revision
                    && self.download_confirmation.as_deref() == Some(&space.package_id)
                {
                    space.available_bytes = available_bytes;
                }
            }
            NativeReply::InputDevices(devices) => {
                if matches!(request, NativeRequest::InputDevices) {
                    self.input_devices = Some(devices);
                    self.status = "设备列表已更新。".into();
                }
            }
            NativeReply::InputStatus { .. } => {}
        }
    }

    fn begin(&mut self, request: NativeRequest) -> Option<WorkerCommand> {
        if self.pending.len() >= 16
            || self.pending.values().any(|pending| pending == &request)
            || matches!(&request, NativeRequest::Cancel { id } if self.cancel_unconfirmed(*id))
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
            || self.microphone_input_draft.is_some()
            || self.speech_cleanup_draft.is_some()
            || self.voice_edit_dirty()
            || !self.reference_path.is_empty()
            || !self.reference_text.is_empty()
    }

    fn confirm_outcome(&mut self) {
        self.unconfirmed = None;
        self.unconfirmed_since = None;
        self.uncertain = !self.package_unconfirmed.is_empty();
        self.status = if self.uncertain {
            "准备或取消操作结果仍未确认；刷新不能证明原请求已经结束。".into()
        } else {
            "状态已重新核对，可继续操作。".into()
        };
    }

    fn cancel_unconfirmed(&self, operation_id: Uuid) -> bool {
        self.package_unconfirmed
            .values()
            .any(|request| matches!(request, NativeRequest::Cancel { id } if *id == operation_id))
    }

    fn retain_cancel_id(&mut self, request: &NativeRequest) {
        let NativeRequest::Cancel { id } = request else {
            return;
        };
        let package = self
            .operations
            .get(id)
            .map(|operation| operation.package_id.clone())
            .or_else(|| {
                self.packages
                    .iter()
                    .find(|package| package.active_operation == Some(*id))
                    .map(|package| package.id.clone())
            })
            .unwrap_or_default();
        self.retained_package_operations
            .entry(*id)
            .or_insert(package);
    }

    pub fn disconnected(&mut self) {
        self.cancel_drop();
        self.close_download_confirmation();
        let initialized = self.state.is_some()
            || self.config.is_some()
            || !self.pending.is_empty()
            || !self.operations.is_empty()
            || self.uncertain
            || self.unconfirmed.is_some();
        let pending_packages: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, request)| package_mutation(request))
            .map(|(id, request)| (*id, request.clone()))
            .collect();
        for (id, request) in pending_packages {
            self.retain_cancel_id(&request);
            self.package_unconfirmed.entry(id).or_insert(request);
        }
        for package in &self.packages {
            if let Some(id) = package.active_operation {
                self.retained_package_operations
                    .entry(id)
                    .or_insert_with(|| package.id.clone());
            }
        }
        for operation in self.operations.values() {
            if self
                .packages
                .iter()
                .any(|package| package.id == operation.package_id)
            {
                self.retained_package_operations
                    .entry(operation.id)
                    .or_insert_with(|| operation.package_id.clone());
            }
        }
        let pending_save = self.pending.values().find_map(|request| match request {
            NativeRequest::SaveSettings { .. } => Some(request.clone()),
            _ => None,
        });
        let unknown_save = self
            .unconfirmed
            .take()
            .filter(|request| matches!(request, NativeRequest::SaveSettings { .. }));
        self.pending.clear();
        self.operations.clear();
        self.preview_operation = None;
        self.voice_imports.clear();
        self.completed_voice_import = None;
        self.state = None;
        self.config = None;
        self.packages.clear();
        self.input_devices = None;
        self.refresh = true;
        self.last_poll = None;
        self.uncertain = initialized || !self.package_unconfirmed.is_empty();
        self.unconfirmed = unknown_save
            .or(pending_save)
            .or_else(|| initialized.then_some(NativeRequest::Settings));
        self.unconfirmed_since = self.unconfirmed.as_ref().map(|_| self.next_request);
        if !self.package_unconfirmed.is_empty() {
            self.status = "连接已断开；准备或取消操作结果仍未确认，不会自动重发。".into();
        } else if initialized {
            self.status = "连接已断开。恢复后会重新核对设置和模型，操作不会重发。".into();
        }
    }

    pub fn runtime_restarted(&mut self) {
        self.disconnected();
        self.applied_microphone = None;
        self.applied_microphone_input = None;
        self.microphone_restart_required = false;
    }

    pub fn microphone_restart_required(&self) -> bool {
        self.microphone_restart_required
    }

    fn saving_settings(&self) -> bool {
        self.uncertain
            || matches!(self.unconfirmed, Some(NativeRequest::SaveSettings { .. }))
            || self
                .pending
                .values()
                .any(|request| matches!(request, NativeRequest::SaveSettings { .. }))
    }

    fn confirm_old_service_stopped(&mut self) -> bool {
        if !matches!(self.unconfirmed, Some(NativeRequest::SaveSettings { .. })) {
            return false;
        }
        self.pending
            .retain(|_, request| !matches!(request, NativeRequest::Settings));
        self.unconfirmed = Some(NativeRequest::Settings);
        self.unconfirmed_since = None;
        self.status = "已确认原服务停止，正在重新核对设置。草稿不会自动保存。".into();
        true
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
        self.package_recovery(ui);
        if !connected {
            self.cancel_drop();
            self.close_download_confirmation();
            ui.label("正在连接本地服务，模型尚未准备也能打开此页面。");
            return Vec::new();
        }
        self.receive_drop(ui);
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
            | NativeRequest::InputDevices
            | NativeRequest::InputStatus { .. }
            | NativeRequest::StorageSpace { .. }
            | NativeRequest::Operation { .. }
    )
}

fn package_mutation(request: &NativeRequest) -> bool {
    matches!(
        request,
        NativeRequest::Download { .. }
            | NativeRequest::Import { .. }
            | NativeRequest::Remove { .. }
            | NativeRequest::Cancel { .. }
    )
}
