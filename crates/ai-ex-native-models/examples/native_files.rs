//! Explicit maintainer file probe; imports fixed offline packages and never opens devices.
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use ai_ex_domain::AppError;
use ai_ex_model_manager::{DistributionCatalog, ModelManager, OperationPhase};
use ai_ex_native_models::NativeWorkerPool;
use serde::Deserialize;
use serde_json::json;

#[path = "support/native_files.rs"]
mod steps;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    catalog: PathBuf,
    packages: PathBuf,
    jobs: PathBuf,
    voices: PathBuf,
    sources: BTreeMap<String, PathBuf>,
    #[serde(default)]
    skip_embedding: bool,
}

#[tokio::main]
async fn main() {
    let result = async {
        let path = std::env::args_os()
            .nth(1)
            .ok_or_else(|| AppError::configuration("usage: native_files ABSOLUTE_SPEC_JSON"))?;
        let spec: Spec = read_json(Path::new(&path))?;
        let catalog: DistributionCatalog = read_json(&spec.catalog)?;
        let manager = ModelManager::open(&spec.packages, catalog).await?;
        let pool = NativeWorkerPool::new(manager.clone(), spec.jobs.clone(), spec.voices.clone())?;
        let result = async {
            for (id, source) in &spec.sources {
                if manager
                    .snapshot()?
                    .iter()
                    .any(|package| package.id == *id && package.installed)
                {
                    continue;
                }
                eprintln!("native file probe: importing {id}");
                let operation = manager.start_import(id, source).await?;
                let completed = manager.wait(operation).await?;
                if completed.phase != OperationPhase::Completed {
                    return Err(AppError::unavailable(format!(
                        "offline import failed for {id}: {:?}",
                        completed.error
                    )));
                }
            }
            let output = spec.jobs.join(format!("probe-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&output)
                .map_err(|error| AppError::configuration(error.to_string()))?;
            steps::run(&pool, &output, spec.skip_embedding).await
        }
        .await;
        let shutdown = pool.shutdown().await;
        manager.shutdown().await?;
        let report = result?;
        shutdown?;
        Ok::<_, AppError>(report)
    }
    .await;
    match result {
        Ok(report) => println!("{}", serde_json::to_string_pretty(&report).unwrap()),
        Err(error) => {
            println!("{}", json!({"status":"failed","error":error}));
            std::process::exit(1);
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, AppError> {
    if !path.is_absolute() {
        return Err(AppError::configuration("file probe paths must be absolute"));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes))
        .map_err(|error| AppError::configuration(error.to_string()))?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(AppError::configuration("file probe JSON exceeds 4 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|error| AppError::configuration(error.to_string()))
}
