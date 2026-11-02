use ai_ex_config::native::{ModelSelection, NativeBackend};
use ai_ex_domain::{
    NativeLicense, NativeOperation, NativeOperationPhase, NativePackage, NativePackageKind,
    NativePackageState, NativeReply,
};
use ai_ex_model_catalog::ModelBackend;
use ai_ex_model_manager::{OperationPhase, OperationSnapshot, PackageKind, PackageState};

use super::*;

impl NativeRuntime {
    pub(super) fn packages(&self) -> Result<Vec<NativePackage>, AppError> {
        let catalog = self.catalog.as_ref().map_err(Clone::clone)?;
        let (manager, loading, failure) = {
            let state = self.lock()?;
            (state.manager.clone(), state.loading, state.error.clone())
        };
        let snapshots = manager
            .as_ref()
            .map(ModelManager::snapshot)
            .transpose()?
            .unwrap_or_default();
        let entries = catalog
            .models
            .iter()
            .map(|model| {
                (
                    &model.manifest.id,
                    &model.name,
                    &model.manifest.revision,
                    PackageKind::Model,
                    &model.manifest.artifacts,
                    &model.manifest.licenses,
                    model
                        .manifest
                        .runtimes
                        .iter()
                        .map(|required| required.id.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .chain(catalog.runtimes.iter().map(|runtime| {
                (
                    &runtime.manifest.id,
                    &runtime.name,
                    &runtime.manifest.revision,
                    PackageKind::Runtime,
                    &runtime.manifest.artifacts,
                    &runtime.manifest.licenses,
                    Vec::new(),
                )
            }));
        Ok(entries
            .map(
                |(id, name, revision, kind, artifacts, licenses, required)| {
                    let snapshot = snapshots.iter().find(|snapshot| snapshot.id == *id);
                    NativePackage {
                        id: id.clone(),
                        name: name.clone(),
                        revision: revision.clone(),
                        kind: if kind == PackageKind::Model {
                            NativePackageKind::Model
                        } else {
                            NativePackageKind::Runtime
                        },
                        state: snapshot
                            .map(|snapshot| package_state(snapshot.state))
                            .unwrap_or(if loading {
                                NativePackageState::Verifying
                            } else {
                                NativePackageState::Error
                            }),
                        bytes: artifacts.iter().map(|file| file.bytes).sum(),
                        installed: snapshot.is_some_and(|snapshot| snapshot.installed),
                        active_operation: snapshot.and_then(|snapshot| snapshot.active_operation),
                        error: snapshot
                            .and_then(|snapshot| snapshot.error.clone())
                            .or_else(|| {
                                failure
                                    .as_ref()
                                    .map(|error| operations::bounded(error.message.clone()))
                            }),
                        licenses: licenses
                            .iter()
                            .map(|license| NativeLicense {
                                id: license.id.clone(),
                                source_url: license.source_url.clone(),
                            })
                            .collect(),
                        required_packages: required,
                    }
                },
            )
            .collect())
    }

    pub(super) async fn remove_package(&self, id: &str) -> Result<NativeReply, AppError> {
        let _mutation = self.mutation.lock().await;
        let manager = self.manager()?;
        let catalog = self.catalog.as_ref().map_err(Clone::clone)?;
        let runtime = catalog
            .runtimes
            .iter()
            .any(|runtime| runtime.manifest.id == id);
        let model = catalog.models.iter().find(|model| model.manifest.id == id);
        if runtime || model.is_some_and(|model| model.manifest.backend == ModelBackend::Ollama) {
            self.ensure_dialogue_idle().await?;
            self.clear_dialogue().await?;
        }
        let stops_preview = self.lock()?.preview.as_ref().is_some_and(|preview| {
            preview.selection.model_id == id || preview.selection.runtime_id == id
        });
        if stops_preview {
            self.stop_preview().await?;
        }
        let pool = self.lock()?.pool.clone();
        if let Some(pool) = pool {
            for selected in catalog.models.iter().filter(|candidate| {
                candidate.manifest.backend != ModelBackend::Ollama
                    && (candidate.manifest.id == id
                        || (runtime
                            && candidate
                                .manifest
                                .runtimes
                                .iter()
                                .any(|required| required.id == id)))
            }) {
                for required in &selected.manifest.runtimes {
                    pool.unload(&ModelSelection {
                        backend: backend(selected.manifest.backend),
                        model_id: selected.manifest.id.clone(),
                        runtime_id: required.id.clone(),
                    })
                    .await?;
                }
            }
        }
        let id = manager.remove(id).await?;
        Ok(NativeReply::Operation(operation(manager.operation(id)?)))
    }
}

fn package_state(state: PackageState) -> NativePackageState {
    match state {
        PackageState::NotPrepared => NativePackageState::NotPrepared,
        PackageState::Downloading => NativePackageState::Downloading,
        PackageState::Verifying => NativePackageState::Verifying,
        PackageState::Ready => NativePackageState::Ready,
        PackageState::InUse => NativePackageState::InUse,
        PackageState::Error => NativePackageState::Error,
    }
}

pub(super) fn operation(snapshot: OperationSnapshot) -> NativeOperation {
    NativeOperation {
        id: snapshot.id,
        package_id: snapshot.package_id,
        completed_bytes: snapshot.completed_bytes,
        total_bytes: snapshot.total_bytes,
        error: snapshot.error.map(operations::bounded),
        phase: match snapshot.phase {
            OperationPhase::Downloading => NativeOperationPhase::Downloading,
            OperationPhase::Importing => NativeOperationPhase::Importing,
            OperationPhase::Verifying => NativeOperationPhase::Verifying,
            OperationPhase::Committing => NativeOperationPhase::Committing,
            OperationPhase::Removing => NativeOperationPhase::Removing,
            OperationPhase::Cancelling => NativeOperationPhase::Cancelling,
            OperationPhase::Completed => NativeOperationPhase::Completed,
            OperationPhase::Cancelled => NativeOperationPhase::Cancelled,
            OperationPhase::Failed => NativeOperationPhase::Failed,
        },
    }
}

pub(super) fn backend(value: ModelBackend) -> NativeBackend {
    match value {
        ModelBackend::Ollama => NativeBackend::Ollama,
        ModelBackend::SherpaWhisper => NativeBackend::SherpaWhisper,
        ModelBackend::SherpaKokoro => NativeBackend::SherpaKokoro,
        ModelBackend::Qwen3TtsOnnx => NativeBackend::Qwen3TtsOnnx,
        ModelBackend::OnnxEmbedding => NativeBackend::OnnxEmbedding,
    }
}
