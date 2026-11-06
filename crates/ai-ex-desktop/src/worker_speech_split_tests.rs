use super::*;
use ai_ex_control::{ControlRequest, ControlResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[tokio::test]
async fn worker_sends_top_level_query_and_set_and_keeps_request_ids() {
    let instance = uuid::Uuid::new_v4();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = ControlClient::new(
        &listener.local_addr().unwrap().to_string(),
        "x".repeat(32),
        8192,
    )
    .unwrap();
    let server = tokio::spawn(async move {
        for index in 0..3 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let request: ControlRequest = serde_json::from_str(&line).unwrap();
            let minimum = if index == 0 {
                assert!(matches!(request.command, ControlCommand::SpeechSplit));
                None
            } else {
                assert!(matches!(request.command, ControlCommand::SetSpeechSplit {
                    instance_id, expected_revision, minimum: Some(18)
                } if instance_id == instance && expected_revision == 8 + index));
                Some(18)
            };
            let response = ControlResponse::Success {
                request_id: request.request_id,
                payload: ControlPayload::SpeechSplit {
                    instance_id: instance,
                    revision: 9 + index,
                    minimum,
                },
            };
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            stream.get_mut().write_all(&bytes).await.unwrap();
        }
    });
    let (commands, receiver) = tokio::sync::mpsc::channel(4);
    let (sent, events) = std::sync::mpsc::channel();
    let worker = tokio::spawn(async move { execute(&client, receiver, &sent).await });
    assert!(
        commands
            .send(WorkerCommand::SpeechSplit {
                request_id: 12,
                request: SpeechSplitRequest::Query
            })
            .await
            .is_ok()
    );
    assert!(
        commands
            .send(WorkerCommand::SpeechSplit {
                request_id: 13,
                request: SpeechSplitRequest::Set {
                    instance_id: instance,
                    expected_revision: 9,
                    minimum: Some(18)
                },
            })
            .await
            .is_ok()
    );
    assert!(
        commands
            .send(WorkerCommand::SpeechSplit {
                request_id: 14,
                request: SpeechSplitRequest::Fence {
                    instance_id: instance,
                    expected_revision: 10,
                    minimum: Some(18),
                },
            })
            .await
            .is_ok()
    );
    drop(commands);
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
    server.await.unwrap();
    assert!(matches!(events.recv().unwrap(), WorkerEvent::SpeechSplit {
        request_id: 12, result: Ok(SpeechSplitStatus { instance_id, revision: 9, minimum: None })
    } if instance_id == instance));
    assert!(matches!(events.recv().unwrap(), WorkerEvent::SpeechSplit {
        request_id: 13, result: Ok(SpeechSplitStatus { instance_id, revision: 10, minimum: Some(18) })
    } if instance_id == instance));
    assert!(matches!(events.recv().unwrap(), WorkerEvent::SpeechSplit {
        request_id: 14, result: Ok(SpeechSplitStatus { instance_id, revision: 11, minimum: Some(18) })
    } if instance_id == instance));
}
