#![forbid(unsafe_code)]

mod actor;
mod bundle;
mod files;
mod pool;
mod providers;
mod voices;

pub use pool::{NativeWorkerOptions, NativeWorkerPool, NativeWorkerState};
pub use providers::{NativeAudio, NativeEmbedder, NativeSynthesizer, NativeTranscriber};

pub const BGE_SPACE: &str = "bge-small-zh-v1.5:46fbe35:tokenizer-48cea5d4:raw-cls512-l2-max512";
