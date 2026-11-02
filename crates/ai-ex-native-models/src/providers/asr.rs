use ai_ex_config::native::{ModelSelection, NativeBackend};
use ai_ex_domain::AppError;
use ai_ex_duplex::{TranscriberPort, Utterance};
use ai_ex_inference::{
    AudioFile, InferenceOutput, InferenceRequest, InferenceTask, RequestIdentity,
    encode_mono_pcm16_wav,
};
use async_trait::async_trait;
use uuid::Uuid;

use crate::{NativeWorkerPool, files};

pub struct NativeTranscriber {
    pool: NativeWorkerPool,
    model: ModelSelection,
    session: Uuid,
    generation: u64,
}
impl NativeTranscriber {
    pub fn new(pool: NativeWorkerPool, model: ModelSelection) -> Result<Self, AppError> {
        if model.backend != NativeBackend::SherpaWhisper {
            return Err(files::error("native transcription requires Whisper"));
        }
        pool.warmup(&model)?;
        Ok(Self {
            pool,
            model,
            session: Uuid::new_v4(),
            generation: 0,
        })
    }
}

#[async_trait]
impl TranscriberPort for NativeTranscriber {
    async fn transcribe(&mut self, utterance: Utterance) -> Result<String, AppError> {
        let channels = usize::from(utterance.channels);
        if channels == 0
            || utterance.samples.is_empty()
            || !utterance.samples.len().is_multiple_of(channels)
            || utterance.samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(files::error(
                "native transcription needs finite complete audio frames",
            ));
        }
        let mono: Vec<f32> = utterance
            .samples
            .chunks_exact(channels)
            .map(|frame| {
                (frame.iter().map(|sample| f64::from(*sample)).sum::<f64>() / channels as f64)
                    as f32
            })
            .collect();
        let bytes = encode_mono_pcm16_wav(&mono, utterance.sample_rate)?;
        if bytes.len() as u64 > files::MAX_AUDIO_BYTES {
            return Err(files::error("recognition WAV exceeds 32 MiB"));
        }
        let files = self.pool.new_request(&self.model)?;
        files::write_new(&files.directory.path.join("utterance.wav"), &bytes)?;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| files::error("transcription generation exhausted"))?;
        let request = InferenceRequest {
            identity: RequestIdentity {
                session_id: self.session,
                request_id: Uuid::new_v4(),
                generation: self.generation,
            },
            model_id: self.model.model_id.clone(),
            task: InferenceTask::Transcribe {
                audio: AudioFile {
                    path: files.relative("utterance.wav"),
                },
                language: "zh".into(),
            },
        };
        let result = self.pool.execute(&self.model, request, files).await?;
        match result.output {
            InferenceOutput::Transcript { text } => Ok(text),
            _ => Err(AppError::protocol(
                "native transcription worker returned a different output kind",
            )),
        }
    }
}
