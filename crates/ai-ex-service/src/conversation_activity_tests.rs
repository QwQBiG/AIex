use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use ai_ex_audio::{SpeechQueue, SpeechReceiver};
use ai_ex_core::{AvatarPort, LanguageModelPort, ModelRequest, Runtime, SpeechPort, spawn_runtime};
use ai_ex_domain::TurnId;
use ai_ex_memory::MemoryStore;
use ai_ex_observability::EventHub;
use ai_ex_safety::{SafetyGate, SafetyPolicy};
use async_trait::async_trait;
use tokio::sync::mpsc;
use uuid::Uuid;

struct HeldModel {
    entered: mpsc::Sender<oneshot::Sender<()>>,
}

#[async_trait]
impl LanguageModelPort for HeldModel {
    async fn stream(
        &mut self,
        _request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let (release, held) = oneshot::channel();
        self.entered
            .send(release)
            .await
            .map_err(|_| AppError::unavailable("test observer closed"))?;
        held.await
            .map_err(|_| AppError::unavailable("test request cancelled"))?;
        let (sender, receiver) = mpsc::channel(1);
        sender.send(Ok("Synthetic answer.".into())).await.unwrap();
        Ok(receiver)
    }
    async fn cancel(&mut self, _turn_id: TurnId) -> Result<(), AppError> {
        Ok(())
    }
}

struct SilentSpeech;
#[async_trait]
impl SpeechPort for SilentSpeech {
    async fn enqueue(&mut self, _turn_id: TurnId, _sentence: String) -> Result<(), AppError> {
        Ok(())
    }
    async fn interrupt(&mut self) -> Result<(), AppError> {
        Ok(())
    }
}

struct SilentAvatar;
#[async_trait]
impl AvatarPort for SilentAvatar {
    async fn set_speaking(&mut self, _speaking: bool) -> Result<(), AppError> {
        Ok(())
    }
    async fn set_neutral(&mut self) -> Result<(), AppError> {
        Ok(())
    }
}

fn fixture() -> (
    ConversationActivity,
    TemplateHost,
    mpsc::Receiver<oneshot::Sender<()>>,
    SpeechReceiver,
    EventHub,
) {
    let (speech, receiver) = SpeechQueue::new(2).unwrap();
    let host = TemplateHost::new(
        Uuid::new_v4(),
        speech.activity(),
        Arc::new(SafetyGate::new(SafetyPolicy {
            automation_enabled: false,
            allowed_capabilities: BTreeSet::new(),
            allowed_targets: Vec::new(),
        })),
    )
    .unwrap();
    let (entered, observed) = mpsc::channel(8);
    let events = EventHub::new(64).unwrap();
    let runtime = Runtime::new(
        HeldModel { entered },
        SilentSpeech,
        SilentAvatar,
        MemoryStore::disabled(),
        events.clone(),
    );
    let handle = spawn_runtime(runtime, 1).unwrap();
    (
        ConversationActivity::new(handle, host.clone()),
        host,
        observed,
        receiver,
        events,
    )
}

async fn pending_count(host: &TemplateHost, expected: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if host.pending_chat_count().unwrap() == expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual conversation owner releases its reservation");
}

#[tokio::test]
async fn speech_split_control_checks_instance_and_reports_actual_state() {
    use ai_ex_control::{ControlBackend, ControlCommand, ControlPayload};
    use ai_ex_domain::PersonaSnapshot;
    let (runtime, host, _observed, _receiver, events) = fixture();
    let control = crate::ServiceControl {
        runtime: runtime.clone(),
        events: events.clone(),
        safety: Arc::new(crate::build_safety(&ai_ex_config::AppConfig::default()).unwrap()),
        health: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        persona: Arc::new(tokio::sync::RwLock::new(PersonaSnapshot::default())),
        persona_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        backup_inspector: crate::backup_inspection_coordinator::BackupInspector::new(
            std::env::current_exe().unwrap(),
        ),
        stage: ai_ex_core::StageJournal::new(8),
        automation: host.clone(),
        native: None,
        native_desktop: None,
        desktop_runtime: None,
        interaction: Arc::new(tokio::sync::Mutex::new(())),
    };
    let ControlPayload::SpeechSplit {
        instance_id,
        revision: 0,
        minimum: None,
    } = control.execute(ControlCommand::SpeechSplit).await.unwrap()
    else {
        panic!("initial state")
    };
    assert!(
        control
            .execute(ControlCommand::SetSpeechSplit {
                instance_id: Uuid::new_v4(),
                expected_revision: 0,
                minimum: Some(4),
            })
            .await
            .is_err()
    );
    // A lost response is resolved by reading state, not replaying the mutation.
    let _lost = control
        .execute(ControlCommand::SetSpeechSplit {
            instance_id,
            expected_revision: 0,
            minimum: Some(4),
        })
        .await
        .unwrap();
    assert_eq!(
        control.execute(ControlCommand::SpeechSplit).await.unwrap(),
        ControlPayload::SpeechSplit {
            instance_id,
            revision: 1,
            minimum: Some(4)
        }
    );
    assert!(
        control
            .execute(ControlCommand::SetSpeechSplit {
                instance_id,
                expected_revision: 0,
                minimum: None,
            })
            .await
            .is_err()
    );
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn backup_inspection_rejects_results_after_persona_changes_back() {
    use ai_ex_control::{ControlBackend, ControlCommand, ControlPayload, SensitivePath};
    use ai_ex_domain::PersonaSnapshot;

    let (runtime, host, _observed, _receiver, events) = fixture();
    let control = crate::ServiceControl {
        runtime: runtime.clone(),
        events: events.clone(),
        safety: Arc::new(crate::build_safety(&ai_ex_config::AppConfig::default()).unwrap()),
        health: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        persona: Arc::new(tokio::sync::RwLock::new(PersonaSnapshot::default())),
        persona_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        backup_inspector: crate::backup_inspection_coordinator::BackupInspector::new(
            std::env::current_exe().unwrap(),
        ),
        stage: ai_ex_core::StageJournal::new(8),
        automation: host.clone(),
        native: None,
        native_desktop: None,
        desktop_runtime: None,
        interaction: Arc::new(tokio::sync::Mutex::new(())),
    };
    let mut unavailable = control.clone();
    unavailable.backup_inspector = Err(AppError::unavailable(
        "backup inspection worker unavailable",
    ));
    assert!(matches!(
        unavailable
            .execute(ControlCommand::MemoryInspectionContext)
            .await
            .unwrap(),
        ControlPayload::MemoryInspectionContext { .. }
    ));
    let unavailable_request = ControlCommand::InspectMemoryBackup {
        instance_id: events.current().instance_id.unwrap(),
        profile_id: "default".into(),
        persona_epoch: 0,
        path: SensitivePath::new("unread"),
    };
    assert!(unavailable.execute(unavailable_request).await.is_err());
    let ControlPayload::MemoryInspectionContext {
        instance_id,
        profile_id,
        persona_epoch,
    } = control
        .execute(ControlCommand::MemoryInspectionContext)
        .await
        .unwrap()
    else {
        panic!("context")
    };
    let (started, received) = oneshot::channel();
    let (release, held) = oneshot::channel();
    let inspecting = control.clone();
    let task = tokio::spawn(async move {
        let path = std::env::temp_dir().join("synthetic-unread.jsonl");
        inspecting
            .inspect_memory_backup_with(
                instance_id,
                profile_id,
                persona_epoch,
                SensitivePath::new(path.to_string_lossy()),
                |_| {
                    Ok(async {
                        started.send(()).unwrap();
                        held.await.unwrap();
                        Ok(ai_ex_memory::BackupInspection {
                            memory_records: 1,
                            entity_candidates: 0,
                            source_associations: 0,
                            inactive_source_associations: 0,
                            embedded_records: 0,
                            distinct_embedding_spaces: 0,
                            matching_embeddings: None,
                        })
                    })
                },
            )
            .await
    });
    received.await.unwrap();
    let other = PersonaSnapshot {
        profile_id: "other".into(),
        ..PersonaSnapshot::default()
    };
    control
        .execute(ControlCommand::SetPersona { profile: other })
        .await
        .unwrap();
    control
        .execute(ControlCommand::SetPersona {
            profile: PersonaSnapshot::default(),
        })
        .await
        .unwrap();
    release.send(()).unwrap();
    assert_eq!(
        task.await.unwrap().unwrap_err().kind,
        ai_ex_domain::ErrorKind::InvalidTransition
    );
    let accepted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = accepted.clone();
    let path = std::env::temp_dir().join("synthetic-unread.jsonl");
    let stale = control
        .inspect_memory_backup_with(
            instance_id,
            "default".into(),
            persona_epoch,
            SensitivePath::new(path.to_string_lossy()),
            move |_| {
                flag.store(true, std::sync::atomic::Ordering::Release);
                Ok(async {
                    Err::<ai_ex_memory::BackupInspection, _>(AppError::unavailable("unexpected"))
                })
            },
        )
        .await;
    assert!(stale.is_err());
    assert!(!accepted.load(std::sync::atomic::Ordering::Acquire));
    assert!(matches!(
        control
            .execute(ControlCommand::MemoryInspectionContext)
            .await
            .unwrap(),
        ControlPayload::MemoryInspectionContext {
            persona_epoch: 2,
            ..
        }
    ));
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn superseded_preview_cannot_interrupt_a_newer_conversation() {
    use ai_ex_control::{ControlBackend, ControlCommand};
    use ai_ex_domain::{NativeReply, NativeRequest, PersonaSnapshot};

    let directory = std::env::temp_dir().join(format!("native-preview-intent-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let native = crate::NativeRuntime::start(directory.join("native-v1.toml"), "default".into());
    native.wait_ready(Duration::from_secs(5)).await.unwrap();
    let NativeReply::State(before) = native.request(NativeRequest::State).await.unwrap() else {
        panic!("state reply");
    };
    native.request(NativeRequest::StopPreview).await.unwrap();
    let (runtime, host, mut observed, _receiver, events) = fixture();
    let pending = runtime
        .enqueue("new conversation after stopping preview")
        .await
        .unwrap();
    let release = entered(&mut observed).await;
    let control = crate::ServiceControl {
        runtime: runtime.clone(),
        events: events.clone(),
        safety: Arc::new(crate::build_safety(&ai_ex_config::AppConfig::default()).unwrap()),
        health: Arc::new(tokio::sync::RwLock::new(Vec::new())),
        persona: Arc::new(tokio::sync::RwLock::new(PersonaSnapshot::default())),
        persona_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        backup_inspector: crate::backup_inspection_coordinator::BackupInspector::new(
            std::env::current_exe().unwrap(),
        ),
        stage: ai_ex_core::StageJournal::new(8),
        automation: host.clone(),
        native: Some(native.clone()),
        native_desktop: None,
        desktop_runtime: None,
        interaction: Arc::new(tokio::sync::Mutex::new(())),
    };
    let error = control
        .execute(ControlCommand::Native {
            request: NativeRequest::PreviewVoice {
                voice_id: "builtin-zh".into(),
                text: "old preview intent".into(),
                expected_generation: before.preview_generation,
            },
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind, ai_ex_domain::ErrorKind::InvalidTransition);
    assert!(error.message.contains("试听状态已改变"));
    assert_eq!(events.current().turns_interrupted, 0);
    release
        .send(())
        .expect("newer model request remains active");
    assert!(matches!(
        pending.wait().await.unwrap(),
        TurnOutcome::Completed(_)
    ));
    assert_eq!(events.current().turns_completed, 1);
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
    native.shutdown().await.unwrap();
    drop(control);
    drop(native);
    std::fs::remove_dir_all(directory).unwrap();
}

async fn entered(observed: &mut mpsc::Receiver<oneshot::Sender<()>>) -> oneshot::Sender<()> {
    tokio::time::timeout(Duration::from_secs(3), observed.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn dropping_proactive_wait_cancels_intent_and_drains_its_chat_guard() {
    let (runtime, host, mut observed, _receiver, events) = fixture();
    let output = runtime.clone();
    let proactive = tokio::spawn(async move { output.proactive().await });
    let release = entered(&mut observed).await;
    assert_eq!(host.pending_chat_count().unwrap(), 1);
    proactive.abort();
    assert!(proactive.await.unwrap_err().is_cancelled());
    pending_count(&host, 0).await;
    assert!(
        release.send(()).is_err(),
        "cancelled model generation was dropped"
    );
    assert_eq!(events.current().turns_interrupted, 1);
    let user = runtime.enqueue("actual user").await.unwrap();
    let release = entered(&mut observed).await;
    runtime.cancel_proactive().await.unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        user.wait().await.unwrap(),
        TurnOutcome::Completed(_)
    ));
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropping_before_proactive_owner_is_polled_cannot_start_an_intent_after_cancel() {
    use std::future::Future;
    let (runtime, host, mut observed, _receiver, events) = fixture();
    let mut proactive = Box::pin(runtime.proactive());
    std::future::poll_fn(|context| {
        assert!(proactive.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    // The owner cannot have run on this single-threaded executor yet.
    drop(proactive);
    pending_count(&host, 0).await;
    let user = runtime
        .enqueue("actual user after cancelled intent")
        .await
        .unwrap();
    // A provider request may have started before cancellation; it must be
    // closed, and no new proactive request may be queued after that cancel.
    let release = loop {
        let candidate = entered(&mut observed).await;
        if !candidate.is_closed() {
            break candidate;
        }
    };
    release.send(()).unwrap();
    assert!(matches!(
        user.wait().await.unwrap(),
        TurnOutcome::Completed(_)
    ));
    assert_eq!(events.current().turns_completed, 1);
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn synchronous_start_registers_before_an_unpolled_wait_is_dropped() {
    let (runtime, host, mut observed, _receiver, events) = fixture();
    let pending = runtime.start_submit("detached request").unwrap();
    assert_eq!(
        host.pending_chat_count().unwrap(),
        1,
        "admission is synchronous before a monitor is scheduled"
    );
    drop(Box::pin(pending.wait()));
    assert_eq!(host.pending_chat_count().unwrap(), 1);
    let release = entered(&mut observed).await;
    assert_eq!(host.pending_chat_count().unwrap(), 1);
    release.send(()).unwrap();
    pending_count(&host, 0).await;
    assert_eq!(
        events.current().turns_completed,
        1,
        "caller loss does not cancel admitted generation"
    );
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropped_admitted_queue_wait_still_blocks_idle_until_its_real_turn_settles() {
    let (runtime, host, mut observed, _receiver, events) = fixture();
    let first = runtime.enqueue("first").await.unwrap();
    let first_release = entered(&mut observed).await;
    let second = runtime.enqueue("queued second").await.unwrap();
    assert_eq!(host.pending_chat_count().unwrap(), 2);
    drop(second);
    first_release.send(()).unwrap();
    assert!(matches!(
        first.wait().await.unwrap(),
        TurnOutcome::Completed(_)
    ));
    let second_release = entered(&mut observed).await;
    pending_count(&host, 1).await;
    runtime
        .interrupt("cancel queued generation after admission")
        .await
        .unwrap();
    pending_count(&host, 0).await;
    assert!(
        second_release.send(()).is_err(),
        "interruption cancels the fake provider future"
    );
    assert_eq!(events.current().turns_started, 2);
    assert_eq!(events.current().turns_interrupted, 1);
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_admission_releases_only_its_reservation_and_keeps_existing_work_busy() {
    let (runtime, host, mut observed, _receiver, _events) = fixture();
    let first = runtime.enqueue("active").await.unwrap();
    let first_release = entered(&mut observed).await;
    let second = runtime.enqueue("pending").await.unwrap();
    assert!(runtime.enqueue("overflow").await.is_err());
    pending_count(&host, 2).await;
    runtime.interrupt("release active").await.unwrap();
    assert!(matches!(
        first.wait().await.unwrap(),
        TurnOutcome::Interrupted(_)
    ));
    assert!(first_release.send(()).is_err());
    let second_release = entered(&mut observed).await;
    pending_count(&host, 1).await;
    second_release.send(()).unwrap();
    assert!(matches!(
        second.wait().await.unwrap(),
        TurnOutcome::Completed(_)
    ));
    pending_count(&host, 0).await;
    runtime.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_drops_queued_response_but_never_leaks_an_activity_reservation() {
    let (runtime, host, mut observed, _receiver, _events) = fixture();
    assert!(runtime.start_submit("  ").is_err());
    assert_eq!(host.pending_chat_count().unwrap(), 0);
    let first = runtime.enqueue("active").await.unwrap();
    let release = entered(&mut observed).await;
    let second = runtime.enqueue("queued").await.unwrap();
    assert_eq!(host.pending_chat_count().unwrap(), 2);
    tokio::time::timeout(Duration::from_secs(3), runtime.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        first.wait().await.unwrap(),
        TurnOutcome::Shutdown(_)
    ));
    assert!(second.wait().await.is_err());
    pending_count(&host, 0).await;
    assert!(release.send(()).is_err());
    host.shutdown().await.unwrap();
    assert!(runtime.start_submit("closed host").is_err());
}
