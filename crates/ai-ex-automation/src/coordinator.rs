use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ai_ex_domain::AppError;
use ai_ex_safety::{ActionRequest, Capability, SafetyGate};
use uuid::Uuid;

use crate::{
    AuditRecord, AuditSink, AuditStage, AutomationAction, AutomationPort, ExecutionFailure,
    ExecutionPhase, ExecutionReceipt,
};

const GUARD_REJECTION_DETAIL: &str = "automation dispatch guard rejected action";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardedExecutionFailure<E> {
    Execution(ExecutionFailure),
    GuardRejected {
        action_id: Uuid,
        error: E,
        rejection_audit_persisted: bool,
    },
}

pub struct AutomationCoordinator<P, A> {
    safety: Arc<SafetyGate>,
    port: P,
    audit: A,
}

impl<P, A> AutomationCoordinator<P, A>
where
    P: AutomationPort,
    A: AuditSink,
{
    pub fn new(safety: Arc<SafetyGate>, port: P, audit: A) -> Self {
        Self {
            safety,
            port,
            audit,
        }
    }

    /// Execute without an additional session guard.
    pub async fn execute(
        &mut self,
        target: impl Into<String>,
        rationale: impl Into<String>,
        action: AutomationAction,
    ) -> Result<ExecutionReceipt, ExecutionFailure> {
        match self
            .execute_guarded(target, rationale, action, || Ok::<(), Infallible>(()))
            .await
        {
            Ok(receipt) => Ok(receipt),
            Err(GuardedExecutionFailure::Execution(error)) => Err(error),
            Err(GuardedExecutionFailure::GuardRejected { error, .. }) => match error {},
        }
    }

    /// Check a caller-owned session before requesting and immediately before dispatching.
    ///
    /// A successful final check admits the adapter call; it cannot cancel an action
    /// that has already been dispatched or make external input atomic with the guard.
    pub async fn execute_guarded<F, E>(
        &mut self,
        target: impl Into<String>,
        rationale: impl Into<String>,
        action: AutomationAction,
        mut guard: F,
    ) -> Result<ExecutionReceipt, GuardedExecutionFailure<E>>
    where
        F: FnMut() -> Result<(), E>,
    {
        let action_id = Uuid::new_v4();
        let target = target.into();
        let rationale = rationale.into();
        let capability = action.required_capability();
        let label = action.audit_label();
        if let Err(error) = action.validate() {
            let _ignored = self
                .record(
                    action_id,
                    AuditStage::Rejected,
                    capability,
                    &target,
                    &label,
                    &error.message,
                )
                .await;
            return Err(GuardedExecutionFailure::Execution(failure(
                action_id,
                ExecutionPhase::BeforeExecution,
                error,
            )));
        }
        if let Err(error) = guard() {
            return Err(self
                .reject_guard(action_id, capability, &target, &label, error)
                .await);
        }
        if let Err(error) = self
            .record(
                action_id,
                AuditStage::Requested,
                capability,
                &target,
                &label,
                &rationale,
            )
            .await
        {
            return Err(GuardedExecutionFailure::Execution(failure(
                action_id,
                ExecutionPhase::BeforeExecution,
                error,
            )));
        }
        let permit = match self.safety.authorize(ActionRequest {
            capability,
            target: target.clone(),
            rationale,
        }) {
            Ok(permit) => permit,
            Err(error) => {
                let _ignored = self
                    .record(
                        action_id,
                        AuditStage::Rejected,
                        capability,
                        &target,
                        &label,
                        &error.message,
                    )
                    .await;
                return Err(GuardedExecutionFailure::Execution(failure(
                    action_id,
                    ExecutionPhase::BeforeExecution,
                    error,
                )));
            }
        };
        if let Err(error) = self
            .record(
                action_id,
                AuditStage::Authorized,
                capability,
                &target,
                &label,
                "permit granted",
            )
            .await
        {
            return Err(GuardedExecutionFailure::Execution(failure(
                action_id,
                ExecutionPhase::BeforeExecution,
                error,
            )));
        }
        if let Err(error) = permit.ensure_active() {
            let _ignored = self
                .record(
                    action_id,
                    AuditStage::Rejected,
                    capability,
                    &target,
                    &label,
                    &error.message,
                )
                .await;
            return Err(GuardedExecutionFailure::Execution(failure(
                action_id,
                ExecutionPhase::BeforeExecution,
                error,
            )));
        }
        if let Err(error) = guard() {
            return Err(self
                .reject_guard(action_id, capability, &target, &label, error)
                .await);
        }
        let result = match self.port.execute(&permit, &action).await {
            Ok(result) => result,
            Err(error) => {
                let _ignored = self
                    .record(
                        action_id,
                        AuditStage::Failed,
                        capability,
                        &target,
                        &label,
                        &error.message,
                    )
                    .await;
                return Err(GuardedExecutionFailure::Execution(failure(
                    action_id,
                    ExecutionPhase::DuringExecution,
                    error,
                )));
            }
        };
        let completion_audit_persisted = self
            .record(
                action_id,
                AuditStage::Completed,
                capability,
                &target,
                &label,
                "completed",
            )
            .await
            .is_ok();
        Ok(ExecutionReceipt {
            action_id,
            result,
            completion_audit_persisted,
        })
    }

    async fn reject_guard<E>(
        &mut self,
        action_id: Uuid,
        capability: Capability,
        target: &str,
        label: &str,
        error: E,
    ) -> GuardedExecutionFailure<E> {
        let rejection_audit_persisted = self
            .record(
                action_id,
                AuditStage::Rejected,
                capability,
                target,
                label,
                GUARD_REJECTION_DETAIL,
            )
            .await
            .is_ok();
        GuardedExecutionFailure::GuardRejected {
            action_id,
            error,
            rejection_audit_persisted,
        }
    }

    async fn record(
        &mut self,
        action_id: Uuid,
        stage: AuditStage,
        capability: Capability,
        target: &str,
        action: &str,
        detail: &str,
    ) -> Result<(), AppError> {
        self.audit
            .record(AuditRecord {
                action_id,
                timestamp_ms: timestamp_ms(),
                stage,
                capability,
                target: target.to_owned(),
                action: action.to_owned(),
                detail: detail.to_owned(),
            })
            .await
    }
}

fn failure(action_id: Uuid, phase: ExecutionPhase, error: AppError) -> ExecutionFailure {
    ExecutionFailure {
        action_id,
        phase,
        error,
    }
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

#[cfg(test)]
mod guarded_tests {
    use std::collections::BTreeSet;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    };

    use ai_ex_safety::{Permit, SafetyPolicy};
    use async_trait::async_trait;

    use super::*;
    use crate::{ActionResult, PointerButton};

    // Deliberately has no Debug or Display implementation.
    struct OpaqueGuardError(u8);

    struct LeaseState {
        generation: AtomicU64,
        running: AtomicBool,
    }

    struct Port {
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    #[async_trait]
    impl AutomationPort for Port {
        async fn execute(
            &mut self,
            permit: &Permit,
            _action: &AutomationAction,
        ) -> Result<ActionResult, AppError> {
            permit.ensure_active()?;
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(AppError::unavailable("injected adapter failure"))
            } else {
                Ok(ActionResult::Completed)
            }
        }
    }

    struct Audit {
        records: Arc<Mutex<Vec<AuditRecord>>>,
        lease: Arc<LeaseState>,
        pause_resume_at: Option<AuditStage>,
        fail_at: Option<AuditStage>,
    }

    #[async_trait]
    impl AuditSink for Audit {
        async fn record(&mut self, record: AuditRecord) -> Result<(), AppError> {
            if self.pause_resume_at == Some(record.stage) {
                self.lease.running.store(false, Ordering::SeqCst);
                self.lease.generation.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                self.lease.generation.fetch_add(1, Ordering::SeqCst);
                self.lease.running.store(true, Ordering::SeqCst);
            }
            if self.fail_at == Some(record.stage) {
                return Err(AppError::unavailable("injected audit failure"));
            }
            self.records.lock().unwrap().push(record);
            Ok(())
        }
    }

    struct Harness {
        coordinator: AutomationCoordinator<Port, Audit>,
        calls: Arc<AtomicUsize>,
        records: Arc<Mutex<Vec<AuditRecord>>>,
        lease: Arc<LeaseState>,
    }

    fn harness(
        pause_resume_at: Option<AuditStage>,
        fail_at: Option<AuditStage>,
        fail_port: bool,
    ) -> Harness {
        let calls = Arc::new(AtomicUsize::new(0));
        let records = Arc::new(Mutex::new(Vec::new()));
        let lease = Arc::new(LeaseState {
            generation: AtomicU64::new(0),
            running: AtomicBool::new(true),
        });
        let safety = Arc::new(SafetyGate::new(SafetyPolicy {
            automation_enabled: true,
            allowed_capabilities: BTreeSet::from([Capability::MouseInput]),
            allowed_targets: vec!["game".to_owned()],
        }));
        Harness {
            coordinator: AutomationCoordinator::new(
                safety,
                Port {
                    calls: Arc::clone(&calls),
                    fail: fail_port,
                },
                Audit {
                    records: Arc::clone(&records),
                    lease: Arc::clone(&lease),
                    pause_resume_at,
                    fail_at,
                },
            ),
            calls,
            records,
            lease,
        }
    }

    fn action() -> AutomationAction {
        AutomationAction::Click {
            button: PointerButton::Left,
        }
    }

    fn stages(harness: &Harness) -> Vec<AuditStage> {
        harness
            .records
            .lock()
            .unwrap()
            .iter()
            .map(|record| record.stage)
            .collect()
    }

    #[tokio::test]
    async fn initial_guard_rejection_is_typed_and_audited_without_requesting() {
        let mut harness = harness(None, None, false);
        let failure = harness
            .coordinator
            .execute_guarded("game", "inspect", action(), || Err(OpaqueGuardError(7)))
            .await
            .expect_err("initial guard must reject");
        let GuardedExecutionFailure::GuardRejected {
            action_id,
            error,
            rejection_audit_persisted,
        } = failure
        else {
            panic!("guard rejection must retain its typed error");
        };
        assert_eq!(error.0, 7);
        assert!(rejection_audit_persisted);
        assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
        let records = harness.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].stage, AuditStage::Rejected);
        assert_eq!(records[0].action_id, action_id);
        assert_eq!(records[0].detail, GUARD_REJECTION_DETAIL);
    }

    async fn assert_pause_resume_rejected(stage: AuditStage) {
        let mut harness = harness(Some(stage), None, false);
        let lease = Arc::clone(&harness.lease);
        let mut checks = 0;
        let failure = harness
            .coordinator
            .execute_guarded("game", "inspect", action(), || {
                checks += 1;
                if lease.running.load(Ordering::SeqCst)
                    && lease.generation.load(Ordering::SeqCst) == 0
                {
                    Ok(())
                } else {
                    Err(OpaqueGuardError(9))
                }
            })
            .await
            .expect_err("pause and resume must invalidate the original lease");
        assert!(matches!(
            failure,
            GuardedExecutionFailure::GuardRejected {
                error: OpaqueGuardError(9),
                rejection_audit_persisted: true,
                ..
            }
        ));
        assert!(harness.lease.running.load(Ordering::SeqCst));
        assert_eq!(harness.lease.generation.load(Ordering::SeqCst), 2);
        assert_eq!(checks, 2);
        assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            stages(&harness),
            vec![
                AuditStage::Requested,
                AuditStage::Authorized,
                AuditStage::Rejected
            ]
        );
    }

    #[tokio::test]
    async fn final_guard_rejects_pause_resume_during_requested_audit() {
        assert_pause_resume_rejected(AuditStage::Requested).await;
    }

    #[tokio::test]
    async fn final_guard_rejects_pause_resume_during_authorized_audit() {
        assert_pause_resume_rejected(AuditStage::Authorized).await;
    }

    #[tokio::test]
    async fn unchanged_guard_preserves_the_three_success_audit_stages() {
        let mut harness = harness(None, None, false);
        let mut checks = 0;
        let receipt = harness
            .coordinator
            .execute_guarded("game", "inspect", action(), || {
                checks += 1;
                Ok::<(), OpaqueGuardError>(())
            })
            .await
            .unwrap_or_else(|_| panic!("unchanged guard should allow dispatch"));
        assert_eq!(checks, 2);
        assert!(receipt.completion_audit_persisted);
        assert_eq!(harness.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            stages(&harness),
            vec![
                AuditStage::Requested,
                AuditStage::Authorized,
                AuditStage::Completed
            ]
        );
        assert!(
            harness
                .records
                .lock()
                .unwrap()
                .iter()
                .all(|record| record.action_id == receipt.action_id)
        );
    }

    #[tokio::test]
    async fn pre_dispatch_audit_failure_remains_an_execution_failure() {
        for stage in [AuditStage::Requested, AuditStage::Authorized] {
            let mut harness = harness(Some(stage), Some(stage), false);
            let mut checks = 0;
            let failure = harness
                .coordinator
                .execute_guarded("game", "inspect", action(), || {
                    checks += 1;
                    Ok::<(), OpaqueGuardError>(())
                })
                .await
                .expect_err("failed audit must prevent dispatch");
            let GuardedExecutionFailure::Execution(failure) = failure else {
                panic!("audit failure must not become a guard rejection");
            };
            assert_eq!(failure.phase, ExecutionPhase::BeforeExecution);
            assert_eq!(failure.error.kind, ai_ex_domain::ErrorKind::Unavailable);
            assert_eq!(checks, 1);
            assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn adapter_failure_remains_during_execution_without_safe_retry() {
        let mut harness = harness(None, None, true);
        let failure = harness
            .coordinator
            .execute_guarded("game", "inspect", action(), || {
                Ok::<(), OpaqueGuardError>(())
            })
            .await
            .expect_err("adapter must fail");
        let GuardedExecutionFailure::Execution(failure) = failure else {
            panic!("adapter failure must remain an execution failure");
        };
        assert_eq!(failure.phase, ExecutionPhase::DuringExecution);
        assert!(!failure.retry_is_safe());
        assert_eq!(harness.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            stages(&harness),
            vec![
                AuditStage::Requested,
                AuditStage::Authorized,
                AuditStage::Failed
            ]
        );
    }

    #[tokio::test]
    async fn guard_rejection_reports_when_its_audit_did_not_persist() {
        for reject_initially in [true, false] {
            let mut harness = harness(None, Some(AuditStage::Rejected), false);
            let mut checks = 0;
            let failure = harness
                .coordinator
                .execute_guarded("game", "inspect", action(), || {
                    checks += 1;
                    if reject_initially || checks == 2 {
                        Err(OpaqueGuardError(11))
                    } else {
                        Ok(())
                    }
                })
                .await
                .expect_err("guard must reject even if rejection audit fails");
            assert!(matches!(
                failure,
                GuardedExecutionFailure::GuardRejected {
                    error: OpaqueGuardError(11),
                    rejection_audit_persisted: false,
                    ..
                }
            ));
            assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                stages(&harness),
                if reject_initially {
                    vec![]
                } else {
                    vec![AuditStage::Requested, AuditStage::Authorized]
                }
            );
        }
    }

    #[tokio::test]
    async fn invalid_action_keeps_validation_failure_before_guard_or_request() {
        let mut harness = harness(None, None, false);
        let mut checks = 0;
        let failure = harness
            .coordinator
            .execute_guarded(
                "game",
                "inspect",
                AutomationAction::MovePointer { x: -0.1, y: 0.5 },
                || {
                    checks += 1;
                    Ok::<(), OpaqueGuardError>(())
                },
            )
            .await
            .expect_err("invalid coordinates must be rejected");
        assert!(matches!(
            failure,
            GuardedExecutionFailure::Execution(ExecutionFailure {
                phase: ExecutionPhase::BeforeExecution,
                ..
            })
        ));
        assert_eq!(checks, 0);
        assert_eq!(harness.calls.load(Ordering::SeqCst), 0);
        assert_eq!(stages(&harness), vec![AuditStage::Rejected]);
    }
}
