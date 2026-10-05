use std::time::Duration;

use ai_ex_core::EmbeddingPort;
use ai_ex_domain::AppError;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

const MAX_INPUT_BYTES: usize = 128 * 1024;
const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_DIMENSIONS: usize = 8192;

/// An opt-in Ollama encoder. Construction never contacts or downloads a model.
pub struct OllamaEmbeddingClient {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    space: String,
}

impl OllamaEmbeddingClient {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        space: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, AppError> {
        let base_url = base_url.into();
        let url = reqwest::Url::parse(&base_url)
            .map_err(|_| AppError::configuration("embedding base URL is invalid"))?;
        let model = model.into();
        let space = space.into();
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || model.trim().is_empty()
            || model.chars().count() > 256
            || space.trim().is_empty()
            || space.chars().count() > 256
            || timeout.is_zero()
            || timeout > Duration::from_secs(300)
        {
            return Err(AppError::configuration(
                "embedding requires an HTTP base URL without credentials, model and space of 1..=256 characters, and a timeout of at most 300 seconds",
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AppError::configuration("cannot build embedding HTTP client"))?;
        Ok(Self {
            client,
            endpoint: format!("{}/api/embed", base_url.trim_end_matches('/')),
            model,
            space,
        })
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
    truncate: bool,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl EmbeddingPort for OllamaEmbeddingClient {
    fn space(&self) -> &str {
        &self.space
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
        if text.trim().is_empty() || text.len() > MAX_INPUT_BYTES {
            return Err(AppError::configuration(
                "embedding input must be nonempty and at most 128 KiB",
            ));
        }
        let response = self
            .client
            .post(&self.endpoint)
            .json(&EmbedRequest {
                model: &self.model,
                input: text,
                truncate: false,
            })
            .send()
            .await
            .map_err(request_error)?;
        if !response.status().is_success() {
            return Err(AppError::unavailable(format!(
                "embedding endpoint returned HTTP {}",
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(AppError::protocol("embedding response exceeds 512 KiB"));
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(next) = stream.next().await {
            let chunk = next.map_err(request_error)?;
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
                return Err(AppError::protocol("embedding response exceeds 512 KiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut response: EmbedResponse = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::protocol("invalid embedding response JSON"))?;
        if response.embeddings.len() != 1 {
            return Err(AppError::protocol(
                "embedding response must contain one vector",
            ));
        }
        let vector = response.embeddings.pop().expect("single embedding vector");
        if vector.is_empty()
            || vector.len() > MAX_DIMENSIONS
            || vector.iter().any(|value| !value.is_finite())
            || vector.iter().all(|value| *value == 0.0)
        {
            return Err(AppError::protocol(
                "embedding vector must be finite, nonzero and contain 1..=8192 dimensions",
            ));
        }
        Ok(vector)
    }
}

fn request_error(error: reqwest::Error) -> AppError {
    // URLs and server bodies may contain private text. Report only the category.
    AppError::connectivity(if error.is_timeout() {
        "embedding request timed out"
    } else {
        "embedding endpoint could not be reached or returned an incomplete response"
    })
}

#[cfg(test)]
#[path = "embedding_tests.rs"]
mod tests;
