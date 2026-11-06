use super::*;
use ai_ex_control::SensitivePath;
use serde_json::json;
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
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    fs::remove_file(path).unwrap();
    drop(service.child.stdin.take());
    service.exited().await;
    for name in ["stdout.log", "stderr.log"] {
        let log = fs::read_to_string(service.directory.join(name)).unwrap();
        assert!(!log.contains("private-path-marker"));
        assert!(!log.contains("private-text-marker"));
    }
}
