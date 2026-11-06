use super::*;

#[cfg(windows)]
fn hanging_worker() -> BackupInspector {
    let executable = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    BackupInspector::for_test(
        executable,
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            "Start-Sleep -Seconds 60".into(),
        ],
    )
}

#[cfg(windows)]
fn request() -> Request {
    Request {
        path: "C:\\synthetic-private-marker.jsonl".into(),
        profile: "a".into(),
        comparison_space: None,
    }
}

#[cfg(windows)]
async fn wait_started(inspector: &BackupInspector) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !inspector.inner.started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn timeout_reaps_owned_child_and_releases_the_only_slot() {
    let inspector = hanging_worker();
    let owner = inspector.clone();
    let task =
        tokio::spawn(async move { owner.inspect(request(), Duration::from_millis(700)).await });
    wait_started(&inspector).await;
    assert!(
        inspector
            .inspect(request(), Duration::from_secs(1))
            .await
            .unwrap_err()
            .message
            .contains("busy")
    );
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(!error.to_string().contains("synthetic-private-marker"));
    assert!(inspector.inner.reaped.load(Ordering::Acquire));
    assert_eq!(inspector.inner.slots.available_permits(), 1);
    inspector.shutdown().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn shutdown_cancels_and_reaps_the_active_child() {
    let inspector = hanging_worker();
    let owner = inspector.clone();
    let task = tokio::spawn(async move { owner.inspect(request(), Duration::from_secs(30)).await });
    wait_started(&inspector).await;
    tokio::time::timeout(Duration::from_secs(5), inspector.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(inspector.inner.reaped.load(Ordering::Acquire));
    assert!(
        inspector
            .inspect(request(), Duration::from_secs(1))
            .await
            .is_err()
    );
}

#[cfg(windows)]
#[tokio::test]
async fn abandoned_request_reaps_before_another_can_start() {
    let inspector = hanging_worker();
    let owner = inspector.clone();
    let task = tokio::spawn(async move { owner.inspect(request(), Duration::from_secs(30)).await });
    wait_started(&inspector).await;
    task.abort();
    let _cancelled = task.await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while inspector.inner.slots.available_permits() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(inspector.inner.reaped.load(Ordering::Acquire));
    assert!(!inspector.inner.poisoned.load(Ordering::Acquire));
    inspector.shutdown().await.unwrap();
}
