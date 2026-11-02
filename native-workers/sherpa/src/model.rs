use ai_ex_native_worker_support::{
    AppError, ArtifactKind, Engine, InferenceOutput, InferenceRequest, InferenceTask, JobDirectory,
    ModelBackend, VerifiedConfig, VoiceSelection,
};
use serde::Deserialize;
use sherpa_onnx::{OfflineRecognizer, OfflineTts};

mod kokoro;
mod whisper;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Files {
    Whisper {
        encoder: String,
        decoder: String,
        tokens: String,
    },
    Kokoro {
        model: String,
        voices: String,
        tokens: String,
        espeak_data: String,
        dict: String,
        lexicon_en: String,
        lexicon_zh: String,
        date_fst: String,
        number_fst: String,
        phone_fst: String,
    },
}

pub struct Sherpa {
    id: String,
    engine: Backend,
    jobs: JobDirectory,
}

enum Backend {
    Whisper(Box<OfflineRecognizer>),
    Kokoro(Box<OfflineTts>),
}

impl Sherpa {
    pub fn load(config: VerifiedConfig<Files>) -> Result<Self, AppError> {
        let expected_revision = match config.manifest.backend {
            ModelBackend::SherpaWhisper => "8f3c18b358db4d1f2fc1eae49d75cd20989e4309",
            ModelBackend::SherpaKokoro => "423bfb9ccd6a66115a76602a9630ffea265ae176",
            _ => return Err(AppError::configuration("unsupported Sherpa backend")),
        };
        if config.manifest.revision != expected_revision {
            return Err(AppError::configuration(
                "Sherpa preset requires its fixed model revision",
            ));
        }
        let engine = match &config.backend {
            Files::Whisper { .. } if config.manifest.backend == ModelBackend::SherpaWhisper => {
                Backend::Whisper(Box::new(whisper::load(&config)?))
            }
            Files::Kokoro { .. } if config.manifest.backend == ModelBackend::SherpaKokoro => {
                Backend::Kokoro(Box::new(kokoro::load(&config)?))
            }
            _ => return Err(AppError::configuration("Sherpa config backend mismatch")),
        };
        Ok(Self {
            id: config.manifest.id,
            engine,
            jobs: JobDirectory::current()?,
        })
    }
}

impl Engine for Sherpa {
    fn model_id(&self) -> &str {
        &self.id
    }

    fn run(&mut self, request: &InferenceRequest) -> Result<InferenceOutput, AppError> {
        match (&mut self.engine, &request.task) {
            (Backend::Whisper(recognizer), InferenceTask::Transcribe { audio, language })
                if language == "zh" =>
            {
                let (samples, rate) = self.jobs.read_audio(audio)?;
                let rate = i32::try_from(rate)
                    .map_err(|_| AppError::configuration("audio sample rate is too large"))?;
                let stream = recognizer.create_stream();
                stream.accept_waveform(rate, &samples);
                eprintln!("NATIVE_INFERENCE_BEGIN {}", request.identity.request_id);
                recognizer.decode(&stream);
                let result = stream
                    .get_result()
                    .ok_or_else(|| AppError::unavailable("Whisper returned no result"))?;
                Ok(InferenceOutput::Transcript { text: result.text })
            }
            (
                Backend::Kokoro(tts),
                InferenceTask::Synthesize {
                    text,
                    language,
                    voice: VoiceSelection::Preset { speaker },
                    output,
                },
            ) if matches!(language.as_str(), "zh" | "en") && speaker == "zf_079" => {
                eprintln!("NATIVE_INFERENCE_BEGIN {}", request.identity.request_id);
                let audio = tts
                    .generate_with_config::<fn(&[f32], f32) -> bool>(
                        text,
                        &sherpa_onnx::GenerationConfig {
                            sid: 46,
                            ..Default::default()
                        },
                        None,
                    )
                    .ok_or_else(|| AppError::unavailable("Kokoro returned no audio"))?;
                self.jobs
                    .write_audio(output, audio.samples(), audio.sample_rate() as u32)?;
                Ok(InferenceOutput::Audio {
                    file: output.clone(),
                    sample_rate: audio.sample_rate() as u32,
                })
            }
            _ => Err(AppError::configuration(
                "task, language or voice is unsupported by loaded Sherpa model",
            )),
        }
    }
}

fn path(
    config: &VerifiedConfig<Files>,
    file: &str,
    kind: ArtifactKind,
) -> Result<String, AppError> {
    config
        .artifact(file, kind)?
        .into_os_string()
        .into_string()
        .map_err(|_| AppError::configuration("Sherpa path is not UTF-8"))
}
