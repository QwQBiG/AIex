use ai_ex_native_worker_support::{
    AppError, ArtifactKind, Engine, InferenceOutput, InferenceRequest, InferenceTask, JobDirectory,
    VerifiedConfig, VoiceSelection,
};
use ort::environment::GlobalThreadPoolOptions;
use serde::Deserialize;
use wavekat_tts::{
    AudioFrame,
    backends::qwen3_tts::{CloneRequest, ModelConfig, ModelPrecision, Qwen3TtsClone},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Files {
    runtime_dll: String,
    model_directory: String,
}

pub struct Qwen {
    id: String,
    backend: Qwen3TtsClone,
    jobs: JobDirectory,
}

impl Qwen {
    pub fn load(config: VerifiedConfig<Files>) -> Result<Self, AppError> {
        if config.manifest.revision != "177cd39e2ddab0d4d7ce5152a7abc2d382621158" {
            return Err(AppError::configuration(
                "Qwen Base preset requires its fixed model revision",
            ));
        }
        let dll = config.artifact(&config.backend.runtime_dll, ArtifactKind::NativeRuntime)?;
        let directory = config.artifact_directory(&config.backend.model_directory)?;
        // Thread pool options call the ORT API, so select the verified DLL before
        // creating them; otherwise ORT may try its default library search path.
        let environment = ort::init_from(&dll).map_err(failure)?;
        let pool = GlobalThreadPoolOptions::default()
            .with_intra_threads(2)
            .map_err(failure)?
            .with_inter_threads(1)
            .map_err(failure)?
            .with_spin_control(false)
            .map_err(failure)?;
        if !environment
            .with_global_thread_pool(pool)
            .with_telemetry(false)
            .commit()
        {
            return Err(AppError::configuration("ORT already initialized"));
        }
        let settings = ModelConfig::default()
            .with_precision(ModelPrecision::Int4)
            .with_dir(directory);
        let backend = Qwen3TtsClone::from_config(settings).map_err(failure)?;
        Ok(Self {
            id: config.manifest.id,
            backend,
            jobs: JobDirectory::current()?,
        })
    }
}

impl Engine for Qwen {
    fn model_id(&self) -> &str {
        &self.id
    }

    fn run(&mut self, request: &InferenceRequest) -> Result<InferenceOutput, AppError> {
        let InferenceTask::Synthesize {
            text,
            language,
            voice:
                VoiceSelection::Reference {
                    audio,
                    text: reference_text,
                },
            output,
        } = &request.task
        else {
            return Err(AppError::configuration(
                "Qwen Base requires a reference voice",
            ));
        };
        if !matches!(language.as_str(), "zh" | "en") {
            return Err(AppError::configuration(
                "Qwen language is not supported by this preset",
            ));
        }
        let (samples, sample_rate) = self.jobs.read_audio(audio)?;
        if samples.len() as u64 > u64::from(sample_rate) * 30 {
            return Err(AppError::configuration(
                "reference WAV must not exceed 30 seconds",
            ));
        }
        let reference = AudioFrame::from_vec(samples, sample_rate);
        let reference = if sample_rate == 24_000 {
            reference
        } else {
            reference.resample(24_000).map_err(failure)?
        };
        let settings = CloneRequest::new(text, reference.samples(), 24_000, reference_text)
            .with_language(language);
        eprintln!("NATIVE_INFERENCE_BEGIN {}", request.identity.request_id);
        let audio = self.backend.synthesize_clone(&settings).map_err(failure)?;
        self.jobs
            .write_audio(output, audio.samples(), audio.sample_rate())?;
        Ok(InferenceOutput::Audio {
            file: output.clone(),
            sample_rate: audio.sample_rate(),
        })
    }
}

fn failure(error: impl std::fmt::Display) -> AppError {
    AppError::unavailable(format!("Qwen native inference failed: {error}"))
}
