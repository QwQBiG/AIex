use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

enum Redirect {
    Secure,
    Insecure,
    Credentials,
}

async fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    while !request.ends_with(b"\r\n\r\n") {
        let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut chunk))
            .await
            .unwrap()
            .unwrap();
        assert!(read > 0);
        request.extend_from_slice(&chunk[..read]);
        assert!(request.len() <= 8192);
    }
    String::from_utf8(request).unwrap()
}

async fn scenario(
    redirect: Redirect,
    excluded: bool,
    expect_connect: bool,
) -> (AppError, Vec<String>, bool) {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_address = target.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let target_task = tokio::spawn(async move {
        tokio::select! {
            result = target.accept() => { let _ = result.unwrap(); true },
            _ = stopped => false,
        }
    });
    let location = match redirect {
        Redirect::Secure => format!("https://{target_address}/weights?signature=private"),
        Redirect::Insecure => format!("http://{target_address}/weights"),
        Redirect::Credentials => format!("https://user:private@{target_address}/weights"),
    };
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let proxy_task = tokio::spawn(async move {
        let (mut first, _) = tokio::time::timeout(Duration::from_secs(2), proxy.accept())
            .await
            .unwrap()
            .unwrap();
        let mut requests = vec![read_request(&mut first).await];
        let reply = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        first.write_all(reply.as_bytes()).await.unwrap();
        drop(first);
        if expect_connect
            && let Ok(Ok((mut second, _))) =
                tokio::time::timeout(Duration::from_secs(2), proxy.accept()).await
        {
            requests.push(read_request(&mut second).await);
            second
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
        requests
    });
    let mut proxy_config = reqwest::Proxy::all(format!("http://{proxy_address}")).unwrap();
    if excluded {
        proxy_config = proxy_config.no_proxy(reqwest::NoProxy::from_string("127.0.0.1"));
    }
    let client = reqwest::Client::builder()
        .proxy(proxy_config)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        response(
            &Cancellation::default(),
            &client,
            "http://origin.invalid/weights",
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    let requests = proxy_task.await.unwrap();
    let _ = stop.send(());
    let target_reached = target_task.await.unwrap();
    (result, requests, target_reached)
}

#[tokio::test]
async fn redirect_uses_the_configured_proxy_without_direct_fallback() {
    let (error, requests, target_reached) = scenario(Redirect::Secure, false, true).await;
    assert_eq!(requests.len(), 2, "both hops must reach the proxy");
    assert!(requests[0].starts_with("GET http://origin.invalid/weights "));
    assert!(requests[1].starts_with("CONNECT 127.0.0.1:"));
    assert!(!target_reached);
    for request in requests {
        let lower = request.to_ascii_lowercase();
        assert!(!lower.contains("\r\nauthorization:"));
        assert!(!lower.contains("\r\nreferer:"));
    }
    assert!(!error.message.contains("signature"));
}

#[tokio::test]
async fn proxy_exclusion_applies_again_after_redirect() {
    let (_, requests, target_reached) = scenario(Redirect::Secure, true, false).await;
    assert_eq!(requests.len(), 1);
    assert!(
        target_reached,
        "the excluded local destination must bypass the proxy"
    );
}

#[tokio::test]
async fn rejects_insecure_or_credentialed_redirects_before_the_next_request() {
    for redirect in [Redirect::Insecure, Redirect::Credentials] {
        let (error, requests, target_reached) = scenario(redirect, false, false).await;
        assert!(error.message.contains("HTTPS without credentials"));
        assert_eq!(requests.len(), 1);
        assert!(!target_reached);
        assert!(!error.message.contains("private"));
    }
}
