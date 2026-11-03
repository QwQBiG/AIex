#![forbid(unsafe_code)]

mod discovery;
#[cfg(test)]
mod discovery_tests;
mod protocol;
pub use discovery::{VtsHotkey, VtsParameter, VtsParameterList};

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use ai_ex_core::AvatarPort;
use ai_ex_domain::{AppError, ComponentHealth, Emotion};
use ai_ex_stage::{StageAction, StageCapability, StageExecutor};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

#[derive(Debug, Clone)]
pub struct VtsSettings {
    pub host: String,
    pub port: u16,
    pub token_path: PathBuf,
    pub plugin_name: String,
    pub developer: String,
    pub expression_hotkeys: BTreeMap<String, String>,
}

#[derive(Debug)]
enum Command {
    Mouth(f64),
    Hotkey(String),
    DiscoverHotkeys(oneshot::Sender<Result<Vec<VtsHotkey>, AppError>>),
    DiscoverParameters(oneshot::Sender<Result<VtsParameterList, AppError>>),
}

#[derive(Clone)]
pub struct VtsClient {
    sender: Option<mpsc::Sender<Command>>,
    health: ComponentHealth,
    expression_hotkeys: BTreeMap<String, String>,
}

impl VtsClient {
    pub fn disabled() -> Self {
        Self {
            sender: None,
            health: ComponentHealth::unavailable("vts", "disabled"),
            expression_hotkeys: BTreeMap::new(),
        }
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            sender: None,
            health: ComponentHealth::unavailable("vts", detail),
            expression_hotkeys: BTreeMap::new(),
        }
    }

    pub async fn connect(settings: VtsSettings) -> Result<Self, AppError> {
        let token = read_token(&settings.token_path).await?;
        let endpoint = format!("ws://{}:{}", settings.host, settings.port);
        let (mut socket, _response) = connect_async(&endpoint)
            .await
            .map_err(|error| AppError::connectivity(error.to_string()))?;

        let request = protocol::authentication(&token, &settings.plugin_name, &settings.developer);
        socket
            .send(Message::Text(request.to_string().into()))
            .await
            .map_err(|error| AppError::connectivity(error.to_string()))?;
        authenticate_response(&mut socket).await?;

        let (sender, receiver) = mpsc::channel(64);
        tokio::spawn(run_actor(socket, receiver));
        Ok(Self {
            sender: Some(sender),
            health: ComponentHealth::ready("vts"),
            expression_hotkeys: settings.expression_hotkeys,
        })
    }

    pub fn health(&self) -> &ComponentHealth {
        &self.health
    }

    pub async fn trigger_hotkey(&self, id: impl Into<String>) -> Result<(), AppError> {
        self.send(Command::Hotkey(id.into())).await
    }

    /// Reads the current model's hotkeys without triggering any action.
    pub async fn discover_hotkeys(&self) -> Result<Vec<VtsHotkey>, AppError> {
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| AppError::unavailable("VTS is not connected"))?;
        let (reply, result) = oneshot::channel();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            sender
                .send(Command::DiscoverHotkeys(reply))
                .await
                .map_err(|_| AppError::unavailable("VTS actor stopped"))?;
            result
                .await
                .map_err(|_| AppError::unavailable("VTS closed during hotkey discovery"))?
        })
        .await
        .map_err(|_| AppError::connectivity("VTS hotkey discovery timed out"))?
    }

    /// Reads available tracking parameters without changing their values.
    pub async fn discover_parameters(&self) -> Result<VtsParameterList, AppError> {
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| AppError::unavailable("VTS is not connected"))?;
        let (reply, result) = oneshot::channel();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            sender
                .send(Command::DiscoverParameters(reply))
                .await
                .map_err(|_| AppError::unavailable("VTS actor stopped"))?;
            result
                .await
                .map_err(|_| AppError::unavailable("VTS closed during parameter discovery"))?
        })
        .await
        .map_err(|_| AppError::connectivity("VTS parameter discovery timed out"))?
    }

    async fn send(&self, command: Command) -> Result<(), AppError> {
        let Some(sender) = &self.sender else {
            return Ok(());
        };
        sender
            .send(command)
            .await
            .map_err(|_| AppError::unavailable("VTS actor stopped"))
    }
}

#[async_trait]
impl AvatarPort for VtsClient {
    async fn set_speaking(&mut self, speaking: bool) -> Result<(), AppError> {
        let value = if speaking { 0.65 } else { 0.0 };
        self.send(Command::Mouth(value)).await
    }

    async fn set_neutral(&mut self) -> Result<(), AppError> {
        self.send(Command::Mouth(0.0)).await?;
        self.set_emotion(Emotion::Neutral).await
    }

    async fn set_emotion(&mut self, emotion: Emotion) -> Result<(), AppError> {
        let Some(hotkey) = self.expression_hotkeys.get(emotion.as_str()).cloned() else {
            return Ok(());
        };
        self.send(Command::Hotkey(hotkey)).await
    }
}

#[async_trait]
impl StageExecutor for VtsClient {
    fn capabilities(&self) -> BTreeSet<StageCapability> {
        BTreeSet::from([
            StageCapability::Expression,
            StageCapability::Hotkey,
            StageCapability::Interrupt,
            StageCapability::Mouth,
        ])
    }

    async fn health(&self) -> ComponentHealth {
        self.health.clone()
    }

    async fn execute(&mut self, action: StageAction) -> Result<(), AppError> {
        action.validate()?;
        match action {
            StageAction::Expression { emotion } => AvatarPort::set_emotion(self, emotion).await,
            StageAction::Mouth { value } => self.send(Command::Mouth(value.into())).await,
            StageAction::Hotkey { id } => self.trigger_hotkey(id).await,
            StageAction::Stop => AvatarPort::set_neutral(self).await,
            StageAction::Speak { .. }
            | StageAction::Subtitle { .. }
            | StageAction::Scene { .. } => Err(AppError::configuration(
                "VTS stage does not support this action",
            )),
        }
    }

    async fn interrupt(&mut self) -> Result<(), AppError> {
        AvatarPort::set_neutral(self).await
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum PendingDiscovery {
    Hotkeys(oneshot::Sender<Result<Vec<VtsHotkey>, AppError>>),
    Parameters(oneshot::Sender<Result<VtsParameterList, AppError>>),
}

impl PendingDiscovery {
    fn is_closed(&self) -> bool {
        match self {
            Self::Hotkeys(reply) => reply.is_closed(),
            Self::Parameters(reply) => reply.is_closed(),
        }
    }

    fn respond(self, response: &Value) {
        match self {
            Self::Hotkeys(reply) => {
                let _ignored = reply.send(discovery::hotkeys(response));
            }
            Self::Parameters(reply) => {
                let _ignored = reply.send(discovery::parameters(response));
            }
        }
    }
}

async fn read_token(path: &PathBuf) -> Result<String, AppError> {
    let content = tokio::fs::read_to_string(path).await.map_err(|error| {
        AppError::configuration(format!("cannot read VTS token {}: {error}", path.display()))
    })?;
    let document: Value = serde_json::from_str(&content)
        .map_err(|error| AppError::configuration(format!("invalid VTS token JSON: {error}")))?;
    document
        .get("token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AppError::configuration("VTS token file requires a string 'token'"))
}

async fn authenticate_response(socket: &mut Socket) -> Result<(), AppError> {
    let response = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
        .await
        .map_err(|_| AppError::connectivity("VTS authentication timed out"))?
        .ok_or_else(|| AppError::connectivity("VTS closed during authentication"))?
        .map_err(|error| AppError::connectivity(error.to_string()))?;
    let text = response
        .into_text()
        .map_err(|error| AppError::protocol(error.to_string()))?;
    let document: Value =
        serde_json::from_str(&text).map_err(|error| AppError::protocol(error.to_string()))?;
    if !protocol::authenticated(&document) {
        return Err(AppError::protocol(format!(
            "VTS rejected authentication: {}",
            document.get("data").cloned().unwrap_or(Value::Null)
        )));
    }
    Ok(())
}

async fn run_actor(socket: Socket, mut receiver: mpsc::Receiver<Command>) {
    let (mut writer, mut reader) = socket.split();
    let mut pending = BTreeMap::<String, PendingDiscovery>::new();
    let mut cleanup = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tokio::select! {
            command = receiver.recv() =>
            {
                let Some(command) = command else
                {
                    break;
                };
                let request = match command
                {
                    Command::Mouth(value) => protocol::mouth_open(value),
                    Command::Hotkey(id) => protocol::trigger_hotkey(&id),
                    Command::DiscoverHotkeys(reply) => {
                        pending.retain(|_, reply| !reply.is_closed());
                        if pending.len() >= 64 {
                            let _ignored = reply.send(Err(AppError::unavailable("VTS hotkey discovery queue is full")));
                            continue;
                        }
                        let request = protocol::discover_hotkeys();
                        let id = request["requestID"].as_str().expect("request ID").to_owned();
                        pending.insert(id, PendingDiscovery::Hotkeys(reply));
                        request
                    },
                    Command::DiscoverParameters(reply) => {
                        pending.retain(|_, reply| !reply.is_closed());
                        if pending.len() >= 64 {
                            let _ignored = reply.send(Err(AppError::unavailable("VTS discovery queue is full")));
                            continue;
                        }
                        let request = protocol::discover_parameters();
                        let id = request["requestID"].as_str().expect("request ID").to_owned();
                        pending.insert(id, PendingDiscovery::Parameters(reply));
                        request
                    },
                };
                if writer.send(Message::Text(request.to_string().into())).await.is_err()
                {
                    break;
                }
            }
            message = reader.next() =>
            {
                match message
                {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(response) = serde_json::from_str::<Value>(&text)
                            && let Some(id) = response.get("requestID").and_then(Value::as_str)
                            && let Some(reply) = pending.remove(id) {
                            reply.respond(&response);
                        }
                    },
                    _ =>
                    {
                    }
                }
            }
            _ = cleanup.tick() => pending.retain(|_, reply| !reply.is_closed()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_vts_exposes_only_supported_stage_actions() {
        let mut client = VtsClient::disabled();
        let capabilities = StageExecutor::capabilities(&client);
        assert!(capabilities.contains(&StageCapability::Expression));
        assert!(!capabilities.contains(&StageCapability::Speech));
        StageExecutor::execute(
            &mut client,
            StageAction::Expression {
                emotion: Emotion::Happy,
            },
        )
        .await
        .expect("disabled VTS safely ignores expression output");
        let error = StageExecutor::execute(
            &mut client,
            StageAction::Subtitle {
                text: "hello".to_owned(),
                duration_ms: 1_000,
            },
        )
        .await
        .expect_err("VTS must reject subtitle actions");
        assert!(error.to_string().contains("VTS"));
    }
}
