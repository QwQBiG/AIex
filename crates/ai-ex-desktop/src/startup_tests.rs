use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

#[test]
fn configured_token_path_is_used_without_a_sibling_token() {
    let path = std::env::temp_dir().join(format!("aiex-custom-{}.token", uuid::Uuid::new_v4()));
    let token = "custom-token-with-at-least-thirty-two-bytes";
    std::fs::write(&path, token).unwrap();
    let mut config = AppConfig::default();
    config.control.token_path = path.to_string_lossy().into_owned();
    assert_eq!(read_token(&config).unwrap(), token);
    std::fs::remove_file(path).unwrap();
    assert!(read_token(&config).is_err());
}

#[test]
fn existing_service_is_reused_only_after_authentication() {
    for authenticated in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = AppConfig::default();
        config.control.bind = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            drop(listener.accept().unwrap());
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(&socket).read_line(&mut line).unwrap();
            let request: ai_ex_control::ControlRequest = serde_json::from_str(&line).unwrap();
            assert_eq!(request.command, ControlCommand::Status);
            let response = if authenticated {
                ai_ex_control::ControlResponse::Success {
                    request_id: request.request_id,
                    payload: ControlPayload::Snapshot(
                        ai_ex_observability::EventHub::new(8).unwrap().current(),
                    ),
                }
            } else {
                ai_ex_control::ControlResponse::Failure {
                    request_id: Some(request.request_id),
                    error: AppError::safety("invalid token"),
                }
            };
            writeln!(socket, "{}", serde_json::to_string(&response).unwrap()).unwrap();
        });
        let result = service_is_running(&config, "startup-test-token-with-thirty-two-bytes");
        if authenticated {
            assert!(result.unwrap());
        } else {
            assert!(result.unwrap_err().to_string().contains("occupied"));
        }
        server.join().unwrap();
    }
}

#[test]
fn unused_address_allows_starting() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = AppConfig::default();
    config.control.bind = listener.local_addr().unwrap().to_string();
    drop(listener);
    assert!(!service_is_running(&config, "startup-test-token-with-thirty-two-bytes").unwrap());
}
