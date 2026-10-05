use ai_ex_domain::{
    AppError, ComponentHealth, MemoryRequest, MemoryResponse, PersonaSnapshot, StageSnapshot,
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
