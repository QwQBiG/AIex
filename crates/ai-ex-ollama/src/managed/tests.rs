use super::*;
use ai_ex_domain::{ErrorKind, Message, Role};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[cfg(windows)]
mod native;

async fn endpoint(bodies: Vec<String>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for body in bodies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0_u8; 4096];
                let read = socket.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0);
                bytes.extend_from_slice(&buffer[..read]);
                if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            requests.push(String::from_utf8(bytes).unwrap());
        }
        requests
    });
    (format!("http://{address}"), server)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

#[tokio::test]
async fn native_health_requires_exact_version_and_manifest() {
    for digest in [MANIFEST_SHA, "another-manifest"] {
        let (url, server) = endpoint(vec![
            json!({"version": MANAGED_VERSION}).to_string(),
            json!({"models":[{"name":MANAGED_MODEL,"digest":digest}]}).to_string(),
        ])
        .await;
        assert_eq!(
            stream::check_endpoint(&client(), &url).await.is_ok(),
            digest == MANIFEST_SHA
        );
        assert_eq!(server.await.unwrap().len(), 2);
    }
    let (url, server) = endpoint(vec![json!({"version":"0.40.10"}).to_string()]).await;
    assert_eq!(
        stream::check_endpoint(&client(), &url)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Protocol
    );
    server.await.unwrap();
}

#[tokio::test]
async fn managed_requests_explicitly_disable_thinking_and_keep_messages() {
    let body = "{\"message\":{\"content\":\"你好\"},\"done\":false}\n{\"done\":true}\n";
    let (url, server) = endpoint(vec![body.into()]).await;
    let turn = TurnId::new();
    let active = Arc::new(Mutex::new(Some(turn)));
    let (sender, mut receiver) = mpsc::channel(4);
    stream::run(
        &client(),
        &url,
        vec![Message::new(Role::User, "原样的中文输入")],
        &sender,
        &active,
        turn,
    )
    .await
    .unwrap();
    assert_eq!(receiver.recv().await.unwrap().unwrap(), "你好");
    let requests = server.await.unwrap();
    let (_, body) = requests[0].split_once("\r\n\r\n").unwrap();
    let value: Value = serde_json::from_str(body).unwrap();
    assert_eq!(value["think"], false);
    assert_eq!(value["model"], MANAGED_MODEL);
    assert_eq!(value["options"]["num_gpu"], 0);
    assert_eq!(value["options"]["num_thread"], 4);
    assert_eq!(value["messages"][0]["content"], "原样的中文输入");
    let legacy = crate::protocol::ChatRequest::new("user-model".into(), vec![]);
    assert!(serde_json::to_value(legacy).unwrap().get("think").is_none());
}

#[tokio::test]
async fn cancelled_delivery_cannot_resume_when_capacity_returns() {
    let turn = TurnId::new();
    let active = Arc::new(Mutex::new(Some(turn)));
    let (sender, mut receiver) = mpsc::channel(1);
    sender.send(Ok("already-delivered".into())).await.unwrap();
    let mut pending = Box::pin(stream::deliver(
        &sender,
        &active,
        turn,
        Ok("late-result".into()),
    ));
    let blocked = std::future::poll_fn(|context| {
        std::task::Poll::Ready(std::future::Future::poll(pending.as_mut(), context).is_pending())
    })
    .await;
    assert!(blocked);
    *active.lock().await = None;
    assert_eq!(receiver.recv().await.unwrap().unwrap(), "already-delivered");
    assert!(pending.await.is_err());
    drop(sender);
    assert!(receiver.recv().await.is_none());
}

#[tokio::test]
async fn managed_stream_rejects_thinking_and_truncation_without_echoing_payload() {
    for body in [
        "{\"message\":{\"thinking\":\"private-reasoning\"},\"done\":true}\n",
        "{\"message\":{\"content\":\"partial\"},\"done\":false}\n",
        "private-malformed-payload\n",
    ] {
        let (url, server) = endpoint(vec![body.into()]).await;
        let turn = TurnId::new();
        let active = Arc::new(Mutex::new(Some(turn)));
        let (sender, _receiver) = mpsc::channel(4);
        let error = stream::run(
            &client(),
            &url,
            vec![Message::new(Role::User, "test")],
            &sender,
            &active,
            turn,
        )
        .await
        .unwrap_err();
        assert!(!error.message.contains("private-reasoning"));
        assert!(!error.message.contains("private-malformed-payload"));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn missing_native_resources_do_not_block_construction_or_start_a_process() {
    let root = std::env::temp_dir().join(format!("ollama-native-missing-{}", TurnId::new().0));
    let mut model = ManagedOllama::new(ManagedOllamaSettings {
        executable: root.join("ollama.exe"),
        private_directory: root,
        model_source: std::env::temp_dir().join("nonexistent-verified-source"),
        startup_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
    })
    .unwrap();
    assert!(!model.health().await.ready);
    let turn = TurnId::new();
    let mut receiver = model
        .stream(ModelRequest {
            turn_id: turn,
            messages: vec![Message::new(Role::User, "你好")],
        })
        .await
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    model.shutdown().await.unwrap();
    assert!(model.state.lock().await.instance_absent());
}
