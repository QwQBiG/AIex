use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    mpsc::{self, Receiver},
};
use std::thread;
use std::time::Duration;

use ai_ex_config::{AppConfig, ModelBackend};
use ai_ex_domain::AppError;
use eframe::egui;

#[cfg(test)]
#[path = "setup_config_tests.rs"]
mod config_tests;

#[cfg(test)]
#[path = "setup_view_tests.rs"]
mod view_tests;

#[path = "setup_view.rs"]
mod view;

#[derive(Debug, Clone)]
pub struct SetupResult {
    pub config_path: PathBuf,
    pub api_key: Option<String>,
}

pub fn run(
    default_path: PathBuf,
    navigation: crate::navigation::Navigation,
) -> Result<Option<SetupResult>, AppError> {
    let original = if default_path.exists() {
        let text = std::fs::read_to_string(&default_path).map_err(|error| {
            AppError::configuration(format!("cannot read existing configuration: {error}"))
        })?;
        crate::startup::parse_config(&default_path, &text)?;
        Some(text)
    } else {
        None
    };
    let result = Arc::new(Mutex::new(None));
    let shared = Arc::clone(&result);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 620.0])
            .with_min_inner_size([640.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "AIex 连接设置",
        options,
        Box::new(move |context| {
            crate::ui::theme::configure_appearance(&context.egui_ctx);
            let mut app = SetupApp::new(default_path, shared, original);
            app.navigation = navigation;
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| AppError::unavailable(error.to_string()))?;
    let selected = result
        .lock()
        .map_err(|_| AppError::unavailable("setup result lock poisoned"))?
        .clone();
    Ok(selected)
}

struct SetupApp {
    navigation: crate::navigation::Navigation,
    original: Option<String>,
    config_path: PathBuf,
    provider: ProviderChoice,
    model: String,
    endpoint: String,
    api_key: String,
    persona_name: String,
    memory_enabled: bool,
    bilibili_enabled: bool,
    bilibili_room_id: String,
    bilibili_cookie_env: String,
    start_service: bool,
    status: String,
    status_error: bool,
    checking: bool,
    probe_receiver: Option<Receiver<Result<String, String>>>,
    result: Arc<Mutex<Option<SetupResult>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderChoice {
    DeepSeek,
    KoboldCpp,
    Ollama,
}

impl ProviderChoice {
    fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "DeepSeek 云端模型",
            Self::KoboldCpp => "KoboldCpp 本地模型",
            Self::Ollama => "Ollama 本地模型",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::DeepSeek => "云端 API，适合直接开始测试；需要 DEEPSEEK_API_KEY。",
            Self::KoboldCpp => "本地兼容 API，默认 127.0.0.1:5001；需要先启动 KoboldCpp。",
            Self::Ollama => "本地模型服务，默认 127.0.0.1:11434；需要先安装并运行 Ollama。",
        }
    }

    fn model_hint(self) -> &'static str {
        match self {
            Self::DeepSeek => "模型名按账户可用清单填写，例如 deepseek-v4-flash。",
            Self::KoboldCpp => "填写 KoboldCpp 当前加载的模型标识；服务会使用已加载模型。",
            Self::Ollama => "填写本机已安装的模型名，例如 llama3.2:latest。",
        }
    }
}

impl SetupApp {
    fn new(
        config_path: PathBuf,
        result: Arc<Mutex<Option<SetupResult>>>,
        original: Option<String>,
    ) -> Self {
        let mut app = Self {
            navigation: Default::default(),
            original,
            config_path,
            provider: ProviderChoice::DeepSeek,
            model: "deepseek-v4-flash".to_owned(),
            endpoint: "https://api.deepseek.com".to_owned(),
            api_key: String::new(),
            persona_name: "AIex".to_owned(),
            memory_enabled: true,
            bilibili_enabled: false,
            bilibili_room_id: String::new(),
            bilibili_cookie_env: "BILIBILI_COOKIE".to_owned(),
            start_service: true,
            status: String::new(),
            status_error: false,
            checking: false,
            probe_receiver: None,
            result,
        };
        if let Some(config) = app
            .original
            .as_deref()
            .and_then(|text| AppConfig::parse(text).ok())
        {
            app.provider = match config.model.backend {
                ModelBackend::DeepSeek => ProviderChoice::DeepSeek,
                ModelBackend::KoboldCpp => ProviderChoice::KoboldCpp,
                ModelBackend::Ollama => ProviderChoice::Ollama,
            };
            let (endpoint, model) = match app.provider {
                ProviderChoice::DeepSeek => (config.deepseek.base_url, config.deepseek.model),
                ProviderChoice::KoboldCpp => (config.koboldcpp.base_url, "koboldcpp".to_owned()),
                ProviderChoice::Ollama => (config.ollama.base_url, config.ollama.model),
            };
            app.endpoint = endpoint;
            app.model = model;
            app.persona_name = config.persona.name;
            app.memory_enabled = config.memory.enabled;
            app.bilibili_enabled = config.bilibili.enabled;
            app.bilibili_room_id = config.bilibili.room_id.to_string();
            app.bilibili_cookie_env = config.bilibili.cookie_env.unwrap_or_default();
            app.start_service = config.desktop.auto_start_service;
        }
        app
    }

    fn provider_changed(&mut self) {
        match self.provider {
            ProviderChoice::DeepSeek => {
                self.endpoint = "https://api.deepseek.com".to_owned();
                self.model = "deepseek-v4-flash".to_owned();
            }
            ProviderChoice::KoboldCpp => {
                self.endpoint = "http://127.0.0.1:5001".to_owned();
                self.model = "koboldcpp".to_owned();
            }
            ProviderChoice::Ollama => {
                self.endpoint = "http://127.0.0.1:11434".to_owned();
                self.model = "llama3.2:latest".to_owned();
            }
        }
    }

    fn set_status(&mut self, message: impl Into<String>, error: bool) {
        self.status = message.into();
        self.status_error = error;
    }

    fn poll_probe(&mut self) {
        let Some(receiver) = self.probe_receiver.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(message)) => {
                self.checking = false;
                self.set_status(message, false);
            }
            Ok(Err(error)) => {
                self.checking = false;
                self.set_status(format!("连接检查失败：{error}"), true);
            }
            Err(mpsc::TryRecvError::Empty) => self.probe_receiver = Some(receiver),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.checking = false;
                self.set_status("连接检查线程已停止，请重试。", true);
            }
        }
    }

    fn check_connection(&mut self) {
        if self.endpoint.trim().is_empty() {
            self.set_status("请先填写模型地址。", true);
            return;
        }
        if self.provider == ProviderChoice::DeepSeek
            && self.api_key.trim().is_empty()
            && std::env::var_os(self.api_key_env()).is_none()
        {
            self.set_status(
                format!(
                    "DeepSeek 需要 API Key，可填写或设置 {}；密钥不会写入配置文件。",
                    self.api_key_env()
                ),
                true,
            );
            return;
        }
        let endpoint = self.endpoint.trim().to_owned();
        let provider = self.provider;
        let (sender, receiver) = mpsc::channel();
        self.checking = true;
        self.set_status("正在检查地址和网络端口……", false);
        self.probe_receiver = Some(receiver);
        thread::spawn(move || {
            let result = probe_endpoint(provider, &endpoint).map_err(|error| error.to_string());
            let _ignored = sender.send(result);
        });
    }
    fn save(&mut self, context: &egui::Context) {
        if self.persona_name.trim().is_empty()
            || self.endpoint.trim().is_empty()
            || self.model.trim().is_empty()
        {
            self.set_status("请填写角色名、模型地址和模型名称。", true);
            return;
        }
        if self.provider == ProviderChoice::DeepSeek
            && self.api_key.trim().is_empty()
            && std::env::var_os(self.api_key_env()).is_none()
        {
            self.set_status(
                format!(
                    "DeepSeek 需要 API Key；可以填写或设置 {} 环境变量。密钥不会写入配置文件。",
                    self.api_key_env()
                ),
                true,
            );
            return;
        }
        let bilibili_room_id = if self.bilibili_enabled {
            match self.bilibili_room_id.trim().parse::<u64>() {
                Ok(room_id) if room_id > 0 => room_id,
                _ => {
                    self.set_status("启用 Bilibili 时必须填写大于 0 的房间号。", true);
                    return;
                }
            }
        } else {
            0
        };

        let result = self.config_text(bilibili_room_id).and_then(|document| {
            crate::setup_storage::save(&self.config_path, &document, self.original.as_deref())
        });
        match result {
            Ok(()) => {
                let api_key = if self.api_key.trim().is_empty() {
                    None
                } else {
                    Some(self.api_key.trim().to_owned())
                };
                if let Ok(mut target) = self.result.lock() {
                    *target = Some(SetupResult {
                        config_path: self.config_path.clone(),
                        api_key,
                    });
                }
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(error) => self.set_status(format!("保存失败：{error}"), true),
        }
    }

    fn api_key_env(&self) -> String {
        self.original
            .as_deref()
            .and_then(|source| AppConfig::parse(source).ok())
            .unwrap_or_default()
            .deepseek
            .api_key_env
    }

    fn config_text(&self, bilibili_room_id: u64) -> Result<String, AppError> {
        let mut config = self
            .original
            .as_deref()
            .map(AppConfig::parse)
            .transpose()?
            .unwrap_or_default();
        if self.original.is_none() {
            let path = std::path::absolute(&self.config_path).map_err(|error| {
                AppError::configuration(format!("invalid configuration path: {error}"))
            })?;
            config.control.token_path = path
                .with_file_name("control.token")
                .to_string_lossy()
                .into_owned();
            config.vts.enabled = false;
        }
        config.control.enabled = true;
        config.memory.enabled = self.memory_enabled;
        config.desktop.auto_start_service = self.start_service;
        if self.original.is_some() && config.persona.name != self.persona_name.trim() {
            config.persona.revision = config
                .persona
                .revision
                .checked_add(1)
                .ok_or_else(|| AppError::configuration("persona revision limit reached"))?;
        }
        config.persona.name = self.persona_name.trim().to_owned();
        config.bilibili.enabled = self.bilibili_enabled;
        if self.bilibili_enabled {
            config.bilibili.room_id = bilibili_room_id;
        }
        config.bilibili.cookie_env = (!self.bilibili_cookie_env.trim().is_empty())
            .then(|| self.bilibili_cookie_env.trim().to_owned());
        match self.provider {
            ProviderChoice::DeepSeek => {
                config.model.backend = ModelBackend::DeepSeek;
                config.deepseek.base_url = self.endpoint.trim().to_owned();
                config.deepseek.model = self.model.trim().to_owned();
            }
            ProviderChoice::KoboldCpp => {
                config.model.backend = ModelBackend::KoboldCpp;
                config.koboldcpp.base_url = self.endpoint.trim().to_owned();
            }
            ProviderChoice::Ollama => {
                config.model.backend = ModelBackend::Ollama;
                config.ollama.base_url = self.endpoint.trim().to_owned();
                config.ollama.model = self.model.trim().to_owned();
            }
        }
        match self.original.as_deref() {
            Some(original) => config.merge_toml(original),
            None => config.to_toml(),
        }
    }
}

fn probe_endpoint(provider: ProviderChoice, endpoint: &str) -> Result<String, AppError> {
    let endpoint = endpoint.trim();
    let (scheme, remainder) = endpoint
        .split_once("://")
        .ok_or_else(|| AppError::configuration("模型地址必须以 http:// 或 https:// 开头"))?;
    if scheme != "http" && scheme != "https" {
        return Err(AppError::configuration("模型地址只支持 HTTP 或 HTTPS"));
    }
    let authority = remainder
        .split(['/', '?', '#'])
        .next()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::configuration("模型地址缺少主机名"))?;
    let (host, port) = endpoint_host_port(scheme, authority)?;
    let address = format!("{host}:{port}");
    let socket = address
        .to_socket_addrs()
        .map_err(|error| AppError::unavailable(format!("无法解析模型地址 {address}: {error}")))?
        .next()
        .ok_or_else(|| AppError::unavailable(format!("模型地址没有可用网络地址：{address}")))?;
    let mut stream = TcpStream::connect_timeout(&socket, Duration::from_secs(5))
        .map_err(|error| AppError::unavailable(format!("无法连接 {address}: {error}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| AppError::unavailable(format!("设置连接超时失败：{error}")))?;
    if scheme == "https" {
        return Ok(format!(
            "{}：网络端口可达；HTTPS/API 密钥将在服务启动时继续验证。",
            provider.label()
        ));
    }
    let request = format!("GET / HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| AppError::unavailable(format!("发送连通性请求失败：{error}")))?;
    let mut buffer = [0_u8; 256];
    let count = stream
        .read(&mut buffer)
        .map_err(|error| AppError::unavailable(format!("读取模型服务响应失败：{error}")))?;
    let response = String::from_utf8_lossy(&buffer[..count]);
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| AppError::protocol("模型服务返回的 HTTP 响应无法识别"))?;
    if status >= 500 {
        return Err(AppError::unavailable(format!("模型服务返回 HTTP {status}")));
    }
    Ok(format!("{}：服务已响应 HTTP {status}。", provider.label()))
}

fn endpoint_host_port(scheme: &str, authority: &str) -> Result<(String, u16), AppError> {
    if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or_else(|| AppError::configuration("IPv6 模型地址缺少右方括号"))?;
        let host = authority[1..end].to_owned();
        let port = authority
            .get(end + 1..)
            .and_then(|value| value.strip_prefix(':'))
            .map(parse_port)
            .transpose()?
            .unwrap_or_else(|| default_port(scheme));
        return Ok((format!("[{host}]"), port));
    }
    if let Some((host, port)) = authority.rsplit_once(':')
        && !host.is_empty()
    {
        return Ok((host.to_owned(), parse_port(port)?));
    }
    Ok((authority.to_owned(), default_port(scheme)))
}

fn parse_port(value: &str) -> Result<u16, AppError> {
    let port = value
        .parse::<u16>()
        .map_err(|_| AppError::configuration("模型地址端口必须是 1 到 65535"))?;
    if port == 0 {
        return Err(AppError::configuration("模型地址端口不能为 0"));
    }
    Ok(port)
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}
impl eframe::App for SetupApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_window(ui);
    }
}
#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn endpoint_parser_uses_provider_defaults() {
        assert_eq!(
            endpoint_host_port("http", "127.0.0.1").expect("default HTTP port"),
            ("127.0.0.1".to_owned(), 80)
        );
        assert_eq!(
            endpoint_host_port("https", "api.deepseek.com").expect("default HTTPS port"),
            ("api.deepseek.com".to_owned(), 443)
        );
        assert_eq!(
            endpoint_host_port("http", "127.0.0.1:5001").expect("explicit port"),
            ("127.0.0.1".to_owned(), 5001)
        );
    }

    #[test]
    fn provider_help_explains_local_and_cloud_requirements() {
        assert!(
            ProviderChoice::DeepSeek
                .description()
                .contains("DEEPSEEK_API_KEY")
        );
        assert!(ProviderChoice::KoboldCpp.description().contains("5001"));
        assert!(ProviderChoice::Ollama.description().contains("Ollama"));
        assert!(ProviderChoice::Ollama.model_hint().contains("已安装"));
    }
    #[test]
    fn probe_endpoint_reports_local_http_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener binds");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request accepts");
            let mut request = [0_u8; 128];
            let _ignored = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .expect("response writes");
        });
        let message = probe_endpoint(ProviderChoice::KoboldCpp, &format!("http://{}", address))
            .expect("local service responds");
        assert!(message.contains("HTTP 200"));
        server.join().expect("server joins");
    }

    #[test]
    fn probe_endpoint_rejects_missing_scheme() {
        assert!(probe_endpoint(ProviderChoice::Ollama, "127.0.0.1:11434").is_err());
    }
}
