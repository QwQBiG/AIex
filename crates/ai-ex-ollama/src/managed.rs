use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ai_ex_core::{LanguageModelPort, ModelRequest};
use ai_ex_domain::{AppError, ComponentHealth, TurnId};
use async_trait::async_trait;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

mod package;
mod process;
mod stream;
#[cfg(test)]
mod tests;

pub const MANAGED_VERSION: &str = "0.40.11";
pub const MANAGED_MODEL: &str = "qwen3-original-4b-nothink:native-v1";
pub const MANAGED_MODEL_ID: &str = "qwen3-4b";
pub use package::{
    MANAGED_MODEL_FILES, ManagedModelFile, managed_model_generated_file, prepare_managed_model,
    verify_managed_model_source,
};
pub(super) const MANIFEST_SHA: &str =
    "fd6edb760e8d0ef4cace9464485e9816b11779ec939a174561af8c144833730f";

/// Only the application's private store is used; an external server has its own
/// existing OllamaClient constructor and configuration.
#[derive(Debug, Clone)]
pub struct ManagedOllamaSettings {
    pub executable: PathBuf,
    pub model_source: PathBuf,
    pub private_directory: PathBuf,
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
}

pub struct ManagedOllama {
    settings: Arc<ManagedOllamaSettings>,
    client: reqwest::Client,
    state: Arc<Mutex<process::State>>,
    active: Arc<Mutex<Option<TurnId>>>,
    task: Option<JoinHandle<()>>,
}

impl ManagedOllama {
    /// No process or model is opened here, so unavailable resources do not prevent
    /// construction of the service's control endpoint.
    pub fn new(settings: ManagedOllamaSettings) -> Result<Self, AppError> {
        package::validate_layout(&settings.model_source, &settings.private_directory)?;
        if !settings.executable.is_absolute()
            || !settings.model_source.is_absolute()
            || !settings.private_directory.is_absolute()
            || settings.startup_timeout.is_zero()
            || settings.startup_timeout > Duration::from_secs(600)
            || settings.request_timeout.is_zero()
            || settings.request_timeout > Duration::from_secs(600)
        {
            return Err(AppError::configuration(
                "invalid managed Ollama paths or timeouts",
            ));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(settings.request_timeout)
            .build()
            .map_err(|_| AppError::configuration("cannot build managed Ollama client"))?;
        Ok(Self {
            settings: Arc::new(settings),
            client,
            state: Arc::new(Mutex::new(process::State::default())),
            active: Arc::new(Mutex::new(None)),
            task: None,
        })
    }

    pub async fn prepare(&mut self) -> Result<(), AppError> {
        self.state
            .lock()
            .await
            .ensure_started(&self.settings, &self.client)
            .await
            .map(|_| ())
    }

    pub async fn health(&self) -> ComponentHealth {
        let endpoint = match self.state.try_lock() {
            Ok(state) => state.confirmed_endpoint(),
            Err(_) => return ComponentHealth::unavailable("ollama_native", "本地对话正在准备"),
        };
        let endpoint = match endpoint {
            Ok(Some(endpoint)) => endpoint,
            Ok(None) => return ComponentHealth::unavailable("ollama_native", "本地对话尚未启动"),
            Err(error) => return ComponentHealth::unavailable("ollama_native", error.message),
        };
        if let Err(error) = stream::check_endpoint(&self.client, &endpoint).await {
            return ComponentHealth::unavailable("ollama_native", error.message);
        }
        let current = self
            .state
            .try_lock()
            .ok()
            .and_then(|s| s.confirmed_endpoint().ok())
            .flatten();
        if current.as_deref() != Some(endpoint.as_str()) {
            return ComponentHealth::unavailable("ollama_native", "本地对话实例已停止");
        }
        ComponentHealth {
            component: "ollama_native".into(),
            ready: true,
            detail: "本地对话已就绪；原生 CPU / 非思考预设".into(),
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), AppError> {
        *self.active.lock().await = None;
        self.stop_task().await;
        self.state.lock().await.stop().await
    }

    /// Drains the owned instance and releases private hard links before the
    /// model manager removes its source package.
    pub async fn clear_prepared_model(&mut self) -> Result<(), AppError> {
        self.shutdown().await?;
        let private = self.settings.private_directory.clone();
        tokio::task::spawn_blocking(move || package::clear_private_model(&private))
            .await
            .map_err(|_| AppError::unavailable("private model cleanup task stopped"))?
    }

    async fn stop_task(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for ManagedOllama {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[async_trait]
impl LanguageModelPort for ManagedOllama {
    async fn stream(
        &mut self,
        request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let mut active = self.active.lock().await;
        if active.is_some() {
            return Err(AppError::invalid_transition(
                "a managed Ollama turn is already active",
            ));
        }
        *active = Some(request.turn_id);
        drop(active);
        let (sender, receiver) = mpsc::channel(64);
        let settings = self.settings.clone();
        let state = self.state.clone();
        let active = self.active.clone();
        let client = self.client.clone();
        self.task = Some(tokio::spawn(async move {
            let result = async {
                let endpoint = state
                    .lock()
                    .await
                    .ensure_started(&settings, &client)
                    .await?;
                stream::run(
                    &client,
                    &endpoint,
                    request.messages,
                    &sender,
                    &active,
                    request.turn_id,
                )
                .await
            }
            .await;
            if let Err(mut error) = result {
                if let Err(cleanup) = state.lock().await.stop().await {
                    error
                        .message
                        .push_str(&format!("; cleanup failed: {}", cleanup.message));
                }
                let _ = stream::deliver(&sender, &active, request.turn_id, Err(error)).await;
            }
            let mut current = active.lock().await;
            if *current == Some(request.turn_id) {
                *current = None;
            }
        }));
        Ok(receiver)
    }

    async fn cancel(&mut self, turn_id: TurnId) -> Result<(), AppError> {
        let mut active = self.active.lock().await;
        if *active != Some(turn_id) {
            return Err(AppError::invalid_transition(
                "cannot cancel an inactive managed Ollama turn",
            ));
        }
        *active = None;
        drop(active);
        self.stop_task().await;
        self.state.lock().await.stop().await
    }
}
