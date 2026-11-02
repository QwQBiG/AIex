use std::time::Duration;

use ai_ex_config::native::ModelSelection;
use ai_ex_core::{LanguageModelPort, ModelRequest};
use ai_ex_domain::TurnId;
use ai_ex_model_manager::PackageLease;
use ai_ex_ollama::{ManagedOllama, ManagedOllamaSettings};
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::*;

#[derive(Default)]
pub(super) struct ManagedState {
    current: Option<Client>,
    turn: Option<TurnId>,
    forward: Option<JoinHandle<()>>,
    pub last_error: Option<AppError>,
}

struct Client {
    selection: ModelSelection,
    client: ManagedOllama,
    _lease: PackageLease,
}

pub(crate) struct NativeLanguageModel {
    runtime: Arc<NativeRuntime>,
}

impl NativeRuntime {
    pub(crate) fn language_model(self: &Arc<Self>) -> NativeLanguageModel {
        NativeLanguageModel {
            runtime: self.clone(),
        }
    }

    pub(super) async fn ensure_dialogue_idle(&self) -> Result<(), AppError> {
        if self.managed.lock().await.turn.is_some() {
            return Err(AppError::invalid_transition(
                "请先停止当前对话，再切换或移除本地对话模型",
            ));
        }
        Ok(())
    }

    pub(super) async fn clear_dialogue(&self) -> Result<(), AppError> {
        let (current, forward) = {
            let mut managed = self.managed.lock().await;
            managed.turn = None;
            (managed.current.take(), managed.forward.take())
        };
        if let Some(task) = forward {
            task.abort();
            let _stopped = task.await;
        }
        if let Some(mut current) = current
            && let Err(error) = current.client.clear_prepared_model().await
        {
            self.managed.lock().await.current = Some(current);
            return Err(error);
        }
        Ok(())
    }

    fn managed_client(&self, selection: ModelSelection) -> Result<Client, AppError> {
        let manager = self.manager()?;
        let lease = manager.acquire(&selection.model_id)?;
        let (runtime, runtime_root) = lease
            .required_runtime(&selection.runtime_id)
            .ok_or_else(|| AppError::configuration("本地对话缺少匹配的原生运行包"))?;
        let executable = runtime
            .ollama_executable
            .as_ref()
            .ok_or_else(|| AppError::configuration("原生运行包未声明本地对话程序"))?;
        let storage = self.store()?.resolve_storage(&self.config()?)?;
        let client = ManagedOllama::new(ManagedOllamaSettings {
            executable: runtime_root.join(executable),
            model_source: lease.data_directory(),
            private_directory: storage.jobs.join("managed-dialogue"),
            startup_timeout: Duration::from_secs(120),
            request_timeout: Duration::from_secs(180),
        })?;
        Ok(Client {
            selection,
            client,
            _lease: lease,
        })
    }
}

#[async_trait]
impl LanguageModelPort for NativeLanguageModel {
    async fn stream(
        &mut self,
        request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let _mutation = self.runtime.mutation.lock().await;
        if self.runtime.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("原生运行已停止"));
        }
        if !self.runtime.lock()?.configured {
            return Err(AppError::unavailable("请先保存原生设置并准备本地模型"));
        }
        let selection = self
            .runtime
            .config()?
            .dialogue
            .ok_or_else(|| AppError::unavailable("本地对话未启用"))?;
        let mut managed = self.runtime.managed.lock().await;
        if managed.turn.is_some() {
            return Err(AppError::invalid_transition("本地对话已有进行中的回合"));
        }
        let replace = managed
            .current
            .as_ref()
            .is_none_or(|current| current.selection != selection);
        if replace {
            if let Some(current) = &mut managed.current {
                current.client.clear_prepared_model().await?;
            }
            managed.current = Some(self.runtime.managed_client(selection)?);
        }
        let turn = request.turn_id;
        let mut source = managed
            .current
            .as_mut()
            .expect("managed client initialized")
            .client
            .stream(request)
            .await?;
        managed.turn = Some(turn);
        managed.last_error = None;
        let (sender, receiver) = mpsc::channel(64);
        let runtime = Arc::downgrade(&self.runtime);
        managed.forward = Some(tokio::spawn(async move {
            let mut failure = None;
            while let Some(chunk) = source.recv().await {
                if let Err(error) = &chunk {
                    failure = Some(error.clone());
                }
                if sender.send(chunk).await.is_err() {
                    break;
                }
            }
            if let Some(runtime) = runtime.upgrade() {
                let mut managed = runtime.managed.lock().await;
                if managed.turn == Some(turn) {
                    managed.turn = None;
                    managed.last_error = failure;
                }
            }
        }));
        Ok(receiver)
    }

    async fn cancel(&mut self, turn: TurnId) -> Result<(), AppError> {
        let mut managed = self.runtime.managed.lock().await;
        if managed.turn != Some(turn) {
            return Ok(());
        }
        managed.turn = None;
        if let Some(task) = managed.forward.take() {
            task.abort();
            let _stopped = task.await;
        }
        let Some(current) = &mut managed.current else {
            return Ok(());
        };
        // The upstream task may already have completed while its final tokens
        // were buffered; shutdown still reaps this owned instance in that case.
        if let Err(error) = current.client.cancel(turn).await {
            current.client.shutdown().await?;
            if error.kind != ai_ex_domain::ErrorKind::InvalidTransition {
                return Err(error);
            }
        }
        Ok(())
    }
}
