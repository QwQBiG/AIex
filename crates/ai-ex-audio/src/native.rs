use std::io::Cursor;
use std::time::Duration;

use ai_ex_domain::SpeechPlaybackSnapshot;
use rodio::Source;

use super::{AppError, Arc, AudioPlayer, Ordering, SpeechJob};
use crate::envelope::SpeechEnvelope;

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

const MAX_DECODED_SAMPLES: usize = 48_000 * 2 * 120;

struct PlaybackGuard<'a, F: Fn(SpeechPlaybackSnapshot)>(&'a F, SpeechPlaybackSnapshot);

impl<F: Fn(SpeechPlaybackSnapshot)> Drop for PlaybackGuard<'_, F> {
    fn drop(&mut self) {
        (self.0)(self.1.clone());
    }
}

impl AudioPlayer {
    pub async fn play_wav_observed<F>(
        &self,
        job: &SpeechJob,
        bytes: Vec<u8>,
        observer: F,
    ) -> Result<(), AppError>
    where
        F: Fn(SpeechPlaybackSnapshot) + Send + 'static,
    {
        if self.cancelled(job) {
            return Ok(());
        }
        let generation = Arc::clone(&self.generation);
        let expected = job.generation;
        let job = job.clone();
        tokio::task::spawn_blocking(move || {
            let decoder = rodio::Decoder::try_from(Cursor::new(bytes))
                .map_err(|error| AppError::protocol(error.to_string()))?;
            let channels = decoder.channels();
            let sample_rate = decoder.sample_rate();
            let samples: Vec<f32> = decoder.take(MAX_DECODED_SAMPLES + 1).collect();
            if samples.len() > MAX_DECODED_SAMPLES {
                return Err(AppError::protocol(
                    "decoded speech exceeds the PCM sample budget",
                ));
            }
            let envelope = SpeechEnvelope::from_samples(&samples, channels, sample_rate)?;
            if generation.load(Ordering::Acquire) != expected {
                return Ok(());
            }
            let stream = rodio::OutputStreamBuilder::open_default_stream()
                .map_err(|error| AppError::unavailable(error.to_string()))?;
            let sink = rodio::Sink::connect_new(stream.mixer());
            sink.pause();
            sink.append(rodio::buffer::SamplesBuffer::new(
                channels,
                sample_rate,
                samples,
            ));
            if generation.load(Ordering::Acquire) != expected {
                sink.stop();
                return Ok(());
            }
            let _guard = PlaybackGuard(&observer, job.stopped_snapshot());
            let report = |position_ms| {
                observer(job.playback_snapshot(
                    position_ms,
                    envelope.duration_ms,
                    envelope.level_at(position_ms),
                ))
            };
            sink.play();
            report(0);
            let mut last_report_ms = 0;
            while !sink.empty() {
                if generation.load(Ordering::Acquire) != expected {
                    sink.stop();
                    break;
                }
                let position_ms = sink.get_pos().as_millis() as u64;
                if position_ms.saturating_sub(last_report_ms) >= 40 {
                    report(position_ms);
                    last_report_ms = position_ms;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        })
        .await
        .map_err(|error| AppError::unavailable(error.to_string()))?
    }
}
