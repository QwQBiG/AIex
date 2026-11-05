//! Service-owned native configuration, package operations and model lifetimes.

mod control;
mod dialogue;
mod lifecycle;
mod operations;
mod packages;
mod preview;
mod settings;
mod storage_space;
mod voices;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ai_ex_config::native::{NativeConfig, NativeConfigStore};
use ai_ex_domain::AppError;
use ai_ex_model_manager::{DistributionCatalog, ModelManager};
use ai_ex_native_models::NativeWorkerPool;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;

pub(crate) use dialogue::NativeLanguageModel;

pub(crate) struct NativeRuntime {
    store: Result<NativeConfigStore, AppError>,
    configuration_path: PathBuf,
    catalog: Result<Arc<DistributionCatalog>, AppError>,
    state: Mutex<State>,
    mutation: AsyncMutex<()>,
    managed: AsyncMutex<dialogue::ManagedState>,
    operations: operations::Operations,
    preview: Mutex<Option<preview::Resources>>,
    bootstrap: Mutex<Option<JoinHandle<()>>>,
    bootstrap_cancel: Arc<AtomicBool>,
    changed: Notify,
    closed: AtomicBool,
}

struct State {
    configured: bool,
    config: Result<NativeConfig, AppError>,
    profile_id: String,
    epoch: u64,
    loading: bool,
    manager: Option<ModelManager>,
    pool: Option<NativeWorkerPool>,
    error: Option<AppError>,
    preview: Option<preview::Active>,
    preview_generation: u64,
}

impl NativeRuntime {
    pub(crate) fn start(path: PathBuf, profile_id: String) -> Arc<Self> {
        let catalog = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/native-catalog.json"
        )))
        .map_err(|error| AppError::configuration(format!("native catalog: {error}")));
        Self::start_inner(path, profile_id, catalog)
    }

    fn start_inner(
        path: PathBuf,
        profile_id: String,
        catalog: Result<DistributionCatalog, AppError>,
    ) -> Arc<Self> {
        let store = NativeConfigStore::new(&path);
        let configuration_path = store
            .as_ref()
            .map(|store| store.path().to_owned())
            .unwrap_or(path);
        let configured = configuration_path.is_file();
        let config = if configured {
            store
                .as_ref()
                .map_err(Clone::clone)
                .and_then(NativeConfigStore::load)
        } else {
            Ok(NativeConfig::default())
        };
        let catalog = catalog.and_then(|catalog| {
            catalog.validate()?;
            Ok(Arc::new(catalog))
        });
        let runtime = Arc::new(Self {
            store,
            configuration_path,
            catalog,
            state: Mutex::new(State {
                configured,
                config,
                profile_id,
                epoch: 1,
                loading: true,
                manager: None,
                pool: None,
                error: None,
                preview: None,
                // A new service must not accept a former instance's queued
                // preview even before its first stop. Keep counter headroom.
                preview_generation: uuid::Uuid::new_v4().as_u64_pair().1 >> 1,
            }),
            mutation: AsyncMutex::new(()),
            managed: AsyncMutex::new(dialogue::ManagedState::default()),
            operations: operations::Operations::default(),
            preview: Mutex::new(None),
            bootstrap: Mutex::new(None),
            bootstrap_cancel: Arc::new(AtomicBool::new(false)),
            changed: Notify::new(),
            closed: AtomicBool::new(false),
        });
        runtime.launch_bootstrap(None, None);
        runtime
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, AppError> {
        self.state
            .lock()
            .map_err(|_| AppError::unavailable("native runtime state is unavailable"))
    }

    fn store(&self) -> Result<&NativeConfigStore, AppError> {
        self.store.as_ref().map_err(Clone::clone)
    }

    pub(crate) fn config(&self) -> Result<NativeConfig, AppError> {
        self.lock()?.config.clone()
    }

    pub(crate) fn manager(&self) -> Result<ModelManager, AppError> {
        let state = self.lock()?;
        if state.loading {
            return Err(AppError::unavailable("本地模型缓存正在校验，请稍后刷新"));
        }
        state.manager.clone().ok_or_else(|| {
            state
                .error
                .clone()
                .unwrap_or_else(|| AppError::unavailable("本地模型管理尚未就绪"))
        })
    }

    pub(crate) fn select_profile(&self, profile_id: &str) -> Result<(), AppError> {
        if profile_id.trim().is_empty() || profile_id.chars().count() > 128 {
            return Err(AppError::configuration(
                "native profile ID is outside persona bounds",
            ));
        }
        self.lock()?.profile_id = profile_id.to_owned();
        Ok(())
    }
}
