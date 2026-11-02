use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use ai_ex_domain::AppError;
use ring::digest::{Context, SHA256};

#[cfg(windows)]
use super::ManagedOllamaSettings;
mod generated;
mod install;
const PRIVATE_MANIFEST: &str =
    "manifests/registry.ollama.ai/library/qwen3-original-4b-nothink/native-v1";

#[derive(Debug, Clone, Copy)]
pub struct ManagedModelFile {
    pub path: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

pub const MANAGED_MODEL_FILES: &[ManagedModelFile] = &[
    ManagedModelFile {
        path: "model.gguf",
        bytes: 2_497_280_256,
        sha256: "7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5",
    },
    ManagedModelFile {
        path: "template",
        bytes: 1517,
        sha256: "0a3e43c71bdec4a59b0ca75916475ff79722c1681b06203ea092bc71e1d0a92f",
    },
    ManagedModelFile {
        path: "config.json",
        bytes: 488,
        sha256: "de0d000466e9b88012625eb1c8fe5178eaaf28d187db48eaaa318844340bf827",
    },
    ManagedModelFile {
        path: "LICENSE.txt",
        bytes: 11_544,
        sha256: "5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd",
    },
    ManagedModelFile {
        path: "params.json",
        bytes: 133,
        sha256: "a52b2de771634a7bbcbc2e46e14958e6dfff5adfe83413389bb94cf4ddcbb02a",
    },
    ManagedModelFile {
        path: "manifest.json",
        bytes: 859,
        sha256: "fd6edb760e8d0ef4cace9464485e9816b11779ec939a174561af8c144833730f",
    },
];

/// Exact control-file bytes used by the installer, without JSON reserialization.
/// Model weights and their license remain separately downloaded artifacts.
pub fn managed_model_generated_file(path: &str) -> Option<&'static [u8]> {
    generated::file(path)
}

/// Verifies the read-only installer output without opening a model process.
pub fn verify_managed_model_source(source: &Path) -> Result<(), AppError> {
    verify_source(source, None)
}

fn verify_source(source: &Path, cancel: Option<&AtomicBool>) -> Result<(), AppError> {
    check_cancel(cancel)?;
    if !source.is_absolute() {
        return Err(AppError::configuration(
            "managed model source must be absolute",
        ));
    }
    check_regular(source, true)?;
    for file in MANAGED_MODEL_FILES {
        verify_file_cancellable(&source.join(file.path), file.bytes, file.sha256, cancel)?;
    }
    Ok(())
}

/// Builds only this preset's private content-addressed store. Existing files
/// must match; the installer source is never rewritten.
pub fn prepare_managed_model(source: &Path, private: &Path) -> Result<(), AppError> {
    prepare(source, private, None)
}

fn prepare(source: &Path, private: &Path, cancel: Option<&AtomicBool>) -> Result<(), AppError> {
    validate_layout(source, private)?;
    verify_source(source, cancel)?;
    if !private.is_absolute() {
        return Err(AppError::configuration(
            "managed model store must be absolute",
        ));
    }
    install::ensure_directory(private)?;
    let model_root = private.join("models");
    let blobs = model_root.join("blobs");
    install::ensure_directory(&blobs)?;
    for file in &MANAGED_MODEL_FILES[..MANAGED_MODEL_FILES.len() - 1] {
        install::file(
            &source.join(file.path),
            &blobs.join(format!("sha256-{}", file.sha256)),
            file,
            cancel,
        )?;
    }
    let manifest = model_root.join(PRIVATE_MANIFEST);
    install::ensure_directory(manifest.parent().expect("fixed manifest has a parent"))?;
    install::file(
        &source.join("manifest.json"),
        &manifest,
        MANAGED_MODEL_FILES.last().unwrap(),
        cancel,
    )
}

pub(super) fn validate_layout(source: &Path, private: &Path) -> Result<(), AppError> {
    if !source.is_absolute()
        || !private.is_absolute()
        || [source, private].iter().any(|path| {
            path.components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        })
    {
        return Err(AppError::configuration(
            "managed model paths must be absolute without parent traversal",
        ));
    }
    let key = |path: &Path| {
        path.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_uppercase()
    };
    let source = key(source);
    let private = key(private);
    if source == private
        || source.starts_with(&(private.clone() + "/"))
        || private.starts_with(&(source + "/"))
    {
        return Err(AppError::configuration(
            "read-only model source and private store must be separate",
        ));
    }
    Ok(())
}

/// Called after stopping this adapter and releasing its model lease. Removes
/// only this preset's declared private names; the installer source is retained.
pub(super) fn clear_private_model(private: &Path) -> Result<(), AppError> {
    if !private.is_absolute() {
        return Err(AppError::configuration(
            "private model directory must be absolute",
        ));
    }
    if !private
        .try_exists()
        .map_err(|_| AppError::unavailable("cannot inspect private model store"))?
    {
        return Ok(());
    }
    install::ensure_directory(private)?;
    let root = private.join("models");
    remove_private_file(&root, PRIVATE_MANIFEST)?;
    for file in &MANAGED_MODEL_FILES[..MANAGED_MODEL_FILES.len() - 1] {
        remove_private_file(&root, &format!("blobs/sha256-{}", file.sha256))?;
    }
    remove_private_file(
        &root,
        &format!("metadata/sha256-{}.json", MANAGED_MODEL_FILES[0].sha256),
    )
}

fn remove_private_file(root: &Path, relative: &str) -> Result<(), AppError> {
    let mut path = root.to_path_buf();
    let parts: Vec<_> = relative.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        match fs::symlink_metadata(&path) {
            Ok(_) => check_regular(&path, true)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(AppError::unavailable("cannot inspect private model store")),
        }
        path.push(part);
        if index + 1 == parts.len() {
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    check_regular(&path, false)?;
                    fs::remove_file(&path).map_err(|_| {
                        AppError::unavailable("private model file could not be removed")
                    })?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(AppError::unavailable("cannot inspect private model file")),
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn verify(
    settings: &ManagedOllamaSettings,
    cancel: &AtomicBool,
) -> Result<(), AppError> {
    verify_file_cancellable(
        &settings.executable,
        38_019_464,
        "cc00918e2d25577969c929a1004be054781b8bff25819c1be19ca2f7b7bb4012",
        Some(cancel),
    )?;
    prepare(
        &settings.model_source,
        &settings.private_directory,
        Some(cancel),
    )
}

pub(super) fn check_regular(path: &Path, directory: bool) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| AppError::unavailable("managed Ollama resources are not prepared"))?;
    let linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        linked || metadata.file_attributes() & 0x400 != 0
    };
    if linked
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        }
    {
        return Err(AppError::unavailable(
            "managed Ollama resource is not a regular private path",
        ));
    }
    Ok(())
}

pub(super) fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), AppError> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(AppError::unavailable(
            "managed model preparation was cancelled",
        ));
    }
    Ok(())
}

pub(super) fn verify_file_cancellable(
    path: &Path,
    bytes: u64,
    expected: &str,
    cancel: Option<&AtomicBool>,
) -> Result<(), AppError> {
    check_cancel(cancel)?;
    check_regular(path, false)?;
    let mut file = File::open(path)
        .map_err(|_| AppError::unavailable("cannot read managed Ollama resource"))?;
    if file
        .metadata()
        .map_err(|_| AppError::unavailable("cannot inspect managed Ollama resource"))?
        .len()
        != bytes
    {
        return Err(AppError::unavailable(
            "managed Ollama resource size mismatch",
        ));
    }
    let mut digest = Context::new(&SHA256);
    let mut buffer = [0_u8; 65536];
    loop {
        check_cancel(cancel)?;
        let read = file
            .read(&mut buffer)
            .map_err(|_| AppError::unavailable("cannot verify managed Ollama resource"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual: String = digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected {
        return Err(AppError::unavailable(
            "managed Ollama resource digest mismatch",
        ));
    }
    Ok(())
}
