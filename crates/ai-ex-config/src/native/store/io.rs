use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use ai_ex_domain::AppError;

use super::failure;

pub(super) fn check_regular_if_present(path: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let mut linked = metadata.file_type().is_symlink();
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                linked |= metadata.file_attributes() & 0x400 != 0;
            }
            if linked || !metadata.is_file() {
                return Err(AppError::configuration(
                    "native config and auxiliary files must be regular files",
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failure(error)),
    }
}

pub(super) fn read_bytes(path: &Path) -> Result<Vec<u8>, AppError> {
    check_regular_if_present(path)?;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(failure)?
        .take((crate::MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() > crate::MAX_CONFIG_BYTES {
        return Err(AppError::configuration(
            "native configuration exceeds 1 MiB",
        ));
    }
    Ok(bytes)
}

pub(super) fn write_new(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(failure)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _cleanup = fs::remove_file(path);
        return Err(failure(error));
    }
    Ok(())
}
