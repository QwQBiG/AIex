use std::path::Path;

use ai_ex_domain::AppError;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Publish in an existing directory without replacing a destination.
/// The caller may run this blocking operation in an isolated process.
pub fn write_new_atomic_existing_dir(path: &Path, content: &[u8]) -> Result<bool, AppError> {
    use std::io::Write;

    let parent = path
        .parent()
        .filter(|parent| parent.is_absolute() && parent.is_dir())
        .ok_or_else(|| AppError::configuration("output directory is unavailable"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::configuration("output path requires a valid file name"))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| AppError::unavailable("cannot create temporary output"))?;
    let result = (|| {
        file.write_all(content)?;
        file.flush()?;
        file.sync_data()?;
        drop(file);
        std::fs::hard_link(&temporary, path)
    })();
    let retained = std::fs::remove_file(&temporary).is_err();
    result.map_err(|_| AppError::unavailable("cannot publish output"))?;
    Ok(retained)
}

/// Publishes a complete new file without replacing an existing destination.
pub async fn write_new_atomic(path: &Path, content: &[u8]) -> Result<(), AppError> {
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
