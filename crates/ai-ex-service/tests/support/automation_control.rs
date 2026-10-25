use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_ex_control::{ControlClient, ControlCommand, ControlPayload};
use ai_ex_domain::{AutomationRequest, AutomationSessionSnapshot, AutomationSnapshot};
use image::{ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

static NEXT_CASE: AtomicU64 = AtomicU64::new(0);
const TOKEN: &str = "automation-control-synthetic-test-token-only";
const EXISTING_REPORT: &[u8] = b"existing CLI report must remain byte-for-byte unchanged\n";
pub const STRATEGY_PROMPT: &str = "Find the synthetic target and return its original-frame ROI.";

pub struct Service {
    child: Child,
    directory: PathBuf,
    input_frames: u64,
    pub client: ControlClient,
}

impl Service {
    pub fn start(model_url: &str) -> Self {
        Self::start_configured(model_url, None)
    }

    pub fn start_with_strategy(model_url: &str, vision_url: &str) -> Self {
        Self::start_configured(model_url, Some(vision_url))
    }

    fn start_configured(model_url: &str, vision_url: Option<&str>) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "aiex-automation-control-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_CASE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&directory).unwrap();
        fs::create_dir(directory.join("templates")).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap().to_string();
        fs::write(directory.join("control.token"), TOKEN).unwrap();
        let vision_config = vision_url.map_or_else(
            || "[vision]\nenabled = false\n".to_owned(),
            |url| format!("[vision]\nenabled = true\nbase_url = '{url}'\nmodel = 'synthetic-vision'\ntimeout_seconds = 10\n"),
        );
        fs::write(directory.join("service.toml"), format!(
            "[model]\nbackend = 'ollama'\n[ollama]\nbase_url = '{model_url}'\nmodel = 'synthetic-test'\ntimeout_seconds = 30\n\
             [vts]\nenabled = false\n[obs]\nenabled = false\n[tts]\nenabled = false\n\
             [duplex]\nenabled = false\n{vision_config}[bilibili]\nenabled = false\n\
             [memory]\nenabled = false\npath = 'must-not-create-memory.jsonl'\n\
             [control]\nenabled = true\nbind = '{address}'\ntoken_path = 'control.token'\n\
             [safety]\nautomation_enabled = false\nallowed_capabilities = []\n\
             allowed_targets = []\naudit_path = 'audit.jsonl'\n"
        )).unwrap();
        fs::write(
            directory.join("profile.json"),
            serde_json::to_vec(&json!({
                "display_name": "Synthetic control fixture", "description": "No live devices",
                "vlm_prompts": [], "default_templates": {"primary_target": "target.png"},
                "action_cooldowns": {}
            }))
            .unwrap(),
        )
        .unwrap();
        let mut template = RgbaImage::new(20, 20);
        let mut canvas = RgbaImage::from_pixel(24, 24, Rgba([0, 0, 0, 255]));
        for y in 0..20 {
            for x in 0..20 {
                let value = ((x * 17 + y * 31) % 251) as u8;
                let pixel = Rgba([value, value, value, 255]);
                template.put_pixel(x, y, pixel);
                canvas.put_pixel(x + 2, y + 2, pixel);
            }
        }
        template
            .save_with_format(directory.join("templates/target.png"), ImageFormat::Png)
            .unwrap();
        canvas
            .save_with_format(directory.join("frame.png"), ImageFormat::Png)
            .unwrap();
        fs::write(directory.join("existing-report.json"), EXISTING_REPORT).unwrap();
        let mut manifest = json!({
            "schema_version": 1, "profile": "profile.json", "target": "primary_target",
            "mode": "click_repeat", "desktop": {"left": 0, "top": 0, "width": 100, "height": 100},
            "interval_ms": 1000, "threshold": 0.95, "max_work": 1000000,
            "report": "existing-report.json",
            "frames": [
                {"image": "frame.png", "timestamp_ms": 0},
                {"image": "frame.png", "timestamp_ms": 1000},
                {"image": "frame.png", "timestamp_ms": 2000}
            ]
        });
        let input_frames = if vision_url.is_some() {
            manifest["interval_ms"] = json!(100);
            manifest["frames"] = Value::Array(
                (0..64)
                    .map(|index| json!({"image": "frame.png", "timestamp_ms": index * 100}))
                    .collect(),
            );
            manifest["strategy"] = json!({
                "prompt": STRATEGY_PROMPT, "interval_ms": 1000,
                "timeout_ms": 5000, "max_age_ms": 5000, "max_requests": 1
            });
            64
        } else {
            3
        };
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        drop(reservation);
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-ex-service"));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = command
            .current_dir(&directory)
            .args(["--config", "service.toml", "--managed"])
            .env_remove("DEEPSEEK_API_KEY")
            .stdin(Stdio::piped())
            .stdout(File::create(directory.join("stdout.log")).unwrap())
            .stderr(File::create(directory.join("stderr.log")).unwrap())
            .spawn()
            .unwrap();
        Self {
            child,
            directory,
            input_frames,
            client: ControlClient::new(&address, TOKEN, 65_536).unwrap(),
        }
    }

    pub fn set_search_budget(&self, max_work: u64) {
        let path = self.directory.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["max_work"] = json!(max_work);
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }
    pub async fn ready(&mut self) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    panic!("service exited {status}: {}", self.logs());
                }
                if matches!(
                    self.client
                        .send(ControlCommand::Automation {
                            request: AutomationRequest::Status
                        })
                        .await,
                    Ok(ControlPayload::Automation(_))
                ) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("managed automation control becomes ready");
    }

    fn logs(&self) -> String {
        fs::read_to_string(self.directory.join("stderr.log")).unwrap_or_default()
    }

    pub async fn automation(&self, request: AutomationRequest) -> AutomationSnapshot {
        match self
            .client
            .send(ControlCommand::Automation { request })
            .await
        {
            Ok(ControlPayload::Automation(snapshot)) => *snapshot,
            other => panic!(
                "automation response expected, got {other:?}; {}",
                self.logs()
            ),
        }
    }

    pub async fn status(&self) -> AutomationSnapshot {
        self.automation(AutomationRequest::Status).await
    }

    pub async fn until(
        &self,
        predicate: impl Fn(&AutomationSnapshot) -> bool,
    ) -> AutomationSnapshot {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = self.status().await;
                if predicate(&snapshot) {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("automation state did not settle; {}", self.logs()))
    }

    pub async fn start_replay(&self) -> (AutomationRequest, AutomationSnapshot) {
        let before = self.status().await;
        assert!(before.available);
        assert!(before.session.is_none());
        let request = AutomationRequest::StartTemplate {
            instance_id: before.instance_id,
            expected_revision: before.revision,
            operation_id: "a7b0f925-1101-4001-8001-000000000001".parse().unwrap(),
            manifest: "manifest.json".to_owned(),
        };
        self.automation(request.clone()).await;
        let active = self
            .until(|snapshot| {
                snapshot.session.as_ref().is_some_and(|session| {
                    session.phase == ai_ex_domain::AutomationSessionPhase::Active
                        && session.frames >= 1
                })
            })
            .await;
        let current = session(&active);
        assert_eq!(current.input_frames, self.input_frames);
        assert_eq!(current.scheduled_frames, self.input_frames);
        assert_eq!(current.skipped_frames, 0);
        assert!(
            current.actions > 0,
            "synthetic matcher exercised the dry-run action"
        );
        assert_eq!(
            current.profile_name.as_deref(),
            Some("Synthetic control fixture")
        );
        (request, active)
    }

    pub async fn submit(&self, text: &str) {
        assert!(matches!(
            self.client
                .send(ControlCommand::Submit {
                    text: text.to_owned()
                })
                .await
                .unwrap(),
            ControlPayload::Accepted
        ));
    }

    pub async fn close_owner(&mut self) {
        drop(self.child.stdin.take());
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(status.success(), "service exited {status}: {}", self.logs());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("owner disconnect shuts down the child without waiting for a model");
        assert!(self.client.send(ControlCommand::Status).await.is_err());
        assert!(!self.directory.join("must-not-create-memory.jsonl").exists());
        assert!(
            !self
                .directory
                .join("must-not-create-memory.jsonl.lock")
                .exists()
        );
        assert!(!self.directory.join("audit.jsonl").exists());
        assert_eq!(
            fs::read(self.directory.join("existing-report.json")).unwrap(),
            EXISTING_REPORT
        );
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // Only exact paths created or reserved by this fixture, after its child exits.
        for name in [
            "service.toml",
            "control.token",
            "stdout.log",
            "stderr.log",
            "audit.jsonl",
            "profile.json",
            "manifest.json",
            "existing-report.json",
            "frame.png",
            "templates/target.png",
            "must-not-create-memory.jsonl",
            "must-not-create-memory.jsonl.lock",
        ] {
            let _ = fs::remove_file(self.directory.join(name));
        }
        let _ = fs::remove_dir(self.directory.join("templates"));
        let _ = fs::remove_dir(&self.directory);
    }
}

pub fn session(snapshot: &AutomationSnapshot) -> &AutomationSessionSnapshot {
    snapshot.session.as_ref().expect("replay session exists")
}

pub fn pause(snapshot: &AutomationSnapshot) -> AutomationRequest {
    AutomationRequest::Pause {
        instance_id: snapshot.instance_id,
        session_id: session(snapshot).session_id,
        expected_revision: snapshot.revision,
    }
}

pub fn resume(snapshot: &AutomationSnapshot) -> AutomationRequest {
    AutomationRequest::Resume {
        instance_id: snapshot.instance_id,
        session_id: session(snapshot).session_id,
        expected_revision: snapshot.revision,
    }
}

pub fn stop(snapshot: &AutomationSnapshot) -> AutomationRequest {
    AutomationRequest::Stop {
        instance_id: snapshot.instance_id,
        session_id: session(snapshot).session_id,
        expected_revision: snapshot.revision,
    }
}

pub struct Generation {
    pub request: Value,
    release: oneshot::Sender<String>,
    closed: oneshot::Receiver<()>,
}

impl Generation {
    pub fn complete(self) {
        self.complete_with("Synthetic answer.");
    }

    pub fn complete_with(self, content: impl Into<String>) {
        self.release
            .send(content.into())
            .expect("generation still pending");
    }

    pub async fn cancelled(self) {
        tokio::time::timeout(Duration::from_secs(2), self.closed)
            .await
            .expect("model request is cancelled")
            .expect("connection observer completed");
    }
}

pub struct Model {
    pub url: String,
    pending: mpsc::Receiver<Generation>,
    task: tokio::task::JoinHandle<()>,
}

impl Model {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sent, pending) = mpsc::channel(8);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        connections.spawn(serve_model(stream, sent.clone()));
                    }
                    done = connections.join_next(), if !connections.is_empty() => {
                        done.unwrap().expect("synthetic model connection succeeds");
                    }
                }
            }
        });
        Self { url, pending, task }
    }

    pub async fn next(&mut self) -> Generation {
        tokio::time::timeout(Duration::from_secs(3), self.pending.recv())
            .await
            .expect("generation reached the local fake provider")
            .expect("fake provider is alive")
    }

    pub fn assert_no_queued_generation_started(&mut self) {
        assert!(matches!(
            self.pending.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_model(mut stream: tokio::net::TcpStream, sent: mpsc::Sender<Generation>) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let boundary = loop {
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0 && bytes.len() < 65_536);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let header = String::from_utf8_lossy(&bytes[..boundary]).into_owned();
    let chat = header.starts_with("POST /api/chat ");
    let length: usize = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse().unwrap())
        })
        .unwrap_or(0);
    assert!(length < 65_536);
    while bytes.len() < boundary + length {
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    let body = if chat {
        let (release, released) = oneshot::channel();
        let (closed, closure) = oneshot::channel();
        sent.send(Generation {
            request: serde_json::from_slice(&bytes[boundary..boundary + length]).unwrap(),
            release,
            closed: closure,
        })
        .await
        .unwrap();
        let content = tokio::select! {
            result = stream.read(&mut buffer) => {
                assert!(matches!(result, Ok(0) | Err(_)), "provider received unexpected request bytes");
                let _ = closed.send(());
                return;
            }
            result = released => {
                let Ok(content) = result else {
                    return;
                };
                content
            }
        };
        serde_json::to_string(&json!({
            "message": {"role": "assistant", "content": content}, "done": true
        }))
        .unwrap()
            + "\n"
    } else {
        assert!(
            header.starts_with("GET /api/tags "),
            "unexpected provider endpoint: {header}"
        );
        "{\"models\":[{\"name\":\"synthetic-test\"},{\"name\":\"synthetic-vision\"}]}".to_owned()
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}
