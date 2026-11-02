use ai_ex_config::native::{
    ModelSelection, NativeBackend, VoiceLanguage, VoicePreset, VoiceSource,
};
use ai_ex_domain::{NativeOperationPhase, NativeReply};

use super::*;

impl NativeRuntime {
    pub(super) async fn import_voice(
        self: &Arc<Self>,
        name: String,
        source: PathBuf,
        text: String,
        language: String,
        expected: u64,
    ) -> Result<NativeReply, AppError> {
        let _mutation = self.mutation.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("原生运行已停止"));
        }
        let config = self.config()?;
        if !self.lock()?.configured {
            return Err(AppError::unavailable("请先创建原生设置"));
        }
        if config.revision != expected {
            return Err(AppError::invalid_transition(
                "原生设置已改变，请刷新后导入声音",
            ));
        }
        let language = match language.as_str() {
            "zh" => VoiceLanguage::Zh,
            "en" => VoiceLanguage::En,
            _ => return Err(AppError::configuration("参考音频语言不受支持")),
        };
        let pool = self.worker_pool()?;
        let storage = self.store()?.resolve_storage(&config)?;
        let context = self
            .operations
            .reserve("voice-import", NativeOperationPhase::Importing)?;
        let operation_id = context.id;
        let task_context = context.clone();
        let runtime = self.clone();
        let future = async move {
            task_context.check()?;
            let audio_id = pool.import_reference_wav(&source).await?;
            let result = async {
                task_context.check()?;
                let _mutation = tokio::select! {
                    _ = task_context.cancelled() => return Err(AppError::invalid_transition("参考声音导入已取消")),
                    guard = runtime.mutation.lock() => guard,
                };
                if runtime.closed.load(Ordering::Acquire) { return Err(AppError::unavailable("原生运行已停止")); }
                task_context.committing()?;
                let mut config = runtime.config()?;
                if config.revision != expected { return Err(AppError::invalid_transition("声音导入期间设置已改变，请刷新后重试")); }
                config.voices.push(VoicePreset {
                    id: format!("voice-{operation_id}"), label: name, language,
                    model: ModelSelection::cpu(NativeBackend::Qwen3TtsOnnx, "qwen3-tts-base-0.6b"),
                    source: VoiceSource::ReferenceAudio { audio_id: audio_id.clone(), text },
                });
                runtime.apply_settings(expected, config).await
            }.await;
            if let Err(mut error) = result {
                // This UUID was created only by this import, never by a caller.
                let path = storage.voices.join(format!("{audio_id}.wav"));
                if let Err(cleanup) = std::fs::remove_file(path)
                    && cleanup.kind() != std::io::ErrorKind::NotFound
                {
                    error
                        .message
                        .push_str(&format!("；新导入音频清理失败：{cleanup}"));
                }
                return Err(error);
            }
            Ok(())
        };
        self.operations.launch(context, future)?;
        Ok(NativeReply::Operation(
            self.operations
                .get(operation_id)?
                .expect("voice operation was reserved"),
        ))
    }

    pub(super) async fn bind_voice(
        self: &Arc<Self>,
        profile_id: String,
        voice_id: String,
        expected: u64,
    ) -> Result<NativeReply, AppError> {
        let _mutation = self.mutation.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("原生运行已停止"));
        }
        let mut config = self.config()?;
        if !config.voices.iter().any(|voice| voice.id == voice_id) {
            return Err(AppError::configuration("所选声音不存在"));
        }
        config.character_voices.insert(profile_id, voice_id);
        self.apply_settings(expected, config).await?;
        self.settings_reply()
    }
}
