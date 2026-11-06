use super::*;
use std::sync::Arc;

#[tokio::test]
async fn queued_inspection_is_not_sent_after_revocation() {
    let (commands, input) = tokio::sync::mpsc::channel(2);
    let (events, output) = std::sync::mpsc::channel();
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let (release, blocked) = tokio::sync::oneshot::channel();
    let guard = Arc::new(AtomicU64::new(0));
    let task = tokio::spawn(async move {
        let mut blocked = Some(blocked);
        execute_with(input, &events, move |_command| {
            started.send(()).unwrap();
            let blocked = blocked.take();
            async move {
                if let Some(blocked) = blocked {
                    let _ = blocked.await;
                }
                Ok(ControlPayload::Accepted)
            }
        })
        .await;
    });
    commands
        .send(WorkerCommand::BackupInspection {
            request_id: 1,
            command: ControlCommand::MemoryInspectionContext,
            generation: 0,
            guard: Arc::clone(&guard),
        })
        .await
        .unwrap();
    starts.recv().await.unwrap();
    commands
        .send(WorkerCommand::BackupInspection {
            request_id: 2,
            command: ControlCommand::InspectMemoryBackup {
                instance_id: uuid::Uuid::new_v4(),
                profile_id: "synthetic".into(),
                persona_epoch: 1,
                path: ai_ex_control::SensitivePath::new("C:\\synthetic\\backup.jsonl"),
            },
            generation: 0,
            guard: Arc::clone(&guard),
        })
        .await
        .unwrap();
    guard.store(1, Ordering::Release);
    release.send(()).unwrap();
    drop(commands);
    task.await.unwrap();
    assert!(starts.try_recv().is_err());
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection {
            request_id: 1,
            result: Ok(_)
        }
    ));
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection {
            request_id: 2,
            result: Err(_)
        }
    ));
}

#[tokio::test]
async fn legacy_inspection_reaches_sender() {
    let (commands, input) = tokio::sync::mpsc::channel(1);
    let (events, output) = std::sync::mpsc::channel();
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    commands
        .send(WorkerCommand::BackupInspection {
            request_id: 1,
            command: ControlCommand::InspectLegacyMemoryBackup {
                instance_id: uuid::Uuid::new_v4(),
                profile_id: "synthetic".into(),
                persona_epoch: 1,
                path: ai_ex_control::SensitivePath::new("C:\\synthetic\\backup.json.gz"),
            },
            generation: 0,
            guard: Arc::new(AtomicU64::new(0)),
        })
        .await
        .unwrap();
    drop(commands);
    execute_with(input, &events, move |command| {
        sent.send(command).unwrap();
        async { Ok(ControlPayload::Accepted) }
    })
    .await;
    assert!(matches!(
        received.recv().await,
        Some(ControlCommand::InspectLegacyMemoryBackup { .. })
    ));
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection {
            request_id: 1,
            result: Ok(ControlPayload::Accepted)
        }
    ));
}

#[tokio::test]
async fn candidate_operations_reach_sender_only_when_generation_is_current() {
    let (commands, input) = tokio::sync::mpsc::channel(2);
    let (events, output) = std::sync::mpsc::channel();
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let guard = Arc::new(AtomicU64::new(0));
    let operation_id = uuid::Uuid::new_v4();
    commands
        .send(WorkerCommand::BackupInspection {
            request_id: 1,
            command: ControlCommand::StartLegacyCandidate {
                instance_id: uuid::Uuid::new_v4(),
                profile_id: "synthetic".into(),
                persona_epoch: 1,
                preview_id: uuid::Uuid::new_v4(),
                operation_id,
                output_directory: ai_ex_control::SensitivePath::new("C:\\synthetic"),
                utc_offset: Some(480),
                embedding_space: None,
            },
            generation: 0,
            guard: Arc::clone(&guard),
        })
        .await
        .unwrap();
    drop(commands);
    execute_with(input, &events, move |command| {
        sent.send(command).unwrap();
        async { Ok(ControlPayload::Accepted) }
    })
    .await;
    assert!(matches!(received.recv().await,
        Some(ControlCommand::StartLegacyCandidate { operation_id: actual, .. }) if actual == operation_id));
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection { result: Ok(_), .. }
    ));
}

#[tokio::test]
async fn merge_operations_use_the_guarded_backup_sender() {
    let (commands, input) = tokio::sync::mpsc::channel(3);
    let (events, output) = std::sync::mpsc::channel();
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let guard = Arc::new(AtomicU64::new(1));
    let instance = uuid::Uuid::new_v4();
    let operation = uuid::Uuid::new_v4();
    let requests = [
        ControlCommand::PrepareMergeCandidate {
            instance_id: instance,
            profile_id: "synthetic".into(),
            persona_epoch: 1,
            base_path: ai_ex_control::SensitivePath::new("C:\\synthetic\\base.jsonl"),
            incoming_path: ai_ex_control::SensitivePath::new("C:\\synthetic\\incoming.jsonl"),
        },
        ControlCommand::StartMergeCandidate {
            instance_id: instance,
            profile_id: "synthetic".into(),
            persona_epoch: 1,
            preview_id: uuid::Uuid::new_v4(),
            operation_id: operation,
            output_directory: ai_ex_control::SensitivePath::new("C:\\synthetic"),
        },
        ControlCommand::MergeCandidateStatus {
            instance_id: instance,
            operation_id: operation,
        },
    ];
    for (index, command) in requests.into_iter().enumerate() {
        commands
            .send(WorkerCommand::BackupInspection {
                request_id: index as u64 + 1,
                command,
                generation: if index == 1 { 0 } else { 1 },
                guard: Arc::clone(&guard),
            })
            .await
            .unwrap();
    }
    drop(commands);
    execute_with(input, &events, move |command| {
        sent.send(command).unwrap();
        async { Ok(ControlPayload::Accepted) }
    })
    .await;
    assert!(matches!(
        received.recv().await,
        Some(ControlCommand::PrepareMergeCandidate { .. })
    ));
    assert!(matches!(
        received.recv().await,
        Some(ControlCommand::MergeCandidateStatus { .. })
    ));
    assert!(received.try_recv().is_err());
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection {
            request_id: 1,
            result: Ok(_),
            ..
        }
    ));
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection {
            request_id: 2,
            result: Err(_),
            ..
        }
    ));
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection {
            request_id: 3,
            result: Ok(_),
            ..
        }
    ));
}

#[tokio::test]
async fn raw_backup_commands_use_the_guarded_sender() {
    let (commands, input) = tokio::sync::mpsc::channel(3);
    let (events, output) = std::sync::mpsc::channel();
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let instance_id = uuid::Uuid::new_v4();
    let operation_id = uuid::Uuid::new_v4();
    let plan_id = uuid::Uuid::new_v4();
    let guard = Arc::new(AtomicU64::new(1));
    for (index, command) in [
        ControlCommand::PrepareRawMemoryBackup {
            instance_id,
            operation_id,
        },
        ControlCommand::StartRawMemoryBackup {
            instance_id,
            operation_id,
            plan_id,
        },
        ControlCommand::RawMemoryBackupStatus {
            instance_id,
            operation_id,
        },
    ]
    .into_iter()
    .enumerate()
    {
        commands
            .send(WorkerCommand::BackupInspection {
                request_id: index as u64 + 1,
                command,
                generation: if index == 1 { 0 } else { 1 },
                guard: Arc::clone(&guard),
            })
            .await
            .unwrap();
    }
    drop(commands);
    execute_with(input, &events, move |command| {
        sent.send(command).unwrap();
        async { Ok(ControlPayload::Accepted) }
    })
    .await;
    assert!(matches!(
        received.recv().await,
        Some(ControlCommand::PrepareRawMemoryBackup { .. })
    ));
    assert!(matches!(
        received.recv().await,
        Some(ControlCommand::RawMemoryBackupStatus { .. })
    ));
    assert!(received.try_recv().is_err());
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection {
            request_id: 1,
            result: Ok(_),
            ..
        }
    ));
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection {
            request_id: 2,
            result: Err(_),
            ..
        }
    ));
    assert!(matches!(
        output.recv().unwrap(),
        WorkerEvent::BackupInspection {
            request_id: 3,
            result: Ok(_),
            ..
        }
    ));
}
