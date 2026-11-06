//! Explicit, review-only merge candidates; never opens the active memory store.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ai_ex_control::{
    ControlPayload, LegacyCandidateState, LegacyCandidateStatus, MergeCandidatePreviewResult,
    SensitivePath,
};
use ai_ex_domain::AppError;
use uuid::Uuid;

use crate::ServiceControl;
use crate::backup_inspection_coordinator::{CandidateOutcome, MergePreviewOutcome};
use crate::backup_inspection_worker::{MAX_FRAME, MergeRequest, MergeWrite};

const PREVIEW_LIFETIME: Duration = Duration::from_secs(300);
const PREVIEW_DEADLINE: Duration = Duration::from_secs(4);
const WRITE_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct MergeCandidates {
    records: Mutex<Records>,
}

#[derive(Default)]
struct Records {
    previews: HashMap<Uuid, Preview>,
    operations: HashMap<Uuid, Operation>,
}

struct Preview {
    instance_id: Uuid,
    profile_id: String,
    persona_epoch: u64,
    base_path: String,
    incoming_path: String,
    base_digest: [u8; 32],
    incoming_digest: [u8; 32],
    expires: Instant,
}

struct Operation {
    request: StartMergeCandidate,
    status: LegacyCandidateStatus,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StartMergeCandidate {
    pub instance_id: Uuid,
    pub profile_id: String,
    pub persona_epoch: u64,
    pub preview_id: Uuid,
    pub operation_id: Uuid,
    pub output_directory: SensitivePath,
}

fn unavailable() -> AppError {
    AppError::unavailable("merge candidate operation unavailable")
}

fn valid_path(path: &str, limit: usize) -> bool {
    !path.is_empty()
        && path.chars().count() <= limit
        && !path.chars().any(char::is_control)
        && Path::new(path).is_absolute()
}

impl MergeCandidates {
    fn insert_preview(&self, preview: Preview) -> Result<Uuid, AppError> {
        let mut records = self.records.lock().map_err(|_| unavailable())?;
        records
            .previews
            .retain(|_, item| item.expires > Instant::now());
        if records.previews.len() >= 16 {
            return Err(unavailable());
        }
        let id = Uuid::new_v4();
        records.previews.insert(id, preview);
        Ok(id)
    }

    fn status(
        &self,
        instance_id: Uuid,
        operation_id: Uuid,
    ) -> Result<LegacyCandidateStatus, AppError> {
        let records = self.records.lock().map_err(|_| unavailable())?;
        Ok(records
            .operations
            .get(&operation_id)
            .map(|item| item.status.clone())
            .unwrap_or(LegacyCandidateStatus {
                instance_id,
                operation_id,
                profile_id: None,
                output: None,
                state: LegacyCandidateState::Unknown,
            }))
    }

    fn finish(&self, operation_id: Uuid, state: LegacyCandidateState) {
        if let Ok(mut records) = self.records.lock()
            && let Some(operation) = records.operations.get_mut(&operation_id)
        {
            operation.status.state = state;
        }
    }

    fn admit(
        &self,
        start: &StartMergeCandidate,
    ) -> Result<(LegacyCandidateStatus, Option<MergeRequest>), AppError> {
        if start.operation_id.get_version_num() != 4
            || !valid_path(start.output_directory.as_str(), 900)
        {
            return Err(AppError::configuration("invalid merge candidate options"));
        }
        let mut records = self.records.lock().map_err(|_| unavailable())?;
        if let Some(existing) = records.operations.get(&start.operation_id) {
            if existing.request != *start {
                return Err(AppError::invalid_transition(
                    "merge operation ID is already in use",
                ));
            }
            return Ok((existing.status.clone(), None));
        }
        let output = Path::new(start.output_directory.as_str())
            .join(format!("merge-candidate-{}.jsonl", start.operation_id));
        let output = output.to_str().ok_or_else(unavailable)?.to_owned();
        if !valid_path(&output, 1024) {
            return Err(AppError::configuration("invalid merge candidate output"));
        }
        if records.operations.len() >= 32 {
            return Ok((
                LegacyCandidateStatus {
                    instance_id: start.instance_id,
                    operation_id: start.operation_id,
                    profile_id: Some(start.profile_id.clone()),
                    output: Some(SensitivePath::new(output)),
                    state: LegacyCandidateState::CapacityReached,
                },
                None,
            ));
        }
        let preview = records
            .previews
            .get(&start.preview_id)
            .ok_or_else(unavailable)?;
        if preview.expires <= Instant::now()
            || preview.instance_id != start.instance_id
            || preview.profile_id != start.profile_id
            || preview.persona_epoch != start.persona_epoch
        {
            return Err(AppError::invalid_transition(
                "merge preview is no longer valid",
            ));
        }
        let request = MergeRequest {
            base_path: preview.base_path.clone(),
            incoming_path: preview.incoming_path.clone(),
            profile: preview.profile_id.clone(),
            write: Some(MergeWrite {
                base_digest: preview.base_digest,
                incoming_digest: preview.incoming_digest,
                output: output.clone(),
            }),
        };
        let frame = serde_json::to_vec(&request).map_err(|_| unavailable())?;
        if frame.is_empty() || frame.len() > MAX_FRAME {
            return Err(AppError::configuration(
                "merge candidate request exceeds the worker frame limit",
            ));
        }
        let status = LegacyCandidateStatus {
            instance_id: start.instance_id,
            operation_id: start.operation_id,
            profile_id: Some(start.profile_id.clone()),
            output: Some(SensitivePath::new(output)),
            state: LegacyCandidateState::Running,
        };
        records.previews.remove(&start.preview_id);
        records.operations.insert(
            start.operation_id,
            Operation {
                request: start.clone(),
                status: status.clone(),
            },
        );
        Ok((status, Some(request)))
    }
}

impl ServiceControl {
    pub(super) async fn prepare_merge_candidate(
        &self,
        instance_id: Uuid,
        profile_id: String,
        persona_epoch: u64,
        base_path: SensitivePath,
        incoming_path: SensitivePath,
    ) -> Result<ControlPayload, AppError> {
        if !valid_path(base_path.as_str(), 1024) || !valid_path(incoming_path.as_str(), 1024) {
            return Err(AppError::configuration("invalid backup inspection path"));
        }
        let inspector = self.backup_inspector.as_ref().map_err(Clone::clone)?;
        let persona = self.persona.read().await;
        if self.events.current().instance_id != Some(instance_id)
            || persona.profile_id != profile_id
            || self.persona_epoch.load(Ordering::Acquire) != persona_epoch
        {
            return Err(AppError::invalid_transition(
                "persona or service instance changed",
            ));
        }
        let pending = inspector.start(
            MergeRequest {
                base_path: base_path.as_str().to_owned(),
                incoming_path: incoming_path.as_str().to_owned(),
                profile: profile_id.clone(),
                write: None,
            },
            PREVIEW_DEADLINE,
        )?;
        drop(persona);
        let outcome = pending.wait_merge_preview().await?;
        let persona = self.persona.read().await;
        if self.events.current().instance_id != Some(instance_id)
            || persona.profile_id != profile_id
            || self.persona_epoch.load(Ordering::Acquire) != persona_epoch
        {
            return Err(AppError::invalid_transition(
                "persona or service instance changed",
            ));
        }
        let (preview_id, expires_in_seconds, result) = match outcome {
            MergePreviewOutcome::Ready {
                summary,
                base_digest,
                incoming_digest,
            } => {
                let preview_id = self.merge_candidates.insert_preview(Preview {
                    instance_id,
                    profile_id: profile_id.clone(),
                    persona_epoch,
                    base_path: base_path.as_str().to_owned(),
                    incoming_path: incoming_path.as_str().to_owned(),
                    base_digest,
                    incoming_digest,
                    expires: Instant::now() + PREVIEW_LIFETIME,
                })?;
                (
                    Some(preview_id),
                    PREVIEW_LIFETIME.as_secs(),
                    MergeCandidatePreviewResult::Ready { summary },
                )
            }
            MergePreviewOutcome::Conflict(reason) => {
                (None, 0, MergeCandidatePreviewResult::Conflict { reason })
            }
        };
        drop(persona);
        Ok(ControlPayload::MergeCandidatePreview {
            instance_id,
            profile_id,
            persona_epoch,
            preview_id,
            expires_in_seconds,
            result,
        })
    }

    pub(super) async fn start_merge_candidate(
        &self,
        start: StartMergeCandidate,
    ) -> Result<ControlPayload, AppError> {
        let inspector = self
            .backup_inspector
            .as_ref()
            .map_err(Clone::clone)?
            .clone();
        let persona = self.persona.read().await;
        if self.events.current().instance_id != Some(start.instance_id)
            || persona.profile_id != start.profile_id
            || self.persona_epoch.load(Ordering::Acquire) != start.persona_epoch
        {
            return Err(AppError::invalid_transition(
                "persona or service instance changed",
            ));
        }
        let (status, request) = self.merge_candidates.admit(&start)?;
        drop(persona);
        if let Some(request) = request {
            let operations = Arc::clone(&self.merge_candidates);
            let operation_id = start.operation_id;
            tokio::spawn(async move {
                let outcome = match inspector.start(request, WRITE_DEADLINE) {
                    Ok(pending) => pending.wait_candidate().await,
                    Err(error) => Err(error),
                };
                let state = match outcome {
                    Ok(CandidateOutcome::Written { temporary_retained }) => {
                        LegacyCandidateState::Written { temporary_retained }
                    }
                    Ok(CandidateOutcome::Rejected) => LegacyCandidateState::Rejected,
                    Ok(CandidateOutcome::Uncertain) | Err(_) => LegacyCandidateState::Unknown,
                };
                operations.finish(operation_id, state);
            });
        }
        Ok(ControlPayload::MergeCandidateStatus(status))
    }

    pub(super) async fn merge_candidate_status(
        &self,
        instance_id: Uuid,
        operation_id: Uuid,
    ) -> Result<ControlPayload, AppError> {
        if self.inspection_context().await?.0 != instance_id {
            return Err(AppError::invalid_transition("service instance changed"));
        }
        Ok(ControlPayload::MergeCandidateStatus(
            self.merge_candidates.status(instance_id, operation_id)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(instance_id: Uuid) -> Preview {
        let path = std::env::temp_dir().join("synthetic-backup.jsonl");
        Preview {
            instance_id,
            profile_id: "test".into(),
            persona_epoch: 4,
            base_path: path.to_str().unwrap().into(),
            incoming_path: path.to_str().unwrap().into(),
            base_digest: [1; 32],
            incoming_digest: [2; 32],
            expires: Instant::now() + Duration::from_secs(60),
        }
    }

    fn start(instance_id: Uuid, preview_id: Uuid) -> StartMergeCandidate {
        StartMergeCandidate {
            instance_id,
            profile_id: "test".into(),
            persona_epoch: 4,
            preview_id,
            operation_id: Uuid::new_v4(),
            output_directory: SensitivePath::new(std::env::temp_dir().to_string_lossy()),
        }
    }

    #[test]
    fn admission_consumes_preview_and_binds_full_request() {
        let manager = MergeCandidates::default();
        let instance_id = Uuid::new_v4();
        let preview_id = manager.insert_preview(preview(instance_id)).unwrap();
        let request = start(instance_id, preview_id);
        let (status, worker) = manager.admit(&request).unwrap();
        assert_eq!(status.state, LegacyCandidateState::Running);
        assert!(worker.is_some());
        let (_, replay) = manager.admit(&request).unwrap();
        assert!(replay.is_none());
        let mut altered = request.clone();
        altered.output_directory =
            SensitivePath::new(std::env::current_dir().unwrap().to_string_lossy());
        assert!(manager.admit(&altered).is_err());
        let mut second = request.clone();
        second.operation_id = Uuid::new_v4();
        assert!(manager.admit(&second).is_err());
        manager.finish(request.operation_id, LegacyCandidateState::Unknown);
        assert_eq!(
            manager
                .status(instance_id, request.operation_id)
                .unwrap()
                .state,
            LegacyCandidateState::Unknown
        );
    }

    #[test]
    fn expired_preview_cannot_start_or_produce_operation() {
        let manager = MergeCandidates::default();
        let instance_id = Uuid::new_v4();
        let preview_id = manager.insert_preview(preview(instance_id)).unwrap();
        manager
            .records
            .lock()
            .unwrap()
            .previews
            .get_mut(&preview_id)
            .unwrap()
            .expires = Instant::now();
        let request = start(instance_id, preview_id);
        assert!(manager.admit(&request).is_err());
        assert_eq!(
            manager
                .status(instance_id, request.operation_id)
                .unwrap()
                .state,
            LegacyCandidateState::Unknown
        );
    }

    #[test]
    fn oversized_encoded_request_does_not_consume_preview_or_admit_operation() {
        let manager = MergeCandidates::default();
        let instance_id = Uuid::new_v4();
        let mut candidate = preview(instance_id);
        let root = std::env::temp_dir();
        candidate.base_path = root.join("中".repeat(990)).to_string_lossy().into_owned();
        candidate.incoming_path = root.join("文".repeat(990)).to_string_lossy().into_owned();
        let preview_id = manager.insert_preview(candidate).unwrap();
        let mut request = start(instance_id, preview_id);
        request.output_directory =
            SensitivePath::new(root.join("字".repeat(800)).to_string_lossy());
        let Err(error) = manager.admit(&request) else {
            panic!("oversized request was accepted")
        };
        assert!(error.to_string().contains("worker frame limit"));
        let records = manager.records.lock().unwrap();
        assert!(records.previews.contains_key(&preview_id));
        assert!(!records.operations.contains_key(&request.operation_id));
    }
}
