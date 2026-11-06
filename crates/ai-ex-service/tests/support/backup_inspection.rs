use super::*;
use ai_ex_control::SensitivePath;
use serde_json::json;
use std::io::Write;
use uuid::Uuid;

#[tokio::test]
async fn backup_inspection_control_uses_instance_profile_and_redacted_response() {
    let mut service = ServiceProcess::start("--managed", true);
    service.ready().await;
    let path = service.directory.join("private-path-marker.jsonl");
    let original = format!(
        "{}\n",
        json!({
            "schema_version":2,"record_type":"memory","id":Uuid::new_v4(),
            "profile_id":"default","turn_id":Uuid::new_v4(),"created_ms":1,
            "user_text":"private-text-marker","assistant_text":"private-answer-marker"
        })
    );
    fs::write(&path, &original).unwrap();
    let ControlPayload::MemoryInspectionContext {
        instance_id,
        profile_id,
        persona_epoch,
    } = service
        .client
        .send(ControlCommand::MemoryInspectionContext)
        .await
        .unwrap()
    else {
        panic!("context")
    };
    let command =
        |instance_id, profile_id: String, persona_epoch| ControlCommand::InspectMemoryBackup {
            instance_id,
            profile_id,
            persona_epoch,
            path: SensitivePath::new(path.to_string_lossy()),
        };
    let result = service
        .client
        .send(command(instance_id, profile_id.clone(), persona_epoch))
        .await
        .unwrap();
    let ControlPayload::MemoryBackupInspection {
        instance_id: reply_instance,
        profile_id: reply_profile,
        persona_epoch: reply_epoch,
        summary,
    } = &result
    else {
        panic!("inspection")
    };
    assert_eq!(
        (*reply_instance, reply_profile, *reply_epoch),
        (instance_id, &profile_id, persona_epoch)
    );
    assert_eq!(summary.memory_records, 1);
    let encoded = serde_json::to_string(&result).unwrap();
    for marker in [
        "private-path-marker",
        "private-text-marker",
        "private-answer-marker",
    ] {
        assert!(!encoded.contains(marker));
    }
    for stale in [
        command(Uuid::new_v4(), profile_id.clone(), persona_epoch),
        command(instance_id, "other".into(), persona_epoch),
        command(instance_id, profile_id.clone(), persona_epoch + 1),
    ] {
        let error = service.client.send(stale).await.unwrap_err();
        assert!(!error.to_string().contains("private-path-marker"));
        assert!(!error.to_string().contains("private-text-marker"));
    }
    let legacy_path = service.directory.join("private-legacy-marker.json.gz");
    let fixture = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ai-ex-migrate/tests/fixtures/legacy-memory.json"
    ));
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(fixture).unwrap();
    let legacy_original = encoder.finish().unwrap();
    fs::write(&legacy_path, &legacy_original).unwrap();
    let legacy_command = |instance_id, profile_id: String, persona_epoch| {
        ControlCommand::InspectLegacyMemoryBackup {
            instance_id,
            profile_id,
            persona_epoch,
            path: SensitivePath::new(legacy_path.to_string_lossy()),
        }
    };
    let legacy_result = service
        .client
        .send(legacy_command(
            instance_id,
            profile_id.clone(),
            persona_epoch,
        ))
        .await
        .unwrap();
    let ControlPayload::LegacyMemoryBackupInspection { summary, .. } = &legacy_result else {
        panic!("legacy inspection")
    };
    assert_eq!(summary.records, 1);
    assert_eq!(summary.local_timestamp_records, 1);
    assert_eq!(summary.nonempty_embedding_records, 1);
    assert!(
        !serde_json::to_string(&legacy_result)
            .unwrap()
            .contains("private-legacy-marker")
    );
    for stale in [
        legacy_command(Uuid::new_v4(), profile_id.clone(), persona_epoch),
        legacy_command(instance_id, "other".into(), persona_epoch),
        legacy_command(instance_id, profile_id.clone(), persona_epoch + 1),
    ] {
        assert!(service.client.send(stale).await.is_err());
    }
    let prepared = service
        .client
        .send(ControlCommand::PrepareLegacyCandidate {
            instance_id,
            profile_id: profile_id.clone(),
            persona_epoch,
            path: SensitivePath::new(legacy_path.to_string_lossy()),
        })
        .await
        .unwrap();
    let ControlPayload::LegacyCandidatePreview {
        preview_id,
        summary,
        ..
    } = prepared
    else {
        panic!("candidate preview expected")
    };
    assert_eq!(summary.records, 1);
    let operation_id = Uuid::new_v4();
    let start = |id| ControlCommand::StartLegacyCandidate {
        instance_id,
        profile_id: profile_id.clone(),
        persona_epoch,
        preview_id,
        operation_id: id,
        output_directory: SensitivePath::new(service.directory.to_string_lossy()),
        utc_offset: Some(480),
        embedding_space: Some("synthetic-space".into()),
    };
    assert!(matches!(
        service.client.send(start(operation_id)).await.unwrap(),
        ControlPayload::LegacyCandidateStatus(_)
    ));
    let status = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let payload = service
                .client
                .send(ControlCommand::LegacyCandidateStatus {
                    instance_id,
                    operation_id,
                })
                .await
                .unwrap();
            let ControlPayload::LegacyCandidateStatus(status) = payload else {
                panic!("candidate status expected")
            };
            if status.state != ai_ex_control::LegacyCandidateState::Running {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        status.state,
        ai_ex_control::LegacyCandidateState::Written { .. }
    ));
    assert_eq!(status.profile_id.as_deref(), Some(profile_id.as_str()));
    let output = status.output.unwrap();
    let candidate = fs::read(output.as_str()).unwrap();
    assert!(serde_json::from_slice::<serde_json::Value>(&candidate).is_ok());
    assert!(service.client.send(start(Uuid::new_v4())).await.is_err());
    assert!(
        matches!(service.client.send(start(operation_id)).await.unwrap(),
        ControlPayload::LegacyCandidateStatus(status)
            if matches!(status.state, ai_ex_control::LegacyCandidateState::Written { .. }))
    );
    assert_eq!(fs::read(output.as_str()).unwrap(), candidate);
    fs::remove_file(output.as_str()).unwrap();
    assert_eq!(fs::read(&legacy_path).unwrap(), legacy_original);
    fs::remove_file(legacy_path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    fs::remove_file(path).unwrap();
    drop(service.child.stdin.take());
    service.exited().await;
    for name in ["stdout.log", "stderr.log"] {
        let log = fs::read_to_string(service.directory.join(name)).unwrap();
        assert!(!log.contains("private-path-marker"));
        assert!(!log.contains("private-text-marker"));
        assert!(!log.contains("private-legacy-marker"));
    }
}

#[tokio::test]
async fn merge_candidate_control_produces_separate_file_without_changing_active_memory() {
    use ai_ex_control::{LegacyCandidateState, MergeCandidatePreviewResult};
    use ai_ex_domain::MemoryRequest;

    let mut service = ServiceProcess::with_memory("--managed", true, "http://127.0.0.1:1", true);
    service.ready().await;
    let ControlPayload::MemoryInspectionContext {
        instance_id,
        profile_id,
        persona_epoch,
    } = service
        .client
        .send(ControlCommand::MemoryInspectionContext)
        .await
        .unwrap()
    else {
        panic!("memory context expected");
    };
    assert!(matches!(
        service
            .client
            .send(ControlCommand::Memory {
                request: MemoryRequest::Remember {
                    profile_id: profile_id.clone(),
                    text: "synthetic active note".into()
                },
            })
            .await
            .unwrap(),
        ControlPayload::Memory(_)
    ));
    let active_path = service.directory.join("memory.jsonl");
    let active_before = fs::read(&active_path).unwrap();
    let record = || {
        json!({
            "schema_version": 2, "record_type": "memory", "id": Uuid::new_v4(),
            "profile_id": profile_id, "turn_id": Uuid::new_v4(), "created_ms": 1,
            "user_text": "private-merge-text-marker", "assistant_text": "",
            "unknown_extension": { "preserved": true }
        })
        .to_string()
        .into_bytes()
    };
    let base = record();
    let incoming = record();
    let base_path = service.directory.join("private-merge-base.jsonl");
    let incoming_path = service.directory.join("private-merge-incoming.jsonl");
    fs::write(&base_path, &base).unwrap();
    fs::write(&incoming_path, &incoming).unwrap();
    let preview = service
        .client
        .send(ControlCommand::PrepareMergeCandidate {
            instance_id,
            profile_id: profile_id.clone(),
            persona_epoch,
            base_path: SensitivePath::new(base_path.to_string_lossy()),
            incoming_path: SensitivePath::new(incoming_path.to_string_lossy()),
        })
        .await
        .unwrap();
    let encoded = serde_json::to_string(&preview).unwrap();
    assert!(!encoded.contains("private-merge"));
    let ControlPayload::MergeCandidatePreview {
        instance_id: returned_instance,
        profile_id: returned_profile,
        persona_epoch: returned_epoch,
        preview_id: Some(preview_id),
        expires_in_seconds,
        result: MergeCandidatePreviewResult::Ready { summary },
    } = preview
    else {
        panic!("merge preview expected")
    };
    assert_eq!(
        (returned_instance, returned_profile, returned_epoch),
        (instance_id, profile_id.clone(), persona_epoch)
    );
    assert!(expires_in_seconds > 0);
    assert_eq!(
        (summary.base_memory_records, summary.incoming_memory_records),
        (1, 1)
    );
    let operation_id = Uuid::new_v4();
    let output = service
        .directory
        .join(format!("merge-candidate-{operation_id}.jsonl"));
    let initial = service
        .client
        .send(ControlCommand::StartMergeCandidate {
            instance_id,
            profile_id: profile_id.clone(),
            persona_epoch,
            preview_id,
            operation_id,
            output_directory: SensitivePath::new(service.directory.to_string_lossy()),
        })
        .await
        .unwrap();
    assert!(matches!(initial, ControlPayload::MergeCandidateStatus(_)));
    let status = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let ControlPayload::MergeCandidateStatus(status) = service
                .client
                .send(ControlCommand::MergeCandidateStatus {
                    instance_id,
                    operation_id,
                })
                .await
                .unwrap()
            else {
                panic!("merge status expected")
            };
            if status.state != LegacyCandidateState::Running {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(status.state, LegacyCandidateState::Written { .. }));
    assert_eq!(status.instance_id, instance_id);
    assert_eq!(status.operation_id, operation_id);
    assert_eq!(status.profile_id.as_deref(), Some(profile_id.as_str()));
    assert_eq!(status.output.unwrap().as_str(), output.to_str().unwrap());
    let mut expected = base.clone();
    expected.push(b'\n');
    expected.extend_from_slice(&incoming);
    assert_eq!(fs::read(&output).unwrap(), expected);
    assert_eq!(fs::read(&base_path).unwrap(), base);
    assert_eq!(fs::read(&incoming_path).unwrap(), incoming);
    assert_eq!(fs::read(&active_path).unwrap(), active_before);
    drop(service.child.stdin.take());
    service.exited().await;
    assert_eq!(fs::read(&active_path).unwrap(), active_before);
    for name in ["stdout.log", "stderr.log"] {
        let log = fs::read_to_string(service.directory.join(name)).unwrap();
        assert!(!log.contains("private-merge"));
    }
    for path in [&base_path, &incoming_path, &output] {
        fs::remove_file(path).unwrap();
    }
}
