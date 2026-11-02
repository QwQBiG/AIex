use std::sync::atomic::AtomicUsize;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::*;

struct Server {
    url: String,
    started: oneshot::Receiver<()>,
    release: oneshot::Sender<()>,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

async fn serve(body: &[u8], stall: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/weights.bin", listener.local_addr().unwrap());
    let (started, received) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let body = body.to_vec();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        count.fetch_add(1, Ordering::SeqCst);
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.ends_with(b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            assert!(request.len() <= 8192);
        }
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        if stall {
            stream.write_all(&body[..2]).await.unwrap();
            let _ = started.send(());
            let _ = released.await;
        } else {
            stream.write_all(&body).await.unwrap();
            let _ = started.send(());
        }
        drop(stream);
        if tokio::time::timeout(Duration::from_millis(150), listener.accept())
            .await
            .is_ok()
        {
            count.fetch_add(1, Ordering::SeqCst);
        }
    });
    Server {
        url,
        started: received,
        release,
        requests,
        task,
    }
}

fn with_url(url: &str) -> DistributionCatalog {
    let mut catalog = catalog();
    if let PackageSource::Files { files } = &mut catalog.models[0].source {
        files[0].location = DownloadLocation::Https { url: url.into() };
    }
    catalog
}

#[tokio::test]
async fn streamed_download_commits_and_restart_rechecks_exact_files() {
    let fixture = Fixture::new();
    let server = serve(b"native model fixture", false).await;
    // Only private tests bypass HTTPS validation to use a loopback HTTP fixture.
    let manager = ModelManager::open_validated(&fixture.store(), with_url(&server.url))
        .await
        .unwrap();
    completed(
        &manager,
        manager.start_download("native-cpu").await.unwrap(),
    )
    .await;
    completed(
        &manager,
        manager.start_download("bge-small-zh-v1.5").await.unwrap(),
    )
    .await;
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::Ready
    );
    manager.shutdown().await.unwrap();
    drop(manager);
    server.task.await.unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    assert_eq!(snapshot(&manager, "native-cpu").state, PackageState::Ready);
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::Ready
    );
    manager.shutdown().await.unwrap();
    drop(manager);
    let path = fixture
        .store()
        .join("models/bge-small-zh-v1.5/fixed-model-1/data/weights.bin");
    fs::write(&path, b"corrupt model bytes!").unwrap();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::Error
    );
    assert!(manager.acquire("bge-small-zh-v1.5").is_err());
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn digest_failure_preserves_previous_revision_and_never_marks_ready() {
    let fixture = Fixture::new();
    let server = serve(b"wrong! model fixture", false).await;
    let manager = ModelManager::open_validated(&fixture.store(), with_url(&server.url))
        .await
        .unwrap();
    let old = fixture
        .store()
        .join("models/bge-small-zh-v1.5/previous/data");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("weights.bin"), b"previous version").unwrap();
    let operation = manager.start_download("bge-small-zh-v1.5").await.unwrap();
    let result = manager.wait(operation).await.unwrap();
    assert_eq!(result.phase, OperationPhase::Failed);
    assert!(result.error.unwrap().contains("SHA256"));
    assert_eq!(
        fs::read(old.join("weights.bin")).unwrap(),
        b"previous version"
    );
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::Error
    );
    assert!(
        !fixture
            .store()
            .join("models/bge-small-zh-v1.5/fixed-model-1")
            .exists()
    );
    manager.shutdown().await.unwrap();
    server.task.await.unwrap();
}

#[tokio::test]
async fn stalled_download_cancel_is_observable_and_not_replayed() {
    let fixture = Fixture::new();
    let server = serve(b"native model fixture", true).await;
    let manager = ModelManager::open_validated(&fixture.store(), with_url(&server.url))
        .await
        .unwrap();
    let operation = manager.start_download("bge-small-zh-v1.5").await.unwrap();
    server.started.await.unwrap();
    assert!(manager.cancel(operation).unwrap());
    let result = tokio::time::timeout(Duration::from_secs(1), manager.wait(operation))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.phase, OperationPhase::Cancelled);
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::NotPrepared
    );
    assert!(!snapshot(&manager, "bge-small-zh-v1.5").installed);
    assert!(!manager.cancel(operation).unwrap());
    let _ = server.release.send(());
    server.task.await.unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    manager.shutdown().await.unwrap();
}
