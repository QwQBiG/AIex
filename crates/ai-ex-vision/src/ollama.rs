use std::time::Duration;

use ai_ex_domain::{AppError, ComponentHealth};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{VisionAnalyzerPort, VisionObservation, VisionRequest};

const MAX_RESPONSE_BODY_BYTES: usize = 256 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
const MAX_ERROR_BODY_CHARS: usize = 512;

#[derive(Debug, Clone)]
pub struct OllamaVisionSettings {
    pub base_url: String,
    pub model: String,
    pub timeout: Duration,
}

pub struct OllamaVisionClient {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaVisionClient {
    pub fn new(settings: OllamaVisionSettings) -> Result<Self, AppError> {
        if !settings.base_url.starts_with("http://") && !settings.base_url.starts_with("https://") {
            return Err(AppError::configuration(
                "vision base URL must be HTTP or HTTPS",
            ));
        }
        if settings.model.trim().is_empty() || settings.timeout.is_zero() {
            return Err(AppError::configuration(
                "vision model and positive timeout are required",
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(settings.timeout)
            .build()
            .map_err(|error| AppError::configuration(format!("invalid vision client: {error}")))?;
        Ok(Self {
            client,
            base_url: settings.base_url.trim_end_matches('/').to_owned(),
            model: settings.model,
        })
    }

    pub async fn health(&self) -> ComponentHealth {
        match self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => ComponentHealth::ready("vision"),
            Ok(response) => ComponentHealth::unavailable(
                "vision",
                format!("Ollama returned {}", response.status()),
            ),
            Err(error) => ComponentHealth::unavailable("vision", error.to_string()),
        }
    }
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    stream: bool,
    messages: Vec<ChatMessage>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

#[async_trait]
impl VisionAnalyzerPort for OllamaVisionClient {
    async fn analyze(&mut self, request: VisionRequest) -> Result<VisionObservation, AppError> {
        let payload = ChatRequest {
            model: self.model.clone(),
            stream: false,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: "Describe observations only. Do not issue or execute actions."
                        .to_owned(),
                    images: Vec::new(),
                },
                ChatMessage {
                    role: "user",
                    content: request.prompt,
                    images: vec![encode_base64(&request.frame.bytes)],
                },
            ],
        };
        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&payload)
            .send()
            .await
            .map_err(|error| AppError::connectivity(format!("vision request failed: {error}")))?;
        let status = response.status();
        let body_limit = if status.is_success() {
            MAX_RESPONSE_BODY_BYTES
        } else {
            MAX_ERROR_BODY_BYTES
        };
        let body = read_bounded_body(response, body_limit)
            .await
            .map_err(|message| {
                AppError::protocol(format!("vision returned {status}: {message}"))
            })?;
        if !status.is_success() {
            return Err(AppError::protocol(format!(
                "vision returned {status}: {}",
                String::from_utf8_lossy(&body)
                    .chars()
                    .take(MAX_ERROR_BODY_CHARS)
                    .collect::<String>(),
            )));
        }
        let response: ChatResponse = serde_json::from_slice(&body)
            .map_err(|error| AppError::protocol(format!("invalid vision response: {error}")))?;
        if response.message.content.trim().is_empty() {
            return Err(AppError::protocol("vision returned an empty observation"));
        }
        Ok(VisionObservation {
            model: self.model.clone(),
            text: response.message.content,
        })
    }
}

// Read in the analyze future so dropping it or reaching the client deadline also
// drops the response. Check both advertised and received sizes before growing it.
async fn read_bounded_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let oversized = || format!("response body exceeds {limit} bytes");
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(oversized());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            "response body read timed out".to_owned()
        } else {
            format!("response body read failed: {error}")
        }
    })? {
        if chunk.len() > limit - body.len() {
            return Err(oversized());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or_default();
        let third = chunk.get(2).copied().unwrap_or_default();
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::encode_base64;

    #[test]
    fn encodes_standard_base64_vectors() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
    }
}

#[cfg(test)]
#[path = "ollama_tests.rs"]
mod response_tests;
