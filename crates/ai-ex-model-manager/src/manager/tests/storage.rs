use super::*;

fn zip(path: &Path, entries: &[(&str, &[u8])]) {
    let file = File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (name, data) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

#[tokio::test]
async fn offline_zip_import_and_unknown_file_rejection() {
    let fixture = Fixture::new();
    let archive = fixture.0.join("模型 包.zip");
    zip(
        &archive,
        &[
            ("weights.bin", b"native model fixture"),
            ("LICENSE.txt", b"synthetic fixture only"),
        ],
    );
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    completed(
        &manager,
        manager
            .start_import("bge-small-zh-v1.5", &archive)
            .await
            .unwrap(),
    )
    .await;
    let installed = fixture
        .store()
        .join("models/bge-small-zh-v1.5/fixed-model-1/data");
    assert_eq!(
        fs::read(installed.join("weights.bin")).unwrap(),
        b"native model fixture"
    );
    // Reusing verified files does not require reading the original source again.
    fs::remove_file(&archive).unwrap();
    completed(
        &manager,
        manager
            .start_import("bge-small-zh-v1.5", &archive)
            .await
            .unwrap(),
    )
    .await;
    fs::write(installed.join("personal.txt"), b"leave this file intact").unwrap();
    let result = manager
        .wait(manager.remove("bge-small-zh-v1.5").await.unwrap())
        .await
        .unwrap();
    assert_eq!(result.phase, OperationPhase::Failed);
    assert_eq!(
        fs::read(installed.join("personal.txt")).unwrap(),
        b"leave this file intact"
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn zip_traversal_and_case_collisions_never_escape_staging() {
    let fixture = Fixture::new();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    for (name, entries) in [
        (
            "traversal.zip",
            vec![("../../../../escaped.txt", b"x".as_slice())],
        ),
        (
            "collision.zip",
            vec![
                ("weights.bin", b"native model fixture".as_slice()),
                ("WEIGHTS.bin", b"native model fixture".as_slice()),
            ],
        ),
        ("unknown.zip", vec![("run.py", b"unused".as_slice())]),
    ] {
        let archive = fixture.0.join(name);
        zip(&archive, &entries);
        let result = manager
            .wait(
                manager
                    .start_import("bge-small-zh-v1.5", &archive)
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.phase, OperationPhase::Failed);
        assert!(!snapshot(&manager, "bge-small-zh-v1.5").installed);
        assert!(!fixture.0.join("escaped.txt").exists());
    }
    assert_eq!(
        fs::read_dir(fixture.store().join(".staging"))
            .unwrap()
            .count(),
        0
    );
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn storage_failure_does_not_replace_existing_entry() {
    let fixture = Fixture::new();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    let protected = fixture.store().join("models/bge-small-zh-v1.5");
    fs::write(&protected, b"existing non-package file").unwrap();
    let result = manager
        .wait(manager.start_download("bge-small-zh-v1.5").await.unwrap())
        .await
        .unwrap();
    assert_eq!(result.phase, OperationPhase::Failed);
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::Error
    );
    assert_eq!(fs::read(protected).unwrap(), b"existing non-package file");
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_rejects_manifest_changes_and_repair_keeps_previous_data() {
    let fixture = Fixture::new();
    let source = fixture.model_source();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    completed(
        &manager,
        manager
            .start_import("bge-small-zh-v1.5", &source)
            .await
            .unwrap(),
    )
    .await;
    manager.shutdown().await.unwrap();
    drop(manager);
    let installed = fixture
        .store()
        .join("models/bge-small-zh-v1.5/fixed-model-1");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(installed.join("manifest.json")).unwrap()).unwrap();
    record["manifest"]["revision"] = serde_json::json!("tampered");
    fs::write(
        installed.join("manifest.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    assert_eq!(
        snapshot(&manager, "bge-small-zh-v1.5").state,
        PackageState::Error
    );
    completed(
        &manager,
        manager
            .start_import("bge-small-zh-v1.5", &source)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        fs::read(installed.join("data/weights.bin")).unwrap(),
        b"native model fixture"
    );
    let backups: Vec<_> = fs::read_dir(fixture.store().join(".retired"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn one_service_owns_storage_lock_and_shutdown_releases_tasks() {
    let fixture = Fixture::new();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    assert!(
        ModelManager::open(fixture.store(), catalog())
            .await
            .is_err()
    );
    completed(
        &manager,
        manager.start_download("native-cpu").await.unwrap(),
    )
    .await;
    manager.shutdown().await.unwrap();
    drop(manager);
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    assert_eq!(snapshot(&manager, "native-cpu").state, PackageState::Ready);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminal_close_releases_lock_after_leases_and_disables_stale_clones() {
    let fixture = Fixture::new();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    completed(
        &manager,
        manager.start_download("native-cpu").await.unwrap(),
    )
    .await;
    let old = manager.clone();
    let lease = manager.acquire("native-cpu").unwrap();
    assert!(manager.close().await.is_err());
    assert!(old.start_download("native-cpu").await.is_err());
    assert!(old.acquire("native-cpu").is_err());
    assert!(
        ModelManager::open(fixture.store(), catalog())
            .await
            .is_err()
    );
    drop(lease);
    manager.close().await.unwrap();
    let reopened = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    assert_eq!(snapshot(&reopened, "native-cpu").state, PackageState::Ready);
    assert!(old.remove("native-cpu").await.is_err());
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn simulated_storage_full_write_fails_operation_without_ready_state() {
    struct Full;
    impl Write for Full {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "synthetic storage full",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    let package = manager.inner.packages["bge-small-zh-v1.5"].clone();
    let id = Uuid::new_v4();
    let cancel = Arc::new(Cancellation::default());
    {
        let mut state = manager.inner.lock().unwrap();
        state.slots.get_mut(package.id()).unwrap().busy = Some(id);
        state.operations.insert(
            id,
            Operation {
                snapshot: OperationSnapshot {
                    id,
                    package_id: package.id().into(),
                    phase: OperationPhase::Importing,
                    completed_bytes: 0,
                    total_bytes: 20,
                    error: None,
                },
                cancel: cancel.clone(),
                task: None,
            },
        );
    }
    let context = TaskContext {
        inner: manager.inner.clone(),
        id,
        cancel,
    };
    let result = crate::transfer::copy_for_test(
        &context,
        &mut std::io::Cursor::new(b"native model fixture"),
        &mut Full,
        20,
    );
    assert!(
        result
            .as_ref()
            .unwrap_err()
            .message
            .contains("synthetic storage full")
    );
    jobs::finish(&context, &package, result);
    assert_eq!(manager.operation(id).unwrap().phase, OperationPhase::Failed);
    assert_eq!(snapshot(&manager, package.id()).state, PackageState::Error);
    assert!(!snapshot(&manager, package.id()).installed);
    manager.shutdown().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn offline_junction_is_rejected_without_reading_its_target() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let fixture = Fixture::new();
    let source = fixture.model_source();
    let junction = fixture.0.join("linked input");
    let output = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&source)
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction creation failed: {:?}",
        output.status
    );
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    let result = manager
        .wait(
            manager
                .start_import("bge-small-zh-v1.5", &junction)
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.phase, OperationPhase::Failed);
    assert!(!snapshot(&manager, "bge-small-zh-v1.5").installed);
    assert_eq!(
        fs::read(source.join("weights.bin")).unwrap(),
        b"native model fixture"
    );
    fs::remove_dir(&junction).unwrap();
    manager.shutdown().await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn restart_rejects_junction_in_installed_package_ancestors() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let fixture = Fixture::new();
    let manager = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    completed(
        &manager,
        manager
            .start_import("bge-small-zh-v1.5", fixture.model_source())
            .await
            .unwrap(),
    )
    .await;
    manager.close().await.unwrap();
    let installed = fixture.store().join("models/bge-small-zh-v1.5");
    let outside = fixture.0.join("external-model");
    fs::rename(&installed, &outside).unwrap();
    let output = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&installed)
        .arg(&outside)
        .creation_flags(0x0800_0000)
        .output()
        .unwrap();
    assert!(output.status.success());
    let restarted = ModelManager::open(fixture.store(), catalog())
        .await
        .unwrap();
    assert_eq!(
        snapshot(&restarted, "bge-small-zh-v1.5").state,
        PackageState::Error
    );
    assert_eq!(
        fs::read(outside.join("fixed-model-1/data/weights.bin")).unwrap(),
        b"native model fixture"
    );
    restarted.close().await.unwrap();
    fs::remove_dir(installed).unwrap();
}

#[tokio::test]
async fn startup_cancellation_interrupts_a_single_file_hash_and_releases_the_lock() {
    use std::sync::atomic::AtomicUsize;

    let fixture = Fixture::new();
    let source = fixture.model_source();
    let content = vec![42_u8; 2 * 1024 * 1024];
    fs::write(source.join("weights.bin"), &content).unwrap();
    let mut catalog = catalog();
    catalog.models[0].manifest.artifacts[0].bytes = content.len() as u64;
    catalog.models[0].manifest.artifacts[0].sha256 = crate::files::hash_bytes(&content);
    let PackageSource::Files { files } = &mut catalog.models[0].source else {
        unreachable!()
    };
    files[0].location = DownloadLocation::Https {
        url: "https://example.invalid/fixed-1/weights.bin".into(),
    };
    let manager = ModelManager::open(fixture.store(), catalog.clone())
        .await
        .unwrap();
    completed(
        &manager,
        manager
            .start_import("bge-small-zh-v1.5", &source)
            .await
            .unwrap(),
    )
    .await;
    manager.close().await.unwrap();
    let checks = Arc::new(AtomicUsize::new(0));
    let counted = checks.clone();
    let result = ModelManager::open_checked(
        &fixture.store(),
        catalog.clone(),
        Arc::new(move || {
            if counted.fetch_add(1, Ordering::SeqCst) >= 7 {
                Err(AppError::invalid_transition(
                    "synthetic startup cancellation",
                ))
            } else {
                Ok(())
            }
        }),
    )
    .await;
    assert_eq!(
        result.err().unwrap().kind,
        ai_ex_domain::ErrorKind::InvalidTransition
    );
    // A complete 2 MiB hash requires at least 32 checks. This stopped after
    // reading blocks within that file, before reaching the next artifact.
    assert!(checks.load(Ordering::SeqCst) < 16);
    let reopened = ModelManager::open(fixture.store(), catalog).await.unwrap();
    assert!(snapshot(&reopened, "bge-small-zh-v1.5").installed);
    assert_eq!(fs::read(source.join("weights.bin")).unwrap(), content);
    reopened.close().await.unwrap();
}
