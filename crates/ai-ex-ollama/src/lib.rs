#![forbid(unsafe_code)]

mod embedding;
mod protocol;

pub use embedding::OllamaEmbeddingClient;

use std::sync::Arc;
use std::time::Duration;

use ai_ex_core::{LanguageModelPort, ModelRequest};
use ai_ex_domain::{AppError, ComponentHealth, TurnId};
use async_trait::async_trait;
use futures_util::StreamExt;
use protocol::{ChatChunk, ChatRequest, drain_lines};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio::task::AbortHandle;

pub struct OllamaClient {
    client: reqwest::Client,
    base_url: String,
    model: String,
    active_turn: Arc<Mutex<Option<TurnId>>>,
    abort_handle: Option<AbortHandle>,
}

impl OllamaClient {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| AppError::configuration(error.to_string()))?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            model: model.into(),
            active_turn: Arc::new(Mutex::new(None)),
            abort_handle: None,
        })
    }

    pub async fn health(&self) -> ComponentHealth {
        let url = format!("{}/api/tags", self.base_url);
        let response = match self.client.get(url).send().await {
            Ok(response) => response,
            Err(error) => {
                return ComponentHealth::unavailable("ollama", request_failure_detail(&error));
            }
        };
        let status = response.status();
        if !status.is_success() {
            return ComponentHealth::unavailable("ollama", http_failure_detail(status.as_u16()));
        }
        let body = match response.json::<Value>().await {
            Ok(body) => body,
            Err(error) => {
                return ComponentHealth::unavailable(
                    "ollama",
                    format!("/api/tags 响应无法解析：{error}"),
                );
            }
        };
        match listed_model(&body, &self.model) {
            Some(true) => ComponentHealth {
                component: "ollama".to_owned(),
                ready: true,
                detail: format!("Ollama 服务可用；model={} 已安装", self.model),
            },
            Some(false) => ComponentHealth::unavailable(
                "ollama",
                format!(
                    "Ollama 服务可用但 model={} 未安装；先执行 ollama pull",
                    self.model
                ),
            ),
            None => ComponentHealth {
                component: "ollama".to_owned(),
                ready: true,
                detail: format!(
                    "Ollama 服务可用；model={} 未能通过 /api/tags 验证",
                    self.model
                ),
            },
        }
    }
}

fn listed_model(body: &Value, model: &str) -> Option<bool> {
    let entries = body.get("models")?.as_array()?;
    Some(entries.iter().any(|entry| {
        entry
            .get("name")
            .or_else(|| entry.get("model"))
            .and_then(Value::as_str)
            == Some(model)
    }))
}
fn http_failure_detail(status: u16) -> String {
    match status {
        404 => "Ollama 接口不存在（HTTP 404）；检查 base_url 是否指向 Ollama 服务".to_owned(),
        408 | 429 => format!("Ollama 请求受限（HTTP {status}）；稍后重试"),
        500..=599 => format!("Ollama 服务端故障（HTTP {status}）；检查本地模型进程"),
        _ => format!("Ollama 返回 HTTP {status}"),
    }
}

fn request_failure_detail(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "Ollama 请求超时；检查模型加载状态或调整 timeout_seconds".to_owned()
    } else if error.is_connect() {
        "无法连接 Ollama；确认 Ollama 已启动并检查 base_url".to_owned()
    } else {
        format!("Ollama 请求失败：{error}")
    }
}
#[async_trait]
impl LanguageModelPort for OllamaClient {
    async fn stream(
        &mut self,
        request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let mut active_turn = self.active_turn.lock().await;
        if active_turn.is_some() {
            return Err(AppError::invalid_transition(
                "an Ollama turn is already active",
            ));
        }
        *active_turn = Some(request.turn_id);
        drop(active_turn);

        let (sender, receiver) = mpsc::channel(64);
        let client = self.client.clone();
        let url = format!("{}/api/chat", self.base_url);
        let body = ChatRequest::new(self.model.clone(), request.messages);
        let active_turn = Arc::clone(&self.active_turn);
        let task = tokio::spawn(async move {
            if let Err(error) = execute_stream(client, url, body, sender.clone()).await {
                let _ignored = sender.send(Err(error)).await;
            }
            *active_turn.lock().await = None;
        });
        self.abort_handle = Some(task.abort_handle());
        Ok(receiver)
    }

    async fn cancel(&mut self, turn_id: TurnId) -> Result<(), AppError> {
        let mut active_turn = self.active_turn.lock().await;
        if *active_turn != Some(turn_id) {
            return Err(AppError::invalid_transition(
                "cannot cancel an inactive Ollama turn",
            ));
        }
        if let Some(handle) = self.abort_handle.take() {
            handle.abort();
        }
        *active_turn = None;
        Ok(())
    }
}

async fn execute_stream(
    client: reqwest::Client,
    url: String,
    body: ChatRequest,
    sender: mpsc::Sender<Result<String, AppError>>,
) -> Result<(), AppError> {
    let response = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::connectivity(error.to_string()))?
        .error_for_status()
        .map_err(|error| AppError::connectivity(error.to_string()))?;
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();

    while let Some(next) = stream.next().await {
        let bytes = next.map_err(|error| AppError::connectivity(error.to_string()))?;
        buffer.extend_from_slice(&bytes);
        for line in drain_lines(&mut buffer) {
            if process_line(&line, &sender).await? {
                return Ok(());
            }
        }
    }

    if !buffer.iter().all(u8::is_ascii_whitespace) {
        process_line(&buffer, &sender).await?;
    }
    Ok(())
}

async fn process_line(
    line: &[u8],
    sender: &mpsc::Sender<Result<String, AppError>>,
) -> Result<bool, AppError> {
    let chunk: ChatChunk = serde_json::from_slice(line)
        .map_err(|error| AppError::protocol(format!("invalid Ollama NDJSON: {error}")))?;
    if let Some(error) = chunk.error {
        return Err(AppError::protocol(error));
    }
    if let Some(message) = chunk.message
        && !message.content.is_empty()
    {
        sender
            .send(Ok(message.content))
            .await
            .map_err(|_| AppError::unavailable("conversation receiver closed"))?;
    }
    Ok(chunk.done)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_installed_model() {
        let body = serde_json::json!({"models": [{"name": "llama3.2:latest"}]});
        assert_eq!(listed_model(&body, "llama3.2:latest"), Some(true));
        assert_eq!(listed_model(&body, "missing"), Some(false));
        assert_eq!(listed_model(&serde_json::json!({}), "missing"), None);
    }

    #[test]
    fn classifies_missing_service_endpoint() {
        assert!(http_failure_detail(404).contains("接口不存在"));
        assert!(http_failure_detail(503).contains("服务端故障"));
    }
}
