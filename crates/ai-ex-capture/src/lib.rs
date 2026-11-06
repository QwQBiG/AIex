#![forbid(unsafe_code)]

#[cfg(feature = "native-capture")]
mod native;

#[cfg(feature = "native-capture")]
pub use native::{CaptureSettings, InputDeviceChoice, NativeAudioSource, list_input_devices};
