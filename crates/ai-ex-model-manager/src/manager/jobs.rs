use std::fs;
use std::io::Read;
use std::time::Duration;

use super::*;
use crate::files::{self, io_error};

#[derive(Clone)]
enum Action {
    Download,
    Import(PathBuf),
    Remove,
}

impl ModelManager {
    pub async fn start_download(&self, id: &str) -> Result<Uuid, AppError> {
        self.begin(id, Action::Download)
    }
    pub async fn start_import(&self, id: &str, source: impl AsRef<Path>) -> Result<Uuid, AppError> {
        if !source.as_ref().is_absolute() {
            return Err(AppError::configuration(
                "offline input must be an absolute path",
            ));
        }
        self.begin(id, Action::Import(source.as_ref().to_path_buf()))
    }
    pub async fn remove(&self, id: &str) -> Result<Uuid, AppError> {
        self.begin(id, Action::Remove)
    }

    pub async fn shutdown(&self) -> Result<(), AppError> {
        let ids: Vec<_> = self
            .inner
            .lock()?
            .operations
            .values()
            .filter(|operation| !operation.snapshot.phase.is_terminal())
            .map(|operation| operation.snapshot.id)
            .collect();
        for id in &ids {
            self.cancel(*id)?;
        }
        let tasks: Vec<_> = self
            .inner
            .lock()?
            .operations
            .values_mut()
            .filter_map(|operation| operation.task.take())
            .collect();
        tokio::time::timeout(Duration::from_secs(30), async {
            for id in ids {
                self.wait(id).await?;
            }
            for task in tasks {
                task.await.map_err(io_error)?;
            }
            Ok::<_, AppError>(())
        })
        .await
        .map_err(|_| {
            AppError::unavailable("model preparation did not stop within its shutdown limit")
        })?
    }

    /// Terminal close allows storage to be reopened while stale read-only owners
    /// still exist. Their leases must be released before the OS lock is dropped.
    pub async fn close(&self) -> Result<(), AppError> {
        let _closing = self.inner.close_gate.lock().await;
        self.inner.closed.store(true, Ordering::Release);
        self.shutdown().await?;
        if self
            .inner
            .lock()?
            .slots
            .values()
            .any(|slot| slot.leases > 0)
        {
            return Err(AppError::invalid_transition(
                "native package leases must be released before closing storage",
            ));
        }
        self.inner
            .lock_file
            .lock()
            .map_err(|_| io_error("model storage lock is unavailable"))?
            .take();
        Ok(())
    }

    fn begin(&self, id: &str, action: Action) -> Result<Uuid, AppError> {
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
        let slot = &state.slots[id];
        if slot.busy.is_some() || slot.leases > 0 {
            return Err(AppError::invalid_transition("package is busy or in use"));
        }
        if state
            .operations
            .values()
            .filter(|operation| !operation.snapshot.phase.is_terminal())
            .count()
            >= 4
        {
            return Err(AppError::unavailable("model preparation queue is full"));
        }
        if state.operations.len() >= 64 {
            let completed: Vec<_> = state
                .operations
                .iter()
                .filter(|(_, op)| op.snapshot.phase.is_terminal())
                .take(32)
                .map(|(id, _)| *id)
                .collect();
            for id in completed {
                state.operations.remove(&id);
            }
        }
        let id = Uuid::new_v4();
        let cancel = Arc::new(Cancellation::default());
        let phase = match action {
            Action::Download => OperationPhase::Downloading,
            Action::Import(_) => OperationPhase::Importing,
            Action::Remove => OperationPhase::Removing,
        };
        state.operations.insert(
            id,
            Operation {
                snapshot: OperationSnapshot {
                    id,
                    package_id: package.id().to_owned(),
                    phase,
                    completed_bytes: 0,
                    total_bytes: package.total_bytes(),
                    error: None,
                },
                cancel: cancel.clone(),
                task: None,
            },
        );
        let slot = state
            .slots
            .get_mut(package.id())
            .expect("catalog slot exists");
        slot.busy = Some(id);
        slot.error = None;
        let context = TaskContext {
            inner: self.inner.clone(),
            id,
            cancel,
        };
        let task = tokio::spawn(async move {
            let result = execute(&context, &package, action).await;
            finish(&context, &package, result);
        });
        state
            .operations
            .get_mut(&id)
            .expect("active operation exists")
            .task = Some(task);
        Ok(id)
    }
}

async fn execute(context: &TaskContext, package: &Package, action: Action) -> Result<(), AppError> {
    context.cancel.check()?;
    if matches!(action, Action::Remove) {
        return remove(context, package).await;
    }
    if context.inner.lock()?.slots[package.id()].installed {
        return complete(context, package, true);
    }
    let staging =
        files::create_directory(&context.inner.root, &format!(".staging/{}", context.id))?;
    let data = files::create_directory(&staging, "data")?;
    let result = async {
        match action {
            Action::Download => {
                crate::transfer::download(context, package, &staging, &data).await?
            }
            Action::Import(source) => {
                let context = context.clone();
                let package = package.clone();
                let data = data.clone();
                tokio::task::spawn_blocking(move || {
                    crate::transfer::import(&context, &package, &source, &data)
                })
                .await
                .map_err(io_error)??;
            }
            Action::Remove => unreachable!(),
        }
        context.phase(OperationPhase::Verifying)?;
        let artifacts = package.artifacts().to_vec();
        let verify_data = data.clone();
        let cancel = context.cancel.clone();
        tokio::task::spawn_blocking(move || {
            files::verify_files(&artifacts, &verify_data, &|| cancel.check())
        })
        .await
        .map_err(io_error)??;
        context.cancel.check()?;
        files::write_json(
            &staging.join("manifest.json"),
            &package.installed_manifest(),
        )?;
        commit(context, package, &staging)
    }
    .await;
    if staging.exists() {
        // This unique directory was created by this operation, never a user input.
        fs::remove_dir_all(&staging).map_err(io_error)?;
    }
    result
}

pub(super) fn verify_installed(
    package: &Package,
    directory: &Path,
    check: &dyn Fn() -> Result<(), AppError>,
) -> Result<(), AppError> {
    check()?;
    let path = files::checked_path(directory, "manifest.json")?;
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes))
        .map_err(io_error)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(io_error("installed manifest exceeds limit"));
    }
    let actual: InstalledManifest = serde_json::from_slice(&bytes).map_err(io_error)?;
    if serde_json::to_value(actual).map_err(io_error)?
        != serde_json::to_value(package.installed_manifest()).map_err(io_error)?
    {
        return Err(io_error(
            "installed manifest differs from the fixed catalog",
        ));
    }
    let data = files::checked_path(directory, "data")?;
    files::verify_files(package.artifacts(), &data, check)
}

fn complete(context: &TaskContext, package: &Package, installed: bool) -> Result<(), AppError> {
    let mut state = context.inner.lock()?;
    context.cancel.check()?;
    complete_locked(context.id, package.id(), installed, &mut state);
    context.inner.changed.notify_waiters();
    Ok(())
}

fn complete_locked(id: Uuid, package_id: &str, installed: bool, state: &mut State) {
    let slot = state
        .slots
        .get_mut(package_id)
        .expect("catalog slot exists");
    slot.busy = None;
    slot.installed = installed;
    slot.error = None;
    let operation = state
        .operations
        .get_mut(&id)
        .expect("active operation exists");
    operation.snapshot.phase = OperationPhase::Completed;
    operation.snapshot.completed_bytes = operation.snapshot.total_bytes;
}

fn commit(context: &TaskContext, package: &Package, staging: &Path) -> Result<(), AppError> {
    let destination = package.directory(&context.inner.root);
    let parent = format!(
        "{}/{}",
        if package.kind() == PackageKind::Model {
            "models"
        } else {
            "runtimes"
        },
        package.id()
    );
    files::create_directory(&context.inner.root, &parent)?;
    let backup = context
        .inner
        .root
        .join(".retired")
        .join(context.id.to_string());
    let mut state = context.inner.lock()?;
    context.cancel.check()?;
    let existed = destination.try_exists().map_err(io_error)?;
    if existed {
        files::checked_path(&context.inner.root, &package.relative_directory())?;
        verify_shell(&destination)?;
        let data = files::checked_path(&destination, "data")?;
        files::verify_inventory(package.artifacts(), &data)?;
        fs::rename(&destination, &backup).map_err(io_error)?;
    }
    if let Err(error) = fs::rename(staging, &destination) {
        if existed {
            fs::rename(&backup, &destination).map_err(|rollback| {
                io_error(format!(
                    "install failed: {error}; restore failed: {rollback}"
                ))
            })?;
        }
        return Err(io_error(error));
    }
    complete_locked(context.id, package.id(), true, &mut state);
    context.inner.changed.notify_waiters();
    Ok(())
}

async fn remove(context: &TaskContext, package: &Package) -> Result<(), AppError> {
    let directory = package.directory(&context.inner.root);
    if !directory.try_exists().map_err(io_error)? {
        return complete(context, package, false);
    }
    files::checked_path(&context.inner.root, &package.relative_directory())?;
    verify_shell(&directory)?;
    let data = files::checked_path(&directory, "data")?;
    files::verify_inventory(package.artifacts(), &data)?;
    let retired = context
        .inner
        .root
        .join(".retired")
        .join(context.id.to_string());
    {
        let mut state = context.inner.lock()?;
        context.cancel.check()?;
        fs::rename(&directory, &retired).map_err(io_error)?;
        state
            .slots
            .get_mut(package.id())
            .expect("catalog slot exists")
            .installed = false;
        state
            .operations
            .get_mut(&context.id)
            .expect("active operation exists")
            .snapshot
            .phase = OperationPhase::Committing;
    }
    // The package no longer admits leases; irreversible cleanup is not cancelled.
    tokio::task::spawn_blocking(move || fs::remove_dir_all(retired))
        .await
        .map_err(io_error)?
        .map_err(io_error)?;
    complete(context, package, false)
}

fn verify_shell(directory: &Path) -> Result<(), AppError> {
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(io_error)?;
        if !matches!(entry.file_name().to_str(), Some("manifest.json" | "data"))
            || files::is_link(&metadata)
        {
            return Err(io_error(
                "installed package contains an undeclared top-level entry",
            ));
        }
    }
    Ok(())
}

pub(super) fn finish(context: &TaskContext, package: &Package, result: Result<(), AppError>) {
    if let Ok(mut state) = context.inner.lock() {
        let operation = state
            .operations
            .get_mut(&context.id)
            .expect("active operation exists");
        if operation.snapshot.phase.is_terminal() {
            return;
        }
        if let Err(error) = result {
            let cancelled = context.cancel.requested.load(Ordering::Acquire)
                && error.kind == ai_ex_domain::ErrorKind::InvalidTransition;
            operation.snapshot.phase = if cancelled {
                OperationPhase::Cancelled
            } else {
                OperationPhase::Failed
            };
            operation.snapshot.error = if cancelled {
                None
            } else {
                Some(error.to_string())
            };
            let slot = state
                .slots
                .get_mut(package.id())
                .expect("catalog slot exists");
            slot.busy = None;
            slot.error = if cancelled {
                None
            } else {
                Some(error.to_string())
            };
        }
        context.inner.changed.notify_waiters();
    }
}
