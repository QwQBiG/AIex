use std::fs::{self, OpenOptions};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use ai_ex_config::native::validate_resource_id;
use ai_ex_domain::AppError;
use ai_ex_inference::encode_mono_pcm16_wav;
use uuid::Uuid;

use crate::{NativeWorkerPool, files};

impl NativeWorkerPool {
    /// Copies a user-selected WAV into the private store; no device is opened.
    pub async fn import_reference_wav(&self, source: &Path) -> Result<String, AppError> {
        let source = source.to_owned();
        let root = self.voices_root().to_owned();
        tokio::task::spawn_blocking(move || import(&root, &source))
            .await
            .map_err(|error| {
                AppError::unavailable(format!("reference import task failed: {error}"))
            })?
    }
}

pub(crate) fn resource_path(root: &Path, id: &str) -> Result<PathBuf, AppError> {
    validate_resource_id(id)?;
    Ok(root.join(format!("{id}.wav")))
}

fn import(root: &Path, source: &Path) -> Result<String, AppError> {
    if !source.is_absolute() {
        return Err(files::error(
            "reference import requires an absolute source path",
        ));
    }
    let bytes = normalize(&files::read_audio(source)?)?;
    let id = format!("audio-{}", Uuid::new_v4());
    let destination = resource_path(root, &id)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(files::error)?;
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _cleanup = fs::remove_file(destination);
        return Err(files::error(error));
    }
    Ok(id)
}

fn normalize(bytes: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut wave = hound::WavReader::new(Cursor::new(bytes)).map_err(files::error)?;
    let spec = wave.spec();
    if !(1..=8).contains(&spec.channels)
        || !(8000..=192000).contains(&spec.sample_rate)
        || u64::from(wave.duration()) > u64::from(spec.sample_rate) * 30
    {
        return Err(files::error(
            "reference WAV needs 1..=8 channels, 8..=192 kHz and at most 30 seconds",
        ));
    }
    let samples: Box<dyn Iterator<Item = Result<f32, hound::Error>> + '_> =
        match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Float, 32) => Box::new(wave.samples::<f32>()),
            (hound::SampleFormat::Int, bits @ (8 | 16 | 24 | 32)) => {
                let scale = (1_u64 << (bits - 1)) as f32;
                Box::new(
                    wave.samples::<i32>()
                        .map(move |sample| sample.map(|value| value as f32 / scale)),
                )
            }
            _ => return Err(files::error("reference WAV encoding is unsupported")),
        };
    let mut mono = Vec::new();
    let mut sum = 0.0_f64;
    let mut count = 0_u64;
    for sample in samples {
        let sample = sample.map_err(files::error)?;
        if !sample.is_finite() {
            return Err(files::error("reference WAV contains nonfinite samples"));
        }
        count += 1;
        if count > files::MAX_AUDIO_BYTES {
            return Err(files::error("reference WAV sample count is too large"));
        }
        sum += f64::from(sample);
        if count.is_multiple_of(u64::from(spec.channels)) {
            mono.push((sum / f64::from(spec.channels)) as f32);
            sum = 0.0;
        }
    }
    if !count.is_multiple_of(u64::from(spec.channels)) {
        return Err(files::error("reference WAV has an incomplete frame"));
    }
    encode_mono_pcm16_wav(&mono, spec.sample_rate)
}

pub(crate) fn inspect_pcm16(bytes: &[u8], rate: Option<u32>) -> Result<u32, AppError> {
    if bytes.len() < 46
        || &bytes[..4] != b"RIFF"
        || &bytes[8..16] != b"WAVEfmt "
        || &bytes[36..40] != b"data"
    {
        return Err(AppError::protocol("native audio is not canonical PCM WAV"));
    }
    let u16_at = |offset| u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
    let u32_at = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let sample_rate = u32_at(24);
    if u32_at(16) != 16
        || u16_at(20) != 1
        || u16_at(22) != 1
        || u16_at(32) != 2
        || u16_at(34) != 16
        || sample_rate == 0
        || rate.is_some_and(|expected| expected != sample_rate)
        || sample_rate.checked_mul(2) != Some(u32_at(28))
        || u32_at(4) as usize + 8 != bytes.len()
        || u32_at(40) as usize + 44 != bytes.len()
        || !u32_at(40).is_multiple_of(2)
    {
        return Err(AppError::protocol(
            "native audio violates mono PCM16 format or size contract",
        ));
    }
    Ok(sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_float_stereo_is_normalized_to_private_mono_pcm16() {
        let mut cursor = Cursor::new(Vec::new());
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 24000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
        for sample in [0.5_f32, 0.5, -0.5, -0.5] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        let source = cursor.into_inner();
        assert!(inspect_pcm16(&source, None).is_err());
        let pcm = normalize(&source).unwrap();
        assert_eq!(inspect_pcm16(&pcm, Some(24000)).unwrap(), 24000);
        let mut decoded = hound::WavReader::new(Cursor::new(pcm)).unwrap();
        assert_eq!(decoded.spec().channels, 1);
        let samples: Vec<_> = decoded.samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(samples, [16384, -16384]);
    }

    #[test]
    fn voice_import_rejects_overlong_reference_and_wrong_native_rate() {
        let pcm = encode_mono_pcm16_wav(&vec![0.2; 8000 * 31], 8000).unwrap();
        assert!(normalize(&pcm).is_err());
        let pcm = encode_mono_pcm16_wav(&[0.2, 0.3], 24000).unwrap();
        assert!(inspect_pcm16(&pcm, Some(16000)).is_err());
        assert!(resource_path(Path::new("unused"), "../private").is_err());
    }
}
