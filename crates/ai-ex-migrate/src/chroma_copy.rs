//! Copies a closed legacy snapshot before opening it with a pinned native server.
use std::collections::BTreeMap;
use std::path::PathBuf;

use ai_ex_domain::AppError;
use serde::Serialize;

#[path = "chroma_copy_files.rs"]
mod files;
#[path = "chroma_copy_server.rs"]
mod server;

/// Explicit paths are required; the user's active legacy database is never inferred.
#[derive(Debug, Clone)]
pub struct ChromaCopyExportOptions {
    pub source: PathBuf,
    pub working_copy: PathBuf,
    pub native_server: PathBuf,
    pub server_sha256: String,
    pub output: PathBuf,
    pub collection: String,
    pub tenant: String,
    pub database: String,
    pub entity_collection: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChromaCopyExportReport {
    pub working_copy: PathBuf,
    pub snapshot: PathBuf,
    pub source_files: usize,
    pub source_bytes: u64,
    pub native_server_sha256: String,
}

impl ChromaCopyExportOptions {
    pub fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, AppError> {
        let mut values = BTreeMap::new();
        let mut arguments = arguments;
        while let Some(flag) = arguments.next() {
            if !matches!(
                flag.as_str(),
                "--source"
                    | "--working-copy"
                    | "--native-server"
                    | "--server-sha256"
                    | "--output"
                    | "--collection"
                    | "--tenant"
                    | "--database"
                    | "--entity-collection"
            ) {
                return Err(invalid("unknown Chroma copy export option"));
            }
            let value = arguments
                .next()
                .ok_or_else(|| invalid("Chroma copy export option requires a value"))?;
            if values.insert(flag, value).is_some() {
                return Err(invalid("duplicate Chroma copy export option"));
            }
        }
        let mut required = |key: &str| {
            values
                .remove(key)
                .ok_or_else(|| invalid(format!("--chroma-copy-export requires {key}")))
        };
        Ok(Self {
            source: required("--source")?.into(),
            working_copy: required("--working-copy")?.into(),
            native_server: required("--native-server")?.into(),
            server_sha256: required("--server-sha256")?,
            output: required("--output")?.into(),
            collection: required("--collection")?,
            tenant: required("--tenant")?,
            database: required("--database")?,
            entity_collection: values.remove("--entity-collection"),
        })
    }
}

pub async fn copy_export(
    options: ChromaCopyExportOptions,
) -> Result<ChromaCopyExportReport, AppError> {
    if !cfg!(windows) {
        return Err(invalid("native Chroma copy export requires Windows"));
    }
    let checked = tokio::task::spawn_blocking(move || files::prepare(options))
        .await
        .map_err(|_| AppError::unavailable("snapshot copy worker failed"))??;
    let result = server::export(&checked).await;
    // The source remains locked until both export and process shutdown finish.
    checked.verify_source()?;
    result?;
    let snapshot = tokio::fs::read(&checked.staging)
        .await
        .map_err(|error| unavailable("cannot read staged snapshot", error))?;
    crate::write_new_atomic(&checked.options.output, &snapshot).await?;
    let _ignored = tokio::fs::remove_file(&checked.staging).await;
    Ok(ChromaCopyExportReport {
        working_copy: checked.options.working_copy.clone(),
        snapshot: checked.options.output.clone(),
        source_files: checked.files.len(),
        source_bytes: checked.files.iter().map(|f| f.length).sum(),
        native_server_sha256: checked.options.server_sha256.clone(),
    })
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::configuration(message)
}
fn unavailable(message: &str, error: impl std::fmt::Display) -> AppError {
    AppError::unavailable(format!("{message}: {error}"))
}
