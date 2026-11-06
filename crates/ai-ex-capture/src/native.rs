use ai_ex_domain::AppError;
use ai_ex_duplex::{AudioFrame, AudioSourcePort};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use tokio::sync::mpsc;

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

const MAX_INPUT_DEVICES: usize = ai_ex_domain::NATIVE_MAX_INPUT_DEVICES;
const MAX_DEVICE_NAME_BYTES_TOTAL: usize = ai_ex_domain::NATIVE_MAX_INPUT_NAME_BYTES;

#[derive(Debug, Clone)]
pub struct CaptureSettings {
    pub device_name: Option<String>,
    pub queue_capacity: usize,
}

/// A named device can be selected only when its name identifies one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDeviceChoice {
    pub name: String,
    pub ambiguous: bool,
}

pub struct NativeAudioSource {
    _stream: Stream,
    receiver: mpsc::Receiver<Result<AudioFrame, AppError>>,
}

impl NativeAudioSource {
    pub fn open(settings: CaptureSettings) -> Result<Self, AppError> {
        Self::open_with_policy(settings, false)
    }

    /// Open an explicitly selected device without guessing between equal names.
    pub fn open_explicit(settings: CaptureSettings) -> Result<Self, AppError> {
        Self::open_with_policy(settings, true)
    }

    fn open_with_policy(settings: CaptureSettings, explicit: bool) -> Result<Self, AppError> {
        if settings.queue_capacity == 0 {
            return Err(AppError::configuration(
                "capture queue capacity must be positive",
            ));
        }
        let host = cpal::default_host();
        let device = if explicit {
            select_device_explicit(&host, settings.device_name.as_deref())?
        } else {
            select_device(&host, settings.device_name.as_deref())?
        };
        let supported = device
            .default_input_config()
            .map_err(|error| AppError::unavailable(format!("input format unavailable: {error}")))?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let sample_rate = config.sample_rate.0;
        let channels = config.channels;
        let (sender, receiver) = mpsc::channel(settings.queue_capacity);
        let stream = match sample_format {
            SampleFormat::F32 => build_stream(
                &device,
                &config,
                sender,
                sample_rate,
                channels,
                |sample: f32| sample,
            ),
            SampleFormat::I16 => build_stream(
                &device,
                &config,
                sender,
                sample_rate,
                channels,
                |sample: i16| f32::from(sample) / 32_768.0,
            ),
            SampleFormat::U16 => build_stream(
                &device,
                &config,
                sender,
                sample_rate,
                channels,
                |sample: u16| (f32::from(sample) - 32_768.0) / 32_768.0,
            ),
            other => Err(AppError::unavailable(format!(
                "unsupported input sample format: {other:?}",
            ))),
        }?;
        stream
            .play()
            .map_err(|error| AppError::unavailable(format!("cannot start capture: {error}")))?;
        Ok(Self {
            _stream: stream,
            receiver,
        })
    }
}

#[async_trait]
impl AudioSourcePort for NativeAudioSource {
    async fn next_frame(&mut self) -> Result<Option<AudioFrame>, AppError> {
        match self.receiver.recv().await {
            Some(Ok(frame)) => Ok(Some(frame)),
            Some(Err(error)) => Err(error),
            None => Ok(None),
        }
    }
}

/// Enumerate only when explicitly requested; this does not open an audio stream.
pub fn list_input_devices() -> Result<Vec<InputDeviceChoice>, AppError> {
    let devices = readable_devices(&cpal::default_host())?;
    Ok(catalog(devices.iter().map(|(name, _)| name.as_str())))
}

fn readable_devices(host: &cpal::Host) -> Result<Vec<(String, Device)>, AppError> {
    let devices = host
        .input_devices()
        .map_err(|_| AppError::unavailable("cannot list input devices"))?;
    readable_items(devices.map(|device| {
        let name = device
            .name()
            .map_err(|_| AppError::unavailable("cannot read input device name"));
        (name, device)
    }))
}

fn readable_items<T>(
    devices: impl IntoIterator<Item = (Result<String, AppError>, T)>,
) -> Result<Vec<(String, T)>, AppError> {
    let mut result = Vec::new();
    let mut total_name_bytes = 0;
    for (name, device) in devices {
        if result.len() == MAX_INPUT_DEVICES {
            return Err(AppError::unavailable("too many input devices"));
        }
        let name = name?;
        if !valid_device_name(&name) {
            return Err(AppError::unavailable("invalid input device name"));
        }
        total_name_bytes += name.len();
        if total_name_bytes > MAX_DEVICE_NAME_BYTES_TOTAL {
            return Err(AppError::unavailable("input device names exceed limit"));
        }
        result.push((name, device));
    }
    Ok(result)
}

fn valid_device_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= 256
        && name.len() <= 1024
        && !name.chars().any(char::is_control)
}

fn catalog<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<InputDeviceChoice> {
    let mut choices: Vec<InputDeviceChoice> = Vec::new();
    for name in names {
        if let Some(choice) = choices
            .iter_mut()
            .find(|choice| choice.name.eq_ignore_ascii_case(name))
        {
            choice.ambiguous = true;
        } else {
            choices.push(InputDeviceChoice {
                name: name.to_owned(),
                ambiguous: false,
            });
        }
    }
    choices
}

fn explicit_name_index(names: &[String], requested: &str) -> Result<usize, AppError> {
    if !valid_device_name(requested) {
        return Err(AppError::configuration("invalid input device name"));
    }
    let mut matches = names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.eq_ignore_ascii_case(requested));
    let index = matches
        .next()
        .map(|(index, _)| index)
        .ok_or_else(|| AppError::unavailable("selected input device unavailable"))?;
    if matches.next().is_some() {
        return Err(AppError::configuration(
            "selected input device name is ambiguous",
        ));
    }
    Ok(index)
}

fn select_device_explicit(host: &cpal::Host, requested: Option<&str>) -> Result<Device, AppError> {
    let Some(requested) = requested else {
        return host
            .default_input_device()
            .ok_or_else(|| AppError::unavailable("no default input device"));
    };
    let mut devices = readable_devices(host)?;
    let names: Vec<_> = devices.iter().map(|(name, _)| name.clone()).collect();
    let index = explicit_name_index(&names, requested)?;
    Ok(devices.swap_remove(index).1)
}

fn select_device(host: &cpal::Host, requested: Option<&str>) -> Result<Device, AppError> {
    if let Some(requested) = requested.filter(|name| !name.trim().is_empty()) {
        let devices = host.input_devices().map_err(|error| {
            AppError::unavailable(format!("cannot list input devices: {error}"))
        })?;
        for device in devices {
            let name = device.name().unwrap_or_default();
            if name.eq_ignore_ascii_case(requested) {
                return Ok(device);
            }
        }
        return Err(AppError::configuration(format!(
            "input device not found: {requested}",
        )));
    }
    host.default_input_device()
        .ok_or_else(|| AppError::unavailable("no default input device"))
}

fn build_stream<T, F>(
    device: &Device,
    config: &StreamConfig,
    sender: mpsc::Sender<Result<AudioFrame, AppError>>,
    sample_rate: u32,
    channels: u16,
    convert: F,
) -> Result<Stream, AppError>
where
    T: cpal::SizedSample + Copy,
    F: Fn(T) -> f32 + Send + Copy + 'static,
{
    let error_sender = sender.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _info| {
                let samples = data.iter().copied().map(convert).collect();
                let frame = AudioFrame::new(samples, sample_rate, channels);
                let _ignored = sender.try_send(frame);
            },
            move |error| {
                let error = AppError::unavailable(format!("audio capture failed: {error}"));
                let _ignored = error_sender.try_send(Err(error));
            },
            None,
        )
        .map_err(|error| AppError::unavailable(format!("cannot build input stream: {error}")))
}
