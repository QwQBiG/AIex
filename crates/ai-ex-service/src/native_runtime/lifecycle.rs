use std::time::Duration;

use ai_ex_config::native::VoicePreset;
use ai_ex_domain::ComponentHealth;

use super::*;

impl NativeRuntime {
    pub(crate) fn worker_pool(&self) -> Result<NativeWorkerPool, AppError> {
        let (manager, storage, epoch) = {
            let state = self.lock()?;
            if let Some(pool) = &state.pool {
                return Ok(pool.clone());
            }
            if state.loading || self.closed.load(Ordering::Acquire) {
                return Err(AppError::unavailable("本地模型缓存尚在准备"));
            }
            let config = state.config.as_ref().map_err(Clone::clone)?;
            let manager = state.manager.clone().ok_or_else(|| {
                state
                    .error
                    .clone()
                    .unwrap_or_else(|| AppError::unavailable("本地模型管理不可用"))
            })?;
            (manager, self.store()?.resolve_storage(config)?, state.epoch)
        };
        let candidate = NativeWorkerPool::new(manager, storage.jobs, storage.voices)?;
        let mut state = self.lock()?;
        if state.epoch != epoch || state.loading || self.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("原生设置已改变，请重新开始操作"));
        }
        Ok(state.pool.get_or_insert(candidate).clone())
    }

    pub(crate) fn current_voice(&self) -> Result<Option<VoicePreset>, AppError> {
        let state = self.lock()?;
        let config = state.config.as_ref().map_err(Clone::clone)?;
        Ok(config.voice_for_profile(&state.profile_id).cloned())
    }

    pub(crate) fn health(&self) -> ComponentHealth {
        let state = match self.state.try_lock() {
            Ok(state) => state,
            Err(_) => return ComponentHealth::unavailable("native", "原生设置正在变更"),
        };
        if self.closed.load(Ordering::Acquire) {
            return ComponentHealth::unavailable("native", "原生运行已停止");
        }
        if let Ok(managed) = self.managed.try_lock()
            && let Some(error) = &managed.last_error
        {
            return ComponentHealth::unavailable("native", error.message.clone());
        }
        if let Some(error) = &state.error {
            return ComponentHealth::unavailable("native", error.message.clone());
        }
        let config = match &state.config {
            Ok(config) => config,
            Err(error) => return ComponentHealth::unavailable("native", error.message.clone()),
        };
        if !state.configured {
            return ComponentHealth::unavailable("native", "请先保存原生设置并准备模型");
        }
        if state.loading {
            return ComponentHealth::unavailable("native", "本地模型缓存正在校验");
        }
        let Some(dialogue) = &config.dialogue else {
            return ComponentHealth::unavailable("native", "本地对话未启用");
        };
        let ready = state
            .manager
            .as_ref()
            .and_then(|manager| manager.snapshot().ok())
            .is_some_and(|packages| {
                packages.iter().any(|package| {
                    package.id == dialogue.model_id
                        && matches!(
                            package.state,
                            ai_ex_model_manager::PackageState::Ready
                                | ai_ex_model_manager::PackageState::InUse
                        )
                })
            });
        if ready {
            ComponentHealth {
                component: "native".into(),
                ready: true,
                detail: "本地对话文件已准备；首次使用在后台加载".into(),
            }
        } else {
            ComponentHealth::unavailable("native", "本地对话尚未准备，外形与设置仍可使用")
        }
    }

    pub(crate) async fn wait_ready(&self, timeout: Duration) -> Result<(), AppError> {
        tokio::time::timeout(timeout, async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if !self.lock()?.loading {
                    return self.manager().map(|_| ());
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| AppError::unavailable("等待本地模型目录校验超时"))?
    }

    pub(super) fn launch_bootstrap(
        self: &Arc<Self>,
        old_manager: Option<ModelManager>,
        old_pool: Option<NativeWorkerPool>,
    ) {
        let runtime = self.clone();
        let task = tokio::spawn(async move {
            let result = runtime.open_manager(old_manager, old_pool).await;
            let mut discarded = None;
            if let Ok(mut state) = runtime.lock() {
                state.loading = false;
                match result {
                    Ok(manager) if !runtime.closed.load(Ordering::Acquire) => {
                        state.manager = Some(manager);
                        state.error = None;
                    }
                    Ok(manager) => discarded = Some(manager),
                    Err(error) => state.error = Some(error),
                }
            }
            if let Some(manager) = discarded {
                let _cleanup = manager.close().await;
            }
            runtime.changed.notify_waiters();
        });
        match self.bootstrap.lock() {
            Ok(mut bootstrap) => *bootstrap = Some(task),
            Err(_) => {
                task.abort();
                if let Ok(mut state) = self.lock() {
                    state.loading = false;
                    state.error = Some(AppError::unavailable("原生目录任务不可用"));
                }
            }
        }
    }

    async fn open_manager(
        &self,
        old_manager: Option<ModelManager>,
        old_pool: Option<NativeWorkerPool>,
    ) -> Result<ModelManager, AppError> {
        let mut failure = None;
        if let Some(pool) = old_pool
            && let Err(error) = pool.shutdown().await
        {
            failure = Some(error);
        }
        if let Some(manager) = old_manager
            && let Err(error) = manager.close().await
        {
            failure.get_or_insert(error);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let config = self.config()?;
        self.validate_config(&config)?;
        let storage = self.store()?.resolve_storage(&config)?;
        let catalog = self
            .catalog
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .clone();
        ModelManager::open_cancellable(storage.packages, catalog, self.bootstrap_cancel.clone())
            .await
    }

    pub(crate) async fn shutdown(&self) -> Result<(), AppError> {
        self.closed.store(true, Ordering::Release);
        self.bootstrap_cancel.store(true, Ordering::Release);
        let _mutation = self.mutation.lock().await;
        let mut failure = None;
        if let Err(error) = self.stop_preview().await {
            failure = Some(error);
        }
        if let Err(error) = self.operations.shutdown().await {
            failure.get_or_insert(error);
        }
        let bootstrap = self
            .bootstrap
            .lock()
            .map_err(|_| AppError::unavailable("原生目录任务不可用"))?
            .take();
        if let Some(task) = bootstrap
            && let Err(error) = task.await
        {
            failure
                .get_or_insert_with(|| AppError::unavailable(format!("原生目录任务退出：{error}")));
        }
        if let Err(error) = self.clear_dialogue().await {
            failure.get_or_insert(error);
        }
        let (pool, manager) = {
            let mut state = self.lock()?;
            (state.pool.take(), state.manager.take())
        };
        if let Some(pool) = pool
            && let Err(error) = pool.shutdown().await
        {
            failure.get_or_insert(error);
        }
        if let Some(manager) = manager
            && let Err(error) = manager.close().await
        {
            failure.get_or_insert(error);
        }
        // No retained sender may keep the service speech receiver alive.
        self.preview
            .lock()
            .map_err(|_| AppError::unavailable("试听上下文不可用"))?
            .take();
        failure.map_or(Ok(()), Err)
    }
}
