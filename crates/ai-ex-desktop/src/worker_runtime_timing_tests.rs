use super::*;
use std::sync::mpsc;
use tokio::sync::{mpsc as async_mpsc, oneshot};
use uuid::Uuid;

#[tokio::test]
async fn slow_timing_query_does_not_block_and_old_generation_is_discarded() {
    let (events, output) = mpsc::channel();
    let (target, receiver) = watch::channel(VoiceTarget {
        generation: 1,
        connected: true,
    });
    let (started, mut starts) = async_mpsc::unbounded_channel();
    let (release, blocked) = oneshot::channel::<()>();
    let instance = Uuid::new_v4();
    let task = tokio::spawn(async move {
        let mut blocked = Some(blocked);
        run_with(&events, receiver, Duration::from_millis(10), || {
            let first = blocked.take();
            if first.is_some() {
                started.send(()).unwrap();
            }
            async move {
                if let Some(blocked) = first {
                    let _ = blocked.await;
                }
                Ok(RuntimeTiming {
                    instance_id: instance,
                    sequence: 2,
                    turn: None,
                })
            }
        })
        .await;
    });
    starts.recv().await.unwrap();
    let (progress, progressed) = oneshot::channel();
    tokio::spawn(async move { progress.send(()).unwrap() });
    tokio::time::timeout(Duration::from_secs(1), progressed)
        .await
        .unwrap()
        .unwrap();
    target.send_replace(VoiceTarget {
        generation: 2,
        connected: true,
    });
    let _ = release.send(());
    let event = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Ok(event) = output.try_recv() {
                break event;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(event, WorkerEvent::RuntimeTiming { generation: 2, timing } if timing.instance_id == instance)
    );
    drop(target);
    task.await.unwrap();
}
