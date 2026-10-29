#![forbid(unsafe_code)]

mod audio;
mod framing;
mod protocol;
mod supervisor;

pub use audio::encode_mono_pcm16_wav;
pub use framing::{MAX_FRAME_BYTES, read_frame, write_frame};
pub use protocol::{
    AudioFile, InferenceOutput, InferenceRequest, InferenceTask, RequestIdentity, VoiceSelection,
    WORKER_PROTOCOL_VERSION, WorkerCommand, WorkerReply,
};
pub use supervisor::{CancellationOutcome, InferenceWorker, WorkerLaunch};
