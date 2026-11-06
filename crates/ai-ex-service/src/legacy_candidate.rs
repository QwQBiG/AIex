//! Explicit, non-overwriting legacy backup candidates. No candidate is activated.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ai_ex_control::{
    ControlPayload, LegacyCandidateState, LegacyCandidateStatus, LegacyMemoryBackupInspection,
    SensitivePath,
};
use ai_ex_domain::AppError;
use uuid::Uuid;

use crate::ServiceControl;
use crate::backup_inspection_coordinator::CandidateOutcome;
use crate::backup_inspection_worker::{CandidateWrite, Request};

const PREVIEW_LIFETIME: Duration = Duration::from_secs(300);
const CANDIDATE_DEADLINE: Duration = Duration::from_secs(30);
const MAX_PREVIEWS: usize = 16;
const MAX_OPERATIONS: usize = 32;

#[derive(Default)]
pub(crate) struct LegacyCandidates {
    records: Mutex<Records>,
}

#[derive(Default)]
struct Records {
    previews: HashMap<Uuid, Preview>,
    operations: HashMap<Uuid, Operation>,
}

struct Operation {
    request: StartCandidate,
    status: LegacyCandidateStatus,
}

struct Preview {
    instance_id: Uuid,
    profile_id: String,
    persona_epoch: u64,
    path: String,
    digest: [u8; 32],
    summary: LegacyMemoryBackupInspection,
    expires: Instant,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StartCandidate {
    pub instance_id: Uuid,
    pub profile_id: String,
    pub persona_epoch: u64,
    pub preview_id: Uuid,
    pub operation_id: Uuid,
    pub output_directory: SensitivePath,
    pub utc_offset: Option<i16>,
    pub embedding_space: Option<String>,
}

impl LegacyCandidates {
    fn insert_preview(&self, preview: Preview) -> Result<Uuid, AppError> {
        let mut records = self.records.lock().map_err(|_| unavailable())?;
        records
            .previews
            .retain(|_, item| item.expires > Instant::now());
        if records.previews.len() >= MAX_PREVIEWS {
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
        start: &StartCandidate,
    ) -> Result<(LegacyCandidateStatus, Option<Request>), AppError> {
        if start.operation_id.get_version_num() != 4
            || !valid_directory(start.output_directory.as_str())
            || start
                .utc_offset
                .is_some_and(|offset| !(-1439..=1439).contains(&offset))
            || start.embedding_space.as_ref().is_some_and(|space| {
                space.trim().is_empty()
                    || space.chars().count() > 256
                    || space.chars().any(char::is_control)
            })
        {
            return Err(AppError::configuration("invalid candidate options"));
        }
        let mut records = self.records.lock().map_err(|_| unavailable())?;
        if let Some(existing) = records.operations.get(&start.operation_id) {
            if existing.request != *start {
                return Err(AppError::invalid_transition(
                    "candidate operation ID is already in use",
                ));
            }
            return Ok((existing.status.clone(), None));
        }
        if records.operations.len() >= MAX_OPERATIONS {
            let output = Path::new(start.output_directory.as_str())
                .join(format!("legacy-candidate-{}.jsonl", start.operation_id));
            let output = output.to_str().ok_or_else(unavailable)?.to_owned();
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
            || (preview.summary.local_timestamp_records > 0 && start.utc_offset.is_none())
            || (preview.summary.nonempty_embedding_records > 0 && start.embedding_space.is_none())
        {
            return Err(AppError::invalid_transition(
                "candidate preview or options are no longer valid",
            ));
        }
        let output = Path::new(start.output_directory.as_str())
            .join(format!("legacy-candidate-{}.jsonl", start.operation_id));
        let output = output.to_str().ok_or_else(unavailable)?.to_owned();
        let request = Request {
            path: preview.path.clone(),
            profile: preview.profile_id.clone(),
            comparison_space: None,
            legacy: true,
            candidate: Some(CandidateWrite {
                digest: preview.digest,
                output: output.clone(),
                utc_offset: start.utc_offset,
                embedding_space: start.embedding_space.clone(),
            }),
        };
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

fn unavailable() -> AppError {
    AppError::unavailable("legacy candidate operation unavailable")
}

fn valid_directory(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 900
        && !value.chars().any(char::is_control)
        && Path::new(value).is_absolute()
}

impl ServiceControl {
    pub(super) async fn start_legacy_candidate(
        &self,
        start: StartCandidate,
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
        let (status, request) = self.legacy_candidates.admit(&start)?;
        drop(persona);
        if let Some(request) = request {
            let operations = Arc::clone(&self.legacy_candidates);
            let operation_id = start.operation_id;
            tokio::spawn(async move {
                let outcome = match inspector.start(request, CANDIDATE_DEADLINE) {
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
        Ok(ControlPayload::LegacyCandidateStatus(status))
    }

    pub(super) async fn prepare_legacy_candidate(
        &self,
        instance_id: Uuid,
        profile_id: String,
        persona_epoch: u64,
        path: SensitivePath,
    ) -> Result<ControlPayload, AppError> {
        let inspector = self.backup_inspector.as_ref().map_err(Clone::clone)?;
        let original_path = path.as_str().to_owned();
        let (expected, (summary, digest)) = self
            .inspect_backup_with_context(
                instance_id,
                profile_id,
                persona_epoch,
                path,
                true,
                |request| {
                    inspector
                        .start(request, Duration::from_secs(4))
                        .map(|pending| async move { pending.wait_legacy_digest().await })
                },
            )
            .await?;
        if summary.records == 0 {
            return Err(AppError::configuration(
                "empty backup cannot identify a profile",
            ));
        }
        let preview = Preview {
            instance_id: expected.0,
            profile_id: expected.1.clone(),
            persona_epoch: expected.2,
            path: original_path,
            digest,
            summary: summary.clone(),
            expires: Instant::now() + PREVIEW_LIFETIME,
        };
        let preview_id = self.legacy_candidates.insert_preview(preview)?;
        Ok(ControlPayload::LegacyCandidatePreview {
            instance_id: expected.0,
            profile_id: expected.1,
            persona_epoch: expected.2,
            preview_id,
            expires_in_seconds: PREVIEW_LIFETIME.as_secs(),
            summary,
        })
    }

    pub(super) async fn legacy_candidate_status(
        &self,
        instance_id: Uuid,
        operation_id: Uuid,
    ) -> Result<ControlPayload, AppError> {
        if self.inspection_context().await?.0 != instance_id {
            return Err(AppError::invalid_transition("service instance changed"));
        }
        Ok(ControlPayload::LegacyCandidateStatus(
            self.legacy_candidates.status(instance_id, operation_id)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(instance_id: Uuid) -> Preview {
        Preview {
            instance_id,
            profile_id: "synthetic".into(),
            persona_epoch: 2,
            path: "synthetic source".into(),
            digest: [42; 32],
            summary: LegacyMemoryBackupInspection {
                records: 1,
                local_timestamp_records: 1,
                unclassified_timestamp_records: 0,
                nonempty_embedding_records: 1,
                unclassified_embedding_records: 0,
                legacy_metadata_fields_present: true,
            },
            expires: Instant::now() + Duration::from_secs(60),
        }
    }

    fn start(instance_id: Uuid, preview_id: Uuid) -> StartCandidate {
        StartCandidate {
            instance_id,
            profile_id: "synthetic".into(),
            persona_epoch: 2,
            preview_id,
            operation_id: Uuid::new_v4(),
            output_directory: SensitivePath::new(std::env::temp_dir().to_string_lossy()),
            utc_offset: Some(480),
            embedding_space: Some("synthetic-space".into()),
        }
    }

    #[test]
    fn candidate_admission_consumes_preview_and_duplicate_id_never_replays() {
        let manager = LegacyCandidates::default();
        let instance_id = Uuid::new_v4();
        let preview_id = manager.insert_preview(preview(instance_id)).unwrap();
        let request_start = start(instance_id, preview_id);
        let (status, request) = manager.admit(&request_start).unwrap();
        assert_eq!(status.state, LegacyCandidateState::Running);
        assert_eq!(request.unwrap().candidate.unwrap().digest, [42; 32]);
        assert!(manager.admit(&request_start).unwrap().1.is_none());
        let mut changed = request_start.clone();
        changed.preview_id = Uuid::new_v4();
        assert!(manager.admit(&changed).is_err());
        changed = request_start.clone();
        changed.persona_epoch += 1;
        assert!(manager.admit(&changed).is_err());
        changed = request_start.clone();
        changed.output_directory =
            SensitivePath::new(std::env::temp_dir().join("different").to_string_lossy());
        assert!(manager.admit(&changed).is_err());
        changed = request_start.clone();
        changed.utc_offset = Some(60);
        assert!(manager.admit(&changed).is_err());
        changed = request_start.clone();
        changed.embedding_space = Some("different-space".into());
        assert!(manager.admit(&changed).is_err());
        changed = request_start.clone();
        changed.profile_id = "different-profile".into();
        assert!(manager.admit(&changed).is_err());
        changed = request_start.clone();
        changed.instance_id = Uuid::new_v4();
        assert!(manager.admit(&changed).is_err());
        let mut another = start(instance_id, preview_id);
        another.operation_id = Uuid::new_v4();
        assert!(manager.admit(&another).is_err());
        manager.finish(request_start.operation_id, LegacyCandidateState::Unknown);
        assert_eq!(
            manager
                .status(instance_id, request_start.operation_id)
                .unwrap()
                .state,
            LegacyCandidateState::Unknown
        );
        assert!(manager.admit(&request_start).unwrap().1.is_none());
    }

    #[test]
    fn candidate_preview_requires_identity_explicit_options_and_live_token() {
        let manager = LegacyCandidates::default();
        let instance_id = Uuid::new_v4();
        let preview_id = manager.insert_preview(preview(instance_id)).unwrap();
        let mut start = start(instance_id, preview_id);
        start.persona_epoch += 1;
        assert!(manager.admit(&start).is_err());
        start.persona_epoch -= 1;
        start.utc_offset = None;
        assert!(manager.admit(&start).is_err());
        start.utc_offset = Some(480);
        start.embedding_space = None;
        assert!(manager.admit(&start).is_err());
        start.embedding_space = Some("synthetic-space".into());
        manager
            .records
            .lock()
            .unwrap()
            .previews
            .get_mut(&preview_id)
            .unwrap()
            .expires = Instant::now();
        assert!(manager.admit(&start).is_err());
        assert!(
            manager
                .status(instance_id, start.operation_id)
                .unwrap()
                .profile_id
                .is_none()
        );
    }

    #[test]
    fn candidate_capacity_reports_not_started_without_replaying() {
        let manager = LegacyCandidates::default();
        let instance_id = Uuid::new_v4();
        for _ in 0..MAX_OPERATIONS {
            let preview_id = manager.insert_preview(preview(instance_id)).unwrap();
            let request = start(instance_id, preview_id);
            assert!(manager.admit(&request).unwrap().1.is_some());
        }
        let preview_id = manager.insert_preview(preview(instance_id)).unwrap();
        let request = start(instance_id, preview_id);
        let (status, pending) = manager.admit(&request).unwrap();
        assert_eq!(status.state, LegacyCandidateState::CapacityReached);
        assert!(pending.is_none());
        assert_eq!(
            manager.admit(&request).unwrap().0.state,
            LegacyCandidateState::CapacityReached
        );
    }
}
