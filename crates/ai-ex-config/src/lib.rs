#![forbid(unsafe_code)]

pub mod appearance;
pub mod character;
pub mod scene;

#[cfg(test)]
mod memory_embedding_tests;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;

use tokio::io::AsyncReadExt;

use ai_ex_domain::{AppError, Emotion, LiveResponseMode};
use serde::{Deserialize, Serialize};

/// Upper bound for startup configuration, including unknown extension fields.
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub desktop: DesktopConfig,
    pub model: ModelConfig,
    pub conversation: ConversationConfig,
    pub persona: PersonaConfig,
    pub deepseek: DeepSeekConfig,
    pub ollama: OllamaConfig,
    pub koboldcpp: KoboldCppConfig,
    pub vts: VtsConfig,
    pub obs: ObsConfig,
    pub audio: AudioConfig,
    pub memory: MemoryConfig,
    pub tts: TtsConfig,
    pub duplex: DuplexConfig,
    pub control: ControlConfig,
    pub vision: VisionConfig,
    pub plugins: PluginConfig,
    pub bilibili: BilibiliConfig,
    pub safety: SafetyConfig,
}

impl AppConfig {
    pub fn to_toml(&self) -> Result<String, AppError> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(|error| {
            AppError::configuration(format!("cannot serialize configuration: {error}"))
        })
    }

    pub fn merge_toml(&self, source: &str) -> Result<String, AppError> {
        if source.len() > MAX_CONFIG_BYTES {
            return Err(AppError::configuration("configuration exceeds 1 MiB"));
        }
        let mut document: toml::Value = toml::from_str(source).map_err(|error| {
            AppError::configuration(format!(
                "invalid existing TOML at byte range {:?}",
                error.span()
            ))
        })?;
        let updated = toml::from_str(&self.to_toml()?)
            .map_err(|error| AppError::configuration(format!("cannot merge TOML: {error}")))?;
        let previous = toml::from_str(&Self::parse(source)?.to_toml()?).map_err(|error| {
            AppError::configuration(format!("cannot read previous TOML: {error}"))
        })?;
        merge_values(&mut document, updated, Some(&previous));
        toml::to_string_pretty(&document)
            .map_err(|error| AppError::configuration(format!("cannot serialize TOML: {error}")))
    }
    pub async fn load(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let path = path.as_ref();
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|error| AppError::configuration(format!("cannot open config: {error}")))?;
        let mut content = Vec::new();
        file.take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut content)
            .await
            .map_err(|error| AppError::configuration(format!("cannot read config: {error}")))?;
        if content.len() > MAX_CONFIG_BYTES {
            return Err(AppError::configuration("configuration exceeds 1 MiB"));
        }
        let content = std::str::from_utf8(&content)
            .map_err(|_| AppError::configuration("configuration is not valid UTF-8"))?;
        Self::parse(content)
    }

    pub fn parse(content: &str) -> Result<Self, AppError> {
        if content.len() > MAX_CONFIG_BYTES {
            return Err(AppError::configuration("configuration exceeds 1 MiB"));
        }
        let config: Self = toml::from_str(content).map_err(|error| {
            AppError::configuration(format!("invalid TOML at byte range {:?}", error.span()))
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn effective_system_prompt(&self) -> String {
        let base = if self.persona.system_prompt.trim().is_empty() {
            self.conversation.system_prompt.trim().to_owned()
        } else {
            self.persona.system_prompt.trim().to_owned()
        };
        let mut prompt = format!("角色名：{}\n语气：{}", self.persona.name, self.persona.tone);
        if !base.is_empty() {
            prompt.push('\n');
            prompt.push_str(&base);
        }
        if !self.persona.taboos.is_empty() {
            prompt.push_str("\n禁忌：");
            prompt.push_str(&self.persona.taboos.join("；"));
        }
        prompt
    }

    pub fn validate(&self) -> Result<(), AppError> {
        self.conversation.validate()?;
        self.persona.validate()?;
        if let Some(embedding) = &self.memory.embedding {
            embedding.validate()?;
        }
        if self.model.backend == ModelBackend::DeepSeek {
            self.deepseek.validate()?;
        }
        if self.model.backend == ModelBackend::Ollama && !is_http_url(&self.ollama.base_url) {
            return Err(AppError::configuration(
                "ollama.base_url must be HTTP or HTTPS",
            ));
        }
        if self.model.backend == ModelBackend::Ollama && self.ollama.model.trim().is_empty() {
            return Err(AppError::configuration("ollama.model must not be empty"));
        }
        if self.model.backend == ModelBackend::KoboldCpp {
            self.koboldcpp.validate()?;
        }
        if self.obs.host.trim().is_empty() || self.obs.port == 0 || self.obs.timeout_seconds == 0 {
            return Err(AppError::configuration(
                "obs host, port, and timeout must be valid",
            ));
        }
        if self.obs.enabled && self.obs.password_env.trim().is_empty() {
            return Err(AppError::configuration("enabled OBS requires password_env"));
        }
        if self.obs.subtitle_input.chars().count() > 256 {
            return Err(AppError::configuration("obs.subtitle_input is too long"));
        }
        if self.vts.host.trim().is_empty() {
            return Err(AppError::configuration("vts.host must not be empty"));
        }
        if self.vts.enabled && self.vts.token_path.trim().is_empty() {
            return Err(AppError::configuration("vts.token_path must not be empty"));
        }
        for (emotion, hotkey) in &self.vts.expression_hotkeys {
            if Emotion::parse(emotion).is_none() || hotkey.trim().is_empty() {
                return Err(AppError::configuration(
                    "vts.expression_hotkeys contains an invalid mapping",
                ));
            }
        }
        if self.safety.emergency_hotkey.trim().is_empty() {
            return Err(AppError::configuration(
                "safety.emergency_hotkey must not be empty",
            ));
        }
        if self.safety.automation_enabled && self.safety.audit_path.trim().is_empty() {
            return Err(AppError::configuration(
                "enabled automation requires safety.audit_path",
            ));
        }
        if self.memory.enabled && self.memory.path.trim().is_empty() {
            return Err(AppError::configuration("memory.path must not be empty"));
        }
        if self.tts.enabled
            && (self.tts.base_url.trim().is_empty() || self.tts.ref_audio_path.trim().is_empty())
        {
            return Err(AppError::configuration(
                "enabled TTS requires base_url and ref_audio_path",
            ));
        }
        if self.audio.queue_capacity == 0 {
            return Err(AppError::configuration(
                "audio.queue_capacity must be positive",
            ));
        }
        self.duplex.validate()?;
        self.control.validate()?;
        self.vision.validate()?;
        self.plugins.validate()?;
        self.bilibili.validate()?;
        Ok(())
    }
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopConfig {
    pub auto_start_service: bool,
}

fn merge_values(original: &mut toml::Value, updated: toml::Value, previous: Option<&toml::Value>) {
    if let (toml::Value::Table(target), toml::Value::Table(changes)) = (&mut *original, &updated) {
        if let Some(toml::Value::Table(previous)) = previous {
            for key in previous.keys().filter(|key| !changes.contains_key(*key)) {
                target.remove(key);
            }
        }
        for (key, value) in changes {
            if let Some(existing) = target.get_mut(key) {
                merge_values(
                    existing,
                    value.clone(),
                    previous.and_then(|table| table.get(key)),
                );
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    } else {
        *original = updated;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelBackend {
    #[default]
    Ollama,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "koboldcpp")]
    KoboldCpp,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub backend: ModelBackend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonaConfig {
    pub profile_id: String,
    pub revision: u64,
    pub name: String,
    pub system_prompt: String,
    pub tone: String,
    pub taboos: Vec<String>,
    pub live_mode: String,
}

impl PersonaConfig {
    fn validate(&self) -> Result<(), AppError> {
        if self.profile_id.trim().is_empty()
            || self.profile_id.chars().count() > 128
            || self.revision == 0
            || self.name.trim().is_empty()
            || self.name.chars().count() > 128
            || self.system_prompt.chars().count() > 16_384
            || self.tone.chars().count() > 512
            || self.taboos.iter().any(|item| item.chars().count() > 512)
        {
            return Err(AppError::configuration(
                "persona configuration is outside supported bounds",
            ));
        }
        Ok(())
    }
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            profile_id: "default".to_owned(),
            revision: 1,
            name: "AIex".to_owned(),
            system_prompt: String::new(),
            tone: "warm, concise, and curious".to_owned(),
            taboos: Vec::new(),
            live_mode: "controlled".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConversationConfig {
    pub system_prompt: String,
    pub history_turn_limit: usize,
    pub memory_recall_limit: usize,
}

impl ConversationConfig {
    fn validate(&self) -> Result<(), AppError> {
        if self.system_prompt.chars().count() > 16_384
            || !(1..=128).contains(&self.history_turn_limit)
            || self.memory_recall_limit > 64
        {
            return Err(AppError::configuration(
                "conversation policy is outside supported bounds",
            ));
        }
        Ok(())
    }
}

impl Default for ConversationConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            history_turn_limit: 12,
            memory_recall_limit: 6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:11434".to_owned(),
            model: "llama3.2:latest".to_owned(),
            timeout_seconds: 120,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DeepSeekConfig {
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    pub timeout_seconds: u64,
    pub thinking: bool,
    pub reasoning_effort: String,
}

impl DeepSeekConfig {
    fn validate(&self) -> Result<(), AppError> {
        if !is_http_url(&self.base_url)
            || self.model.trim().is_empty()
            || self.api_key_env.trim().is_empty()
            || self.timeout_seconds == 0
        {
            return Err(AppError::configuration(
                "DeepSeek requires an HTTP base URL, model, API key environment name, and timeout",
            ));
        }
        if !matches!(self.reasoning_effort.as_str(), "high" | "max") {
            return Err(AppError::configuration(
                "deepseek.reasoning_effort must be high or max",
            ));
        }
        Ok(())
    }
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.deepseek.com".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            api_key_env: "DEEPSEEK_API_KEY".to_owned(),
            timeout_seconds: 120,
            thinking: false,
            reasoning_effort: "high".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KoboldCppConfig {
    pub base_url: String,
    pub timeout_seconds: u64,
    pub max_context_length: usize,
    pub max_length: usize,
    pub temperature: f32,
}

impl KoboldCppConfig {
    fn validate(&self) -> Result<(), AppError> {
        if !is_http_url(&self.base_url) {
            return Err(AppError::configuration(
                "koboldcpp.base_url must be HTTP or HTTPS",
            ));
        }
        if self.timeout_seconds == 0 || self.max_context_length == 0 || self.max_length == 0 {
            return Err(AppError::configuration(
                "KoboldCpp timeout and token limits must be positive",
            ));
        }
        if !self.temperature.is_finite() || !(0.0..=2.0).contains(&self.temperature) {
            return Err(AppError::configuration(
                "koboldcpp.temperature must be between 0 and 2",
            ));
        }
        Ok(())
    }
}

impl Default for KoboldCppConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:5001".to_owned(),
            timeout_seconds: 120,
            max_context_length: 2_048,
            max_length: 256,
            temperature: 0.7,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VtsConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub token_path: String,
    pub plugin_name: String,
    pub developer: String,
    pub expression_hotkeys: BTreeMap<String, String>,
}

impl Default for VtsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: "127.0.0.1".to_owned(),
            port: 8001,
            token_path: "token.json".to_owned(),
            plugin_name: "AIex Rust".to_owned(),
            developer: "AIex contributors".to_owned(),
            expression_hotkeys: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ObsConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub password_env: String,
    pub subtitle_input: String,
    pub timeout_seconds: u64,
}

impl Default for ObsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".to_owned(),
            port: 4455,
            password_env: "OBS_WEBSOCKET_PASSWORD".to_owned(),
            subtitle_input: "AIexSubtitle".to_owned(),
            timeout_seconds: 10,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub queue_capacity: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { queue_capacity: 32 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DuplexConfig {
    pub enabled: bool,
    pub input_device: String,
    pub capture_queue_capacity: usize,
    pub start_threshold: f32,
    pub continue_threshold: f32,
    pub start_frames: usize,
    pub end_silence_frames: usize,
    pub max_utterance_frames: usize,
    pub asr: AsrConfig,
}

impl DuplexConfig {
    fn validate(&self) -> Result<(), AppError> {
        if !self.start_threshold.is_finite()
            || !self.continue_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.start_threshold)
            || !(0.0..=1.0).contains(&self.continue_threshold)
            || self.continue_threshold > self.start_threshold
        {
            return Err(AppError::configuration("duplex VAD thresholds are invalid"));
        }
        if self.start_frames == 0
            || self.end_silence_frames == 0
            || self.max_utterance_frames == 0
            || self.capture_queue_capacity == 0
        {
            return Err(AppError::configuration(
                "duplex VAD frame counts must be positive",
            ));
        }
        if self.enabled {
            self.asr.validate()?;
        }
        Ok(())
    }
}

impl Default for DuplexConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            input_device: String::new(),
            capture_queue_capacity: 64,
            start_threshold: 0.025,
            continue_threshold: 0.012,
            start_frames: 3,
            end_silence_frames: 8,
            max_utterance_frames: 3_000,
            asr: AsrConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AsrConfig {
    pub endpoint: String,
    pub model: String,
    pub language: String,
    pub timeout_seconds: u64,
}

impl AsrConfig {
    fn validate(&self) -> Result<(), AppError> {
        if !self.endpoint.starts_with("http://") && !self.endpoint.starts_with("https://") {
            return Err(AppError::configuration(
                "duplex.asr.endpoint must be HTTP or HTTPS",
            ));
        }
        if self.model.trim().is_empty() || self.timeout_seconds == 0 {
            return Err(AppError::configuration(
                "duplex ASR requires a model and positive timeout",
            ));
        }
        Ok(())
    }
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:8000/v1/audio/transcriptions".to_owned(),
            model: "whisper-1".to_owned(),
            language: "zh".to_owned(),
            timeout_seconds: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ControlConfig {
    pub enabled: bool,
    pub bind: String,
    pub token_path: String,
    pub max_message_bytes: usize,
}

impl ControlConfig {
    fn validate(&self) -> Result<(), AppError> {
        let address: SocketAddr = self
            .bind
            .parse()
            .map_err(|error| AppError::configuration(format!("invalid control.bind: {error}")))?;
        if !address.ip().is_loopback() {
            return Err(AppError::configuration(
                "control.bind must use a loopback address",
            ));
        }
        if self.enabled && self.token_path.trim().is_empty() {
            return Err(AppError::configuration(
                "enabled control server requires token_path",
            ));
        }
        if self.max_message_bytes < 256 {
            return Err(AppError::configuration(
                "control.max_message_bytes must be at least 256",
            ));
        }
        Ok(())
    }
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "127.0.0.1:7878".to_owned(),
            token_path: "config/control.token".to_owned(),
            max_message_bytes: 65_536,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginConfig {
    pub enabled: bool,
    pub commands: Vec<PluginCommandConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCommandConfig {
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
}

impl PluginConfig {
    fn validate(&self) -> Result<(), AppError> {
        if self.commands.len() > 32 {
            return Err(AppError::configuration(
                "plugins.commands must contain at most 32 entries",
            ));
        }
        for command in &self.commands {
            if command.id.trim().is_empty()
                || command.id.chars().count() > 128
                || command.program.trim().is_empty()
                || command.program.chars().count() > 1_024
                || command.args.len() > 64
                || command.args.iter().any(|arg| arg.chars().count() > 4_096)
            {
                return Err(AppError::configuration(
                    "plugin command configuration is outside supported bounds",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VisionConfig {
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
}

impl VisionConfig {
    fn validate(&self) -> Result<(), AppError> {
        if self.enabled
            && ((!self.base_url.starts_with("http://") && !self.base_url.starts_with("https://"))
                || self.model.trim().is_empty()
                || self.timeout_seconds == 0)
        {
            return Err(AppError::configuration(
                "enabled vision configuration is invalid",
            ));
        }
        Ok(())
    }
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://127.0.0.1:11434".to_owned(),
            model: "llava:latest".to_owned(),
            timeout_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BilibiliConfig {
    pub enabled: bool,
    pub room_id: u64,
    pub endpoint: String,
    pub cookie_env: Option<String>,
    pub reconnect_delay_ms: u64,
    pub auto_react: bool,
    pub response_mode: LiveResponseMode,
    pub reaction_cooldown_ms: u64,
}

impl BilibiliConfig {
    pub fn response_mode(&self) -> LiveResponseMode {
        if self.auto_react {
            LiveResponseMode::Automatic
        } else {
            self.response_mode
        }
    }

    fn validate(&self) -> Result<(), AppError> {
        if !self.enabled {
            return Ok(());
        }
        if self.room_id == 0
            || (!self.endpoint.starts_with("ws://") && !self.endpoint.starts_with("wss://"))
            || self.reconnect_delay_ms == 0
            || self.reaction_cooldown_ms == 0
        {
            return Err(AppError::configuration(
                "enabled bilibili configuration is invalid",
            ));
        }
        Ok(())
    }
}

impl Default for BilibiliConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            room_id: 0,
            endpoint: "wss://broadcastlv.chat.bilibili.com:443/sub".to_owned(),
            cookie_env: None,
            reconnect_delay_ms: 2_000,
            auto_react: false,
            response_mode: LiveResponseMode::Suggest,
            reaction_cooldown_ms: 5_000,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    pub automation_enabled: bool,
    pub emergency_hotkey: String,
    pub audit_path: String,
    pub allowed_capabilities: Vec<String>,
    pub allowed_targets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub enabled: bool,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<MemoryEmbeddingConfig>,
}

/// Explicit opt-in semantic model. A new model revision/preprocessing needs a new space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEmbeddingConfig {
    pub base_url: String,
    pub model: String,
    pub space: String,
    pub timeout_seconds: u64,
}

impl MemoryEmbeddingConfig {
    fn validate(&self) -> Result<(), AppError> {
        if !is_http_url(&self.base_url)
            || self.base_url.chars().any(char::is_whitespace)
            || self.base_url.contains(['@', '?', '#'])
            || self.base_url.trim_end_matches('/').ends_with(':')
            || self.model.trim().is_empty()
            || self.model.chars().count() > 256
            || self.space.trim().is_empty()
            || self.space.chars().count() > 256
            || !(1..=300).contains(&self.timeout_seconds)
        {
            return Err(AppError::configuration(
                "memory.embedding requires an HTTP base URL without credentials, model and space of 1..=256 characters, and timeout_seconds in 1..=300",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TtsConfig {
    pub enabled: bool,
    pub base_url: String,
    pub timeout_seconds: u64,
    pub text_lang: String,
    pub ref_audio_path: String,
    pub prompt_text: String,
    pub prompt_lang: String,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://127.0.0.1:9880".to_owned(),
            timeout_seconds: 30,
            text_lang: "zh".to_owned(),
            ref_audio_path: String::new(),
            prompt_text: String::new(),
            prompt_lang: "zh".to_owned(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "memory_db/ai-ex.jsonl".to_owned(),
            embedding: None,
        }
    }
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            automation_enabled: false,
            emergency_hotkey: "F9".to_owned(),
            audit_path: "logs/automation-audit.jsonl".to_owned(),
            allowed_capabilities: Vec::new(),
            allowed_targets: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_configuration() {
        let config = AppConfig::parse(include_str!("../../../config/ai-ex.example.toml"))
            .expect("example configuration must parse");
        assert_eq!(config.vts.port, 8001);
        assert!(!config.obs.enabled);
        assert!(!config.safety.automation_enabled);
        assert!(!config.plugins.enabled);
    }

    #[test]
    fn rejects_enabled_obs_without_password_env() {
        let mut config = AppConfig::default();
        config.obs.enabled = true;
        config.obs.password_env.clear();
        assert!(config.validate().is_err());
    }
    #[test]
    fn supports_explicit_response_modes_and_legacy_auto_react() {
        let mut config = AppConfig::default();
        config.bilibili.response_mode = LiveResponseMode::Confirm;
        assert_eq!(config.bilibili.response_mode(), LiveResponseMode::Confirm);
        config.bilibili.auto_react = true;
        assert_eq!(config.bilibili.response_mode(), LiveResponseMode::Automatic);
    }

    #[test]
    fn validates_bilibili_only_when_enabled() {
        let mut config = AppConfig::default();
        assert!(config.validate().is_ok());
        config.bilibili.enabled = true;
        assert!(config.validate().is_err());
        config.bilibili.room_id = 123;
        assert!(config.validate().is_ok());
        config.bilibili.endpoint = "http://127.0.0.1:1".to_owned();
        assert!(config.validate().is_err());
        config.bilibili.endpoint = "wss://example.invalid/sub".to_owned();
        config.bilibili.reaction_cooldown_ms = 0;
        assert!(config.validate().is_err());
    }
    #[test]
    fn validates_only_the_selected_model_backend() {
        let mut config = AppConfig::default();
        config.model.backend = ModelBackend::KoboldCpp;
        config.koboldcpp.base_url = "file:///unsafe".to_owned();
        assert!(config.validate().is_err());

        config.model.backend = ModelBackend::Ollama;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn parses_koboldcpp_backend_name() {
        let config = AppConfig::parse("[model]\nbackend = \"koboldcpp\"")
            .expect("KoboldCpp configuration parses");

        assert_eq!(config.model.backend, ModelBackend::KoboldCpp);
    }
}

#[cfg(test)]
mod input_boundary_tests {
    use super::*;

    #[test]
    fn syntax_errors_do_not_echo_private_values() {
        let sentinel = "synthetic-private-marker";
        for input in [
            format!("[model]\nbackend = '{sentinel}'"),
            format!("unknown = '{sentinel}\n"),
        ] {
            let error = AppConfig::parse(&input).unwrap_err();
            assert!(!error.to_string().contains(sentinel));
            let error = AppConfig::default().merge_toml(&input).unwrap_err();
            assert!(!error.to_string().contains(sentinel));
        }
    }

    #[tokio::test]
    async fn file_and_in_memory_inputs_enforce_the_same_limit() {
        let oversized = " ".repeat(MAX_CONFIG_BYTES + 1);
        assert!(AppConfig::parse(&oversized).is_err());
        assert!(AppConfig::default().merge_toml(&oversized).is_err());
        let path = std::env::temp_dir().join(format!(
            "aiex-config-boundary-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::write(&path, &oversized).await.unwrap();
        assert!(
            AppConfig::load(&path)
                .await
                .unwrap_err()
                .to_string()
                .contains("1 MiB")
        );
        tokio::fs::write(&path, [0xff]).await.unwrap();
        assert!(
            AppConfig::load(&path)
                .await
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
        tokio::fs::remove_file(path).await.unwrap();
        assert!(AppConfig::parse(&" ".repeat(MAX_CONFIG_BYTES)).is_ok());
    }
}
