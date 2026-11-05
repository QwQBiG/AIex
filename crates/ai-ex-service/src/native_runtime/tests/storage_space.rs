use super::*;
use std::io;

#[tokio::test]
async fn space_query_uses_confirmed_directory_and_preserves_unknown() {
    let fixture = Fixture::new();
    let runtime = fixture.start().await;
    runtime
        .request(NativeRequest::CreateDefaults)
        .await
        .unwrap();
    let missing = fixture.0.join("missing-model-root");
    runtime
        .lock()
        .unwrap()
        .config
        .as_mut()
        .unwrap()
        .storage
        .packages = missing.clone();
    let expected = missing.clone();
    let reply = runtime
        .storage_space_with(1, move |path| {
            assert_eq!(path, expected);
            assert!(!path.exists());
            Err(io::Error::from(io::ErrorKind::NotFound))
        })
        .await
        .unwrap();
    assert!(matches!(
        reply,
        NativeReply::StorageSpace {
            revision: 1,
            available_bytes: None
        }
    ));
    assert!(!missing.exists());
    let expected = missing.clone();
    let reply = runtime
        .storage_space_with(1, move |path| {
            assert_eq!(path, expected);
            Ok(12345)
        })
        .await
        .unwrap();
    assert!(matches!(
        reply,
        NativeReply::StorageSpace {
            revision: 1,
            available_bytes: Some(12345)
        }
    ));
    assert!(runtime.storage_space_with(2, |_| Ok(1)).await.is_err());
    assert!(!missing.exists());
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn space_query_discards_response_after_configuration_changes() {
    let fixture = Fixture::new();
    let runtime = fixture.start().await;
    runtime
        .request(NativeRequest::CreateDefaults)
        .await
        .unwrap();
    let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task_runtime = runtime.clone();
    let task = tokio::spawn(async move {
        task_runtime
            .storage_space_with(1, move |_| {
                arrived_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(12345)
            })
            .await
    });
    arrived_rx.await.unwrap();
    runtime.lock().unwrap().config.as_mut().unwrap().revision = 2;
    release_tx.send(()).unwrap();
    assert!(task.await.unwrap().is_err());
    runtime.shutdown().await.unwrap();
}
