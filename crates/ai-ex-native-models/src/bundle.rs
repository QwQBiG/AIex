use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ai_ex_config::native::{ModelSelection, NativeBackend};
use ai_ex_domain::AppError;
use ai_ex_inference::WorkerLaunch;
use ai_ex_model_catalog::{ArtifactKind, ModelBackend, validate_relative_path};
use ai_ex_model_manager::{ModelManager, PackageLease};
use serde_json::json;

use crate::files::{self, PrivateDirectory};

mod configuration;
use configuration::configuration;

pub(crate) struct PreparedWorker {
    _lease: PackageLease,
    pub root: Arc<PrivateDirectory>,
    executable: PathBuf,
    config: PathBuf,
    ready_timeout: Duration,
}

impl PreparedWorker {
    pub fn prepare(
        manager: &ModelManager,
        selection: &ModelSelection,
        jobs: &Path,
        ready_timeout: Duration,
    ) -> Result<Self, AppError> {
        selection.validate()?;
        let lease = manager.acquire(&selection.model_id)?;
        let model = lease
            .model_manifest()
            .ok_or_else(|| files::error("selection is not a model package"))?;
        if model.backend != backend(selection.backend)? {
            return Err(files::error(
                "selected model backend differs from its catalog",
            ));
        }
        let (runtime, runtime_data) = lease
            .required_runtime(&selection.runtime_id)
            .ok_or_else(|| files::error("selected runtime is not a leased model requirement"))?;
        if runtime.protocol_version != ai_ex_inference::WORKER_PROTOCOL_VERSION {
            return Err(files::error(
                "native runtime protocol version is incompatible",
            ));
        }
        let worker = runtime
            .workers
            .iter()
            .find(|worker| worker.backend == model.backend)
            .ok_or_else(|| files::error("runtime does not declare this native worker"))?;
        let model_prefix = relative(lease.root_directory(), &lease.data_directory())?;
        let runtime_prefix = relative(lease.root_directory(), &runtime_data)?;
        let mut merged = model.clone();
        merged.artifacts = model
            .artifacts
            .iter()
            .map(|artifact| {
                let mut artifact = artifact.clone();
                artifact.path = format!("{model_prefix}/{}", artifact.path);
                artifact
            })
            .chain(
                runtime
                    .artifacts
                    .iter()
                    .filter(|artifact| artifact.kind != ArtifactKind::SourceArchive)
                    .map(|artifact| {
                        let mut artifact = artifact.clone();
                        artifact.path = format!("{runtime_prefix}/{}", artifact.path);
                        artifact
                    }),
            )
            .collect();
        merged.licenses.extend(runtime.licenses.iter().cloned());
        merged.validate()?;
        let root = PrivateDirectory::create(jobs, "worker")?;
        let manifest = root.path.join("package.json");
        write_json(&manifest, &merged)?;
        let library = worker
            .runtime_library
            .as_ref()
            .map(|path| format!("{runtime_prefix}/{path}"));
        let config = root.path.join("worker.json");
        let backend_config = configuration(model.backend, &model_prefix, library.as_deref())?;
        write_json(
            &config,
            &json!({"schema_version":1,"manifest":manifest,"directory":worker_directory(lease.root_directory(), model.backend),"backend":backend_config}),
        )?;
        let executable = runtime_data.join(&worker.executable);
        files::regular(&executable)?;
        Ok(Self {
            _lease: lease,
            root,
            executable,
            config,
            ready_timeout,
        })
    }

    pub fn launch(&self) -> WorkerLaunch {
        let environment = BTreeMap::from([
            ("OMP_NUM_THREADS".into(), "2".into()),
            ("RAYON_NUM_THREADS".into(), "2".into()),
            ("OPENBLAS_NUM_THREADS".into(), "2".into()),
        ]);
        WorkerLaunch {
            executable: self.executable.clone(),
            arguments: vec!["--config".into(), self.config.clone().into_os_string()],
            environment,
            job_directory: self.root.path.clone(),
            ready_timeout: self.ready_timeout,
        }
    }
}

fn relative(root: &Path, directory: &Path) -> Result<String, AppError> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| files::error("leased data directory escapes its package root"))?;
    let text = relative
        .to_str()
        .ok_or_else(|| files::error("leased data path is not UTF-8"))?
        .replace('\\', "/");
    validate_relative_path(&text)?;
    Ok(text)
}

fn worker_directory(root: &Path, backend: ModelBackend) -> PathBuf {
    #[cfg(windows)]
    if backend == ModelBackend::SherpaKokoro {
        // Sherpa 1.13.8 appends '/phontab' to data_dir before GetFileAttributesW.
        // Verbatim Win32 paths disable slash normalization; ordinary Unicode
        // paths preserve the same leased location and allow that native join.
        if let Some(text) = root.to_str() {
            if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
                return PathBuf::from(format!(r"\\{unc}"));
            }
            if let Some(drive) = text.strip_prefix(r"\\?\")
                && drive.as_bytes().get(1..3) == Some(b":\\")
            {
                return PathBuf::from(drive);
            }
        }
    }
    let _backend = backend;
    root.to_owned()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn kokoro_gets_ordinary_unicode_paths_without_moving_package_data() {
        let root = Path::new(r"\\?\C:\中文 空格\packages");
        assert_eq!(
            worker_directory(root, ModelBackend::SherpaKokoro),
            PathBuf::from(r"C:\中文 空格\packages")
        );
        assert_eq!(worker_directory(root, ModelBackend::OnnxEmbedding), root);
        assert_eq!(
            worker_directory(
                Path::new(r"\\?\UNC\server\share\中文"),
                ModelBackend::SherpaKokoro
            ),
            PathBuf::from(r"\\server\share\中文")
        );
    }
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), AppError> {
    files::write_new(path, &serde_json::to_vec(value).map_err(files::error)?)
}

fn backend(backend: NativeBackend) -> Result<ModelBackend, AppError> {
    match backend {
        NativeBackend::SherpaWhisper => Ok(ModelBackend::SherpaWhisper),
        NativeBackend::SherpaKokoro => Ok(ModelBackend::SherpaKokoro),
        NativeBackend::Qwen3TtsOnnx => Ok(ModelBackend::Qwen3TtsOnnx),
        NativeBackend::OnnxEmbedding => Ok(ModelBackend::OnnxEmbedding),
        NativeBackend::Ollama => Err(files::error(
            "dialogue uses its separately managed native server",
        )),
    }
}
