#[cfg(windows)]
use std::{collections::BTreeMap, ffi::OsString, net::TcpListener, time::Duration};

#[cfg(windows)]
use super::package;
use super::{ManagedOllamaSettings, stream};
use ai_ex_domain::AppError;
#[cfg(windows)]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
pub(super) struct State {
    instance: Option<Instance>,
    #[cfg(windows)]
    preparation: Option<Preparation>,
}

#[cfg(windows)]
struct Preparation {
    cancel: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<Result<(), AppError>>,
}

struct Instance {
    #[cfg(windows)]
    job: ai_ex_platform_windows::OwnedJobProcess,
    #[cfg(windows)]
    endpoint: String,
    #[cfg(windows)]
    port: u16,
}

impl State {
    #[cfg(test)]
    pub(super) fn instance_absent(&self) -> bool {
        self.instance.is_none()
    }
    #[cfg(all(test, windows))]
    pub(super) fn preparation_absent(&self) -> bool {
        self.preparation.is_none()
    }
    pub(super) fn confirmed_endpoint(&self) -> Result<Option<String>, AppError> {
        let Some(instance) = &self.instance else {
            return Ok(None);
        };
        #[cfg(not(windows))]
        let _ = instance;
        #[cfg(windows)]
        {
            if instance.job.exit_code().map_err(failure)?.is_none()
                && instance
                    .job
                    .owns_loopback_listener(instance.port)
                    .map_err(failure)?
            {
                return Ok(Some(instance.endpoint.clone()));
            }
        }
        Err(AppError::unavailable(
            "managed Ollama listener is not owned by the running instance",
        ))
    }

    pub(super) async fn ensure_started(
        &mut self,
        settings: &ManagedOllamaSettings,
        client: &reqwest::Client,
    ) -> Result<String, AppError> {
        if let Ok(Some(endpoint)) = self.confirmed_endpoint() {
            stream::check_endpoint(client, &endpoint).await?;
            return Ok(endpoint);
        }
        self.stop().await?;
        let result =
            tokio::time::timeout(settings.startup_timeout, self.start(settings, client)).await;
        match result {
            Ok(Ok(endpoint)) => Ok(endpoint),
            error => {
                let mut cause = match error {
                    Ok(Err(error)) => error,
                    _ => AppError::unavailable("managed Ollama startup timed out"),
                };
                if let Err(cleanup) = self.stop().await {
                    cause
                        .message
                        .push_str(&format!("; cleanup failed: {}", cleanup.message));
                }
                Err(cause)
            }
        }
    }

    async fn start(
        &mut self,
        settings: &ManagedOllamaSettings,
        client: &reqwest::Client,
    ) -> Result<String, AppError> {
        #[cfg(not(windows))]
        {
            let _ = (settings, client);
            Err(AppError::unavailable(
                "managed Ollama requires Windows owned-process support",
            ))
        }
        #[cfg(windows)]
        {
            let verification = settings.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let token = cancel.clone();
            self.preparation = Some(Preparation {
                cancel,
                task: tokio::task::spawn_blocking(move || package::verify(&verification, &token)),
            });
            let result = (&mut self.preparation.as_mut().unwrap().task).await;
            self.preparation = None;
            result
                .map_err(|_| AppError::unavailable("managed Ollama verification task stopped"))??;
            let environment = environment(settings)?;
            let listener = TcpListener::bind("127.0.0.1:0").map_err(failure)?;
            let port = listener.local_addr().map_err(failure)?.port();
            drop(listener);
            if port == 11434 {
                return Err(AppError::unavailable(
                    "private Ollama cannot use the default server port",
                ));
            }
            let mut environment = environment;
            environment.insert("OLLAMA_HOST".into(), format!("127.0.0.1:{port}").into());
            let job = ai_ex_platform_windows::OwnedJobProcess::spawn(
                ai_ex_platform_windows::ProcessLaunch {
                    executable: settings.executable.clone(),
                    arguments: vec!["serve".into()],
                    directory: settings.private_directory.clone(),
                    environment,
                },
            )
            .map_err(failure)?;
            let endpoint = format!("http://127.0.0.1:{port}");
            self.instance = Some(Instance {
                job,
                endpoint: endpoint.clone(),
                port,
            });
            loop {
                let instance = self
                    .instance
                    .as_ref()
                    .expect("owned instance was installed");
                if let Some(code) = instance.job.exit_code().map_err(failure)? {
                    return Err(AppError::unavailable(format!(
                        "managed Ollama exited during startup: {code:#x}"
                    )));
                }
                if instance.job.owns_loopback_listener(port).map_err(failure)? {
                    stream::check_endpoint(client, &endpoint).await?;
                    return Ok(endpoint);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }

    pub(super) async fn stop(&mut self) -> Result<(), AppError> {
        #[cfg(windows)]
        if let Some(preparation) = self.preparation.as_mut() {
            preparation.cancel.store(true, Ordering::Release);
            let _ = tokio::time::timeout(Duration::from_secs(1), &mut preparation.task)
                .await
                .map_err(|_| {
                    AppError::unavailable("managed model preparation is still stopping")
                })?;
            self.preparation = None;
        }
        let Some(instance) = self.instance.as_ref() else {
            return Ok(());
        };
        #[cfg(not(windows))]
        let _ = instance;
        #[cfg(windows)]
        {
            instance.job.terminate().map_err(failure)?;
            tokio::time::timeout(Duration::from_secs(1), async {
                while instance.job.active_processes().map_err(failure)? != 0
                    || instance.job.exit_code().map_err(failure)?.is_none()
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok::<_, AppError>(())
            })
            .await
            .map_err(|_| {
                AppError::unavailable("managed Ollama tree did not stop within one second")
            })??;
        }
        self.instance = None;
        Ok(())
    }
}

impl Drop for State {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(preparation) = &self.preparation {
            preparation.cancel.store(true, Ordering::Release);
            preparation.task.abort();
        }
    }
}

#[cfg(windows)]
fn failure(error: std::io::Error) -> AppError {
    AppError::unavailable(format!("managed Ollama process failed: {error}"))
}

#[cfg(windows)]
fn environment(settings: &ManagedOllamaSettings) -> Result<BTreeMap<OsString, OsString>, AppError> {
    let mut environment = BTreeMap::new();
    let system = std::env::var_os("SystemRoot")
        .ok_or_else(|| AppError::unavailable("Windows system directory is unavailable"))?;
    environment.insert("SystemRoot".into(), system.clone());
    environment.insert("WINDIR".into(), system.clone());
    let parent = settings
        .executable
        .parent()
        .ok_or_else(|| AppError::configuration("managed Ollama executable has no directory"))?;
    let path = std::env::join_paths([
        parent.to_path_buf(),
        parent.join("lib/ollama"),
        std::path::PathBuf::from(&system).join("System32"),
    ])
    .map_err(|_| AppError::configuration("invalid managed Ollama runtime path"))?;
    environment.insert("PATH".into(), path);
    for child in ["home", "tmp"] {
        let path = settings.private_directory.join(child);
        match std::fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(failure(error)),
        }
        package::check_regular(&path, true)?;
    }
    for key in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        environment.insert(
            key.into(),
            settings.private_directory.join("home").into_os_string(),
        );
    }
    for key in ["TEMP", "TMP"] {
        environment.insert(
            key.into(),
            settings.private_directory.join("tmp").into_os_string(),
        );
    }
    environment.insert(
        "OLLAMA_MODELS".into(),
        settings.private_directory.join("models").into_os_string(),
    );
    for (key, value) in [
        ("OLLAMA_NO_CLOUD", "true"),
        ("OLLAMA_NOPRUNE", "true"),
        ("OLLAMA_NOHISTORY", "true"),
        ("OLLAMA_NUM_PARALLEL", "1"),
        ("OLLAMA_MAX_LOADED_MODELS", "1"),
        ("OLLAMA_LLM_LIBRARY", "cpu"),
        ("OMP_NUM_THREADS", "4"),
        ("GOMAXPROCS", "4"),
        ("CUDA_VISIBLE_DEVICES", "-1"),
        ("ROCR_VISIBLE_DEVICES", "-1"),
        ("GGML_VK_VISIBLE_DEVICES", "-1"),
        ("HTTP_PROXY", "http://127.0.0.1:9"),
        ("HTTPS_PROXY", "http://127.0.0.1:9"),
        ("ALL_PROXY", "http://127.0.0.1:9"),
        ("NO_PROXY", "127.0.0.1,localhost"),
    ] {
        environment.insert(key.into(), value.into());
    }
    Ok(environment)
}
