use ai_ex_native_worker_support::{AppError, ArtifactKind, VerifiedConfig};
use sherpa_onnx::{
    OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsModelConfig,
};

use super::{Files, path};

pub fn load(config: &VerifiedConfig<Files>) -> Result<OfflineTts, AppError> {
    let Files::Kokoro {
        model,
        voices,
        tokens,
        espeak_data,
        dict,
        lexicon_en,
        lexicon_zh,
        date_fst,
        number_fst,
        phone_fst,
    } = &config.backend
    else {
        unreachable!()
    };
    let directory = |relative: &str| -> Result<String, AppError> {
        config
            .artifact_directory(relative)?
            .into_os_string()
            .into_string()
            .map_err(|_| AppError::configuration("Sherpa data path is not UTF-8"))
    };
    OfflineTts::create(&OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            kokoro: OfflineTtsKokoroModelConfig {
                model: Some(path(config, model, ArtifactKind::Model)?),
                voices: Some(path(config, voices, ArtifactKind::Model)?),
                tokens: Some(path(config, tokens, ArtifactKind::Tokenizer)?),
                data_dir: Some(directory(espeak_data)?),
                dict_dir: Some(directory(dict)?),
                lexicon: Some(format!(
                    "{},{}",
                    path(config, lexicon_en, ArtifactKind::Configuration)?,
                    path(config, lexicon_zh, ArtifactKind::Configuration)?
                )),
                ..Default::default()
            },
            num_threads: 4,
            provider: Some("cpu".into()),
            ..Default::default()
        },
        rule_fsts: Some(format!(
            "{},{},{}",
            path(config, date_fst, ArtifactKind::Configuration)?,
            path(config, number_fst, ArtifactKind::Configuration)?,
            path(config, phone_fst, ArtifactKind::Configuration)?
        )),
        ..Default::default()
    })
    .ok_or_else(|| AppError::unavailable("could not load Kokoro"))
}
