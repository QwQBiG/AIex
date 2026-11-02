use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use ai_ex_inference::InferenceRequest;
use serde::Deserialize;
use serde::de::DeserializeOwned;

const MAX_JSON_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeSpec {
    pub schema_version: u16,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub job_directory: PathBuf,
    pub ready_timeout_ms: u64,
    pub models: Vec<ModelInput>,
    pub steps: Vec<ProbeStep>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInput {
    pub manifest: PathBuf,
    pub directory: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeStep {
    pub request: InferenceRequest,
    pub timeout_ms: u64,
    #[serde(default)]
    pub cancel_after_ms: Option<u64>,
}

impl ProbeSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported native probe schema".into());
        }
        if !self.executable.is_absolute() || !self.job_directory.is_absolute() {
            return Err("worker executable and job directory must be absolute paths".into());
        }
        if !valid_timeout(self.ready_timeout_ms)
            || self.models.is_empty()
            || self.models.len() > 8
            || self.steps.is_empty()
            || self.steps.len() > 16
        {
            return Err("invalid probe timeout, model count or step count".into());
        }
        for model in &self.models {
            if !model.manifest.is_absolute() || !model.directory.is_absolute() {
                return Err("model manifest and directory must be absolute paths".into());
            }
        }
        let mut identities = HashSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            step.request.validate().map_err(|error| error.to_string())?;
            if !identities.insert(step.request.identity.request_id) {
                return Err("probe request IDs must be unique".into());
            }
            if !valid_timeout(step.timeout_ms) {
                return Err("request timeout must be between 1 and 600000 ms".into());
            }
            if let Some(delay) = step.cancel_after_ms
                && (delay == 0 || delay >= step.timeout_ms || index + 1 != self.steps.len())
            {
                return Err(
                    "cancellation must be the final step with a delay below its timeout".into(),
                );
            }
        }
        Ok(())
    }
}

fn valid_timeout(milliseconds: u64) -> bool {
    (1..=600_000).contains(&milliseconds)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?
        .take(MAX_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return Err("verification JSON exceeds 1 MiB".into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid UTF-8 JSON at {}: {error}", path.display()))
}

#[cfg(test)]
mod tests;
