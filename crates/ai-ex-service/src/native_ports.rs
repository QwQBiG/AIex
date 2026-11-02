use std::sync::Arc;
use std::time::Duration;

use ai_ex_config::AppConfig;
use ai_ex_core::EmbeddingPort;
use ai_ex_domain::AppError;
use ai_ex_inference::RequestIdentity;
use ai_ex_memory::MemoryStore;
use ai_ex_native_models::{NativeEmbedder, NativeSynthesizer};
use ai_ex_tts::SynthesizedAudio;
use async_trait::async_trait;

use crate::native_runtime::NativeRuntime;

struct NativeMemoryEncoder(Arc<NativeRuntime>);

#[async_trait]
impl EmbeddingPort for NativeMemoryEncoder {
    fn space(&self) -> &str {
        ai_ex_native_models::BGE_SPACE
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
        let selection = self
            .0
            .config()?
            .embedding
            .ok_or_else(|| AppError::unavailable("本地记忆向量功能尚未启用"))?;
        let pool = self.0.worker_pool()?;
        // Provider construction starts loading; the first request needs its ready state.
        let started = std::time::Instant::now();
        tracing::debug!(model_id = %selection.model_id, "waiting for native memory encoder");
        if let Err(error) = pool.wait_ready(&selection, Duration::from_secs(120)).await {
            tracing::warn!(elapsed_ms = started.elapsed().as_millis(), %error, "native memory encoder unavailable");
            return Err(error);
        }
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            "native memory encoder ready"
        );
        let encoder = NativeEmbedder::new(pool, selection)?;
        encoder.embed(text).await
    }
}

pub async fn build_memory(
    config: &AppConfig,
    native: Arc<NativeRuntime>,
) -> Result<MemoryStore, AppError> {
    if !config.memory.enabled {
        return Ok(MemoryStore::disabled());
    }
    let memory = MemoryStore::open(&config.memory.path)
        .await?
        .with_recall_policy(
            config.memory.recall_policy,
            config.memory.exclude_recent_turns,
        )?;
    if native
        .config()
        .is_ok_and(|config| config.embedding.is_some())
    {
        Ok(memory.with_embedder(Arc::new(NativeMemoryEncoder(native))))
    } else {
        Ok(memory)
    }
}

pub struct NativeSpeech {
    synthesizer: NativeSynthesizer,
    identity: RequestIdentity,
    text: String,
}

pub fn speech(
    native: &Arc<NativeRuntime>,
    identity: RequestIdentity,
    text: String,
) -> Result<Option<NativeSpeech>, AppError> {
    if !native.config()?.devices.playback_enabled {
        return Ok(None);
    }
    let Some(voice) = native.current_voice()? else {
        return Ok(None);
    };
    let synthesizer = NativeSynthesizer::new(native.worker_pool()?, voice)?;
    Ok(Some(NativeSpeech {
        synthesizer,
        identity,
        text,
    }))
}

impl NativeSpeech {
    pub async fn synthesize(&self) -> Result<SynthesizedAudio, AppError> {
        let audio = self
            .synthesizer
            .synthesize(self.identity, &self.text)
            .await?;
        Ok(SynthesizedAudio {
            bytes: audio.bytes,
            content_type: "audio/wav".into(),
        })
    }

    pub async fn cancel(&self) -> Result<(), AppError> {
        self.synthesizer.cancel(self.identity).await
    }
}

#[cfg(not(feature = "native-capture"))]
pub fn spawn_duplex(
    _config: &AppConfig,
    native: Arc<NativeRuntime>,
    _runtime: crate::conversation_activity::ConversationActivity,
    _automation: crate::template_host::TemplateHost,
) -> Result<Option<tokio::task::JoinHandle<()>>, AppError> {
    if native.config().is_ok_and(|c| c.devices.microphone_enabled) {
        tracing::warn!("Microphone requested, but this binary lacks native-capture");
    }
    Ok(None)
}

#[cfg(feature = "native-capture")]
pub fn spawn_duplex(
    config: &AppConfig,
    native: Arc<NativeRuntime>,
    runtime: crate::conversation_activity::ConversationActivity,
    automation: crate::template_host::TemplateHost,
) -> Result<Option<tokio::task::JoinHandle<()>>, AppError> {
    if !native.config().is_ok_and(|c| c.devices.microphone_enabled) {
        return Ok(None);
    }
    let settings = config.duplex.clone();
    Ok(Some(tokio::spawn(async move {
        if let Err(error) = run_native_capture(settings, native, runtime, automation).await {
            // An unavailable recognizer or input device leaves the control and
            // conversation service alive so the user can correct its settings.
            tracing::error!(%error, "native microphone input unavailable");
        }
    })))
}

#[cfg(feature = "native-capture")]
async fn run_native_capture(
    settings: ai_ex_config::DuplexConfig,
    native: Arc<NativeRuntime>,
    runtime: crate::conversation_activity::ConversationActivity,
    automation: crate::template_host::TemplateHost,
) -> Result<(), AppError> {
    use ai_ex_capture::{CaptureSettings, NativeAudioSource};
    use ai_ex_duplex::{DuplexController, EnergyVad, VadConfig};
    use ai_ex_native_models::NativeTranscriber;

    native.wait_ready(Duration::from_secs(120)).await?;
    let selection = native
        .config()?
        .transcription
        .ok_or_else(|| AppError::unavailable("尚未选择本地语音识别模型"))?;
    let pool = native.worker_pool()?;
    let transcriber = NativeTranscriber::new(pool.clone(), selection.clone())?;
    pool.wait_ready(&selection, Duration::from_secs(120))
        .await?;
    let source = NativeAudioSource::open(CaptureSettings {
        device_name: match settings.input_device.trim() {
            "" => None,
            device => Some(device.to_owned()),
        },
        queue_capacity: settings.capture_queue_capacity,
    })?;
    let vad = EnergyVad::new(VadConfig {
        start_threshold: settings.start_threshold,
        continue_threshold: settings.continue_threshold,
        start_frames: settings.start_frames,
        end_silence_frames: settings.end_silence_frames,
        max_utterance_frames: settings.max_utterance_frames,
    })?;
    crate::run_duplex(
        source,
        DuplexController::new(vad, transcriber),
        runtime,
        automation,
    )
    .await
}
