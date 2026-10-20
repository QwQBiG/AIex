#![forbid(unsafe_code)]

mod chroma_bridge;
mod legacy;
mod legacy_memory;
mod multimodal_memory;

use std::path::{Path, PathBuf};

use ai_ex_domain::AppError;
use legacy::LegacyConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("AIex migration failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), AppError> {
    let mut arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments == ["--version"] {
        println!("ai-ex-migrate {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if arguments == ["--help"] || arguments == ["-h"] {
        println!(
            "AIex Rust migration\nConfiguration: ai-ex-migrate [--input OLD.json] [--output NEW.toml]\nMemory backup: ai-ex-migrate --memory-backup --input OLD.json.gz --output NEW.jsonl --profile ID [--utc-offset-minutes MINUTES] [--embedding-space NAME]\nMultimodal dataclass snapshot: ai-ex-migrate --multimodal-memory --input SNAPSHOT.json --output NEW.jsonl --profile ID\nThis mode accepts explicitly exported dataclass JSON arrays, not an existing legacy backup format.\nChroma export: ai-ex-migrate --chroma-export --url URL --collection NAME --tenant TENANT --database DATABASE --output SNAPSHOT.json [--entity-collection NAME]\nChroma snapshot: ai-ex-migrate --chroma-snapshot --input SNAPSHOT.json --output NEW.jsonl --profile ID [--utc-offset-minutes MINUTES] [--embedding-space NAME]\nThe Chroma bridge uses the read-only v2 API. Entity collections are archived only; their source graph is not restored.\nOutputs must be new files. Sources and complete metadata are retained. Backups with vectors require an explicit embedding space."
        );
        return Ok(());
    }
    let memory_modes: Vec<_> = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, value)| match value.as_str() {
            "--memory-backup" => Some((index, MemoryImportKind::Backup)),
            "--multimodal-memory" => Some((index, MemoryImportKind::Multimodal)),
            "--chroma-snapshot" => Some((index, MemoryImportKind::ChromaSnapshot)),
            "--chroma-export" => Some((index, MemoryImportKind::ChromaExport)),
            _ => None,
        })
        .collect();
    if memory_modes.len() > 1 {
        return Err(AppError::configuration(
            "migration modes are mutually exclusive and must not be repeated",
        ));
    }
    if let Some((index, mode)) = memory_modes.first().copied() {
        arguments.remove(index);
        if mode == MemoryImportKind::ChromaExport {
            return chroma_bridge::export(arguments.into_iter()).await;
        }
        return migrate_memory(arguments.into_iter(), mode).await;
    }
    let (input, output) = parse_args(arguments.into_iter())?;
    if tokio::fs::try_exists(&output)
        .await
        .map_err(|error| AppError::unavailable(format!("cannot inspect output path: {error}")))?
    {
        return Err(AppError::configuration(format!(
            "refusing to overwrite existing output {}",
            output.display(),
        )));
    }
    let file = tokio::fs::File::open(&input)
        .await
        .map_err(|error| AppError::configuration(format!("cannot open input: {error}")))?;
    let mut content = Vec::new();
    file.take((ai_ex_config::MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut content)
        .await
        .map_err(|error| AppError::configuration(format!("cannot read input: {error}")))?;
    if content.len() > ai_ex_config::MAX_CONFIG_BYTES {
        return Err(AppError::configuration(
            "legacy configuration exceeds 1 MiB",
        ));
    }
    let legacy: LegacyConfig = serde_json::from_slice(&content).map_err(|error| {
        AppError::configuration(format!(
            "invalid legacy JSON at line {} column {}",
            error.line(),
            error.column()
        ))
    })?;
    let migration = legacy.migrate();
    migration.config.validate()?;
    let output_content = toml::to_string_pretty(&migration.config)
        .map_err(|error| AppError::protocol(format!("cannot encode migrated TOML: {error}")))?;
    write_new_atomic(&output, output_content.as_bytes()).await?;
    println!("Migrated configuration written to {}", output.display());
    for warning in migration.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

fn parse_args(arguments: impl Iterator<Item = String>) -> Result<(PathBuf, PathBuf), AppError> {
    let mut arguments = arguments;
    let mut input = PathBuf::from("config.json");
    let mut output = PathBuf::from("config/ai-ex.local.toml");
    while let Some(argument) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| AppError::configuration(format!("{argument} requires a path")))?;
        match argument.as_str() {
            "--input" => input = PathBuf::from(value),
            "--output" => output = PathBuf::from(value),
            _ => {
                return Err(AppError::configuration(format!(
                    "unknown migration argument: {argument}",
                )));
            }
        }
    }
    Ok((input, output))
}

async fn write_new_atomic(path: &Path, content: &[u8]) -> Result<(), AppError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            AppError::unavailable(format!("cannot create output directory: {error}"))
        })?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::configuration("output path requires a valid file name"))?;
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .await
        .map_err(|error| {
            AppError::unavailable(format!("cannot create temporary output: {error}"))
        })?;
    let result = async {
        file.write_all(content).await?;
        file.flush().await?;
        file.sync_data().await?;
        drop(file);
        // Publish the fully synced file without replacing a concurrent destination.
        // Both paths are siblings, so they are on the same filesystem.
        tokio::fs::hard_link(&temporary, path).await
    }
    .await;
    if let Err(error) = result {
        let _ignored = tokio::fs::remove_file(&temporary).await;
        return Err(AppError::unavailable(format!(
            "cannot commit migrated output: {error}"
        )));
    }
    let _ignored = tokio::fs::remove_file(&temporary).await;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemoryImportKind {
    Backup,
    Multimodal,
    ChromaSnapshot,
    ChromaExport,
}

impl MemoryImportKind {
    fn flag(self) -> &'static str {
        match self {
            Self::Backup => "--memory-backup",
            Self::Multimodal => "--multimodal-memory",
            Self::ChromaSnapshot => "--chroma-snapshot",
            Self::ChromaExport => "--chroma-export",
        }
    }
}

async fn migrate_memory(
    arguments: impl Iterator<Item = String>,
    mode: MemoryImportKind,
) -> Result<(), AppError> {
    let mut input = None;
    let mut output = None;
    let mut profile = None;
    let mut utc_offset = None;
    let mut embedding_space = None;
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| AppError::configuration("memory migration option requires a value"))?;
        match argument.as_str() {
            "--input" => input = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--profile" => profile = Some(value),
            "--embedding-space" if mode != MemoryImportKind::Multimodal => {
                embedding_space = Some(value)
            }
            "--utc-offset-minutes" if mode != MemoryImportKind::Multimodal => {
                utc_offset = Some(
                    value
                        .parse::<i16>()
                        .map_err(|_| AppError::configuration("invalid UTC offset"))?,
                )
            }
            _ => return Err(AppError::configuration("unknown memory migration option")),
        }
    }
    let input = input
        .ok_or_else(|| AppError::configuration(format!("{} requires --input", mode.flag())))?;
    let output = output
        .ok_or_else(|| AppError::configuration(format!("{} requires --output", mode.flag())))?;
    let profile = profile
        .ok_or_else(|| AppError::configuration(format!("{} requires --profile", mode.flag())))?;
    let content = tokio::task::spawn_blocking(move || match mode {
        MemoryImportKind::Backup => {
            legacy_memory::import_backup(&input, &profile, utc_offset, embedding_space.as_deref())
        }
        MemoryImportKind::Multimodal => multimodal_memory::import_snapshot(&input, &profile),
        MemoryImportKind::ChromaSnapshot => {
            chroma_bridge::import_snapshot(&input, &profile, utc_offset, embedding_space.as_deref())
        }
        MemoryImportKind::ChromaExport => {
            Err(AppError::configuration("export is not an import mode"))
        }
    })
    .await
    .map_err(|_| AppError::unavailable("memory import worker failed"))??;
    write_new_atomic(&output, &content).await?;
    match mode {
        MemoryImportKind::Backup => println!(
            "Migrated memories written to {} with legacy metadata and available embeddings. The source backup is unchanged. Imported records remain automatic, not confirmed facts.",
            output.display()
        ),
        MemoryImportKind::Multimodal => println!(
            "Multimodal snapshot written to {} with complete features and original snapshots. The source JSON is unchanged. Imported records remain automatic, not confirmed facts.",
            output.display()
        ),
        MemoryImportKind::ChromaSnapshot => println!(
            "Chroma main memories written to {} with original metadata and available embeddings. The source snapshot is unchanged. Imported records remain automatic, not confirmed facts.",
            output.display()
        ),
        MemoryImportKind::ChromaExport => {
            return Err(AppError::configuration("export is not an import mode"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_paths() {
        let (input, output) = parse_args(
            ["--input", "old.json", "--output", "new.toml"]
                .into_iter()
                .map(str::to_owned),
        )
        .expect("arguments parse");

        assert_eq!(input, PathBuf::from("old.json"));
        assert_eq!(output, PathBuf::from("new.toml"));
    }

    #[test]
    fn rejects_unknown_argument() {
        assert!(parse_args(["--force", "yes"].into_iter().map(str::to_owned)).is_err());
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[tokio::test]
    async fn refuses_destinations_created_after_the_initial_check() {
        let directory = std::env::temp_dir().join(format!("aiex-migrate-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let output = directory.join("existing.toml");
        tokio::fs::write(&output, b"keep this concurrent file")
            .await
            .unwrap();
        assert!(write_new_atomic(&output, b"replacement").await.is_err());
        assert_eq!(
            tokio::fs::read(&output).await.unwrap(),
            b"keep this concurrent file"
        );
        let mut entries = tokio::fs::read_dir(&directory).await.unwrap();
        assert!(entries.next_entry().await.unwrap().is_some());
        assert!(entries.next_entry().await.unwrap().is_none());
        tokio::fs::remove_file(&output).await.unwrap();
        tokio::fs::remove_dir(&directory).await.unwrap();
    }

    #[tokio::test]
    async fn exactly_one_concurrent_publication_wins() {
        let directory = std::env::temp_dir().join(format!("aiex-migrate-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let output = directory.join("config.toml");
        let (first, second) = tokio::join!(
            write_new_atomic(&output, b"first"),
            write_new_atomic(&output, b"second")
        );
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(
            tokio::fs::read(&output).await.unwrap(),
            if first.is_ok() {
                b"first".as_slice()
            } else {
                b"second".as_slice()
            }
        );
        let mut entries = tokio::fs::read_dir(&directory).await.unwrap();
        assert!(entries.next_entry().await.unwrap().is_some());
        assert!(entries.next_entry().await.unwrap().is_none());
        tokio::fs::remove_file(&output).await.unwrap();
        tokio::fs::remove_dir(&directory).await.unwrap();
    }
}
