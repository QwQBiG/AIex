use super::*;
use ai_ex_domain::ErrorKind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn endpoint(status: u16, body: String) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
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
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        );
        let _ = stream.write_all(response.as_bytes()).await;
        String::from_utf8(bytes).unwrap()
    });
    (format!("http://{address}"), task)
}

fn client(base_url: &str) -> OllamaEmbeddingClient {
    OllamaEmbeddingClient::new(
        base_url,
        "local-encoder:test",
        "encoder-revision-1",
        Duration::from_secs(2),
    )
    .unwrap()
}

#[tokio::test]
async fn posts_single_utf8_input_without_silent_truncation() {
    let (url, server) = endpoint(
        200,
        r#"{"model":"local-encoder:test","embeddings":[[0.6,0.8]],"prompt_eval_count":5}"#
            .to_owned(),
    )
    .await;
    let client = client(&format!("{url}/"));
    assert_eq!(client.space(), "encoder-revision-1");
    assert_eq!(
        client.embed("喜欢蓝莓\n和茶").await.unwrap(),
        vec![0.6, 0.8]
    );
    let request = server.await.unwrap();
    assert!(request.starts_with("POST /api/embed HTTP/1.1\r\n"));
    let (_, body) = request.split_once("\r\n\r\n").unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        serde_json::json!({
            "model":"local-encoder:test", "input":"喜欢蓝莓\n和茶", "truncate":false,
        })
    );
}

#[tokio::test]
async fn rejects_invalid_vectors_and_bounds_responses() {
    for body in [
        r#"{"embeddings":[]}"#.to_owned(),
        r#"{"embeddings":[[],[1.0]]}"#.to_owned(),
        r#"{"embeddings":[[]]}"#.to_owned(),
        r#"{"embeddings":[[0.0,0.0]]}"#.to_owned(),
        r#"{"embeddings":[[1e100]]}"#.to_owned(),
        r#"{"embedding":[1.0]}"#.to_owned(),
        "private-malformed-payload".to_owned(),
        serde_json::json!({"embeddings":[vec![1.0_f32; MAX_DIMENSIONS + 1]]}).to_string(),
        "x".repeat(MAX_RESPONSE_BYTES + 1),
    ] {
        let (url, server) = endpoint(200, body).await;
        let error = client(&url)
            .embed("synthetic private query")
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(!error.message.contains("private"));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn remote_failure_does_not_expose_content_or_auto_pull_models() {
    let (url, server) = endpoint(404, "private server detail".to_owned()).await;
    let error = client(&url)
        .embed("synthetic private query")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert!(error.message.contains("404"));
    assert!(!error.message.contains("private"));
    assert!(server.await.unwrap().starts_with("POST /api/embed "));
}

#[tokio::test]
async fn request_timeout_is_bounded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = OllamaEmbeddingClient::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "encoder",
        "space",
        Duration::from_millis(30),
    )
    .unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(stream);
    });
    let error = client.embed("text").await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Connectivity);
    assert!(error.message.contains("timed out"));
    server.await.unwrap();
}

#[tokio::test]
async fn validates_local_settings_and_input_before_http() {
    for (url, model, space, timeout) in [
        ("file:///tmp/model", "model", "space", 1),
        ("http://user:secret@localhost", "model", "space", 1),
        ("http://localhost?key=secret", "model", "space", 1),
        ("http://localhost", "", "space", 1),
        ("http://localhost", "model", " ", 1),
        ("http://localhost", "model", "space", 0),
        ("http://localhost", "model", "space", 301),
    ] {
        assert!(
            OllamaEmbeddingClient::new(url, model, space, Duration::from_secs(timeout)).is_err()
        );
    }
    let client = client("http://127.0.0.1:1");
    for text in [" ".to_owned(), "字".repeat(MAX_INPUT_BYTES / 3 + 1)] {
        assert_eq!(
            client.embed(&text).await.unwrap_err().kind,
            ErrorKind::Configuration
        );
    }
}
