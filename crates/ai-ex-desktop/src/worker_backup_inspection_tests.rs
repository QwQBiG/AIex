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
