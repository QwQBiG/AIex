use std::fs::{self, File};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ai_ex_config::AppConfig;
use ai_ex_config::native::NativeConfig;
use ai_ex_control::{ControlClient, ControlCommand, ControlPayload};
use ai_ex_domain::{
    NativeOperation, NativePackage, NativePackageState, NativeReply, NativeRequest,
};

const TOKEN: &str = "native-setup-synthetic-token-not-a-private-secret";

pub(super) struct NativeService {
    pub directory: PathBuf,
    pub native_path: PathBuf,
    pub client: ControlClient,
    child: Option<Child>,
    address: String,
    bootstrap: Vec<u8>,
    attempt: u32,
}

impl NativeService {
    pub fn new(memory: bool) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "native setup 合成-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&directory).unwrap();
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap().to_string();
        let mut config = AppConfig::default();
        config.model.backend = ai_ex_config::ModelBackend::Ollama;
        config.ollama.base_url = "http://127.0.0.1:9".into();
        config.vts.enabled = false;
        config.obs.enabled = false;
        config.tts.enabled = false;
        config.duplex.enabled = false;
        config.bilibili.enabled = false;
        config.vision.enabled = false;
        config.plugins.enabled = false;
        config.safety.automation_enabled = false;
        config.persona.profile_id = "synthetic-companion".into();
        config.persona.name = "合成伙伴".into();
        config.memory.enabled = memory;
        config.memory.path = directory
            .join("memory.jsonl")
            .to_string_lossy()
            .into_owned();
        config.safety.audit_path = directory.join("audit.jsonl").to_string_lossy().into_owned();
        config.control.enabled = true;
        config.control.bind = address.clone();
        config.control.max_message_bytes = 4 * 1024 * 1024;
        config.control.token_path = directory
            .join("control.token")
            .to_string_lossy()
            .into_owned();
        fs::write(&config.control.token_path, TOKEN.as_bytes()).unwrap();
        let bootstrap = config.to_toml().unwrap().into_bytes();
        fs::write(directory.join("bootstrap.toml"), &bootstrap).unwrap();
        drop(reserved);
        let mut service = Self {
            native_path: directory.join("native-v1.toml"),
            directory,
            client: ControlClient::new(&address, TOKEN, 4 * 1024 * 1024).unwrap(),
            child: None,
            address,
            bootstrap,
            attempt: 0,
        };
        service.start();
        service
    }

    pub fn start(&mut self) {
        assert!(self.child.is_none());
        self.attempt += 1;
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-ex-service"));
        command
            .current_dir(&self.directory)
            .args([
                "--managed",
                "--config",
                "bootstrap.toml",
                "--native-setup",
                "--native-config",
                "native-v1.toml",
            ])
            .env("TOKIO_WORKER_THREADS", "2")
            .env_remove("DEEPSEEK_API_KEY")
            .stdin(Stdio::piped())
            .stdout(
                File::create(self.directory.join(format!("stdout-{}.log", self.attempt))).unwrap(),
            )
            .stderr(
                File::create(self.directory.join(format!("stderr-{}.log", self.attempt))).unwrap(),
            );
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000 | 0x0000_4000);
        }
        self.child = Some(command.spawn().unwrap());
    }

    pub fn diagnostics(&self) -> String {
        fs::read_to_string(self.directory.join(format!("stderr-{}.log", self.attempt)))
            .unwrap_or_default()
    }

    pub async fn ready(&mut self, deadline: Duration) {
        tokio::time::timeout(deadline, async {
            loop {
                if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                    panic!("native service exited {status}: {}", self.diagnostics());
                }
                if matches!(
                    self.client.send(ControlCommand::Status).await,
                    Ok(ControlPayload::Snapshot(_))
                ) && matches!(
                    self.client.native(NativeRequest::State).await,
                    Ok(NativeReply::State(_))
                ) && matches!(
                    self.client.native(NativeRequest::Packages).await,
                    Ok(NativeReply::Packages(packages))
                        if packages.iter().all(|package| package.state != NativePackageState::Verifying)
                ) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("native control becomes ready: {}", self.diagnostics()));
    }

    pub async fn request(&self, request: NativeRequest) -> NativeReply {
        self.client.native(request).await.unwrap_or_else(|error| {
            panic!("native request failed: {error}; {}", self.diagnostics())
        })
    }

    pub async fn settings(&self) -> NativeConfig {
        let NativeReply::Settings { revision, document } =
            self.request(NativeRequest::Settings).await
        else {
            panic!("settings reply");
        };
        let config = NativeConfig::parse(&document).unwrap();
        assert_eq!(revision, config.revision);
        assert!(!config.devices.microphone_enabled && !config.devices.playback_enabled);
        config
    }

    pub async fn packages(&self) -> Vec<NativePackage> {
        let NativeReply::Packages(packages) = self.request(NativeRequest::Packages).await else {
            panic!("packages reply");
        };
        packages
    }

    pub async fn operation(&self, id: uuid::Uuid, deadline: Duration) -> NativeOperation {
        tokio::time::timeout(deadline, async {
            loop {
                let NativeReply::Operation(operation) =
                    self.request(NativeRequest::Operation { id }).await
                else {
                    panic!("operation reply");
                };
                if operation.phase.is_terminal() {
                    return operation;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("native operation finishes: {}", self.diagnostics()))
    }

    pub fn bootstrap_unchanged(&self) {
        assert_eq!(
            fs::read(self.directory.join("bootstrap.toml")).unwrap(),
            self.bootstrap
        );
    }

    pub async fn close(&mut self) {
        drop(self.child.as_mut().unwrap().stdin.take());
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "native shutdown: {status}; {}",
                        self.diagnostics()
                    );
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("owned native service shuts down: {}", self.diagnostics()));
        drop(self.child.take());
        self.released().await;
    }

    pub async fn reindex(&mut self) -> (u128, String) {
        self.reindex_path("native-v1.toml", true).await
    }

    pub async fn reindex_path(&mut self, native_path: &str, success: bool) -> (u128, String) {
        assert!(self.child.is_none());
        let config = self.settings_from_disk();
        assert!(!config.devices.microphone_enabled && !config.devices.playback_enabled);
        self.attempt += 1;
        let started = Instant::now();
        let stdout = self.directory.join(format!("stdout-{}.log", self.attempt));
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-ex-service"));
        command
            .current_dir(&self.directory)
            .args([
                "--config",
                "bootstrap.toml",
                "--native-config",
                native_path,
                "--reindex-memory",
            ])
            .env("TOKIO_WORKER_THREADS", "2")
            .env_remove("DEEPSEEK_API_KEY")
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).unwrap())
            .stderr(
                File::create(self.directory.join(format!("stderr-{}.log", self.attempt))).unwrap(),
            );
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000 | 0x0000_4000);
        }
        self.child = Some(command.spawn().unwrap());
        let status = tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("explicit native reindex finishes: {}", self.diagnostics()));
        assert_eq!(
            status.success(),
            success,
            "native reindex {status}: {}",
            self.diagnostics()
        );
        drop(self.child.take());
        self.released().await;
        (
            started.elapsed().as_millis(),
            fs::read_to_string(stdout).unwrap(),
        )
    }

    async fn released(&self) {
        self.bootstrap_unchanged();
        assert!(self.client.send(ControlCommand::Status).await.is_err());
        assert!(TcpListener::bind(&self.address).is_ok());
        let root = self
            .settings_from_disk()
            .storage
            .resolve(&self.native_path)
            .unwrap();
        let lock = File::options()
            .read(true)
            .write(true)
            .open(root.packages.join("manager.lock"))
            .unwrap();
        lock.try_lock()
            .expect("native package lock was released with its owner");
    }

    fn settings_from_disk(&self) -> NativeConfig {
        NativeConfig::parse(&fs::read_to_string(&self.native_path).unwrap()).unwrap()
    }

    pub fn wrong_token_client(&self) -> ControlClient {
        ControlClient::new(&self.address, "x".repeat(32), 4 * 1024 * 1024).unwrap()
    }
}

impl Drop for NativeService {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            drop(child.stdin.take());
            let deadline = Instant::now() + Duration::from_secs(12);
            while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if child.try_wait().ok().flatten().is_none() {
                let _stop = child.kill();
                let _reap = child.wait();
            }
        }
        if std::thread::panicking() {
            eprintln!(
                "native setup fixture retained: {}",
                self.directory.display()
            );
            return;
        }
        let root = fs::canonicalize(&self.directory).unwrap();
        let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
        assert_eq!(
            root.parent(),
            Some(temp.as_path()),
            "cleanup stays in the owned temporary directory"
        );
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("native setup 合成-")
        );
        fs::remove_dir_all(root).unwrap();
    }
}
