use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

use ai_ex_domain::{AppError, DesktopRuntimeSettings};

pub(super) fn load(path: &Path) -> Result<DesktopRuntimeSettings, AppError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(_) => {
            return Err(AppError::configuration(
                "cannot read desktop behavior settings",
            ));
        }
    };
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::configuration("cannot read desktop behavior settings"))?;
    if bytes.len() > 65536 {
        return Err(AppError::configuration(
            "desktop settings exceed size limit",
        ));
    }
    let settings: DesktopRuntimeSettings = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::configuration("invalid desktop behavior settings JSON"))?;
    settings.validate()?;
    Ok(settings)
}

pub(super) fn save(path: &Path, settings: &DesktopRuntimeSettings) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::configuration("desktop settings directory missing"))?;
    std::fs::create_dir_all(parent)
        .map_err(|_| AppError::configuration("cannot prepare desktop settings directory"))?;
    let temporary = parent.join(format!(".desktop-runtime-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let bytes = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _cleanup = std::fs::remove_file(&temporary);
    }
    result.map_err(|_| AppError::configuration("cannot save desktop behavior settings"))
}
