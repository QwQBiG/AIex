use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::{AppError, AudioFile};

pub struct JobDirectory(PathBuf);

impl JobDirectory {
    #[cfg(test)]
    pub(crate) fn at(root: PathBuf) -> Self {
        Self(root)
    }

    pub fn current() -> Result<Self, AppError> {
        let root = std::env::current_dir().map_err(io_error)?;
        if is_link(&fs::symlink_metadata(&root).map_err(io_error)?) {
            return Err(AppError::configuration("job directory cannot be a link"));
        }
        Ok(Self(root))
    }

    pub fn input(&self, audio: &AudioFile) -> Result<PathBuf, AppError> {
        let path = self.checked(audio, false)?;
        if fs::metadata(&path).map_err(io_error)?.len() > 32 * 1024 * 1024 {
            return Err(AppError::configuration(
                "reference or recognition WAV exceeds 32 MiB",
            ));
        }
        Ok(path)
    }

    fn checked(&self, audio: &AudioFile, output: bool) -> Result<PathBuf, AppError> {
        audio.validate()?;
        let mut path = self.0.clone();
        let mut parts = audio.path.split('/').peekable();
        while let Some(part) = parts.next() {
            path.push(part);
            let final_part = parts.peek().is_none();
            if output && final_part {
                if path.try_exists().map_err(io_error)? {
                    return Err(AppError::configuration("worker output already exists"));
                }
            } else {
                let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
                if is_link(&metadata)
                    || (final_part && !metadata.is_file())
                    || (!final_part && !metadata.is_dir())
                {
                    return Err(AppError::configuration(
                        "worker audio path is not a regular job file",
                    ));
                }
            }
        }
        Ok(path)
    }

    pub fn write_audio(
        &self,
        audio: &AudioFile,
        samples: &[f32],
        rate: u32,
    ) -> Result<(), AppError> {
        let path = self.checked(audio, true)?;
        let bytes = ai_ex_inference::encode_mono_pcm16_wav(samples, rate)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(io_error)?;
        file.write_all(&bytes)
            .and_then(|()| file.flush())
            .map_err(io_error)
    }

    pub fn read_audio(&self, audio: &AudioFile) -> Result<(Vec<f32>, u32), AppError> {
        let file = File::open(self.input(audio)?).map_err(io_error)?;
        let mut reader = hound::WavReader::new(file)
            .map_err(|error| AppError::configuration(error.to_string()))?;
        let spec = reader.spec();
        if spec.channels != 1
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
            || spec.sample_rate == 0
        {
            return Err(AppError::configuration(
                "input audio must be mono PCM16 WAV",
            ));
        }
        let samples: Result<Vec<f32>, _> = reader
            .samples::<i16>()
            .map(|sample| sample.map(|value| f32::from(value) / 32768.0))
            .collect();
        let samples = samples.map_err(|error| AppError::configuration(error.to_string()))?;
        if samples.is_empty() {
            return Err(AppError::configuration("input audio is empty"));
        }
        Ok((samples, spec.sample_rate))
    }
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn io_error(error: std::io::Error) -> AppError {
    AppError::configuration(format!("worker job file failed: {error}"))
}
