use std::path::Path;

use ai_ex_config::native::{ModelSelection, VoiceSource};
use ai_ex_domain::NativeReply;

use super::*;

impl NativeRuntime {
    pub(super) fn settings_reply(&self) -> Result<NativeReply, AppError> {
        let config = self.config()?;
        Ok(NativeReply::Settings {
            revision: config.revision,
            document: config.to_toml()?,
        })
    }

    pub(super) async fn create_defaults(self: &Arc<Self>) -> Result<NativeReply, AppError> {
        let _mutation = self.mutation.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("原生运行已停止"));
        }
        let config = NativeConfig::default();
        self.validate_config(&config)?;
        let store = self.store()?;
        if let Some(parent) = store.path().parent() {
            std::fs::create_dir_all(parent).map_err(file_error)?;
        }
        store.create_new(&config)?;
        let restart = {
            let mut state = self.lock()?;
            state.configured = true;
            state.config = Ok(config);
            if !state.loading && state.manager.is_none() {
                state.loading = true;
                state.error = None;
                state.epoch = state.epoch.wrapping_add(1);
                true
            } else {
                false
            }
        };
        if restart {
            self.launch_bootstrap(None, None);
        }
        self.settings_reply()
    }

    pub(super) async fn save_settings(
        self: &Arc<Self>,
        expected: u64,
        document: &str,
    ) -> Result<NativeReply, AppError> {
        let replacement = NativeConfig::parse(document)?;
        let _mutation = self.mutation.lock().await;
        self.apply_settings(expected, replacement).await?;
        self.settings_reply()
    }

    pub(super) async fn apply_settings(
        self: &Arc<Self>,
        expected: u64,
        replacement: NativeConfig,
    ) -> Result<(), AppError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("原生运行已停止"));
        }
        self.validate_config(&replacement)?;
        let previous = self.config()?;
        let storage_changed = self.store()?.resolve_storage(&previous)?
            != self.store()?.resolve_storage(&replacement)?;
        if storage_changed {
            if self.lock()?.loading {
                return Err(AppError::invalid_transition(
                    "目录正在校验，请稍后再修改存储位置",
                ));
            }
            let manager = self.lock()?.manager.clone();
            let package_active = manager
                .map(|manager| manager.snapshot())
                .transpose()?
                .is_some_and(|packages| {
                    packages
                        .iter()
                        .any(|package| package.active_operation.is_some())
                });
            if self.operations.has_active()? || package_active {
                return Err(AppError::invalid_transition(
                    "请先完成或取消模型与声音操作，再修改存储位置",
                ));
            }
        }
        if storage_changed || previous.dialogue != replacement.dialogue {
            self.ensure_dialogue_idle().await?;
            self.stop_preview().await?;
            self.clear_dialogue().await?;
        }
        if previous.devices.playback_enabled && !replacement.devices.playback_enabled {
            self.stop_preview().await?;
        }
        let update = self.store()?.update(expected, &replacement)?;
        let retired = {
            let mut state = self.lock()?;
            state.config = Ok(update.config);
            state.configured = true;
            if storage_changed || (!state.loading && state.manager.is_none()) {
                state.epoch = state.epoch.wrapping_add(1);
                state.loading = true;
                state.error = None;
                Some((state.manager.take(), state.pool.take()))
            } else {
                None
            }
        };
        if let Some((manager, pool)) = retired {
            self.launch_bootstrap(manager, pool);
        }
        Ok(())
    }

    pub(super) fn validate_config(&self, config: &NativeConfig) -> Result<(), AppError> {
        config.validate()?;
        let catalog = self.catalog.as_ref().map_err(Clone::clone)?;
        let selection = |selection: &ModelSelection| -> Result<(), AppError> {
            let model = catalog
                .models
                .iter()
                .find(|model| model.manifest.id == selection.model_id)
                .ok_or_else(|| AppError::configuration("设置引用的模型不在当前固定目录中"))?;
            if packages::backend(model.manifest.backend) != selection.backend
                || !model
                    .manifest
                    .runtimes
                    .iter()
                    .any(|required| required.id == selection.runtime_id)
            {
                return Err(AppError::configuration("模型、能力和原生运行包不匹配"));
            }
            Ok(())
        };
        for selected in [&config.dialogue, &config.transcription, &config.embedding]
            .into_iter()
            .flatten()
        {
            selection(selected)?;
        }
        for voice in &config.voices {
            selection(&voice.model)?;
            if matches!(&voice.source, VoiceSource::Preset { speaker } if speaker != "zf_079") {
                return Err(AppError::configuration(
                    "当前默认声音只提供 zf_079；专属声音请导入参考 WAV",
                ));
            }
        }
        if config.devices.microphone_enabled && config.transcription.is_none() {
            return Err(AppError::configuration("开启麦克风前需要选择识别模型"));
        }
        let storage = self.store()?.resolve_storage(config)?;
        let roots = [&storage.packages, &storage.voices, &storage.jobs];
        for (index, root) in roots.iter().enumerate() {
            let root = storage_identity(root)?;
            for other in &roots[..index] {
                let other = storage_identity(other)?;
                if root == other
                    || root.starts_with(&format!("{other}\\"))
                    || other.starts_with(&format!("{root}\\"))
                {
                    return Err(AppError::configuration(
                        "模型、私人声音和临时作业目录必须相互独立",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn storage_identity(path: &Path) -> Result<String, AppError> {
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.try_exists().map_err(file_error)? {
        suffix.push(
            existing
                .file_name()
                .ok_or_else(|| AppError::configuration("存储目录没有可用的父路径"))?
                .to_owned(),
        );
        existing = existing
            .parent()
            .ok_or_else(|| AppError::configuration("存储目录没有可用的父路径"))?;
    }
    let mut canonical = std::fs::canonicalize(existing).map_err(file_error)?;
    if !std::fs::metadata(&canonical).map_err(file_error)?.is_dir() {
        return Err(AppError::configuration("原生存储位置必须是目录"));
    }
    for component in suffix.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_uppercase())
}

pub(super) fn file_error(error: std::io::Error) -> AppError {
    AppError::unavailable(format!("本地设置文件：{error}"))
}
