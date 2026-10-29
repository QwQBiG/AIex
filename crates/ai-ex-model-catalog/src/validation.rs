use std::collections::HashSet;

use ai_ex_domain::AppError;

use crate::{
    Artifact, ArtifactKind, CATALOG_SCHEMA_VERSION, ModelBackend, ModelCapability, ModelCatalog,
    ModelManifest, validate_relative_path,
};

impl ModelCatalog {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != CATALOG_SCHEMA_VERSION {
            return Err(AppError::configuration("unsupported model catalog schema"));
        }
        let mut ids = HashSet::new();
        for model in &self.models {
            model.validate()?;
            if !ids.insert(model.id.to_ascii_lowercase()) {
                return Err(AppError::configuration("duplicate model identifier"));
            }
        }
        Ok(())
    }
}

impl ModelManifest {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_identifier(&self.id)?;
        validate_revision(&self.revision)?;
        if self.capabilities.is_empty() || self.artifacts.is_empty() || self.licenses.is_empty() {
            return Err(AppError::configuration(
                "model requires capabilities, artifacts and license provenance",
            ));
        }
        let mut capabilities = HashSet::new();
        for capability in &self.capabilities {
            if !self.backend.supports(*capability) || !capabilities.insert(*capability) {
                return Err(AppError::configuration("invalid model capabilities"));
            }
        }
        let mut paths = HashSet::new();
        for artifact in &self.artifacts {
            artifact.validate()?;
            if !paths.insert(artifact.path.to_uppercase()) {
                return Err(AppError::configuration("duplicate artifact path"));
            }
        }
        if !self.artifacts.iter().any(|a| a.kind == ArtifactKind::Model) {
            return Err(AppError::configuration("model requires a model artifact"));
        }
        if !self
            .artifacts
            .iter()
            .any(|a| a.kind == ArtifactKind::License)
        {
            return Err(AppError::configuration("model requires a license artifact"));
        }
        for license in &self.licenses {
            validate_identifier(&license.id)?;
            let source = license.source_url.strip_prefix("https://");
            if source.is_none_or(|value| value.is_empty() || value.starts_with('/'))
                || license
                    .source_url
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control())
                || license.source_url.contains(['\\', '@'])
            {
                return Err(AppError::configuration("license provenance requires HTTPS"));
            }
        }
        let mut runtimes = HashSet::new();
        for runtime in &self.runtimes {
            validate_identifier(&runtime.id)?;
            validate_revision(&runtime.revision)?;
            if !runtimes.insert(runtime.id.to_ascii_lowercase()) {
                return Err(AppError::configuration("duplicate runtime requirement"));
            }
        }
        Ok(())
    }
}

impl Artifact {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_relative_path(&self.path)?;
        let filename = self.path.rsplit('/').next().unwrap_or("");
        let extension = filename
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        let allowed = if self.kind == ArtifactKind::NativeRuntime {
            matches!(extension.as_str(), "exe" | "dll")
        } else {
            matches!(
                extension.as_str(),
                "" | "onnx"
                    | "onnx_data"
                    | "bin"
                    | "safetensors"
                    | "gguf"
                    | "data"
                    | "npy"
                    | "npz"
                    | "ot"
                    | "model"
                    | "json"
                    | "txt"
                    | "md"
                    | "rst"
                    | "yaml"
                    | "yml"
                    | "csv"
                    | "tsv"
                    | "bpe"
                    | "vocab"
                    | "merges"
                    | "tiktoken"
                    | "fst"
                    | "utf8"
                    | "wav"
                    | "flac"
            )
        };
        if !allowed {
            return Err(AppError::configuration("artifact type is not permitted"));
        }
        if self.bytes == 0
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(AppError::configuration("artifact requires size and SHA256"));
        }
        Ok(())
    }
}

impl ModelBackend {
    fn supports(self, capability: ModelCapability) -> bool {
        matches!(
            (self, capability),
            (
                Self::Ollama,
                ModelCapability::Dialogue | ModelCapability::Embedding | ModelCapability::Vision
            ) | (Self::SherpaWhisper, ModelCapability::Transcription)
                | (Self::SherpaKokoro, ModelCapability::SpeechPreset)
                | (Self::Qwen3TtsOnnx, ModelCapability::SpeechReference)
                | (Self::OnnxEmbedding, ModelCapability::Embedding)
        )
    }
}

fn validate_identifier(value: &str) -> Result<(), AppError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(AppError::configuration("invalid catalog identifier"));
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), AppError> {
    validate_identifier(value)?;
    if value.to_ascii_lowercase().contains("latest")
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "main" | "master" | "head" | "stable" | "nightly"
        )
    {
        return Err(AppError::configuration("catalog revision must be fixed"));
    }
    Ok(())
}
