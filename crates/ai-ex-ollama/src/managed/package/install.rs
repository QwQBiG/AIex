use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{ManagedModelFile, check_cancel, check_regular, verify_file_cancellable};
use ai_ex_domain::{AppError, TurnId};
use std::sync::atomic::AtomicBool;

pub(super) fn ensure_directory(path: &Path) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        ensure_directory(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(_) => check_regular(path, true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if path.parent().is_none() {
                return Err(AppError::configuration("private directory has no parent"));
            }
            match fs::create_dir(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    check_regular(path, true)
                }
                Err(_) => Err(AppError::unavailable(
                    "cannot create private model directory",
                )),
            }
        }
        Err(_) => Err(AppError::unavailable(
            "cannot inspect private model directory",
        )),
    }
}

pub(super) fn file(
    source: &Path,
    destination: &Path,
    expected: &ManagedModelFile,
    cancel: Option<&AtomicBool>,
) -> Result<(), AppError> {
    check_cancel(cancel)?;
    if existing_matches(destination, expected, cancel)? {
        return Ok(());
    }
    if fs::hard_link(source, destination).is_ok() {
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| AppError::configuration("private file has no parent"))?;
    let temporary = Temporary(parent.join(format!(".model-copy-{}", TurnId::new().0)));
    let mut input = File::open(source)
        .map_err(|_| AppError::unavailable("cannot read prepared model source"))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary.0)
        .map_err(|_| AppError::unavailable("cannot stage private model copy"))?;
    let mut bytes = [0_u8; 65536];
    loop {
        check_cancel(cancel)?;
        let read = input
            .read(&mut bytes)
            .map_err(|_| AppError::unavailable("model copy read failed"))?;
        if read == 0 {
            break;
        }
        output
            .write_all(&bytes[..read])
            .map_err(|_| AppError::unavailable("model copy write failed"))?;
    }
    output
        .sync_all()
        .map_err(|_| AppError::unavailable("cannot finish model copy"))?;
    drop(output);
    verify_file_cancellable(&temporary.0, expected.bytes, expected.sha256, cancel)?;
    if existing_matches(destination, expected, cancel)? {
        return Ok(());
    }
    fs::rename(&temporary.0, destination)
        .map_err(|_| AppError::unavailable("cannot publish private model copy"))
}

fn existing_matches(
    path: &Path,
    expected: &ManagedModelFile,
    cancel: Option<&AtomicBool>,
) -> Result<bool, AppError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            verify_file_cancellable(path, expected.bytes, expected.sha256, cancel).map(|()| true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(AppError::unavailable("cannot inspect private model file")),
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installs_only_matching_files_and_preserves_source() {
        let root = std::env::temp_dir().join(format!("managed-model-copy-{}", TurnId::new().0));
        ensure_directory(&root).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        fs::write(&source, b"x").unwrap();
        let expected = ManagedModelFile {
            path: "source",
            bytes: 1,
            sha256: "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881",
        };
        file(&source, &destination, &expected, None).unwrap();
        file(&source, &destination, &expected, None).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"x");
        let mismatch = ManagedModelFile {
            bytes: 2,
            ..expected
        };
        assert!(file(&source, &destination, &mismatch, None).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"x");
        fs::remove_file(destination).unwrap();
        fs::remove_file(source).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn source_mismatch_is_rejected_before_creating_private_store() {
        let root =
            std::env::temp_dir().join(format!("managed-source-mismatch-{}", TurnId::new().0));
        let source = root.join("source");
        let private = root.join("private");
        ensure_directory(&source).unwrap();
        fs::write(source.join("model.gguf"), b"bad").unwrap();
        assert!(super::super::prepare_managed_model(&source, &private).is_err());
        assert!(!private.exists());
        assert_eq!(fs::read(source.join("model.gguf")).unwrap(), b"bad");
        assert!(super::super::prepare_managed_model(&source, &source).is_err());
        fs::remove_file(source.join("model.gguf")).unwrap();
        fs::remove_dir(source).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn clearing_private_model_keeps_source_and_unrelated_files() {
        let root = std::env::temp_dir().join(format!("managed-clear-{}", TurnId::new().0));
        let private = root.join("private");
        let blobs = private.join("models/blobs");
        ensure_directory(&blobs).unwrap();
        let known = blobs.join(format!(
            "sha256-{}",
            super::super::MANAGED_MODEL_FILES[0].sha256
        ));
        let unrelated = blobs.join("unrelated.bin");
        let source = root.join("source.gguf");
        fs::write(&source, b"source").unwrap();
        fs::write(&known, b"private").unwrap();
        fs::write(&unrelated, b"keep").unwrap();
        super::super::clear_private_model(&private).unwrap();
        assert!(!known.exists());
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
        fs::remove_file(source).unwrap();
        fs::remove_file(unrelated).unwrap();
        fs::remove_dir(blobs).unwrap();
        fs::remove_dir(private.join("models")).unwrap();
        fs::remove_dir(private).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
