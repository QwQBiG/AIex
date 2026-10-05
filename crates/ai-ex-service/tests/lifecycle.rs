use std::fs::{self, File};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_ex_control::{ControlClient, ControlCommand, ControlPayload};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[path = "support/persona_switch.rs"]
mod persona_switch;

#[path = "support/memory_management.rs"]
mod memory_management;

struct ServiceProcess {
    child: Child,
    directory: PathBuf,
    client: ControlClient,
}

impl ServiceProcess {
    fn start(mode: &str, piped: bool) -> Self {
        Self::with_model(mode, piped, "http://127.0.0.1:1")
    }

    fn with_model(mode: &str, piped: bool, model_url: &str) -> Self {
        Self::with_memory(mode, piped, model_url, false)
    }

    fn with_memory(mode: &str, piped: bool, model_url: &str, memory: bool) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "aiex-lifecycle-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap().to_string();
        let token = "lifecycle-test-token-not-a-private-secret";
        fs::write(directory.join("control.token"), token).unwrap();
        fs::write(directory.join("service.toml"), format!(
            "[model]\nbackend = 'ollama'\n[ollama]\nbase_url = '{model_url}'\ntimeout_seconds = 30\n\
             [vts]\nenabled = false\n[obs]\nenabled = false\n[memory]\nenabled = {memory}\npath = 'memory.jsonl'\n\
             [control]\nenabled = true\nbind = '{address}'\ntoken_path = 'control.token'\n",
        )).unwrap();
        drop(reservation);
        let child = Command::new(env!("CARGO_BIN_EXE_ai-ex-service"))
            .current_dir(&directory)
            .args(["--config", "service.toml", mode])
            .stdin(if piped { Stdio::piped() } else { Stdio::null() })
            .stdout(File::create(directory.join("stdout.log")).unwrap())
            .stderr(File::create(directory.join("stderr.log")).unwrap())
            .spawn()
            .unwrap();
        Self {
            child,
            directory,
            client: ControlClient::new(&address, token, 65_536).unwrap(),
        }
    }

    async fn ready(&mut self) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    panic!(
                        "service exited {status}: {}",
                        fs::read_to_string(self.directory.join("stderr.log")).unwrap()
                    );
                }
                if matches!(
                    self.client.send(ControlCommand::Status).await,
                    Ok(ControlPayload::Snapshot(_))
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("control service becomes ready");
    }

    async fn exited(&mut self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "service failed: {}",
                        fs::read_to_string(self.directory.join("stderr.log")).unwrap()
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("service shuts down after its owner closes the pipe");
    }
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ignored = self.child.kill();
            let _ignored = self.child.wait();
        }
        for name in [
            "service.toml",
            "control.token",
            "stdout.log",
            "stderr.log",
            "memory.jsonl",
            "memory.jsonl.lock",
        ] {
            let _ignored = fs::remove_file(self.directory.join(name));
        }
        let _ignored = fs::remove_dir(&self.directory);
    }
}

#[tokio::test]
async fn managed_service_stays_ready_and_exits_when_owner_disconnects() {
    let mut service = ServiceProcess::start("--managed", true);
    service.ready().await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(matches!(
        service
            .client
            .send(ControlCommand::Interrupt {
                reason: "lifecycle verification".to_owned(),
            })
            .await
            .unwrap(),
        ControlPayload::Accepted
    ));
    drop(service.child.stdin.take());
    service.exited().await;
    assert!(service.client.send(ControlCommand::Status).await.is_err());
}

#[tokio::test]
async fn standalone_service_ignores_closed_standard_input() {
    let mut service = ServiceProcess::start("--serve", false);
    service.ready().await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(service.child.try_wait().unwrap().is_none());
    assert!(matches!(
        service.client.send(ControlCommand::Status).await.unwrap(),
        ControlPayload::Snapshot(_)
    ));
}

#[tokio::test]
async fn closing_owner_cancels_a_model_request_that_has_not_responded() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}", model.local_addr().unwrap());
    let (started, received) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = model.accept().await.unwrap();
            let mut header = Vec::new();
            let mut buffer = [0; 1024];
            while !header.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                header.extend_from_slice(&buffer[..count]);
                assert!(header.len() < 65_536);
            }
            if header.starts_with(b"POST /api/chat ") {
                started.send(()).unwrap();
                // Never answer this request; service shutdown must cancel it.
                held.await.unwrap();
                break;
            }
            let body = b"{\"models\":[]}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(body).await.unwrap();
        }
    });
    let mut service = ServiceProcess::with_model("--managed", true, &model_url);
    service.ready().await;
    service
        .client
        .send(ControlCommand::Submit {
            text: "hello".to_owned(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), received)
        .await
        .unwrap()
        .unwrap();
    drop(service.child.stdin.take());
    service.exited().await;
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}
