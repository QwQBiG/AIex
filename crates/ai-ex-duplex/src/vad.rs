use ai_ex_domain::AppError;

use crate::{AudioFrame, Utterance, VadConfig, VadEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Silent,
    Speaking,
}

pub struct EnergyVad {
    config: VadConfig,
    state: State,
    hot_frames: usize,
    silence_frames: usize,
    speech_frames: usize,
    candidate: Vec<f32>,
    utterance: Vec<f32>,
    sample_rate: u32,
    channels: u16,
}

impl EnergyVad {
    pub fn new(config: VadConfig) -> Result<Self, AppError> {
        config.validate()?;
        Ok(Self {
            config,
            state: State::Silent,
            hot_frames: 0,
            silence_frames: 0,
            speech_frames: 0,
            candidate: Vec::new(),
            utterance: Vec::new(),
            sample_rate: 0,
            channels: 0,
        })
    }

    /// Whether speech has started and has not yet ended or been flushed.
    pub fn speech_in_progress(&self) -> bool {
        self.state == State::Speaking
    }

    pub fn process(&mut self, frame: AudioFrame) -> Result<Option<VadEvent>, AppError> {
        if self.sample_rate != 0
            && (frame.sample_rate != self.sample_rate || frame.channels != self.channels)
        {
            return Err(AppError::protocol(
                "audio format changed inside a VAD utterance",
            ));
        }
        let rms = frame.rms();
        match self.state {
            State::Silent => self.process_silence(frame, rms),
            State::Speaking => Ok(Some(self.process_speech(frame, rms))),
        }
    }

    pub fn flush(&mut self) -> Option<VadEvent> {
        if self.state == State::Speaking && !self.utterance.is_empty() {
            return Some(VadEvent::SpeechEnded(self.take_utterance()));
        }
        self.reset();
        None
    }

    fn process_silence(
        &mut self,
        frame: AudioFrame,
        rms: f32,
    ) -> Result<Option<VadEvent>, AppError> {
        if rms < self.config.start_threshold {
            self.hot_frames = 0;
            self.candidate.clear();
            self.sample_rate = 0;
            self.channels = 0;
            return Ok(None);
        }
        if self.hot_frames == 0 {
            self.sample_rate = frame.sample_rate;
            self.channels = frame.channels;
        }
        self.hot_frames += 1;
        self.candidate.extend(frame.samples);
        if self.hot_frames < self.config.start_frames {
            return Ok(None);
        }
        self.state = State::Speaking;
        self.speech_frames = self.hot_frames;
        self.utterance.append(&mut self.candidate);
        Ok(Some(VadEvent::SpeechStarted))
    }

    fn process_speech(&mut self, frame: AudioFrame, rms: f32) -> VadEvent {
        self.utterance.extend(frame.samples);
        self.speech_frames += 1;
        if rms < self.config.continue_threshold {
            self.silence_frames += 1;
        } else {
            self.silence_frames = 0;
        }
        if self.silence_frames >= self.config.end_silence_frames
            || self.speech_frames >= self.config.max_utterance_frames
        {
            VadEvent::SpeechEnded(self.take_utterance())
        } else {
            VadEvent::SpeechContinued
        }
    }

    fn take_utterance(&mut self) -> Utterance {
        let utterance = Utterance {
            samples: std::mem::take(&mut self.utterance),
            sample_rate: self.sample_rate,
            channels: self.channels,
        };
        self.reset();
        utterance
    }

    fn reset(&mut self) {
        self.state = State::Silent;
        self.hot_frames = 0;
        self.silence_frames = 0;
        self.speech_frames = 0;
        self.candidate.clear();
        self.utterance.clear();
        self.sample_rate = 0;
        self.channels = 0;
    }
}
