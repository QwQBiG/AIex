use ai_ex_native_worker_support::{AppError, ArtifactKind, VerifiedConfig};
use sherpa_onnx::{
    OfflineModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineWhisperModelConfig,
};

use super::{Files, path};

pub fn load(config: &VerifiedConfig<Files>) -> Result<OfflineRecognizer, AppError> {
    let Files::Whisper {
        encoder,
        decoder,
        tokens,
    } = &config.backend
    else {
        unreachable!()
    };
    OfflineRecognizer::create(&OfflineRecognizerConfig {
        model_config: OfflineModelConfig {
            whisper: OfflineWhisperModelConfig {
                encoder: Some(path(config, encoder, ArtifactKind::Model)?),
                decoder: Some(path(config, decoder, ArtifactKind::Model)?),
                language: Some("zh".into()),
                task: Some("transcribe".into()),
                ..Default::default()
            },
            tokens: Some(path(config, tokens, ArtifactKind::Tokenizer)?),
            num_threads: 4,
            provider: Some("cpu".into()),
            ..Default::default()
        },
        ..Default::default()
    })
    .ok_or_else(|| AppError::unavailable("could not load Whisper"))
}
