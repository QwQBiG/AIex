use ai_ex_domain::{
    AppError, AutomationRequest, AutomationSnapshot, ComponentHealth, MemoryRequest,
    MemoryResponse, PersonaSnapshot, StageSnapshot,
};
use ai_ex_observability::{RuntimeSnapshot, SequencedEvent};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub request_id: Uuid,
    pub token: String,
    pub command: ControlCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlCommand {
    Submit { text: String },
    Interrupt { reason: String },
    Status,
    Persona,
    Memory { request: MemoryRequest },
    Automation { request: AutomationRequest },
    SetPersona { profile: PersonaSnapshot },
    Health,
    Stage,
    Events { after: u64, limit: usize },
    EmergencyStop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ControlPayload {
    Accepted,
    Snapshot(RuntimeSnapshot),
    Persona(PersonaSnapshot),
    Memory(Box<MemoryReply>),
    Automation(Box<AutomationSnapshot>),
    Health(Vec<ComponentHealth>),
    Stage(StageSnapshot),
    Events(Vec<SequencedEvent>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReply {
    pub response: MemoryResponse,
    pub snapshot: RuntimeSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ControlResponse {
    Success {
        request_id: Uuid,
        payload: ControlPayload,
    },
    Failure {
        request_id: Option<Uuid>,
        error: AppError,
    },
}

#[cfg(test)]
mod memory_protocol_tests {
    use super::*;
    use ai_ex_domain::{MemoryEntry, MemoryKind, MemoryPage, MemorySource, TurnId};

    #[test]
    fn memory_commands_and_page_metadata_round_trip() {
        let id = Uuid::new_v4();
        for request in [
            MemoryRequest::List {
                profile_id: "partner".to_owned(),
                query: "茶".to_owned(),
                kind: Some(MemoryKind::Persona),
                offset: 12,
                limit: 12,
            },
            MemoryRequest::Remember {
                profile_id: "partner".to_owned(),
                text: "偏好".to_owned(),
            },
            MemoryRequest::Correct {
                profile_id: "partner".to_owned(),
                id,
                expected_revision: 3,
                text: "更正".to_owned(),
            },
            MemoryRequest::Forget {
                profile_id: "partner".to_owned(),
                id,
                expected_revision: 3,
            },
        ] {
            let command = ControlCommand::Memory { request };
            let json = serde_json::to_string(&command).unwrap();
            assert_eq!(
                serde_json::from_str::<ControlCommand>(&json).unwrap(),
                command
            );
        }
        let response = ControlPayload::Memory(Box::new(MemoryReply {
            response: MemoryResponse::Page(MemoryPage {
                profile_id: "partner".to_owned(),
                enabled: true,
                total: 20,
                offset: 12,
                truncated_ids: vec![id],
                knowledge: Default::default(),
                entries: vec![MemoryEntry {
                    id,
                    profile_id: "partner".to_owned(),
                    turn_id: TurnId::new(),
                    created_ms: 1,
                    updated_ms: Some(2),
                    kind: MemoryKind::Persona,
                    user_text: "新偏好".to_owned(),
                    assistant_text: String::new(),
                    source: MemorySource::UserCorrection,
                    revision: 3,
                }],
            }),
            snapshot: RuntimeSnapshot::default(),
        }));
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlPayload>(&json).unwrap(),
            response
        );
        let response = ControlResponse::Success {
            request_id: Uuid::new_v4(),
            payload: response,
        };
        let json = serde_json::to_vec(&response).unwrap();
        assert_eq!(
            serde_json::from_slice::<ControlResponse>(&json).unwrap(),
            response
        );
        let mut wide = response.clone();
        if let ControlResponse::Success {
            payload: ControlPayload::Memory(reply),
            ..
        } = &mut wide
            && let MemoryResponse::Page(page) = &mut reply.response
        {
            page.entries[0].created_ms = u128::MAX;
            page.entries[0].updated_ms = Some(u128::MAX);
        }
        let json = serde_json::to_vec(&wide).unwrap();
        assert_eq!(
            serde_json::from_slice::<ControlResponse>(&json).unwrap(),
            wide
        );
        let page = serde_json::json!({ "profile_id": "default", "enabled": true,
            "total": 0, "offset": 0, "entries": [] });
        assert!(
            serde_json::from_value::<MemoryPage>(page)
                .unwrap()
                .truncated_ids
                .is_empty()
        );
    }
}

#[cfg(test)]
mod automation_protocol_tests {
    use super::*;
    use ai_ex_domain::{
        AutomationControlMode, AutomationSessionPhase, AutomationSessionSnapshot,
        AutomationStepOutcome,
    };

    #[test]
    fn automation_request_uses_existing_authenticated_correlated_envelope() {
        let request = ControlRequest {
            request_id: Uuid::from_u128(1),
            token: "synthetic-test-token".into(),
            command: ControlCommand::Automation {
                request: AutomationRequest::StartTemplate {
                    instance_id: Uuid::from_u128(2),
                    expected_revision: 4,
                    operation_id: Uuid::from_u128(3),
                    manifest: "fixture/replay.json".into(),
                },
            },
        };
        let bytes = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            serde_json::from_slice::<ControlRequest>(&bytes).unwrap(),
            request
        );
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["command"]["type"], "automation");
        assert_eq!(value["command"]["request"]["type"], "start_template");
        assert_eq!(value["command"]["request"]["expected_revision"], 4);
        assert_eq!(value["request_id"], Uuid::from_u128(1).to_string());
        assert_eq!(
            value["command"]["request"]["operation_id"],
            Uuid::from_u128(3).to_string()
        );
    }

    #[test]
    fn bounded_terminal_automation_snapshot_round_trips_as_a_typed_success() {
        let mut snapshot = AutomationSnapshot::new(Uuid::from_u128(1), true);
        snapshot.revision = 11;
        snapshot.projection_sequence = 24;
        let mut session =
            AutomationSessionSnapshot::loading(Uuid::from_u128(2), Uuid::from_u128(3));
        session.phase = AutomationSessionPhase::Finished;
        session.control_mode = AutomationControlMode::Stopped;
        session.latest_outcome = Some(AutomationStepOutcome::Stopped);
        snapshot.session = Some(session);
        snapshot.validate().unwrap();
        let response = ControlResponse::Success {
            request_id: Uuid::from_u128(4),
            payload: ControlPayload::Automation(Box::new(snapshot.clone())),
        };
        let bytes = serde_json::to_vec(&response).unwrap();
        assert_eq!(
            serde_json::from_slice::<ControlResponse>(&bytes).unwrap(),
            response
        );
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["payload"]["type"], "automation");
        assert_eq!(value["payload"]["data"]["revision"], 11);
        assert_eq!(value["payload"]["data"]["projection_sequence"], 24);
        assert_eq!(
            value["payload"]["data"]["session"]["latest_outcome"],
            "stopped"
        );
        assert_eq!(
            serde_json::from_value::<AutomationSnapshot>(value["payload"]["data"].clone()).unwrap(),
            snapshot
        );
    }
}
