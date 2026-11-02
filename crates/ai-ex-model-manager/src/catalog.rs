use std::collections::HashSet;

use ai_ex_domain::AppError;
use ai_ex_model_catalog::{
    Artifact, ArtifactKind, License, ModelBackend, ModelManifest, validate_relative_path,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionCatalog {
    pub schema_version: u16,
    pub models: Vec<ModelPackage>,
    pub runtimes: Vec<RuntimePackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPackage {
    pub name: String,
    pub manifest: ModelManifest,
    pub source: PackageSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePackage {
    pub name: String,
    pub manifest: RuntimeManifest,
    pub source: PackageSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeManifest {
    pub id: String,
    pub revision: String,
    pub protocol_version: u16,
    pub backends: Vec<ModelBackend>,
    pub artifacts: Vec<Artifact>,
    pub licenses: Vec<License>,
    pub workers: Vec<RuntimeWorker>,
    pub ollama_executable: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeWorker {
    pub backend: ModelBackend,
    pub executable: String,
    #[serde(default)]
    pub runtime_library: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageSource {
    Files {
        files: Vec<ArtifactSource>,
    },
    Archive {
        url: String,
        bytes: u64,
        sha256: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSource {
    pub path: String,
    pub location: DownloadLocation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DownloadLocation {
    Https { url: String },
    Inline { content: String },
}

impl DistributionCatalog {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != 1 || self.models.len() + self.runtimes.len() > 64 {
            return Err(invalid("unsupported catalog schema or package count"));
        }
        let mut ids = HashSet::new();
        for package in &self.runtimes {
            package.manifest.validate()?;
            validate_name(&package.name)?;
            validate_source(&package.source, &package.manifest.artifacts)?;
            if !ids.insert(package.manifest.id.to_ascii_lowercase()) {
                return Err(invalid("duplicate package identifier"));
            }
        }
        for package in &self.models {
            package.manifest.validate()?;
            validate_identifier(&package.manifest.id)?;
            validate_identifier(&package.manifest.revision)?;
            validate_name(&package.name)?;
            validate_source(&package.source, &package.manifest.artifacts)?;
            if !ids.insert(package.manifest.id.to_ascii_lowercase()) {
                return Err(invalid("duplicate package identifier"));
            }
            for required in &package.manifest.runtimes {
                if !self.runtimes.iter().any(|runtime| {
                    runtime.manifest.id == required.id
                        && runtime.manifest.revision == required.revision
                        && runtime
                            .manifest
                            .backends
                            .contains(&package.manifest.backend)
                }) {
                    return Err(invalid(
                        "model requires an undeclared or incompatible runtime",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn invalid(message: &str) -> AppError {
    AppError::configuration(message)
}

fn validate_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(invalid("invalid package display name"));
    }
    Ok(())
}

impl RuntimeManifest {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_identifier(&self.id)?;
        validate_identifier(&self.revision)?;
        if self.protocol_version != 1
            || self.backends.is_empty()
            || self.licenses.is_empty()
            || self.artifacts.is_empty()
            || self.artifacts.len() > 10_000
        {
            return Err(invalid("invalid native runtime manifest"));
        }
        let mut backends = HashSet::new();
        let mut paths = HashSet::new();
        for backend in &self.backends {
            if !backends.insert(*backend) {
                return Err(invalid("duplicate runtime backend"));
            }
        }
        for artifact in &self.artifacts {
            artifact.validate()?;
            if !paths.insert(artifact.path.to_uppercase()) {
                return Err(invalid("duplicate runtime artifact"));
            }
        }
        if !self
            .artifacts
            .iter()
            .any(|item| item.kind == ArtifactKind::License)
        {
            return Err(invalid("runtime license artifact is required"));
        }
        for license in &self.licenses {
            validate_identifier(&license.id)?;
            validate_url(&license.source_url)?;
        }
        let executable = |path: &str| -> Result<(), AppError> {
            if !path.to_ascii_lowercase().ends_with(".exe")
                || !self
                    .artifacts
                    .iter()
                    .any(|item| item.kind == ArtifactKind::NativeRuntime && item.path == path)
            {
                return Err(invalid(
                    "runtime executable must be a declared EXE artifact",
                ));
            }
            Ok(())
        };
        let mut covered = HashSet::new();
        for worker in &self.workers {
            if worker.backend == ModelBackend::Ollama
                || !backends.contains(&worker.backend)
                || !covered.insert(worker.backend)
            {
                return Err(invalid("invalid runtime worker mapping"));
            }
            executable(&worker.executable)?;
            match (worker.backend, &worker.runtime_library) {
                (ModelBackend::OnnxEmbedding | ModelBackend::Qwen3TtsOnnx, Some(library)) => {
                    if !library.to_ascii_lowercase().ends_with(".dll")
                        || dll_directory(library) != dll_directory(&worker.executable)
                        || !self.artifacts.iter().any(|file| {
                            file.path == *library && file.kind == ArtifactKind::NativeRuntime
                        })
                    {
                        return Err(invalid(
                            "worker runtime DLL must be declared in its own native directory",
                        ));
                    }
                }
                (ModelBackend::SherpaWhisper | ModelBackend::SherpaKokoro, None) => {}
                _ => {
                    return Err(invalid(
                        "worker runtime DLL selection does not match its backend",
                    ));
                }
            }
        }
        if let Some(path) = &self.ollama_executable {
            if !backends.contains(&ModelBackend::Ollama) {
                return Err(invalid("undeclared Ollama runtime"));
            }
            executable(path)?;
            covered.insert(ModelBackend::Ollama);
        }
        if covered != backends {
            return Err(invalid("runtime lacks a backend executable"));
        }
        for (index, worker) in self.workers.iter().enumerate() {
            for other in &self.workers[..index] {
                let sherpa = |backend| {
                    matches!(
                        backend,
                        ModelBackend::SherpaWhisper | ModelBackend::SherpaKokoro
                    )
                };
                if dll_directory(&worker.executable) == dll_directory(&other.executable)
                    && !(sherpa(worker.backend) && sherpa(other.backend))
                {
                    return Err(invalid("native backends require separate DLL directories"));
                }
            }
        }
        if let Some(ollama) = &self.ollama_executable
            && self
                .workers
                .iter()
                .any(|worker| dll_directory(&worker.executable) == dll_directory(ollama))
        {
            return Err(invalid("Ollama requires a separate native directory"));
        }
        Ok(())
    }
}

fn dll_directory(executable: &str) -> String {
    executable
        .rsplit_once('/')
        .map(|value| value.0)
        .unwrap_or_default()
        .to_uppercase()
}

fn validate_identifier(value: &str) -> Result<(), AppError> {
    validate_relative_path(value)?;
    let lower = value.to_ascii_lowercase();
    if value.len() > 256
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
        || lower.contains("latest")
        || matches!(
            lower.as_str(),
            "main" | "master" | "head" | "stable" | "nightly"
        )
    {
        return Err(invalid(
            "package identifiers and revisions must be fixed portable names",
        ));
    }
    Ok(())
}

pub(crate) fn validate_url(value: &str) -> Result<(), AppError> {
    let url = reqwest::Url::parse(value).map_err(|_| invalid("invalid download URL"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path().split('/').any(|segment| {
            matches!(
                segment.to_ascii_lowercase().as_str(),
                "latest" | "main" | "master" | "head"
            )
        })
    {
        return Err(invalid(
            "downloads require fixed HTTPS URLs without credentials",
        ));
    }
    if url.host_str() == Some("huggingface.co") && url.path().contains("/resolve/") {
        let revision = url
            .path()
            .split("/resolve/")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or_default();
        if revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid(
                "Hugging Face downloads require a full immutable revision",
            ));
        }
    }
    Ok(())
}

fn validate_source(source: &PackageSource, artifacts: &[Artifact]) -> Result<(), AppError> {
    if artifacts.len() > 10_000 {
        return Err(invalid("too many package artifacts"));
    }
    if artifacts
        .iter()
        .try_fold(0_u64, |total, file| total.checked_add(file.bytes))
        .is_none_or(|total| total > 512 * 1024 * 1024 * 1024)
    {
        return Err(invalid("package exceeds supported total size"));
    }
    match source {
        PackageSource::Archive { url, bytes, sha256 } => {
            validate_url(url)?;
            if *bytes == 0 || sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("archive requires a size and SHA256"));
            }
        }
        PackageSource::Files { files } => {
            let mut seen = HashSet::new();
            for file in files {
                let artifact = artifacts
                    .iter()
                    .find(|item| item.path == file.path)
                    .ok_or_else(|| invalid("download references an undeclared artifact"))?;
                if !seen.insert(file.path.to_uppercase()) {
                    return Err(invalid("duplicate download artifact"));
                }
                match &file.location {
                    DownloadLocation::Https { url } => validate_url(url)?,
                    DownloadLocation::Inline { content } => {
                        if content.len() > 1024 * 1024
                            || content.len() as u64 != artifact.bytes
                            || crate::files::hash_bytes(content.as_bytes())
                                != artifact.sha256.to_ascii_lowercase()
                        {
                            return Err(invalid("inline artifact does not match its manifest"));
                        }
                    }
                }
            }
            if files.len() != artifacts.len() {
                return Err(invalid("every artifact requires one source"));
            }
        }
    }
    Ok(())
}
