//! Bounded control-plane summaries for host-owned synthetic template sessions.
//!
//! These types carry no automation handles, image bytes, or execution authority.
//! The service validates instance/session/revision under its own arbitration lock.
//! A timed-out mutation has an unknown outcome; callers refresh Status before
//! deciding what to do, and do not automatically replay commands.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppError;

pub const AUTOMATION_SCHEMA_VERSION: u16 = 1;
pub const AUTOMATION_MAX_MANIFEST_BYTES: usize = 4_096;
pub const AUTOMATION_MAX_TARGET_BYTES: usize = 512;
pub const AUTOMATION_MAX_PROFILE_BYTES: usize = 512;
pub const AUTOMATION_MAX_ERROR_BYTES: usize = 1_024;
pub const AUTOMATION_MAX_SNAPSHOT_BYTES: usize = 16 * 1_024;
pub const AUTOMATION_MAX_FRAMES: u64 = 256;
pub const AUTOMATION_MAX_ACTIONS: u64 = 4_096;

/// Start uses the existing manifest file format and always selects synthetic
/// frames with a dry-run port. There is no live-device execution option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AutomationRequest {
    Status,
    /// The host reserves this operation before decoding. Repeating the same
    /// operation must not launch it twice; a different start replaces only the
    /// state matching the supplied instance and mutation revision.
    StartTemplate {
        instance_id: Uuid,
        expected_revision: u64,
        operation_id: Uuid,
        manifest: String,
    },
    Pause {
        instance_id: Uuid,
        session_id: Uuid,
        expected_revision: u64,
    },
    Resume {
        instance_id: Uuid,
        session_id: Uuid,
        expected_revision: u64,
    },
    /// Terminal intent for this exact instance/session. The host accepts an
    /// observed revision at or below its current revision, and rejects a future
    /// revision. Start/Pause/Resume keep strict revision equality instead.
    Stop {
        instance_id: Uuid,
        session_id: Uuid,
        expected_revision: u64,
    },
}

impl AutomationRequest {
    /// Validate syntax and resource bounds, without filesystem access or a claim
    /// that the supplied host cursor is current. The host performs that check.
    pub fn validate(&self) -> Result<(), AppError> {
        match self {
            Self::Status => Ok(()),
            Self::StartTemplate {
                instance_id,
                operation_id,
                manifest,
                ..
            } => {
                if instance_id.is_nil() || operation_id.is_nil() {
                    return Err(AppError::configuration(
                        "automation instance and operation identifiers must not be nil",
                    ));
                }
                if manifest.trim().is_empty()
                    || manifest.len() > AUTOMATION_MAX_MANIFEST_BYTES
                    || manifest.chars().any(char::is_control)
                {
                    return Err(AppError::configuration(
                        "automation manifest path must contain 1..=4096 UTF-8 bytes without control characters",
                    ));
                }
                Ok(())
            }
            Self::Pause {
                instance_id,
                session_id,
                ..
            }
            | Self::Resume {
                instance_id,
                session_id,
                ..
            }
            | Self::Stop {
                instance_id,
                session_id,
                ..
            } => {
                if instance_id.is_nil() || session_id.is_nil() {
                    return Err(AppError::configuration(
                        "automation instance and session identifiers must not be nil",
                    ));
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSessionPhase {
    Loading,
    Active,
    /// Stop has revoked dispatch; the host is draining an already-started step.
    Stopping,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationControlMode {
    Running,
    PausedForChat,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTemplateMode {
    ClickOnce,
    ClickRepeat,
    Hover,
}

/// The latest bounded execution or terminal host outcome, without a full report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationStepOutcome {
    Waiting,
    Matched,
    Missed,
    Completed,
    BudgetExhausted,
    SearchIncomplete,
    Inactive,
    Interrupted,
    Failed,
    InputExhausted,
    Stopped,
}

/// Frame-local region of the incomplete search, without an input/capture handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSearchRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSearchIncompleteReason {
    WorkBudgetExhausted,
}

/// Uncertified search progress only. No provisional score or target bounds cross
/// this boundary, and consumed work is never a new allowance to retry a search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSearchSnapshot {
    pub region: AutomationSearchRegion,
    pub candidates: u64,
    pub verified: u64,
    pub bound_queries: u64,
    pub exact_evaluations: u64,
    pub exact_pixel_pairs: u64,
    pub bound_work: u64,
    pub work: u64,
    pub max_work: u64,
    pub reason: AutomationSearchIncompleteReason,
}

impl AutomationSearchSnapshot {
    pub fn validate(&self) -> Result<(), AppError> {
        const MAX_CANDIDATES: u64 = 16_777_216;
        let area = u64::from(self.region.width) * u64::from(self.region.height);
        if self.region.width == 0
            || self.region.height == 0
            || self.region.x.checked_add(self.region.width).is_none()
            || self.region.y.checked_add(self.region.height).is_none()
            || area > MAX_CANDIDATES
            || !(1..=MAX_CANDIDATES).contains(&self.candidates)
            || self.candidates > area
            || self.verified > self.candidates
            || self.exact_evaluations > self.verified
            || self.bound_queries > self.candidates * 2
            || self.max_work == 0
            || self.work > self.max_work
            || self.exact_pixel_pairs.checked_add(self.bound_work) != Some(self.work)
            || self
                .exact_evaluations
                .checked_mul(40_000)
                .is_none_or(|cap| self.exact_pixel_pairs > cap)
        {
            return Err(AppError::protocol(
                "invalid bounded incomplete-search diagnostics",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationPauseReason {
    Chat,
    Manual,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationResumeEligibility {
    Idle,
    Busy,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSessionSnapshot {
    pub session_id: Uuid,
    pub operation_id: Uuid,
    pub control_generation: u64,
    pub phase: AutomationSessionPhase,
    pub control_mode: AutomationControlMode,
    /// Display summaries may be truncated without changing the loaded manifest.
    pub profile_name: Option<String>,
    pub target: Option<String>,
    pub mode: Option<AutomationTemplateMode>,
    pub input_frames: u64,
    pub scheduled_frames: u64,
    pub skipped_frames: u64,
    /// Consumed or possibly executed captures, matching SDK budget semantics.
    pub frames: u64,
    /// Consumed or possibly executed pointer actions, never a retry allowance.
    pub actions: u64,
    /// Zero while a loading session has not acquired validated manifest budgets.
    pub max_frames: u64,
    pub max_actions: u64,
    pub latest_outcome: Option<AutomationStepOutcome>,
    /// Present only when this step could not certify a complete search result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<AutomationSearchSnapshot>,
    pub last_error: Option<String>,
    pub pause_reason: Option<AutomationPauseReason>,
    pub resume_eligibility: AutomationResumeEligibility,
    /// An observation in the host's monotonic clock domain, not a wall-clock time
    /// or a transferable owned-pause token.
    pub chat_deadline_ms: Option<u64>,
}

impl AutomationSessionSnapshot {
    pub fn loading(session_id: Uuid, operation_id: Uuid) -> Self {
        Self {
            session_id,
            operation_id,
            control_generation: 0,
            phase: AutomationSessionPhase::Loading,
            control_mode: AutomationControlMode::Running,
            profile_name: None,
            target: None,
            mode: None,
            input_frames: 0,
            scheduled_frames: 0,
            skipped_frames: 0,
            frames: 0,
            actions: 0,
            max_frames: 0,
            max_actions: 0,
            latest_outcome: None,
            search: None,
            last_error: None,
            pause_reason: None,
            resume_eligibility: AutomationResumeEligibility::Unknown,
            chat_deadline_ms: None,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.session_id.is_nil() || self.operation_id.is_nil() {
            return Err(AppError::protocol(
                "automation snapshot identifiers must not be nil",
            ));
        }
        if self
            .profile_name
            .as_ref()
            .is_some_and(|text| text.len() > AUTOMATION_MAX_PROFILE_BYTES)
            || self
                .target
                .as_ref()
                .is_some_and(|text| text.len() > AUTOMATION_MAX_TARGET_BYTES)
            || self
                .last_error
                .as_ref()
                .is_some_and(|text| text.len() > AUTOMATION_MAX_ERROR_BYTES)
        {
            return Err(AppError::protocol(
                "automation snapshot display text exceeds its byte limit",
            ));
        }
        if self.search.is_some()
            != (self.latest_outcome == Some(AutomationStepOutcome::SearchIncomplete))
        {
            return Err(AppError::protocol(
                "search diagnostics require the incomplete-search outcome",
            ));
        }
        if let Some(search) = &self.search {
            if self.phase != AutomationSessionPhase::Finished
                || self.control_mode != AutomationControlMode::Stopped
            {
                return Err(AppError::protocol(
                    "incomplete search requires a finished and stopped session",
                ));
            }
            search.validate()?;
        }
        if self.input_frames > AUTOMATION_MAX_FRAMES
            || self.scheduled_frames > AUTOMATION_MAX_FRAMES
            || self.skipped_frames > AUTOMATION_MAX_FRAMES
            || self.frames > AUTOMATION_MAX_FRAMES
            || self.max_frames > AUTOMATION_MAX_FRAMES
            || self.actions > AUTOMATION_MAX_ACTIONS
            || self.max_actions > AUTOMATION_MAX_ACTIONS
        {
            return Err(AppError::protocol(
                "automation snapshot counters exceed synthetic replay bounds",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSnapshot {
    pub schema_version: u16,
    /// The same service instance identity used by the runtime EventHub snapshot.
    pub instance_id: Uuid,
    /// Mutation compare-and-set cursor. Includes ownership/deadline changes and
    /// repeated manual pause; frame-only progress does not change this cursor.
    pub revision: u64,
    /// Orders all published summaries, including progress and command responses.
    pub projection_sequence: u64,
    pub available: bool,
    pub session: Option<AutomationSessionSnapshot>,
}

impl AutomationSnapshot {
    pub fn new(instance_id: Uuid, available: bool) -> Self {
        Self {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            instance_id,
            revision: 0,
            projection_sequence: 0,
            available,
            session: None,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != AUTOMATION_SCHEMA_VERSION || self.instance_id.is_nil() {
            return Err(AppError::protocol(
                "automation snapshot requires schema 1 and a service instance identifier",
            ));
        }
        if let Some(session) = &self.session {
            session.validate()?;
        }
        let encoded = serde_json::to_vec(self)
            .map_err(|_| AppError::protocol("cannot encode automation snapshot"))?;
        if encoded.len() > AUTOMATION_MAX_SNAPSHOT_BYTES {
            return Err(AppError::protocol(
                "automation snapshot exceeds its serialized byte limit",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    fn request() -> AutomationRequest {
        AutomationRequest::StartTemplate {
            instance_id: Uuid::from_u128(1),
            expected_revision: 7,
            operation_id: Uuid::from_u128(2),
            manifest: "fixtures/合成 replay.json".into(),
        }
    }

    fn snapshot() -> AutomationSnapshot {
        let mut snapshot = AutomationSnapshot::new(Uuid::from_u128(1), true);
        let mut session =
            AutomationSessionSnapshot::loading(Uuid::from_u128(2), Uuid::from_u128(3));
        session.phase = AutomationSessionPhase::Active;
        session.profile_name = Some("合成模板".into());
        session.target = Some("primary_target".into());
        session.mode = Some(AutomationTemplateMode::ClickRepeat);
        session.input_frames = 4;
        session.scheduled_frames = 3;
        session.skipped_frames = 1;
        session.frames = 2;
        session.actions = 3;
        session.max_frames = 3;
        session.max_actions = 6;
        session.latest_outcome = Some(AutomationStepOutcome::Matched);
        snapshot.session = Some(session);
        snapshot
    }

    fn assert_round_trip<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let encoded = serde_json::to_vec(value).unwrap();
        let decoded: T = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn requests_round_trip_with_explicit_operation_and_mutation_cursors() {
        let instance_id = Uuid::from_u128(1);
        let session_id = Uuid::from_u128(3);
        for request in [
            AutomationRequest::Status,
            request(),
            AutomationRequest::Pause {
                instance_id,
                session_id,
                expected_revision: 8,
            },
            AutomationRequest::Resume {
                instance_id,
                session_id,
                expected_revision: 9,
            },
            AutomationRequest::Stop {
                instance_id,
                session_id,
                expected_revision: 10,
            },
        ] {
            request.validate().unwrap();
            assert_round_trip(&request);
        }
        assert_eq!(
            serde_json::to_value(request()).unwrap(),
            serde_json::json!({
                "type": "start_template",
                "instance_id": instance_id,
                "expected_revision": 7,
                "operation_id": Uuid::from_u128(2),
                "manifest": "fixtures/合成 replay.json"
            })
        );
    }

    #[test]
    fn requests_reject_unknown_execution_options_and_missing_mutation_cursors() {
        let mut value = serde_json::to_value(request()).unwrap();
        value["real_device"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AutomationRequest>(value).is_err());
        let mut value = serde_json::to_value(request()).unwrap();
        value.as_object_mut().unwrap().remove("operation_id");
        assert!(serde_json::from_value::<AutomationRequest>(value).is_err());
        let mut value = serde_json::to_value(AutomationRequest::Pause {
            instance_id: Uuid::from_u128(1),
            session_id: Uuid::from_u128(2),
            expected_revision: 0,
        })
        .unwrap();
        value.as_object_mut().unwrap().remove("expected_revision");
        assert!(serde_json::from_value::<AutomationRequest>(value).is_err());
    }

    #[test]
    fn manifest_validation_counts_utf8_bytes_and_never_normalizes_the_path() {
        for manifest in [
            "x".repeat(AUTOMATION_MAX_MANIFEST_BYTES),
            "界".repeat(AUTOMATION_MAX_MANIFEST_BYTES / 3),
            " fixtures/replay.json ".into(),
        ] {
            let request = AutomationRequest::StartTemplate {
                instance_id: Uuid::from_u128(1),
                expected_revision: 0,
                operation_id: Uuid::from_u128(2),
                manifest: manifest.clone(),
            };
            request.validate().unwrap();
            assert_eq!(serde_json::to_value(request).unwrap()["manifest"], manifest);
        }
        for manifest in [
            String::new(),
            "   ".into(),
            "replay\n.json".into(),
            "replay\0.json".into(),
            "x".repeat(AUTOMATION_MAX_MANIFEST_BYTES + 1),
            "界".repeat(AUTOMATION_MAX_MANIFEST_BYTES / 3 + 1),
        ] {
            let request = AutomationRequest::StartTemplate {
                instance_id: Uuid::from_u128(1),
                expected_revision: 0,
                operation_id: Uuid::from_u128(2),
                manifest,
            };
            assert_eq!(
                request.validate().unwrap_err().kind,
                ErrorKind::Configuration
            );
        }
    }

    #[test]
    fn mutation_identifiers_cannot_be_nil() {
        for request in [
            AutomationRequest::StartTemplate {
                instance_id: Uuid::nil(),
                expected_revision: 0,
                operation_id: Uuid::from_u128(2),
                manifest: "replay.json".into(),
            },
            AutomationRequest::StartTemplate {
                instance_id: Uuid::from_u128(1),
                expected_revision: 0,
                operation_id: Uuid::nil(),
                manifest: "replay.json".into(),
            },
            AutomationRequest::Pause {
                instance_id: Uuid::nil(),
                session_id: Uuid::from_u128(2),
                expected_revision: 0,
            },
            AutomationRequest::Resume {
                instance_id: Uuid::from_u128(1),
                session_id: Uuid::nil(),
                expected_revision: 0,
            },
            AutomationRequest::Stop {
                instance_id: Uuid::nil(),
                session_id: Uuid::nil(),
                expected_revision: 0,
            },
        ] {
            assert_eq!(
                request.validate().unwrap_err().kind,
                ErrorKind::Configuration
            );
        }
    }

    #[test]
    fn unavailable_and_loading_snapshots_are_valid_without_loaded_metadata() {
        let unavailable = AutomationSnapshot::new(Uuid::from_u128(1), false);
        unavailable.validate().unwrap();
        assert_eq!(unavailable.schema_version, AUTOMATION_SCHEMA_VERSION);
        assert_eq!(unavailable.revision, 0);
        assert_eq!(unavailable.projection_sequence, 0);
        assert!(unavailable.session.is_none());
        assert_round_trip(&unavailable);

        let mut loading = AutomationSnapshot::new(Uuid::from_u128(1), true);
        let session = AutomationSessionSnapshot::loading(Uuid::from_u128(2), Uuid::from_u128(3));
        assert_eq!(session.phase, AutomationSessionPhase::Loading);
        assert_eq!(session.control_mode, AutomationControlMode::Running);
        assert_eq!(session.control_generation, 0);
        assert_eq!(
            session.resume_eligibility,
            AutomationResumeEligibility::Unknown
        );
        loading.session = Some(session);
        loading.validate().unwrap();
        let session = loading.session.as_mut().unwrap();
        session.phase = AutomationSessionPhase::Finished;
        session.control_mode = AutomationControlMode::Stopped;
        session.latest_outcome = Some(AutomationStepOutcome::Failed);
        session.last_error = Some("manifest is unavailable".into());
        loading.validate().unwrap();
        assert_round_trip(&loading);
    }

    #[test]
    fn terminal_outcomes_and_wide_cursors_round_trip_without_float_fields() {
        let mut snapshot = snapshot();
        snapshot.revision = u64::MAX;
        snapshot.projection_sequence = u64::MAX;
        {
            let session = snapshot.session.as_mut().unwrap();
            session.control_generation = u64::MAX;
            session.phase = AutomationSessionPhase::Finished;
            session.control_mode = AutomationControlMode::Stopped;
            session.chat_deadline_ms = Some(u64::MAX);
        }
        for outcome in [
            AutomationStepOutcome::Waiting,
            AutomationStepOutcome::Matched,
            AutomationStepOutcome::Missed,
            AutomationStepOutcome::Completed,
            AutomationStepOutcome::BudgetExhausted,
            AutomationStepOutcome::Inactive,
            AutomationStepOutcome::Interrupted,
            AutomationStepOutcome::Failed,
            AutomationStepOutcome::InputExhausted,
            AutomationStepOutcome::Stopped,
        ] {
            snapshot.session.as_mut().unwrap().latest_outcome = Some(outcome);
            snapshot.validate().unwrap();
            assert_round_trip(&snapshot);
        }
        let mut value = serde_json::to_value(&snapshot).unwrap();
        value["session"]["frames"] = serde_json::json!(1.5);
        assert!(serde_json::from_value::<AutomationSnapshot>(value).is_err());
    }

    #[test]
    fn display_text_bounds_cover_escaped_and_multibyte_content() {
        let mut snapshot = snapshot();
        {
            let session = snapshot.session.as_mut().unwrap();
            session.profile_name = Some("\\".repeat(AUTOMATION_MAX_PROFILE_BYTES));
            session.target = Some("\"".repeat(AUTOMATION_MAX_TARGET_BYTES));
            session.last_error = Some("\0".repeat(AUTOMATION_MAX_ERROR_BYTES));
        }
        snapshot.validate().unwrap();
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        assert!(encoded.len() <= AUTOMATION_MAX_SNAPSHOT_BYTES);
        assert_round_trip(&snapshot);
        for field in ["profile", "target", "error"] {
            let mut too_wide = snapshot.clone();
            let session = too_wide.session.as_mut().unwrap();
            match field {
                "profile" => {
                    session.profile_name = Some("界".repeat(AUTOMATION_MAX_PROFILE_BYTES / 3 + 1));
                }
                "target" => {
                    session.target = Some("x".repeat(AUTOMATION_MAX_TARGET_BYTES + 1));
                }
                "error" => {
                    session.last_error = Some("x".repeat(AUTOMATION_MAX_ERROR_BYTES + 1));
                }
                _ => unreachable!(),
            }
            assert_eq!(too_wide.validate().unwrap_err().kind, ErrorKind::Protocol);
        }
    }

    #[test]
    fn snapshots_reject_wrong_schema_nil_identifiers_and_unbounded_counters() {
        let mut wrong_schema = snapshot();
        wrong_schema.schema_version = AUTOMATION_SCHEMA_VERSION + 1;
        assert!(wrong_schema.validate().is_err());
        let mut nil_instance = snapshot();
        nil_instance.instance_id = Uuid::nil();
        assert!(nil_instance.validate().is_err());
        for field in ["session", "operation"] {
            let mut invalid = snapshot();
            let session = invalid.session.as_mut().unwrap();
            match field {
                "session" => session.session_id = Uuid::nil(),
                "operation" => session.operation_id = Uuid::nil(),
                _ => unreachable!(),
            }
            assert!(invalid.validate().is_err());
        }
        for field in [
            "input",
            "scheduled",
            "skipped",
            "frames",
            "max_frames",
            "actions",
            "max_actions",
        ] {
            let mut invalid = snapshot();
            let session = invalid.session.as_mut().unwrap();
            match field {
                "input" => session.input_frames = AUTOMATION_MAX_FRAMES + 1,
                "scheduled" => session.scheduled_frames = AUTOMATION_MAX_FRAMES + 1,
                "skipped" => session.skipped_frames = AUTOMATION_MAX_FRAMES + 1,
                "frames" => session.frames = AUTOMATION_MAX_FRAMES + 1,
                "max_frames" => session.max_frames = AUTOMATION_MAX_FRAMES + 1,
                "actions" => session.actions = AUTOMATION_MAX_ACTIONS + 1,
                "max_actions" => session.max_actions = AUTOMATION_MAX_ACTIONS + 1,
                _ => unreachable!(),
            }
            assert_eq!(invalid.validate().unwrap_err().kind, ErrorKind::Protocol);
        }
    }

    #[test]
    fn summaries_do_not_accept_raw_images_scores_or_audit_blobs() {
        for field in ["rgba", "score", "audit", "receipts"] {
            let mut value = serde_json::to_value(snapshot()).unwrap();
            value["session"][field] = serde_json::json!([]);
            assert!(serde_json::from_value::<AutomationSnapshot>(value).is_err());
        }
    }

    fn incomplete_search() -> AutomationSearchSnapshot {
        AutomationSearchSnapshot {
            region: AutomationSearchRegion {
                x: 7,
                y: 11,
                width: 100,
                height: 100,
            },
            candidates: 100,
            verified: 40,
            bound_queries: 80,
            exact_evaluations: 20,
            exact_pixel_pairs: 8_000,
            bound_work: 160,
            work: 8_160,
            max_work: 10_000,
            reason: AutomationSearchIncompleteReason::WorkBudgetExhausted,
        }
    }

    #[test]
    fn incomplete_search_round_trips_bounded_costs_without_provisional_match_data() {
        let search = incomplete_search();
        search.validate().unwrap();
        assert_round_trip(&search);
        let mut snapshot = snapshot();
        let session = snapshot.session.as_mut().unwrap();
        session.phase = AutomationSessionPhase::Finished;
        session.control_mode = AutomationControlMode::Stopped;
        session.latest_outcome = Some(AutomationStepOutcome::SearchIncomplete);
        session.search = Some(search);
        snapshot.validate().unwrap();
        assert_round_trip(&snapshot);
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["session"]["latest_outcome"], "search_incomplete");
        assert_eq!(
            value["session"]["search"]["reason"],
            "work_budget_exhausted"
        );
        assert_eq!(value["session"]["search"]["verified"], 40);
        for field in [
            "score",
            "matched_bounds",
            "absolute",
            "normalized",
            "rgba",
            "action",
        ] {
            let mut invalid = value.clone();
            invalid["session"]["search"][field] = serde_json::json!(0);
            assert!(
                serde_json::from_value::<AutomationSnapshot>(invalid).is_err(),
                "{field}"
            );
        }
        let mut invalid = value;
        invalid["session"]["search"]["region"]["score"] = serde_json::json!(1.0);
        assert!(serde_json::from_value::<AutomationSnapshot>(invalid).is_err());
    }

    #[test]
    fn missing_search_remains_compatible_and_absence_is_not_serialized() {
        let original = snapshot();
        assert!(original.session.as_ref().unwrap().search.is_none());
        let mut value = serde_json::to_value(&original).unwrap();
        assert!(value["session"].get("search").is_none());
        let decoded: AutomationSnapshot = serde_json::from_value(value.clone()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, original);
        value["session"]["search"] = serde_json::Value::Null;
        let decoded: AutomationSnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, original);
        let loading = AutomationSessionSnapshot::loading(Uuid::from_u128(2), Uuid::from_u128(3));
        assert!(loading.search.is_none());
        assert!(
            serde_json::to_value(loading)
                .unwrap()
                .get("search")
                .is_none()
        );
    }

    #[test]
    fn incomplete_search_rejects_invalid_regions_counters_costs_and_reason() {
        let prototype = serde_json::to_value(incomplete_search()).unwrap();
        for (field, value) in [
            ("candidates", serde_json::json!(0)),
            ("candidates", serde_json::json!(16_777_217u64)),
            ("candidates", serde_json::json!(10_001)),
            ("verified", serde_json::json!(101)),
            ("exact_evaluations", serde_json::json!(41)),
            ("exact_pixel_pairs", serde_json::json!(800_001)),
            ("bound_queries", serde_json::json!(201)),
            ("bound_queries", serde_json::json!(u64::MAX)),
            ("work", serde_json::json!(8_161)),
            ("max_work", serde_json::json!(0)),
            ("max_work", serde_json::json!(8_159)),
        ] {
            let mut invalid = prototype.clone();
            invalid[field] = value;
            let search: AutomationSearchSnapshot = serde_json::from_value(invalid).unwrap();
            assert_eq!(
                search.validate().unwrap_err().kind,
                ErrorKind::Protocol,
                "{field}"
            );
        }
        for (field, value) in [
            ("width", 0),
            ("height", 0),
            ("x", u32::MAX),
            ("y", u32::MAX),
            ("width", 16_777_216),
        ] {
            let mut invalid = prototype.clone();
            invalid["region"][field] = serde_json::json!(value);
            let search: AutomationSearchSnapshot = serde_json::from_value(invalid).unwrap();
            assert!(search.validate().is_err(), "{field}");
        }
        let mut overflow = incomplete_search();
        overflow.exact_pixel_pairs = u64::MAX;
        overflow.bound_work = 1;
        overflow.work = 0;
        assert!(overflow.validate().is_err());
        for value in [serde_json::json!(-1), serde_json::json!(1.5)] {
            let mut invalid = prototype.clone();
            invalid["verified"] = value;
            assert!(serde_json::from_value::<AutomationSearchSnapshot>(invalid).is_err());
        }
        let mut invalid = prototype;
        invalid["reason"] = serde_json::json!("timeout_or_miss");
        assert!(serde_json::from_value::<AutomationSearchSnapshot>(invalid).is_err());
    }

    #[test]
    fn incomplete_search_caps_accept_wide_integer_budgets_and_exact_boundary_values() {
        let candidates = 16_777_216;
        let search = AutomationSearchSnapshot {
            region: AutomationSearchRegion {
                x: 0,
                y: 0,
                width: 4096,
                height: 4096,
            },
            candidates,
            verified: candidates,
            bound_queries: candidates * 2,
            exact_evaluations: candidates,
            exact_pixel_pairs: candidates * 40_000,
            bound_work: 0,
            work: candidates * 40_000,
            max_work: u64::MAX,
            reason: AutomationSearchIncompleteReason::WorkBudgetExhausted,
        };
        search.validate().unwrap();
        assert_round_trip(&search);
        let mut invalid = search;
        invalid.region.x = u32::MAX - 4095;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn incomplete_diagnostics_and_step_outcome_cannot_contradict_each_other() {
        let mut invalid = snapshot();
        invalid.session.as_mut().unwrap().search = Some(incomplete_search());
        assert!(invalid.validate().is_err());
        invalid.session.as_mut().unwrap().search = None;
        invalid.session.as_mut().unwrap().latest_outcome =
            Some(AutomationStepOutcome::SearchIncomplete);
        assert!(invalid.validate().is_err());
        invalid.session.as_mut().unwrap().search = Some(incomplete_search());
        invalid.session.as_mut().unwrap().phase = AutomationSessionPhase::Finished;
        invalid.session.as_mut().unwrap().control_mode = AutomationControlMode::Stopped;
        invalid.validate().unwrap();
    }

    #[test]
    fn incomplete_search_is_valid_only_after_the_session_is_finished_and_stopped() {
        for phase in [
            AutomationSessionPhase::Loading,
            AutomationSessionPhase::Active,
            AutomationSessionPhase::Stopping,
            AutomationSessionPhase::Finished,
        ] {
            for control_mode in [
                AutomationControlMode::Running,
                AutomationControlMode::PausedForChat,
                AutomationControlMode::Stopped,
            ] {
                let mut value = snapshot();
                let session = value.session.as_mut().unwrap();
                session.phase = phase;
                session.control_mode = control_mode;
                session.resume_eligibility = AutomationResumeEligibility::Idle;
                session.latest_outcome = Some(AutomationStepOutcome::SearchIncomplete);
                session.search = Some(incomplete_search());
                let encoded = serde_json::to_value(&value).unwrap();
                let decoded: AutomationSnapshot = serde_json::from_value(encoded).unwrap();
                if phase == AutomationSessionPhase::Finished
                    && control_mode == AutomationControlMode::Stopped
                {
                    decoded.validate().unwrap();
                } else {
                    assert_eq!(
                        decoded.validate().unwrap_err().kind,
                        ErrorKind::Protocol,
                        "{phase:?}/{control_mode:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn legacy_missing_search_keeps_active_chat_pause_and_terminal_budget_snapshots_compatible() {
        let active = snapshot();
        let mut paused = active.clone();
        let session = paused.session.as_mut().unwrap();
        session.control_mode = AutomationControlMode::PausedForChat;
        session.pause_reason = Some(AutomationPauseReason::Manual);
        session.resume_eligibility = AutomationResumeEligibility::Idle;
        session.latest_outcome = Some(AutomationStepOutcome::Waiting);
        let mut finished = active.clone();
        let session = finished.session.as_mut().unwrap();
        session.phase = AutomationSessionPhase::Finished;
        session.control_mode = AutomationControlMode::Stopped;
        session.latest_outcome = Some(AutomationStepOutcome::BudgetExhausted);
        for original in [active, paused, finished] {
            let value = serde_json::to_value(&original).unwrap();
            assert!(value["session"].get("search").is_none());
            let decoded: AutomationSnapshot = serde_json::from_value(value).unwrap();
            decoded.validate().unwrap();
            assert_eq!(decoded, original);
        }
    }
}
