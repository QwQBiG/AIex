use ai_ex_config::native::{VoicePreset, VoiceSource};
use ai_ex_domain::AppError;
use ai_ex_inference::{
    AudioFile, InferenceOutput, InferenceRequest, InferenceTask, RequestIdentity, VoiceSelection,
};

use crate::{NativeWorkerPool, files, voices};

#[derive(Debug)]
pub struct NativeAudio {
    pub bytes: Vec<u8>,
    pub sample_rate: u32,
}

#[derive(Clone)]
pub struct NativeSynthesizer {
    pool: NativeWorkerPool,
    voice: VoicePreset,
}

impl NativeSynthesizer {
    pub fn new(pool: NativeWorkerPool, voice: VoicePreset) -> Result<Self, AppError> {
        voice.validate()?;
        pool.warmup(&voice.model)?;
        Ok(Self { pool, voice })
    }

    pub async fn synthesize(
        &self,
        identity: RequestIdentity,
        text: &str,
    ) -> Result<NativeAudio, AppError> {
        // Only speech waits for a cold model. Dropping this wait cannot enqueue
        // the old sentence; embeddings retain their immediate lexical fallback.
        self.pool
            .wait_ready(&self.voice.model, std::time::Duration::from_secs(120))
            .await?;
        let files = self.pool.new_request(&self.voice.model)?;
        let voice = match &self.voice.source {
            VoiceSource::Preset { speaker } => VoiceSelection::Preset {
                speaker: speaker.clone(),
            },
            VoiceSource::ReferenceAudio { audio_id, text } => {
                let source = voices::resource_path(self.pool.voices_root(), audio_id)?;
                let bytes = files::read_audio(&source)?;
                voices::inspect_pcm16(&bytes, None)?;
                files::write_new(&files.directory.path.join("reference.wav"), &bytes)?;
                VoiceSelection::Reference {
                    audio: AudioFile {
                        path: files.relative("reference.wav"),
                    },
                    text: text.clone(),
                }
            }
        };
        let task = InferenceTask::Synthesize {
            text: text.to_owned(),
            language: self.voice.language.as_str().into(),
            voice,
            output: AudioFile {
                path: files.relative("output.wav"),
            },
        };
        let result = self
            .pool
            .execute(
                &self.voice.model,
                InferenceRequest {
                    identity,
                    model_id: self.voice.model.model_id.clone(),
                    task,
                },
                files,
            )
            .await?;
        let InferenceOutput::Audio { sample_rate, .. } = result.output else {
            return Err(AppError::protocol(
                "native speech worker returned a different output kind",
            ));
        };
        let bytes = files::read_audio(&result.files.directory.path.join("output.wav"))?;
        voices::inspect_pcm16(&bytes, Some(sample_rate))?;
        Ok(NativeAudio { bytes, sample_rate })
    }

    pub async fn cancel(&self, identity: RequestIdentity) -> Result<(), AppError> {
        self.pool.cancel(&self.voice.model, identity).await
    }
}
