use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ai_ex_domain::AppError;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use uuid::Uuid;

use crate::server::read_line_limited;
use crate::{ControlCommand, ControlPayload, ControlRequest, ControlResponse};

#[derive(Clone)]
pub struct ControlClient {
    address: SocketAddr,
    token: Arc<str>,
    max_message_bytes: usize,
    request_timeout: Duration,
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

impl ControlClient {
    pub async fn desktop_runtime(
        &self,
        request: ai_ex_domain::DesktopRuntimeRequest,
    ) -> Result<ai_ex_domain::DesktopRuntimeReply, AppError> {
        if let ai_ex_domain::DesktopRuntimeRequest::Apply { settings, .. } = &request {
            settings.validate()?;
        }
        // The authenticated VTS actor has its own ten-second discovery timeout.
        let mut client = self.clone();
        client.request_timeout = Duration::from_secs(12);
        match client
            .send(ControlCommand::DesktopRuntime { request })
            .await?
        {
            ControlPayload::DesktopRuntime(reply) => Ok(*reply),
            _ => Err(AppError::protocol(
                "desktop runtime returned an unexpected payload",
            )),
        }
    }

    pub async fn native(
        &self,
        request: ai_ex_domain::NativeRequest,
    ) -> Result<ai_ex_domain::NativeReply, AppError> {
        request.validate()?;
        match self.send(ControlCommand::Native { request }).await? {
            ControlPayload::Native(reply) => {
                reply.validate()?;
                Ok(*reply)
            }
            _ => Err(AppError::protocol(
                "native control returned an unexpected payload",
            )),
        }
    }

    pub fn new(
        address: &str,
        token: impl Into<String>,
        max_message_bytes: usize,
    ) -> Result<Self, AppError> {
        let address: SocketAddr = address.parse().map_err(|error| {
            AppError::configuration(format!("invalid control address: {error}"))
        })?;
        if !address.ip().is_loopback() {
            return Err(AppError::safety(
                "control client requires a loopback address",
            ));
        }
        let token = token.into();
        if token.len() < 32 {
            return Err(AppError::configuration(
                "control token must contain at least 32 bytes",
            ));
        }
        if max_message_bytes < 256 {
            return Err(AppError::configuration(
                "control message limit must be at least 256 bytes",
            ));
        }
        Ok(Self {
            address,
            token: token.into(),
            max_message_bytes,
            request_timeout: Duration::from_secs(5),
        })
    }

    pub async fn send(&self, command: ControlCommand) -> Result<ControlPayload, AppError> {
        tokio::time::timeout(self.request_timeout, self.send_request(command))
            .await
            .map_err(|_| {
                AppError::connectivity("control request timed out; command outcome may be unknown")
            })?
    }

    async fn send_request(&self, command: ControlCommand) -> Result<ControlPayload, AppError> {
        let request_id = Uuid::new_v4();
        let request = ControlRequest {
            request_id,
            token: self.token.to_string(),
            command,
        };
        let mut bytes = serde_json::to_vec(&request)
            .map_err(|error| AppError::protocol(format!("cannot encode request: {error}")))?;
        bytes.push(b'\n');
        if bytes.len() > self.max_message_bytes {
            return Err(AppError::protocol("control request exceeds size limit"));
        }
        let mut stream = TcpStream::connect(self.address)
            .await
            .map_err(|error| AppError::connectivity(format!("control connect failed: {error}")))?;
        stream
            .write_all(&bytes)
            .await
            .map_err(|error| AppError::connectivity(format!("control write failed: {error}")))?;
        let response = read_line_limited(&mut BufReader::new(stream), self.max_message_bytes)
            .await?
            .ok_or_else(|| AppError::protocol("control server closed without a response"))?;
        let response: ControlResponse = serde_json::from_slice(&response)
            .map_err(|error| AppError::protocol(format!("invalid control response: {error}")))?;
        match response {
            ControlResponse::Success {
                request_id: response_id,
                payload,
            } if response_id == request_id => Ok(payload),
            ControlResponse::Failure {
                request_id: Some(response_id),
                error,
            } if response_id == request_id => Err(error),
            ControlResponse::Failure {
                request_id: None,
                error,
            } => Err(error),
            _ => Err(AppError::protocol("control response request ID mismatch")),
        }
    }
}
