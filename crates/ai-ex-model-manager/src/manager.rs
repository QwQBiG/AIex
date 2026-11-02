use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ai_ex_domain::AppError;
use ai_ex_model_catalog::{Artifact, ModelBackend, ModelCapability, RuntimeRequirement};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{DistributionCatalog, ModelPackage, PackageSource, RuntimeManifest, RuntimePackage};

mod jobs;
mod state;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageKind {
    Model,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageState {
    NotPrepared,
    Downloading,
    Verifying,
    Ready,
    InUse,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationPhase {
    Downloading,
    Importing,
    Verifying,
    Committing,
    Removing,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}

impl OperationPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationSnapshot {
    pub id: Uuid,
    pub package_id: String,
    pub phase: OperationPhase,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSnapshot {
    pub id: String,
    pub name: String,
    pub revision: String,
    pub kind: PackageKind,
    pub backend: Option<ModelBackend>,
    pub capabilities: Vec<ModelCapability>,
    pub runtimes: Vec<RuntimeRequirement>,
    pub state: PackageState,
    pub installed: bool,
    pub bytes: u64,
    pub active_operation: Option<Uuid>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct ModelManager {
    inner: Arc<Inner>,
}

pub struct PackageLease {
    inner: Arc<Inner>,
    ids: Vec<String>,
    package: Package,
}

struct Inner {
    root: PathBuf,
    packages: BTreeMap<String, Package>,
    state: Mutex<State>,
    changed: Notify,
    lock_file: Mutex<Option<File>>,
    closed: AtomicBool,
    close_gate: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    slots: BTreeMap<String, Slot>,
    operations: BTreeMap<Uuid, Operation>,
}

#[derive(Default)]
struct Slot {
    installed: bool,
    busy: Option<Uuid>,
    leases: usize,
    error: Option<String>,
}

struct Operation {
    snapshot: OperationSnapshot,
    cancel: Arc<Cancellation>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Default)]
pub(crate) struct Cancellation {
    requested: AtomicBool,
    wake: Notify,
}

impl Cancellation {
    pub(crate) fn check(&self) -> Result<(), AppError> {
        if self.requested.load(Ordering::Acquire) {
            Err(AppError::invalid_transition("model preparation cancelled"))
        } else {
            Ok(())
        }
    }
    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.wake.notified();
            if self.requested.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
    fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "manifest",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum InstalledManifest {
    Model(ai_ex_model_catalog::ModelManifest),
    Runtime(RuntimeManifest),
}

#[derive(Clone)]
pub(crate) enum Package {
    Model(ModelPackage),
    Runtime(RuntimePackage),
}

impl Package {
    fn id(&self) -> &str {
        match self {
            Self::Model(p) => &p.manifest.id,
            Self::Runtime(p) => &p.manifest.id,
        }
    }
    fn revision(&self) -> &str {
        match self {
            Self::Model(p) => &p.manifest.revision,
            Self::Runtime(p) => &p.manifest.revision,
        }
    }
    fn kind(&self) -> PackageKind {
        match self {
            Self::Model(_) => PackageKind::Model,
            Self::Runtime(_) => PackageKind::Runtime,
        }
    }
    pub(crate) fn artifacts(&self) -> &[Artifact] {
        match self {
            Self::Model(p) => &p.manifest.artifacts,
            Self::Runtime(p) => &p.manifest.artifacts,
        }
    }
    pub(crate) fn source(&self) -> &PackageSource {
        match self {
            Self::Model(p) => &p.source,
            Self::Runtime(p) => &p.source,
        }
    }
    fn directory(&self, root: &Path) -> PathBuf {
        root.join(self.relative_directory())
    }
    fn relative_directory(&self) -> String {
        format!(
            "{}/{}/{}",
            if self.kind() == PackageKind::Model {
                "models"
            } else {
                "runtimes"
            },
            self.id(),
            self.revision()
        )
    }
    fn installed_manifest(&self) -> InstalledManifest {
        match self {
            Self::Model(p) => InstalledManifest::Model(p.manifest.clone()),
            Self::Runtime(p) => InstalledManifest::Runtime(p.manifest.clone()),
        }
    }
    fn total_bytes(&self) -> u64 {
        self.artifacts().iter().map(|file| file.bytes).sum()
    }
}

#[derive(Clone)]
pub(crate) struct TaskContext {
    inner: Arc<Inner>,
    id: Uuid,
    pub(crate) cancel: Arc<Cancellation>,
}

#[cfg(test)]
mod tests;
