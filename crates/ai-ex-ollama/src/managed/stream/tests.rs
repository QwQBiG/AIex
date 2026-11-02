use super::*;
use ai_ex_domain::{ErrorKind, Role};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

async fn error_server(
    body: String,
    unfinished_chunk: bool,
) -> (String, oneshot::Sender<()>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (release, finish) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0_u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&buffer[..read]);
            if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&request[..end]);
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        assert!(request.starts_with(b"POST /api/chat HTTP/1.1\r\n"));
        let response = if unfinished_chunk {
            format!(
                "HTTP/1.1 500 Internal Server Error\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n",
                body.len()
            )
        } else {
            format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{body}",
                body.len()
            )
        };
        // The bounded reader may close the connection before the whole error body.
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = finish.await;
    });
    (endpoint, release, server)
}

async fn failure(body: String, unfinished_chunk: bool) -> AppError {
    let (endpoint, release, server) = error_server(body, unfinished_chunk).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let turn = TurnId::new();
    let active = Arc::new(Mutex::new(Some(turn)));
    let (sender, _receiver) = mpsc::channel(4);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        run(
            &client,
            &endpoint,
            vec![Message::new(Role::User, "test")],
            &sender,
            &active,
            turn,
        ),
    )
    .await;
    let _ = release.send(());
    server.await.unwrap();
    result
        .expect("error body must be bounded before the HTTP timeout")
        .unwrap_err()
}

#[tokio::test]
async fn chat_http_failure_preserves_status_and_only_the_json_error() {
    let error = failure(json!({"error":"failed to load native CPU runner", "configuration":"private-config-marker"}).to_string(), false).await;
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert!(error.message.contains("HTTP 500"));
    assert!(error.message.contains("failed to load native CPU runner"));
    assert!(!error.message.contains("private-config-marker"));
}

#[tokio::test]
async fn malformed_private_http_error_body_is_not_echoed() {
    let error = failure("private-config-and-input{invalid-json".into(), false).await;
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert_eq!(error.message, "managed Ollama chat failed (HTTP 500)");
}

#[tokio::test]
async fn oversized_http_error_stops_without_waiting_for_a_body_terminator() {
    let error = failure("private-oversized-body".repeat(512), true).await;
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert_eq!(error.message, "managed Ollama chat failed (HTTP 500)");
}
