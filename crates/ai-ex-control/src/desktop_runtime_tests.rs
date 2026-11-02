use crate::{ControlBackend, ControlClient, ControlCommand, ControlPayload, ControlServer};
use ai_ex_domain::{
    AppError, DesktopRuntimeReply, DesktopRuntimeRequest, DesktopRuntimeSettings,
    DesktopRuntimeSnapshot,
};
use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

struct Backend;
#[async_trait]
impl ControlBackend for Backend {
    async fn execute(&self, command: ControlCommand) -> Result<ControlPayload, AppError> {
        match command {
            ControlCommand::DesktopRuntime {
                request: DesktopRuntimeRequest::State,
            } => Ok(ControlPayload::DesktopRuntime(Box::new(
                DesktopRuntimeReply::State(DesktopRuntimeSnapshot {
                    instance_id: Uuid::from_u128(10),
                    revision: 2,
                    settings: Default::default(),
                    hotkeys_registered: false,
                    hotkeys_error: false,
                    proactive_available: true,
                    vts_discovery_available: false,
                }),
            ))),
            _ => Err(AppError::protocol("unexpected test command")),
        }
    }
}

#[tokio::test]
async fn behavior_settings_use_existing_authenticated_correlated_protocol() {
    let server = ControlServer::bind("127.0.0.1:0", TOKEN, 4096)
        .await
        .unwrap();
    let client =
        ControlClient::new(&server.local_addr().unwrap().to_string(), TOKEN, 4096).unwrap();
    let server = tokio::spawn(server.serve(Arc::new(Backend)));
    let DesktopRuntimeReply::State(snapshot) = client
        .desktop_runtime(DesktopRuntimeRequest::State)
        .await
        .unwrap()
    else {
        panic!("expected state")
    };
    assert_eq!(snapshot.revision, 2);
    let invalid = DesktopRuntimeSettings {
        cooldown_seconds: 0,
        ..Default::default()
    };
    assert!(
        client
            .desktop_runtime(DesktopRuntimeRequest::Apply {
                instance_id: snapshot.instance_id,
                expected_revision: snapshot.revision,
                settings: invalid
            })
            .await
            .is_err()
    );
    let command = ControlCommand::DesktopRuntime {
        request: DesktopRuntimeRequest::Reload {
            instance_id: snapshot.instance_id,
            expected_revision: 2,
        },
    };
    let bytes = serde_json::to_vec(&command).unwrap();
    assert_eq!(
        serde_json::from_slice::<ControlCommand>(&bytes).unwrap(),
        command
    );
    server.abort();
}
