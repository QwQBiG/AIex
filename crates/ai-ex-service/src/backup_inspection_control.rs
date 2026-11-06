//! Read-only control for explicitly selected backup files.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ai_ex_control::{
    ControlPayload, LegacyMemoryBackupInspection, MemoryBackupInspection, SensitivePath,
};
use ai_ex_domain::AppError;
use uuid::Uuid;

use crate::{ServiceControl, backup_inspection_worker::Request};

const INSPECTION_DEADLINE: Duration = Duration::from_secs(4);

impl ServiceControl {
    pub(super) async fn memory_inspection_context(&self) -> Result<ControlPayload, AppError> {
        let (instance_id, profile_id, persona_epoch) = self.inspection_context().await?;
        Ok(ControlPayload::MemoryInspectionContext {
            instance_id,
            profile_id,
            persona_epoch,
        })
    }

    pub(super) async fn inspect_memory_backup(
        &self,
        instance_id: Uuid,
        profile_id: String,
        persona_epoch: u64,
        path: SensitivePath,
    ) -> Result<ControlPayload, AppError> {
        let inspector = self.backup_inspector.as_ref().map_err(Clone::clone)?;
        self.inspect_memory_backup_with(instance_id, profile_id, persona_epoch, path, |request| {
            inspector
                .start(request, INSPECTION_DEADLINE)
                .map(|pending| async move { pending.wait().await })
        })
        .await
    }

    pub(crate) async fn inspect_memory_backup_with<F, Fut>(
        &self,
        instance_id: Uuid,
        profile_id: String,
        persona_epoch: u64,
        path: SensitivePath,
        inspect: F,
    ) -> Result<ControlPayload, AppError>
    where
        F: FnOnce(Request) -> Result<Fut, AppError>,
        Fut: std::future::Future<Output = Result<ai_ex_memory::BackupInspection, AppError>>,
    {
        let (expected, summary) = self
            .inspect_backup_with_context(
                instance_id,
                profile_id,
                persona_epoch,
                path,
                false,
                inspect,
            )
            .await?;
        Ok(ControlPayload::MemoryBackupInspection {
            instance_id: expected.0,
            profile_id: expected.1,
            persona_epoch: expected.2,
            summary: MemoryBackupInspection {
                memory_records: summary.memory_records,
                entity_candidates: summary.entity_candidates,
                source_associations: summary.source_associations,
                inactive_source_associations: summary.inactive_source_associations,
                embedded_records: summary.embedded_records,
                distinct_embedding_spaces: summary.distinct_embedding_spaces,
                matching_embeddings: summary.matching_embeddings,
            },
        })
    }

    pub(super) async fn inspect_legacy_memory_backup(
        &self,
        instance_id: Uuid,
        profile_id: String,
        persona_epoch: u64,
        path: SensitivePath,
    ) -> Result<ControlPayload, AppError> {
        let inspector = self.backup_inspector.as_ref().map_err(Clone::clone)?;
        self.inspect_legacy_memory_backup_with(
            instance_id,
            profile_id,
            persona_epoch,
            path,
            |request| {
                inspector
                    .start(request, INSPECTION_DEADLINE)
                    .map(|pending| async move { pending.wait_legacy().await })
            },
        )
        .await
    }

    pub(crate) async fn inspect_legacy_memory_backup_with<F, Fut>(
        &self,
        instance_id: Uuid,
        profile_id: String,
        persona_epoch: u64,
        path: SensitivePath,
        inspect: F,
    ) -> Result<ControlPayload, AppError>
    where
        F: FnOnce(Request) -> Result<Fut, AppError>,
        Fut: std::future::Future<Output = Result<LegacyMemoryBackupInspection, AppError>>,
    {
        let (expected, summary) = self
            .inspect_backup_with_context(
                instance_id,
                profile_id,
                persona_epoch,
                path,
                true,
                inspect,
            )
            .await?;
        Ok(ControlPayload::LegacyMemoryBackupInspection {
            instance_id: expected.0,
            profile_id: expected.1,
            persona_epoch: expected.2,
            summary,
        })
    }

    async fn inspect_backup_with_context<F, Fut, T>(
        &self,
        instance_id: Uuid,
        profile_id: String,
        persona_epoch: u64,
        path: SensitivePath,
        legacy: bool,
        inspect: F,
    ) -> Result<((Uuid, String, u64), T), AppError>
    where
        F: FnOnce(Request) -> Result<Fut, AppError>,
        Fut: std::future::Future<Output = Result<T, AppError>>,
    {
        let path = path.as_str();
        if path.is_empty()
            || path.chars().count() > 1024
            || path.chars().any(char::is_control)
            || !Path::new(path).is_absolute()
        {
            return Err(AppError::configuration("invalid backup inspection path"));
        }
        let expected = (instance_id, profile_id, persona_epoch);
        let persona = self.persona.read().await;
        let current_instance = self.events.current().instance_id;
        if current_instance != Some(expected.0)
            || persona.profile_id != expected.1
            || self.persona_epoch.load(Ordering::Acquire) != expected.2
        {
            return Err(AppError::invalid_transition(
                "persona or service instance changed",
            ));
        }
        // Admit the bounded task while the persona read lock still protects the
        // identity. No filesystem access or process wait is performed here.
        let pending = inspect(Request {
            path: path.to_owned(),
            profile: expected.1.clone(),
            // No current model identity is sent to this independent worker.
            comparison_space: None,
            legacy,
        })?;
        drop(persona);
        let summary = pending.await?;
        if self.inspection_context().await? != expected {
            return Err(AppError::invalid_transition(
                "persona or service instance changed",
            ));
        }
        Ok((expected, summary))
    }

    async fn inspection_context(&self) -> Result<(Uuid, String, u64), AppError> {
        let persona = self.persona.read().await;
        let instance = self
            .events
            .current()
            .instance_id
            .ok_or_else(|| AppError::unavailable("service instance identity is unavailable"))?;
        Ok((
            instance,
            persona.profile_id.clone(),
            self.persona_epoch.load(Ordering::Acquire),
        ))
    }
}
