use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use ai_ex_core::MemoryPort;
use ai_ex_domain::TurnId;
use ai_ex_memory::MemoryStore;
use serde_json::json;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("aiex-reindex-{}", TurnId::new().0));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn run(&self, mode: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_ai-ex-service"))
            .current_dir(&self.0)
            .args(["--config", "config.toml", mode])
            .env_remove("DEEPSEEK_API_KEY")
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for name in ["config.toml", "memories.jsonl", "memories.jsonl.lock"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

#[test]
fn missing_embedding_configuration_never_creates_a_memory_file() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("config.toml"),
        "[memory]\npath='memories.jsonl'\n",
    )
    .unwrap();
    let output = fixture.run("--reindex-memory");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("[memory.embedding]"));
    assert!(!fixture.0.join("memories.jsonl").exists());
}

#[tokio::test]
async fn maintenance_reindexes_only_selected_profile_before_connecting_other_services() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < until,
                        "maintenance did not request an embedding"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("mock accept failed: {error}"),
            }
        };
        // Windows can inherit the listener's nonblocking mode on accepted sockets.
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|chunk| chunk == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let body = r#"{"model":"mock-encoder","embeddings":[[0.6,0.8]]}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    fs::write(
        fixture.0.join("config.toml"),
        format!(
            r#"
[model]
backend='deepseek'
[persona]
profile_id='selected'
[memory]
path='memories.jsonl'
[memory.embedding]
base_url='http://{address}'
model='mock-encoder'
space='mock-revision-1'
timeout_seconds=2
[control]
enabled=true
token_path='must-not-be-read'
"#
        ),
    )
    .unwrap();
    let records: Vec<_> = [
        ("selected", "喜欢蓝莓"),
        ("other", "private-other-character"),
    ]
    .into_iter()
    .map(|(profile, text)| {
        json!({
            "id":TurnId::new().0, "turn_id":TurnId::new().0, "profile_id":profile,
            "created_ms":1, "user_text":text, "assistant_text":"ok",
        })
    })
    .collect();
    let original = records
        .iter()
        .map(|record| format!("{record}\n"))
        .collect::<String>();
    fs::write(fixture.0.join("memories.jsonl"), &original).unwrap();
    let output = fixture.run("--reindex-memory");
    let request = server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Reindexed 1 memory record(s)"));
    assert!(request.starts_with("POST /api/embed "));
    assert!(request.contains("喜欢蓝莓"));
    assert!(!request.contains("private-other-character"));
    let content = fs::read_to_string(fixture.0.join("memories.jsonl")).unwrap();
    assert!(content.contains("mock-revision-1"));
    let store = MemoryStore::open(fixture.0.join("memories.jsonl"))
        .await
        .unwrap();
    store.select_profile("selected").await.unwrap();
    assert_eq!(store.len().await, 1);
    assert!(
        store.recall("蓝莓", 1).await.unwrap()[0]
            .content
            .contains("喜欢蓝莓")
    );
    store.select_profile("other").await.unwrap();
    assert_eq!(store.len().await, 1);
    assert!(
        store.recall("private-other-character", 1).await.unwrap()[0]
            .content
            .contains("private-other-character")
    );
    drop(store);
    assert!(fixture.0.join("memories.jsonl.lock").exists());
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 3);
}

#[tokio::test]
async fn maintenance_rejects_an_active_writer_before_requesting_embeddings() {
    let fixture = Fixture::new();
    let path = fixture.0.join("memories.jsonl");
    let mut writer = MemoryStore::open(&path).await.unwrap();
    writer.select_profile("selected").await.unwrap();
    writer
        .remember(
            TurnId::new(),
            "active writer owns this synthetic memory".to_owned(),
            "unchanged original reply".to_owned(),
        )
        .await
        .unwrap();
    let original = fs::read(&path).unwrap();
    assert!(fixture.0.join("memories.jsonl.lock").exists());

    // Reserve an ephemeral loopback address, then close it. A faulty implementation
    // can only fail against this synthetic endpoint; it cannot reach a real model.
    let closed_endpoint = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = closed_endpoint.local_addr().unwrap();
    drop(closed_endpoint);
    fs::write(
        fixture.0.join("config.toml"),
        format!(
            r#"
[persona]
profile_id='selected'
[memory]
path='memories.jsonl'
[memory.embedding]
base_url='http://{address}'
model='must-not-be-requested'
space='synthetic-lock-test'
timeout_seconds=1
"#
        ),
    )
    .unwrap();

    let output = fixture.run("--reindex-memory");
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("another active writer"), "{error}");
    assert!(!error.contains("embedding endpoint"), "{error}");
    assert!(!error.contains("embedding request"), "{error}");
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(writer.len().await, 1);

    // The fixture may remove its exact sidecar only after releasing the writer.
    drop(writer);
}
