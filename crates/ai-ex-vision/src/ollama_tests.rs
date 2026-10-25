use super::*;
use ai_ex_domain::ErrorKind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const TEST_DEADLINE: Duration = Duration::from_secs(5);
const CLIENT_DEADLINE: Duration = Duration::from_secs(3);
const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy)]
enum Framing {
    ContentLength,
    CloseDelimited,
    Chunked,
}

struct FakeServer {
    base_url: String,
    task: JoinHandle<()>,
}

impl FakeServer {
    async fn start(response: Vec<u8>, wait_for_close: bool) -> (Self, oneshot::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("loopback address")
        );
        let (sent, ready) = oneshot::channel();
        let task = tokio::spawn(async move {
            timeout(TEST_DEADLINE, async move {
                let (mut socket, _) = listener.accept().await.expect("fake vision connection");
                read_request(&mut socket).await;
                if let Err(error) = socket.write_all(&response).await {
                    assert!(connection_closed(&error), "fake response write: {error}");
                    let _ = sent.send(());
                    return;
                }
                let _ = sent.send(());
                if wait_for_close {
                    let mut byte = [0];
                    match socket.read(&mut byte).await {
                        Ok(0) => {}
                        Err(error) if connection_closed(&error) => {}
                        result => panic!("analyze must close its partial response: {result:?}"),
                    }
                } else {
                    socket.shutdown().await.expect("finish fake response");
                }
            })
            .await
            .expect("fake vision server deadline");
        });
        (Self { base_url, task }, ready)
    }

    async fn finish(mut self) {
        match timeout(TEST_DEADLINE + Duration::from_secs(1), &mut self.task).await {
            Ok(result) => result.expect("fake vision server task"),
            Err(_) => {
                self.task.abort();
                let _ = (&mut self.task).await;
                panic!("fake vision server did not finish");
            }
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn connection_closed(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::BrokenPipe
    )
}

async fn read_request(socket: &mut TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let read = socket
            .read(&mut buffer)
            .await
            .expect("fake vision request read");
        assert_ne!(read, 0, "client closed before sending request");
        assert!(
            read <= MAX_REQUEST_BYTES - request.len(),
            "bounded fake request"
        );
        request.extend_from_slice(&buffer[..read]);
        let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&request[..header_end]).expect("request headers");
        assert_eq!(headers.lines().next(), Some("POST /api/chat HTTP/1.1"));
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length").then(|| {
                    value
                        .trim()
                        .parse::<usize>()
                        .expect("request content length")
                })
            })
            .expect("request content length header");
        let body_start = header_end + 4;
        assert!(
            content_length <= MAX_REQUEST_BYTES - body_start,
            "bounded request body"
        );
        if request.len() < body_start + content_length {
            continue;
        }
        assert_eq!(request.len(), body_start + content_length);
        let payload: serde_json::Value =
            serde_json::from_slice(&request[body_start..]).expect("fake vision JSON request");
        assert_eq!(payload["model"], "fake-vision:bounded");
        assert_eq!(payload["stream"], false);
        assert_eq!(payload["messages"][0]["role"], "system");
        assert_eq!(payload["messages"][1]["content"], "synthetic observation");
        assert_eq!(payload["messages"][1]["images"][0], "iVBORw0KGgo=");
        return;
    }
}

fn client(base_url: &str, deadline: Duration) -> OllamaVisionClient {
    OllamaVisionClient::new(OllamaVisionSettings {
        base_url: base_url.to_owned(),
        model: "fake-vision:bounded".to_owned(),
        timeout: deadline,
    })
    .expect("fake vision settings")
}

fn request() -> VisionRequest {
    VisionRequest::new(
        "synthetic observation",
        crate::VisualFrame::new(crate::ImageMediaType::Png, b"\x89PNG\r\n\x1a\n".to_vec())
            .expect("synthetic PNG signature"),
    )
    .expect("synthetic request")
}

fn response_head(status: u16, framing: Framing, length: usize) -> Vec<u8> {
    let framing = match framing {
        Framing::ContentLength => format!("Content-Length: {length}\r\n"),
        Framing::CloseDelimited => String::new(),
        Framing::Chunked => "Transfer-Encoding: chunked\r\n".to_owned(),
    };
    format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nConnection: close\r\n{framing}\r\n")
        .into_bytes()
}

fn response(status: u16, framing: Framing, body: &[u8]) -> Vec<u8> {
    let mut bytes = response_head(status, framing, body.len());
    if matches!(framing, Framing::Chunked) {
        for chunk in body.chunks(1024) {
            bytes.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            bytes.extend_from_slice(chunk);
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(b"0\r\n\r\n");
    } else {
        bytes.extend_from_slice(body);
    }
    bytes
}

fn json_body_of_size(size: usize) -> Vec<u8> {
    let empty = serde_json::to_vec(&serde_json::json!({"message": {"content": ""}}))
        .expect("empty response JSON");
    let body = serde_json::to_vec(&serde_json::json!({
        "message": {"content": "x".repeat(size - empty.len())}
    }))
    .expect("bounded response JSON");
    assert_eq!(body.len(), size);
    body
}

async fn analyze_response(
    bytes: Vec<u8>,
    wait_for_close: bool,
) -> Result<VisionObservation, AppError> {
    let (server, _) = FakeServer::start(bytes, wait_for_close).await;
    let mut client = client(&server.base_url, CLIENT_DEADLINE);
    let result = timeout(TEST_DEADLINE, client.analyze(request()))
        .await
        .expect("bounded analyze future");
    // Keep the client alive while joining so a partial response must be closed
    // by the completed analyze future, rather than dropping the client pool.
    server.finish().await;
    drop(client);
    result
}

#[tokio::test]
async fn accepts_json_at_the_exact_success_body_limit() {
    let body = json_body_of_size(MAX_RESPONSE_BODY_BYTES);
    for framing in [
        Framing::ContentLength,
        Framing::CloseDelimited,
        Framing::Chunked,
    ] {
        let observation = analyze_response(response(200, framing, &body), false)
            .await
            .expect("exact-limit response");
        assert_eq!(observation.model, "fake-vision:bounded");
        assert_eq!(observation.text.len(), MAX_RESPONSE_BODY_BYTES - 26);
        assert!(observation.text.bytes().all(|byte| byte == b'x'));
    }
}

#[tokio::test]
async fn rejects_oversized_success_content_length_before_reading_body() {
    // No body or EOF is sent. Waiting for the body would reach the client
    // deadline, so an explicit size error proves rejection from headers.
    let error = analyze_response(
        response_head(200, Framing::ContentLength, MAX_RESPONSE_BODY_BYTES + 1),
        true,
    )
    .await
    .expect_err("oversized advertised success body");
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert_eq!(
        error.message,
        format!("vision returned 200 OK: response body exceeds {MAX_RESPONSE_BODY_BYTES} bytes")
    );
}

#[tokio::test]
async fn rejects_success_bodies_accumulating_past_the_limit_without_content_length() {
    let body = json_body_of_size(MAX_RESPONSE_BODY_BYTES + 1);
    for framing in [Framing::CloseDelimited, Framing::Chunked] {
        let error = analyze_response(response(200, framing, &body), true)
            .await
            .expect_err("oversized received success body");
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.message.contains(&format!(
            "response body exceeds {MAX_RESPONSE_BODY_BYTES} bytes"
        )));
    }
}

#[tokio::test]
async fn rejects_oversized_error_content_length_before_reading_body() {
    let error = analyze_response(
        response_head(500, Framing::ContentLength, MAX_ERROR_BODY_BYTES + 1),
        true,
    )
    .await
    .expect_err("oversized advertised error body");
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert_eq!(
        error.message,
        format!(
            "vision returned 500 Internal Server Error: response body exceeds {MAX_ERROR_BODY_BYTES} bytes"
        )
    );
}

#[tokio::test]
async fn rejects_error_bodies_accumulating_past_the_limit_without_content_length() {
    let body = vec![b'e'; MAX_ERROR_BODY_BYTES + 1];
    for framing in [Framing::CloseDelimited, Framing::Chunked] {
        let error = analyze_response(response(500, framing, &body), true)
            .await
            .expect_err("oversized received error body");
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert_eq!(
            error.message,
            format!(
                "vision returned 500 Internal Server Error: response body exceeds {MAX_ERROR_BODY_BYTES} bytes"
            )
        );
    }
}

#[tokio::test]
async fn accepts_error_body_at_the_limit_and_keeps_only_512_unicode_characters() {
    let prefix = "界".repeat(MAX_ERROR_BODY_CHARS);
    let body = format!(
        "{prefix}{}",
        "x".repeat(MAX_ERROR_BODY_BYTES - prefix.len())
    );
    assert_eq!(body.len(), MAX_ERROR_BODY_BYTES);
    for framing in [
        Framing::ContentLength,
        Framing::CloseDelimited,
        Framing::Chunked,
    ] {
        let error = analyze_response(response(500, framing, body.as_bytes()), false)
            .await
            .expect_err("bounded remote failure");
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert_eq!(
            error.message,
            format!("vision returned 500 Internal Server Error: {prefix}")
        );
    }
}

#[tokio::test]
async fn preserves_invalid_json_and_empty_observation_errors() {
    for (body, expected) in [
        (b"not JSON".as_slice(), "invalid vision response:"),
        (
            br#"{"message":{"content":" \n "}}"#.as_slice(),
            "vision returned an empty observation",
        ),
    ] {
        let error = analyze_response(response(200, Framing::ContentLength, body), false)
            .await
            .expect_err("invalid vision response");
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.message.starts_with(expected), "{}", error.message);
    }
}

#[tokio::test]
async fn cancelling_partial_success_and_error_bodies_closes_the_response() {
    for status in [200, 500] {
        let mut partial = response_head(status, Framing::ContentLength, 1024);
        partial.extend_from_slice(b"{");
        let (server, ready) = FakeServer::start(partial, true).await;
        let mut client = client(&server.base_url, CLIENT_DEADLINE);
        // Own the pinned future: dropping only a Pin<&mut _> would leave the
        // underlying request alive and would not exercise cancellation.
        let mut analyze = Box::pin(client.analyze(request()));
        tokio::select! {
            result = &mut analyze => panic!("partial response finished early: {result:?}"),
            sent = ready => sent.expect("partial fake response sent"),
        }
        assert!(
            timeout(Duration::from_millis(30), &mut analyze)
                .await
                .is_err()
        );
        drop(analyze);
        // The reusable client stays alive until after the peer observes EOF.
        server.finish().await;
        drop(client);
    }
}

#[tokio::test]
async fn the_client_deadline_covers_partial_success_and_error_body_reads() {
    for status in [200, 500] {
        let mut partial = response_head(status, Framing::ContentLength, 1024);
        partial.extend_from_slice(b"{");
        let (server, ready) = FakeServer::start(partial, true).await;
        let mut client = client(&server.base_url, Duration::from_millis(300));
        let error = timeout(TEST_DEADLINE, client.analyze(request()))
            .await
            .expect("bounded client body deadline")
            .expect_err("partial response must time out");
        ready.await.expect("partial response sent before timeout");
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(
            error.message.ends_with("response body read timed out"),
            "{}",
            error.message
        );
        server.finish().await;
        drop(client);
    }
}
