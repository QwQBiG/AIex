use std::sync::Arc;

use ai_ex_domain::{AppError, NativeReply, NativeRequest, NativeState};
use async_trait::async_trait;

use crate::{ControlBackend, ControlClient, ControlCommand, ControlPayload, ControlServer};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

struct NativeBackend;
#[async_trait]
impl ControlBackend for NativeBackend {
    async fn execute(&self, command: ControlCommand) -> Result<ControlPayload, AppError> {
        match command {
            ControlCommand::Native {
                request: NativeRequest::State,
            } => Ok(ControlPayload::Native(Box::new(NativeReply::State(
                NativeState {
                    configured: false,
                    configuration_path: "config/native-v1.toml".into(),
                    preview_active: false,
                    preview_generation: 0,
                },
            )))),
            ControlCommand::Native { .. } => {
                Ok(ControlPayload::Native(Box::new(NativeReply::Accepted)))
            }
            _ => Err(AppError::protocol("unexpected test command")),
        }
    }
}

#[tokio::test]
async fn native_uses_authenticated_correlated_envelope_and_rejects_invalid_fields() {
    let server = ControlServer::bind("127.0.0.1:0", TOKEN, 4096)
        .await
        .unwrap();
    let address = server.local_addr().unwrap().to_string();
    let server = tokio::spawn(server.serve(Arc::new(NativeBackend)));
    let client = ControlClient::new(&address, TOKEN, 4096).unwrap();
    assert!(matches!(
        client.native(NativeRequest::State).await.unwrap(),
        NativeReply::State(_)
    ));
    let other = ControlClient::new(&address, "x".repeat(32), 4096).unwrap();
    assert!(other.native(NativeRequest::State).await.is_err());
    // Raw send deliberately bypasses the helper's validation.
    let error = client
        .send(ControlCommand::Native {
            request: NativeRequest::PreviewVoice {
                voice_id: "voice".into(),
                text: "".into(),
                expected_generation: 0,
            },
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, ai_ex_domain::ErrorKind::Protocol);
    assert!(error.message.contains("bounds or is empty"));
    server.abort();
}
