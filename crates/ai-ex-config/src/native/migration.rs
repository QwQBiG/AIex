use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use ai_ex_domain::AppError;
use serde::{Deserialize, Serialize};

use super::{
    NATIVE_CONFIG_FILENAME, NativeConfig, NativeConfigStore, VoiceLanguage, storage::absolute,
};

#[derive(Debug, Clone)]
pub struct NativeMigrationPreview {
    pub legacy_config_path: PathBuf,
    pub destination: PathBuf,
    pub config: NativeConfig,
    pub notices: Vec<NativeMigrationNotice>,
    /// Local preview only. This is not part of the portable native configuration.
    pub reference_candidate: Option<LegacyReferenceCandidate>,
}

#[derive(Debug, Clone)]
pub struct LegacyReferenceCandidate {
    pub source: PathBuf,
    pub transcript: String,
    pub reference_language: Option<VoiceLanguage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeMigrationNotice {
    ManagedModelsRequirePreparation,
    LegacyReferenceRequiresImport,
    LegacyReferenceLanguageRequiresSelection,
    ExistingMemoryRemainsInLegacyConfiguration,
    ExistingExternalServicesRemainInLegacyConfiguration,
    MicrophoneRequiresExplicitOptIn,
}

impl NativeMigrationPreview {
    /// Writes only the new native file. Importing a reference is a separate,
    /// explicit operation that copies it to the private voice resource store.
    pub fn save_new(&self) -> Result<NativeConfigStore, AppError> {
        let store = NativeConfigStore::new(&self.destination)?;
        store.create_new(&self.config)?;
        Ok(store)
    }
}

pub fn preview_legacy_file(path: impl AsRef<Path>) -> Result<NativeMigrationPreview, AppError> {
    let path = absolute(path.as_ref())?;
    let mut text = String::new();
    File::open(&path)
        .map_err(failure)?
        .take((crate::MAX_CONFIG_BYTES + 1) as u64)
        .read_to_string(&mut text)
        .map_err(failure)?;
    let previous = crate::AppConfig::parse(&text)?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::configuration("legacy configuration has no parent"))?;
    let mut notices = vec![NativeMigrationNotice::ManagedModelsRequirePreparation];
    if previous.memory.enabled {
        notices.push(NativeMigrationNotice::ExistingMemoryRemainsInLegacyConfiguration);
    }
    if previous.tts.enabled || previous.model.backend != crate::ModelBackend::Ollama {
        notices.push(NativeMigrationNotice::ExistingExternalServicesRemainInLegacyConfiguration);
    }
    if previous.duplex.enabled {
        notices.push(NativeMigrationNotice::MicrophoneRequiresExplicitOptIn);
    }
    let reference_candidate = if !previous.tts.ref_audio_path.trim().is_empty()
        && !previous.tts.prompt_text.trim().is_empty()
    {
        notices.push(NativeMigrationNotice::LegacyReferenceRequiresImport);
        let language = match previous.tts.prompt_lang.as_str() {
            "zh" => Some(VoiceLanguage::Zh),
            "en" => Some(VoiceLanguage::En),
            _ => None,
        };
        if language.is_none() {
            notices.push(NativeMigrationNotice::LegacyReferenceLanguageRequiresSelection);
        }
        let source = PathBuf::from(&previous.tts.ref_audio_path);
        Some(LegacyReferenceCandidate {
            source: if source.is_absolute() {
                source
            } else {
                parent.join(source)
            },
            transcript: previous.tts.prompt_text.clone(),
            reference_language: language,
        })
    } else {
        None
    };
    Ok(NativeMigrationPreview {
        destination: parent.join(NATIVE_CONFIG_FILENAME),
        legacy_config_path: path,
        config: NativeConfig::default(),
        notices,
        reference_candidate,
    })
}

fn failure(error: std::io::Error) -> AppError {
    AppError::configuration(format!("cannot read legacy configuration preview: {error}"))
}
