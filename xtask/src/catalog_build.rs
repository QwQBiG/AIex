mod models;

use std::path::Path;

use ai_ex_model_catalog::{Artifact, ArtifactKind, License};
use ai_ex_model_manager::{DistributionCatalog, PackageSource, RuntimeManifest, RuntimePackage};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::bundle_io::{Result, hash_file, read_json, verify_file, write_json};

pub const RUNTIME_ID: &str = "native-cpu";
pub const REVISION: &str = "0.9.0-alpha.1";
pub const RUNTIME_FILE: &str = "AIex-native-cpu-0.9.0-alpha.1-windows-x64.zip";

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct FileRecord {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

impl FileRecord {
    pub fn artifact(&self, path: String, kind: ArtifactKind) -> Artifact {
        Artifact {
            path,
            kind,
            bytes: self.bytes,
            sha256: self.sha256.to_ascii_lowercase(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Material {
    #[serde(flatten)]
    pub file: FileRecord,
    pub id: String,
    pub source_url: String,
}

#[derive(Deserialize)]
pub struct Materials {
    pub materials: Vec<Material>,
}

impl Material {
    pub fn text(&self, directory: &Path) -> Result<String> {
        ai_ex_model_catalog::validate_relative_path(&self.file.path).map_err(|e| e.to_string())?;
        let path = directory.join(&self.file.path);
        verify_file(&path, self.file.bytes, &self.file.sha256)?;
        String::from_utf8(std::fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    }

    pub fn license(&self) -> License {
        License {
            id: self.id.clone(),
            source_url: self.source_url.clone(),
        }
    }
}

pub fn build(
    directory: &Path,
    manifest: &Path,
    archive: &Path,
    output: &Path,
) -> Result<(Value, bool)> {
    let runtime: RuntimeManifest = read_json(manifest)?;
    runtime.validate().map_err(|error| error.to_string())?;
    if runtime.id != RUNTIME_ID || runtime.revision != REVISION {
        return Err("runtime does not match the fixed CPU release".into());
    }
    crate::bundle_zip::verify_archive(archive, &runtime.artifacts)?;
    let (bytes, sha256) = hash_file(archive)?;
    let catalog = DistributionCatalog {
        schema_version: 1,
        models: models::build(directory)?,
        runtimes: vec![RuntimePackage {
            name: "Windows CPU 原生运行组件".into(),
            manifest: runtime,
            source: PackageSource::Archive {
                url: format!(
                    "https://github.com/QwQBiG/AIex/releases/download/v{REVISION}/{RUNTIME_FILE}"
                ),
                bytes,
                sha256,
            },
        }],
    };
    catalog.validate().map_err(|error| error.to_string())?;
    write_json(output, &catalog)?;
    Ok((
        json!({"status":"passed", "output":output, "models":catalog.models.len(),
        "runtime":RUNTIME_ID, "revision":REVISION, "runtime_bytes":bytes}),
        true,
    ))
}
