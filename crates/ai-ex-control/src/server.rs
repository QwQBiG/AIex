use std::net::SocketAddr;
use std::sync::Arc;

use ai_ex_domain::AppError;
use async_trait::async_trait;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::{ControlCommand, ControlPayload, ControlRequest, ControlResponse};

#[async_trait]
pub trait ControlBackend: Send + Sync + 'static {
    async fn execute(&self, command: ControlCommand) -> Result<ControlPayload, AppError>;
}

pub struct ControlServer {
    listener: TcpListener,
    token: Arc<str>,
    max_line_bytes: usize,
}

impl ControlServer {
    pub async fn bind(
        address: &str,
        token: impl Into<String>,
        max_line_bytes: usize,
    ) -> Result<Self, AppError> {
        let address: SocketAddr = address.parse().map_err(|error| {
            AppError::configuration(format!("invalid control address: {error}"))
        })?;
        if !address.ip().is_loopback() {
            return Err(AppError::safety(
                "control server must bind to a loopback address",
            ));
        }
        let token = token.into();
        if token.len() < 32 {
            return Err(AppError::configuration(
                "control token must contain at least 32 bytes",
            ));
        }
        if max_line_bytes < 256 {
            return Err(AppError::configuration(
                "control message limit must be at least 256 bytes",
            ));
        }
        let listener = TcpListener::bind(address).await.map_err(|error| {
            AppError::unavailable(format!("cannot bind control server: {error}"))
        })?;
        Ok(Self {
            listener,
            token: token.into(),
            max_line_bytes,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, AppError> {
        self.listener
            .local_addr()
            .map_err(|error| AppError::unavailable(format!("control address unavailable: {error}")))
    }

    pub async fn serve<B>(self, backend: Arc<B>) -> Result<(), AppError>
    where
        B: ControlBackend,
    {
        loop {
            let (stream, peer) = self.listener.accept().await.map_err(|error| {
                AppError::unavailable(format!("control accept failed: {error}"))
            })?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let backend = Arc::clone(&backend);
            let token = Arc::clone(&self.token);
            let max_line_bytes = self.max_line_bytes;
            tokio::spawn(async move {
                let _ignored = handle_connection(stream, backend, token, max_line_bytes).await;
            });
        }
    }
}

pub(crate) async fn read_line_limited<R>(
    reader: &mut R,
    max_line_bytes: usize,
) -> Result<Option<Vec<u8>>, AppError>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let (amount, complete) = {
            let available = reader
                .fill_buf()
                .await
                .map_err(|error| AppError::connectivity(format!("control read failed: {error}")))?;
            if available.is_empty() {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(line))
                };
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let amount = newline.map_or(available.len(), |position| position + 1);
            if line.len().saturating_add(amount) > max_line_bytes {
                return Err(AppError::protocol("control message exceeds size limit"));
            }
            line.extend_from_slice(&available[..amount]);
            (amount, newline.is_some())
        };
        reader.consume(amount);
        if complete {
            return Ok(Some(line));
        }
    }
}

async fn write_response(
    stream: &mut TcpStream,
    response: &ControlResponse,
) -> Result<(), AppError> {
    let mut bytes = serde_json::to_vec(response)
        .map_err(|error| AppError::protocol(format!("cannot encode control response: {error}")))?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .await
        .map_err(|error| AppError::connectivity(format!("control write failed: {error}")))
}

fn tokens_equal(provided: &str, expected: &str) -> bool {
    let provided = provided.as_bytes();
    let expected = expected.as_bytes();
    let length = provided.len().max(expected.len());
    let mut difference = provided.len() ^ expected.len();
    for index in 0..length {
        let left = provided.get(index).copied().unwrap_or_default();
        let right = expected.get(index).copied().unwrap_or_default();
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

async fn handle_connection<B>(
    stream: TcpStream,
    backend: Arc<B>,
    token: Arc<str>,
    max_line_bytes: usize,
) -> Result<(), AppError>
where
    B: ControlBackend,
{
    let mut reader = BufReader::new(stream);
    loop {
        let line = match read_line_limited(&mut reader, max_line_bytes).await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) => {
                write_response(
                    reader.get_mut(),
                    &ControlResponse::Failure {
                        request_id: None,
                        error,
                    },
                )
                .await?;
                return Ok(());
            }
        };
        let response = dispatch(&line, backend.as_ref(), &token).await;
        write_response(reader.get_mut(), &response).await?;
    }
}

async fn dispatch<B>(line: &[u8], backend: &B, token: &str) -> ControlResponse
where
    B: ControlBackend,
{
    let request: ControlRequest = match serde_json::from_slice(line) {
        Ok(request) => request,
        Err(error) => {
            return ControlResponse::Failure {
                request_id: None,
                error: AppError::protocol(format!("invalid control request: {error}")),
            };
        }
    };
    if !tokens_equal(&request.token, token) {
        return ControlResponse::Failure {
            request_id: Some(request.request_id),
            error: AppError::safety("control authentication failed"),
        };
    }
    if let ControlCommand::Native { request: native } = &request.command
        && let Err(error) = native.validate()
    {
        return ControlResponse::Failure {
            request_id: Some(request.request_id),
            error,
        };
    }
    match backend.execute(request.command).await {
        Ok(payload) => {
            if let ControlPayload::Native(reply) = &payload
                && let Err(error) = reply.validate()
            {
                return ControlResponse::Failure {
                    request_id: Some(request.request_id),
                    error,
                };
            }
            ControlResponse::Success {
                request_id: request.request_id,
                payload,
            }
        }
        Err(error) => ControlResponse::Failure {
            request_id: Some(request.request_id),
            error,
        },
    }
}

#[cfg(test)]
mod tests {
    use ai_ex_domain::PersonaSnapshot;
    use ai_ex_observability::RuntimeSnapshot;

    use super::*;
    use crate::ControlClient;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    struct MockBackend;

    #[async_trait]
    impl ControlBackend for MockBackend {
        async fn execute(&self, command: ControlCommand) -> Result<ControlPayload, AppError> {
            match command {
                ControlCommand::Status => Ok(ControlPayload::Snapshot(RuntimeSnapshot::default())),
                ControlCommand::Persona => Ok(ControlPayload::Persona(PersonaSnapshot::default())),
                ControlCommand::Stage => {
                    Ok(ControlPayload::Stage(ai_ex_domain::StageSnapshot::default()))
                }
                _ => Ok(ControlPayload::Accepted),
            }
        }
    }

    #[tokio::test]
    async fn serves_an_authenticated_status_request() {
        let server = ControlServer::bind("127.0.0.1:0", TOKEN, 4_096)
            .await
            .expect("bind server");
        let address = server.local_addr().expect("local address");
        let task = tokio::spawn(server.serve(Arc::new(MockBackend)));
        let client =
            ControlClient::new(&address.to_string(), TOKEN, 4_096).expect("control client");
        let response = client
            .send(ControlCommand::Status)
            .await
            .expect("status response");

        assert!(matches!(response, ControlPayload::Snapshot(_)));
        let response = client
            .send(ControlCommand::Persona)
            .await
            .expect("persona response");
        assert_eq!(
            response,
            ControlPayload::Persona(PersonaSnapshot::default())
        );

        let stage = client
            .send(ControlCommand::Stage)
            .await
            .expect("stage response");
        assert_eq!(
            stage,
            ControlPayload::Stage(ai_ex_domain::StageSnapshot::default())
        );

        task.abort();
    }

    #[tokio::test]
    async fn rejects_non_loopback_binding() {
        let result = ControlServer::bind("0.0.0.0:0", TOKEN, 4_096).await;

        assert!(result.is_err());
    }

    #[test]
    fn token_comparison_checks_content_and_length() {
        assert!(tokens_equal(TOKEN, TOKEN));
        assert!(!tokens_equal("wrong", TOKEN));
        assert!(!tokens_equal("1123456789abcdef0123456789abcdef", TOKEN));
    }
}
