//! Explicit whole-store copies with service-owned, non-replayed operations.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ai_ex_control::{ControlPayload, RawMemoryBackupState, RawMemoryBackupStatus, SensitivePath};
use ai_ex_domain::AppError;
use ai_ex_memory::MemoryStore;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::ServiceControl;

const PLAN_LIFETIME: Duration = Duration::from_secs(300);
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
const MAX_PLANS: usize = 16;
const MAX_OPERATIONS: usize = 32;

#[derive(Default)]
pub(crate) struct RawBackups {
    records: Mutex<Records>,
    changed: Notify,
}

#[derive(Default)]
struct Records {
    closed: bool,
    plans: HashMap<Uuid, Plan>,
    operations: HashMap<Uuid, Operation>,
}

struct Plan {
    instance_id: Uuid,
    operation_id: Uuid,
    output: SensitivePath,
    expires: Instant,
}

struct Operation {
    plan_id: Uuid,
    status: RawMemoryBackupStatus,
}

fn unavailable() -> AppError {
    AppError::unavailable("raw memory backup unavailable")
}

impl RawBackups {
    fn prepare(
        &self,
        instance_id: Uuid,
        operation_id: Uuid,
        output: SensitivePath,
    ) -> Result<ControlPayload, AppError> {
        if operation_id.get_version_num() != 4 {
            return Err(AppError::configuration(
                "raw memory backup requires a random operation ID",
            ));
        }
        let mut records = self.records.lock().map_err(|_| unavailable())?;
        if records.closed {
            return Err(unavailable());
        }
        records
            .plans
            .retain(|_, plan| plan.expires > Instant::now());
        if records.operations.contains_key(&operation_id) {
            return Err(AppError::invalid_transition(
                "raw memory backup ID is already in use",
            ));
        }
        if records
            .plans
            .values()
            .any(|plan| plan.operation_id == operation_id)
        {
            return Err(AppError::invalid_transition(
                "raw memory backup ID already has a plan",
            ));
        }
        if records.plans.len() >= MAX_PLANS {
            return Err(unavailable());
        }
        let plan_id = Uuid::new_v4();
        records.plans.insert(
            plan_id,
            Plan {
                instance_id,
                operation_id,
                output: output.clone(),
                expires: Instant::now() + PLAN_LIFETIME,
            },
        );
        Ok(ControlPayload::RawMemoryBackupPlan {
            instance_id,
            operation_id,
            plan_id,
            expires_in_seconds: PLAN_LIFETIME.as_secs(),
            output,
        })
    }

    fn admit(
        &self,
        instance_id: Uuid,
        operation_id: Uuid,
        plan_id: Uuid,
    ) -> Result<(RawMemoryBackupStatus, bool), AppError> {
        let mut records = self.records.lock().map_err(|_| unavailable())?;
        if records.closed {
            return Err(unavailable());
        }
        if let Some(existing) = records.operations.get(&operation_id) {
            if existing.plan_id != plan_id || existing.status.instance_id != instance_id {
                return Err(AppError::invalid_transition(
                    "raw memory backup ID is already in use",
                ));
            }
            return Ok((existing.status.clone(), false));
        }
        let plan = records.plans.get(&plan_id).ok_or_else(unavailable)?;
        if plan.expires <= Instant::now()
            || plan.instance_id != instance_id
            || plan.operation_id != operation_id
        {
            return Err(AppError::invalid_transition(
                "raw memory backup plan is no longer valid",
            ));
        }
        let output = plan.output.clone();
        if records.operations.len() >= MAX_OPERATIONS {
            return Ok((
                RawMemoryBackupStatus {
                    instance_id,
                    operation_id,
                    output: Some(output),
                    state: RawMemoryBackupState::CapacityReached,
                },
                false,
            ));
        }
        let status = RawMemoryBackupStatus {
            instance_id,
            operation_id,
            output: Some(output),
            state: RawMemoryBackupState::Running,
        };
        records.plans.remove(&plan_id);
        records.operations.insert(
            operation_id,
            Operation {
                plan_id,
                status: status.clone(),
            },
        );
        Ok((status, true))
    }

    fn status(
        &self,
        instance_id: Uuid,
        operation_id: Uuid,
    ) -> Result<RawMemoryBackupStatus, AppError> {
        let records = self.records.lock().map_err(|_| unavailable())?;
        Ok(records
            .operations
            .get(&operation_id)
            .map(|operation| operation.status.clone())
            .unwrap_or(RawMemoryBackupStatus {
                instance_id,
                operation_id,
                output: None,
                state: RawMemoryBackupState::Unknown,
            }))
    }

    fn finish(&self, operation_id: Uuid, state: RawMemoryBackupState) {
        if let Ok(mut records) = self.records.lock()
            && let Some(operation) = records.operations.get_mut(&operation_id)
            && operation.status.state == RawMemoryBackupState::Running
        {
            operation.status.state = state;
        }
        self.changed.notify_one();
    }

    pub(crate) fn close(&self) -> Result<(), AppError> {
        let mut records = self.records.lock().map_err(|_| unavailable())?;
        records.closed = true;
        records.plans.clear();
        Ok(())
    }

    pub(crate) async fn shutdown(&self) -> Result<(), AppError> {
        self.close()?;
        tokio::time::timeout(SHUTDOWN_WAIT, async {
            loop {
                let running = self
                    .records
                    .lock()
                    .map_err(|_| unavailable())?
                    .operations
                    .values()
                    .any(|operation| operation.status.state == RawMemoryBackupState::Running);
                if !running {
                    return Ok(());
                }
                self.changed.notified().await;
            }
        })
        .await
        .map_err(|_| {
            AppError::unavailable("raw memory backup is still running; outcome is unknown")
        })?
    }
}

impl ServiceControl {
    pub(super) fn prepare_raw_backup(
        &self,
        instance_id: Uuid,
        operation_id: Uuid,
    ) -> Result<ControlPayload, AppError> {
        self.check_raw_instance(instance_id)?;
        let output = self.raw_backup_memory.raw_backup_target(operation_id)?;
        let output = output.to_str().ok_or_else(unavailable)?;
        if output.len() > 2048
            || serde_json::to_vec(output).map_or(true, |frame| frame.len() > 8192)
        {
            return Err(AppError::configuration(
                "raw memory backup path exceeds response limit",
            ));
        }
        self.raw_backups
            .prepare(instance_id, operation_id, SensitivePath::new(output))
    }

    pub(super) fn start_raw_backup(
        &self,
        instance_id: Uuid,
        operation_id: Uuid,
        plan_id: Uuid,
    ) -> Result<ControlPayload, AppError> {
        self.check_raw_instance(instance_id)?;
        let (status, start) = self.raw_backups.admit(instance_id, operation_id, plan_id)?;
        if start {
            let operations = Arc::clone(&self.raw_backups);
            let memory: MemoryStore = self.raw_backup_memory.clone();
            let expected = status
                .output
                .as_ref()
                .ok_or_else(unavailable)?
                .as_str()
                .to_owned();
            tokio::spawn(async move {
                let state = match memory
                    .backup_raw_copy_planned(operation_id, std::path::Path::new(&expected))
                    .await
                {
                    Ok((_, temporary_retained)) => {
                        RawMemoryBackupState::Written { temporary_retained }
                    }
                    Err(_) => RawMemoryBackupState::Unknown,
                };
                operations.finish(operation_id, state);
            });
        }
        Ok(ControlPayload::RawMemoryBackupStatus(status))
    }

    pub(super) fn raw_backup_status(
        &self,
        instance_id: Uuid,
        operation_id: Uuid,
    ) -> Result<ControlPayload, AppError> {
        self.check_raw_instance(instance_id)?;
        Ok(ControlPayload::RawMemoryBackupStatus(
            self.raw_backups.status(instance_id, operation_id)?,
        ))
    }

    fn check_raw_instance(&self, instance_id: Uuid) -> Result<(), AppError> {
        if self.events.current().instance_id != Some(instance_id) || instance_id.is_nil() {
            return Err(AppError::invalid_transition("service instance changed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare(manager: &RawBackups, instance_id: Uuid, operation_id: Uuid) -> Uuid {
        let output = SensitivePath::new("synthetic-private-path");
        let ControlPayload::RawMemoryBackupPlan { plan_id, .. } =
            manager.prepare(instance_id, operation_id, output).unwrap()
        else {
            panic!("plan expected")
        };
        plan_id
    }

    #[test]
    fn plan_is_single_use_and_operation_never_replays() {
        let manager = RawBackups::default();
        let instance_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        let plan_id = prepare(&manager, instance_id, operation_id);
        let (initial, start) = manager.admit(instance_id, operation_id, plan_id).unwrap();
        assert!(start);
        assert_eq!(initial.state, RawMemoryBackupState::Running);
        let (duplicate, start) = manager.admit(instance_id, operation_id, plan_id).unwrap();
        assert!(!start);
        assert_eq!(initial, duplicate);
        assert!(
            manager
                .admit(instance_id, operation_id, Uuid::new_v4())
                .is_err()
        );
        assert!(manager.admit(instance_id, Uuid::new_v4(), plan_id).is_err());
        manager.finish(
            operation_id,
            RawMemoryBackupState::Written {
                temporary_retained: false,
            },
        );
        manager.finish(operation_id, RawMemoryBackupState::Unknown);
        assert!(matches!(
            manager.status(instance_id, operation_id).unwrap().state,
            RawMemoryBackupState::Written {
                temporary_retained: false
            }
        ));
        assert!(
            manager
                .prepare(instance_id, operation_id, SensitivePath::new("another"))
                .is_err()
        );
    }

    #[test]
    fn expired_plan_and_closed_manager_never_start() {
        let manager = RawBackups::default();
        let instance_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        let plan_id = prepare(&manager, instance_id, operation_id);
        manager
            .records
            .lock()
            .unwrap()
            .plans
            .get_mut(&plan_id)
            .unwrap()
            .expires = Instant::now();
        assert!(manager.admit(instance_id, operation_id, plan_id).is_err());
        assert_eq!(
            manager.status(instance_id, operation_id).unwrap().state,
            RawMemoryBackupState::Unknown
        );
        manager.close().unwrap();
        assert!(
            manager
                .prepare(instance_id, Uuid::new_v4(), SensitivePath::new("path"))
                .is_err()
        );
    }

    #[test]
    fn full_operation_table_rejects_without_forgetting_previous_ids() {
        let manager = RawBackups::default();
        let instance_id = Uuid::new_v4();
        let mut first = None;
        for _ in 0..MAX_OPERATIONS {
            let operation_id = Uuid::new_v4();
            let plan_id = prepare(&manager, instance_id, operation_id);
            let (_, start) = manager.admit(instance_id, operation_id, plan_id).unwrap();
            assert!(start);
            manager.finish(operation_id, RawMemoryBackupState::Unknown);
            first.get_or_insert((operation_id, plan_id));
        }
        let operation_id = Uuid::new_v4();
        let plan_id = prepare(&manager, instance_id, operation_id);
        let (status, start) = manager.admit(instance_id, operation_id, plan_id).unwrap();
        assert!(!start);
        assert_eq!(status.state, RawMemoryBackupState::CapacityReached);
        let (old_id, old_plan) = first.unwrap();
        assert!(!manager.admit(instance_id, old_id, old_plan).unwrap().1);
    }

    #[tokio::test]
    async fn shutdown_rejects_new_work_and_waits_for_running_operation() {
        let manager = Arc::new(RawBackups::default());
        let instance_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        let plan_id = prepare(&manager, instance_id, operation_id);
        assert!(manager.admit(instance_id, operation_id, plan_id).unwrap().1);
        let worker = Arc::clone(&manager);
        let shutdown = tokio::spawn(async move { worker.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        manager.finish(operation_id, RawMemoryBackupState::Unknown);
        shutdown.await.unwrap().unwrap();
        assert!(manager.admit(instance_id, operation_id, plan_id).is_err());
    }
}
