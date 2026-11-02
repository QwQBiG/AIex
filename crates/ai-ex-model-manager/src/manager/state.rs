use std::fs::{self, OpenOptions};
use std::sync::MutexGuard;

use super::*;
use crate::files::{self, io_error};

type OpenCheck = Arc<dyn Fn() -> Result<(), AppError> + Send + Sync>;

impl ModelManager {
    pub async fn open(
        root: impl AsRef<Path>,
        catalog: DistributionCatalog,
    ) -> Result<Self, AppError> {
        Self::open_cancellable(root, catalog, Arc::new(AtomicBool::new(false))).await
    }

    /// Cancellation is observed between 64 KiB hash reads. The caller awaits
    /// completion so no verification task outlives the directory ownership lock.
    pub async fn open_cancellable(
        root: impl AsRef<Path>,
        catalog: DistributionCatalog,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, AppError> {
        catalog.validate()?;
        Self::open_checked(
            root.as_ref(),
            catalog,
            Arc::new(move || {
                if cancelled.load(Ordering::Acquire) {
                    Err(AppError::invalid_transition(
                        "model cache verification cancelled",
                    ))
                } else {
                    Ok(())
                }
            }),
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn open_validated(
        root: &Path,
        catalog: DistributionCatalog,
    ) -> Result<Self, AppError> {
        Self::open_checked(root, catalog, Arc::new(|| Ok(()))).await
    }

    pub(super) async fn open_checked(
        root: &Path,
        catalog: DistributionCatalog,
        check: OpenCheck,
    ) -> Result<Self, AppError> {
        check()?;
        fs::create_dir_all(root).map_err(io_error)?;
        if files::is_link(&fs::symlink_metadata(root).map_err(io_error)?) {
            return Err(io_error("model root is a link"));
        }
        let root = fs::canonicalize(root).map_err(io_error)?;
        let lock_path = root.join("manager.lock");
        if lock_path.try_exists().map_err(io_error)? {
            files::checked_path(&root, "manager.lock")?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(io_error)?;
        lock.try_lock()
            .map_err(|_| io_error("model directory is already managed by another service"))?;
        for directory in ["models", "runtimes", ".staging", ".retired"] {
            files::create_directory(&root, directory)?;
        }
        let packages: BTreeMap<_, _> = catalog
            .runtimes
            .into_iter()
            .map(Package::Runtime)
            .chain(catalog.models.into_iter().map(Package::Model))
            .map(|package| (package.id().to_owned(), package))
            .collect();
        let state = State {
            slots: packages
                .keys()
                .map(|id| (id.clone(), Slot::default()))
                .collect(),
            operations: BTreeMap::new(),
        };
        let manager = Self {
            inner: Arc::new(Inner {
                root,
                packages,
                state: Mutex::new(state),
                changed: Notify::new(),
                lock_file: Mutex::new(Some(lock)),
                closed: AtomicBool::new(false),
                close_gate: tokio::sync::Mutex::new(()),
            }),
        };
        // Runtime requirements are reconciled before model readiness is exposed.
        for kind in [PackageKind::Runtime, PackageKind::Model] {
            let packages: Vec<_> = manager
                .inner
                .packages
                .values()
                .filter(|p| p.kind() == kind)
                .cloned()
                .collect();
            for package in packages {
                check()?;
                let directory = package.directory(&manager.inner.root);
                if !directory.try_exists().map_err(io_error)? {
                    continue;
                }
                let checked = package.clone();
                let root = manager.inner.root.clone();
                let verify_check = check.clone();
                let result = tokio::task::spawn_blocking(move || {
                    files::checked_path(&root, &checked.relative_directory())?;
                    jobs::verify_installed(&checked, &directory, verify_check.as_ref())
                })
                .await
                .map_err(io_error)?;
                check()?;
                let mut state = manager.inner.lock()?;
                let slot = state
                    .slots
                    .get_mut(package.id())
                    .expect("catalog slot exists");
                match result {
                    Ok(()) => slot.installed = true,
                    Err(error) => slot.error = Some(error.to_string()),
                }
            }
        }
        check()?;
        Ok(manager)
    }

    pub fn snapshot(&self) -> Result<Vec<PackageSnapshot>, AppError> {
        let state = self.inner.lock()?;
        Ok(self
            .inner
            .packages
            .values()
            .map(|package| self.inner.snapshot(package, &state))
            .collect())
    }

    pub fn operation(&self, id: Uuid) -> Result<OperationSnapshot, AppError> {
        self.inner
            .lock()?
            .operations
            .get(&id)
            .map(|operation| operation.snapshot.clone())
            .ok_or_else(|| AppError::invalid_transition("unknown model operation"))
    }

    pub fn cancel(&self, id: Uuid) -> Result<bool, AppError> {
        let mut state = self.inner.lock()?;
        let operation = state
            .operations
            .get_mut(&id)
            .ok_or_else(|| AppError::invalid_transition("unknown model operation"))?;
        if operation.snapshot.phase.is_terminal()
            || operation.snapshot.phase == OperationPhase::Committing
        {
            return Ok(false);
        }
        operation.snapshot.phase = OperationPhase::Cancelling;
        operation.cancel.request();
        self.inner.changed.notify_waiters();
        Ok(true)
    }

    pub async fn wait(&self, id: Uuid) -> Result<OperationSnapshot, AppError> {
        loop {
            let notified = self.inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let operation = self.operation(id)?;
            if operation.phase.is_terminal() {
                return Ok(operation);
            }
            notified.await;
        }
    }

    pub fn acquire(&self, id: &str) -> Result<PackageLease, AppError> {
        let package = self
            .inner
            .packages
            .get(id)
            .ok_or_else(|| AppError::configuration("unknown package"))?
            .clone();
        let mut state = self.inner.lock()?;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(AppError::unavailable("model storage manager is closed"));
        }
        if !self.inner.ready(&package, &state) {
            return Err(AppError::unavailable("package or its runtime is not ready"));
        }
        let mut ids = vec![id.to_owned()];
        if let Package::Model(model) = &package {
            ids.extend(
                model
                    .manifest
                    .runtimes
                    .iter()
                    .map(|requirement| requirement.id.clone()),
            );
        }
        for id in &ids {
            state.slots.get_mut(id).expect("catalog slot exists").leases += 1;
        }
        Ok(PackageLease {
            inner: self.inner.clone(),
            ids,
            package,
        })
    }

    pub fn catalog(&self) -> DistributionCatalog {
        DistributionCatalog {
            schema_version: 1,
            models: self
                .inner
                .packages
                .values()
                .filter_map(|p| {
                    if let Package::Model(model) = p {
                        Some(model.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            runtimes: self
                .inner
                .packages
                .values()
                .filter_map(|p| {
                    if let Package::Runtime(runtime) = p {
                        Some(runtime.clone())
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }
}

impl Inner {
    pub(super) fn lock(&self) -> Result<MutexGuard<'_, State>, AppError> {
        self.state
            .lock()
            .map_err(|_| io_error("model state is unavailable"))
    }

    fn ready(&self, package: &Package, state: &State) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let available = |id: &str| {
            state
                .slots
                .get(id)
                .is_some_and(|slot| slot.installed && slot.busy.is_none() && slot.error.is_none())
        };
        available(package.id())
            && match package {
                Package::Runtime(_) => true,
                Package::Model(model) => model
                    .manifest
                    .runtimes
                    .iter()
                    .all(|required| available(&required.id)),
            }
    }

    fn snapshot(&self, package: &Package, state: &State) -> PackageSnapshot {
        let slot = &state.slots[package.id()];
        let operation = slot.busy.and_then(|id| state.operations.get(&id));
        let ready = self.ready(package, state);
        let status = if let Some(operation) = operation {
            match operation.snapshot.phase {
                OperationPhase::Verifying | OperationPhase::Committing => PackageState::Verifying,
                _ => PackageState::Downloading,
            }
        } else if slot.error.is_some() {
            PackageState::Error
        } else if slot.leases > 0 {
            PackageState::InUse
        } else if ready {
            PackageState::Ready
        } else {
            PackageState::NotPrepared
        };
        let (name, backend, capabilities, runtimes) = match package {
            Package::Model(model) => (
                model.name.clone(),
                Some(model.manifest.backend),
                model.manifest.capabilities.clone(),
                model.manifest.runtimes.clone(),
            ),
            Package::Runtime(runtime) => (runtime.name.clone(), None, Vec::new(), Vec::new()),
        };
        PackageSnapshot {
            id: package.id().to_owned(),
            name,
            revision: package.revision().to_owned(),
            kind: package.kind(),
            backend,
            capabilities,
            runtimes,
            state: status,
            installed: slot.installed,
            bytes: package.total_bytes(),
            active_operation: slot.busy,
            error: slot.error.clone().or_else(|| {
                (slot.installed && !ready && operation.is_none())
                    .then(|| "required native runtime is not prepared".to_owned())
            }),
        }
    }
}

impl PackageLease {
    /// Common verified-package root. Model and runtime artifact paths can be
    /// prefixed relative to this root without copying large weights into jobs.
    pub fn root_directory(&self) -> &Path {
        &self.inner.root
    }

    pub fn id(&self) -> &str {
        self.package.id()
    }
    pub fn data_directory(&self) -> PathBuf {
        self.package.directory(&self.inner.root).join("data")
    }
    pub fn model_manifest(&self) -> Option<&ai_ex_model_catalog::ModelManifest> {
        if let Package::Model(model) = &self.package {
            Some(&model.manifest)
        } else {
            None
        }
    }
    pub fn runtime_manifest(&self) -> Option<&RuntimeManifest> {
        if let Package::Runtime(runtime) = &self.package {
            Some(&runtime.manifest)
        } else {
            None
        }
    }
    pub fn required_runtime(&self, id: &str) -> Option<(&RuntimeManifest, PathBuf)> {
        if !self.ids.iter().any(|leased| leased == id) {
            return None;
        }
        match self.inner.packages.get(id)? {
            Package::Runtime(runtime) => Some((
                &runtime.manifest,
                self.inner.packages[id]
                    .directory(&self.inner.root)
                    .join("data"),
            )),
            _ => None,
        }
    }
}

impl Drop for PackageLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.lock() {
            for id in &self.ids {
                let slot = state
                    .slots
                    .get_mut(id)
                    .expect("leased package is registered");
                slot.leases -= 1;
            }
        }
    }
}

impl TaskContext {
    pub(crate) fn progress(&self, bytes: u64) -> Result<(), AppError> {
        let mut state = self.inner.lock()?;
        self.cancel.check()?;
        let operation = state
            .operations
            .get_mut(&self.id)
            .expect("active operation exists");
        operation.snapshot.completed_bytes = operation
            .snapshot
            .completed_bytes
            .saturating_add(bytes)
            .min(operation.snapshot.total_bytes);
        self.inner.changed.notify_waiters();
        Ok(())
    }

    pub(crate) fn phase(&self, phase: OperationPhase) -> Result<(), AppError> {
        let mut state = self.inner.lock()?;
        self.cancel.check()?;
        state
            .operations
            .get_mut(&self.id)
            .expect("active operation exists")
            .snapshot
            .phase = phase;
        self.inner.changed.notify_waiters();
        Ok(())
    }
}
