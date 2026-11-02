use std::sync::Arc;
use std::time::Duration;

use ai_ex_domain::{AppError, Message, TurnId};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

use super::{MANAGED_MODEL, MANAGED_VERSION, MANIFEST_SHA};
use crate::protocol::{ChatMessage, drain_lines};

const MAX_REPLY_BYTES: usize = 256 * 1024;
const MAX_HTTP_ERROR_BYTES: usize = 8192;
const MAX_HTTP_ERROR_TEXT_BYTES: usize = 4096;

async fn chat_http_error(response: reqwest::Response) -> AppError {
    let status = response.status().as_u16();
    let generic = || AppError::unavailable(format!("managed Ollama chat failed (HTTP {status})"));
    if response
        .content_length()
        .is_some_and(|bytes| bytes > MAX_HTTP_ERROR_BYTES as u64)
    {
        return generic();
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return generic();
        };
        if bytes.len() + chunk.len() > MAX_HTTP_ERROR_BYTES {
            return generic();
        }
        bytes.extend_from_slice(&chunk);
    }
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return generic();
    };
    let Some(error) = value.get("error").and_then(Value::as_str) else {
        return generic();
    };
    let error = error.trim();
    if error.is_empty() || error.len() > MAX_HTTP_ERROR_TEXT_BYTES {
        return generic();
    }
    AppError::unavailable(format!(
        "managed Ollama chat failed (HTTP {status}): {error}"
    ))
}

pub(super) async fn check_endpoint(
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<(), AppError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let version = read_json(client, &format!("{endpoint}/api/version")).await?;
        if version.get("version").and_then(Value::as_str) != Some(MANAGED_VERSION) {
            return Err(AppError::protocol(
                "managed Ollama version does not match the pinned runtime",
            ));
        }
        let tags = read_json(client, &format!("{endpoint}/api/tags")).await?;
        let correct = tags
            .get("models")
            .and_then(Value::as_array)
            .is_some_and(|models| {
                models.iter().any(|m| {
                    m.get("name").and_then(Value::as_str) == Some(MANAGED_MODEL)
                        && m.get("digest").and_then(Value::as_str) == Some(MANIFEST_SHA)
                })
            });
        if !correct {
            return Err(AppError::unavailable(
                "managed Ollama preset manifest is not installed",
            ));
        }
        Ok(())
    })
    .await
    .map_err(|_| AppError::unavailable("managed Ollama health check timed out"))?
}

async fn read_json(client: &reqwest::Client, url: &str) -> Result<Value, AppError> {
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|_| AppError::unavailable("managed Ollama health endpoint is unavailable"))?;
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| AppError::unavailable("managed Ollama health response failed"))?;
        if bytes.len() + chunk.len() > MAX_REPLY_BYTES {
            return Err(AppError::protocol(
                "managed Ollama health response is too large",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| AppError::protocol("invalid managed Ollama health JSON"))
}

pub(super) async fn deliver(
    sender: &mpsc::Sender<Result<String, AppError>>,
    active: &Arc<Mutex<Option<TurnId>>>,
    turn: TurnId,
    value: Result<String, AppError>,
) -> Result<(), AppError> {
    let permit = sender
        .reserve()
        .await
        .map_err(|_| AppError::unavailable("managed conversation receiver closed"))?;
    let current = active.lock().await;
    if *current != Some(turn) {
        return Err(AppError::invalid_transition(
            "managed conversation was cancelled",
        ));
    }
    permit.send(value);
    Ok(())
}

pub(super) async fn run(
    client: &reqwest::Client,
    endpoint: &str,
    messages: Vec<Message>,
    sender: &mpsc::Sender<Result<String, AppError>>,
    active: &Arc<Mutex<Option<TurnId>>>,
    turn: TurnId,
) -> Result<(), AppError> {
    let messages: Vec<_> = messages.into_iter().map(ChatMessage::from).collect();
    let body = json!({"model": MANAGED_MODEL, "messages": messages, "stream": true,
    "think": false, "keep_alive": "5m", "options": {
        "num_gpu": 0, "num_thread": 4, "num_ctx": 4096, "num_predict": 512,
        "temperature": 0.7, "top_p": 0.8, "top_k": 20, "min_p": 0,
        "presence_penalty": 1.5, "repeat_penalty": 1.0,
    }});
    let body =
        serde_json::to_vec(&body).map_err(|_| AppError::protocol("cannot encode managed chat"))?;
    if body.len() > 1024 * 1024 {
        return Err(AppError::configuration(
            "managed conversation exceeds one MiB",
        ));
    }
    let response = client
        .post(format!("{endpoint}/api/chat"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| AppError::unavailable("managed Ollama chat request failed"))?;
    if !response.status().is_success() {
        return Err(chat_http_error(response).await);
    }
    let mut chunks = response.bytes_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk =
            chunk.map_err(|_| AppError::unavailable("managed Ollama response interrupted"))?;
        if buffer.len() + chunk.len() > MAX_REPLY_BYTES {
            return Err(AppError::protocol("managed Ollama frame is too large"));
        }
        buffer.extend_from_slice(&chunk);
        for line in drain_lines(&mut buffer) {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if process_line(&line, sender, active, turn).await? {
                return Ok(());
            }
        }
    }
    Err(AppError::unavailable(
        "managed Ollama stream ended before completion",
    ))
}

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    message: Option<ChunkMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<String>,
}
#[derive(Deserialize)]
struct ChunkMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: String,
}

async fn process_line(
    line: &[u8],
    sender: &mpsc::Sender<Result<String, AppError>>,
    active: &Arc<Mutex<Option<TurnId>>>,
    turn: TurnId,
) -> Result<bool, AppError> {
    let chunk: Chunk = serde_json::from_slice(line)
        .map_err(|_| AppError::protocol("invalid managed Ollama stream JSON"))?;
    if let Some(error) = chunk.error {
        return Err(if error.len() <= 4096 {
            AppError::unavailable(format!("managed Ollama inference failed: {error}"))
        } else {
            AppError::protocol("managed Ollama error is too large")
        });
    }
    if let Some(message) = chunk.message {
        if !message.thinking.trim().is_empty() {
            return Err(AppError::protocol(
                "managed model violated the non-thinking preset",
            ));
        }
        if !message.content.is_empty() {
            deliver(sender, active, turn, Ok(message.content)).await?;
        }
    }
    Ok(chunk.done)
}
