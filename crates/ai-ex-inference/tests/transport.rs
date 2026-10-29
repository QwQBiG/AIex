#![forbid(unsafe_code)]

use ai_ex_inference::{MAX_FRAME_BYTES, read_frame, write_frame};
use tokio::io::{AsyncWriteExt, BufReader};

#[tokio::test]
async fn fragmented_frames_and_adjacent_frames_are_preserved() {
    let (mut writer, reader) = tokio::io::duplex(16);
    let producer = tokio::spawn(async move {
        for part in [b"{\"x\":".as_slice(), b"1}\n{", b"\"x\":2}\n"] {
            writer.write_all(part).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    let mut reader = BufReader::with_capacity(3, reader);
    assert_eq!(
        read_frame::<_, serde_json::Value>(&mut reader)
            .await
            .unwrap()
            .unwrap()["x"],
        1
    );
    assert_eq!(
        read_frame::<_, serde_json::Value>(&mut reader)
            .await
            .unwrap()
            .unwrap()["x"],
        2
    );
    producer.await.unwrap();
    assert!(
        read_frame::<_, serde_json::Value>(&mut reader)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn truncated_or_oversized_frames_fail() {
    for data in [b"{}".to_vec(), vec![b' '; MAX_FRAME_BYTES + 1]] {
        let mut reader = BufReader::new(data.as_slice());
        assert!(
            read_frame::<_, serde_json::Value>(&mut reader)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn invalid_json_does_not_echo_private_payload() {
    let mut reader = BufReader::new(b"private transcript is not JSON\n".as_slice());
    let error = read_frame::<_, serde_json::Value>(&mut reader)
        .await
        .unwrap_err();
    assert!(!error.message.contains("private transcript"));
}

#[tokio::test]
async fn exact_size_limit_round_trips_and_oversize_write_is_atomic() {
    let text = "x".repeat(MAX_FRAME_BYTES - 3);
    let mut output = Vec::new();
    write_frame(&mut output, &text).await.unwrap();
    assert_eq!(output.len(), MAX_FRAME_BYTES);
    let mut reader = BufReader::new(output.as_slice());
    assert_eq!(
        read_frame::<_, String>(&mut reader).await.unwrap(),
        Some(text)
    );
    let mut rejected = Vec::new();
    assert!(
        write_frame(&mut rejected, &"x".repeat(MAX_FRAME_BYTES))
            .await
            .is_err()
    );
    assert!(rejected.is_empty());
}
