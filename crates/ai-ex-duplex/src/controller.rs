use ai_ex_domain::AppError;

use crate::{AudioFrame, EnergyVad, TranscriberPort, VadEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplexDirective {
    InterruptCurrentTurn,
    SubmitTranscript(String),
}

pub struct DuplexController<T> {
    vad: EnergyVad,
    transcriber: T,
}

impl<T> DuplexController<T>
where
    T: TranscriberPort,
{
    pub fn new(vad: EnergyVad, transcriber: T) -> Self {
        Self { vad, transcriber }
    }

    /// Whether VAD is collecting speech; transcription may already have completed.
    pub fn speech_in_progress(&self) -> bool {
        self.vad.speech_in_progress()
    }

    pub async fn process_frame(
        &mut self,
        frame: AudioFrame,
    ) -> Result<Option<DuplexDirective>, AppError> {
        match self.vad.process(frame)? {
            Some(VadEvent::SpeechStarted) => Ok(Some(DuplexDirective::InterruptCurrentTurn)),
            Some(VadEvent::SpeechEnded(utterance)) => self.transcribe(utterance).await,
            Some(VadEvent::SpeechContinued) | None => Ok(None),
        }
    }

    pub async fn flush(&mut self) -> Result<Option<DuplexDirective>, AppError> {
        match self.vad.flush() {
            Some(VadEvent::SpeechEnded(utterance)) => self.transcribe(utterance).await,
            Some(VadEvent::SpeechStarted | VadEvent::SpeechContinued) | None => Ok(None),
        }
    }

    async fn transcribe(
        &mut self,
        utterance: crate::Utterance,
    ) -> Result<Option<DuplexDirective>, AppError> {
        let transcript = self.transcriber.transcribe(utterance).await?;
        let transcript = transcript.trim();
        if transcript.is_empty() {
            return Ok(None);
        }
        Ok(Some(DuplexDirective::SubmitTranscript(
            transcript.to_owned(),
        )))
    }
}
