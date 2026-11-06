use super::*;

fn ready() -> (BackupInspectionPanel, Uuid) {
    let id = Uuid::new_v4();
    let mut panel = BackupInspectionPanel::default();
    panel.synchronize(Some(id), "synthetic", 1, true);
    panel.path = "C:\\synthetic\\memory.jsonl".into();
    panel.path_edited();
    (panel, id)
}

fn summary() -> MemoryBackupInspection {
    MemoryBackupInspection {
        memory_records: 2,
        entity_candidates: 1,
        source_associations: 1,
        inactive_source_associations: 0,
        embedded_records: 1,
        distinct_embedding_spaces: 1,
        matching_embeddings: None,
    }
}

fn context(id: Uuid, profile: &str, epoch: u64) -> ControlPayload {
    ControlPayload::MemoryInspectionContext {
        instance_id: id,
        profile_id: profile.into(),
        persona_epoch: epoch,
    }
}

fn request_id(command: WorkerCommand) -> (u64, ControlCommand) {
    let WorkerCommand::BackupInspection {
        request_id,
        command,
        ..
    } = command
    else {
        panic!("inspection command")
    };
    (request_id, command)
}

#[test]
fn inspection_requires_user_start_and_matches_context_and_summary() {
    let (mut panel, id) = ready();
    assert!(panel.pending.is_none());
    let (query, command) = request_id(panel.start().unwrap());
    assert!(matches!(command, ControlCommand::MemoryInspectionContext));
    let (inspect, command) = request_id(
        panel
            .receive(query, Ok(context(id, "synthetic", 7)))
            .unwrap(),
    );
    assert!(
        matches!(command, ControlCommand::InspectMemoryBackup { instance_id, ref profile_id, persona_epoch: 7, ref path } if instance_id == id && profile_id == "synthetic" && path.as_str() == "C:\\synthetic\\memory.jsonl")
    );
    assert!(
        panel
            .receive(
                inspect,
                Ok(ControlPayload::MemoryBackupInspection {
                    instance_id: id,
                    profile_id: "synthetic".into(),
                    persona_epoch: 7,
                    summary: summary()
                })
            )
            .is_none()
    );
    assert_eq!(panel.result.as_ref().unwrap().memory_records, 2);
    panel.path = "C:\\synthetic\\other.jsonl".into();
    panel.path_edited();
    assert!(panel.result.is_none());
    let (query, _) = request_id(panel.start().unwrap());
    panel.synchronize(Some(id), "synthetic", 2, true);
    assert!(
        panel
            .receive(query, Ok(context(id, "synthetic", 7)))
            .is_none()
    );
    assert!(panel.pending.is_none());
}

#[test]
fn inspection_discards_late_replies_after_edit_disconnect_and_role_change() {
    let (mut panel, id) = ready();
    let (old_query, _) = request_id(panel.start().unwrap());
    panel.path_edited();
    assert!(
        panel
            .receive(old_query, Ok(context(id, "synthetic", 0)))
            .is_none()
    );
    let (query, _) = request_id(panel.start().unwrap());
    let (old_inspect, _) = request_id(
        panel
            .receive(query, Ok(context(id, "synthetic", 1)))
            .unwrap(),
    );
    panel.synchronize(Some(id), "synthetic", 1, false);
    panel.synchronize(Some(id), "synthetic", 1, true);
    let (new_query, _) = request_id(panel.start().unwrap());
    assert!(
        panel
            .receive(
                old_inspect,
                Ok(ControlPayload::MemoryBackupInspection {
                    instance_id: id,
                    profile_id: "synthetic".into(),
                    persona_epoch: 1,
                    summary: summary()
                })
            )
            .is_none()
    );
    let (inspect, _) = request_id(
        panel
            .receive(new_query, Ok(context(id, "synthetic", 2)))
            .unwrap(),
    );
    panel.synchronize(Some(id), "different", 1, true);
    assert!(panel.path.is_empty());
    assert!(
        panel
            .receive(
                inspect,
                Ok(ControlPayload::MemoryBackupInspection {
                    instance_id: id,
                    profile_id: "synthetic".into(),
                    persona_epoch: 2,
                    summary: summary()
                })
            )
            .is_none()
    );
    assert!(panel.result.is_none());
}

#[test]
fn inspection_rejects_wrong_epoch_instance_and_invalid_summary() {
    let (mut panel, id) = ready();
    let (query, _) = request_id(panel.start().unwrap());
    assert!(
        panel
            .receive(query, Ok(context(Uuid::new_v4(), "synthetic", 1)))
            .is_none()
    );
    assert!(panel.result.is_none());
    let (query, _) = request_id(panel.start().unwrap());
    let (inspect, _) = request_id(
        panel
            .receive(query, Ok(context(id, "synthetic", 2)))
            .unwrap(),
    );
    assert!(
        panel
            .receive(
                inspect,
                Ok(ControlPayload::MemoryBackupInspection {
                    instance_id: id,
                    profile_id: "synthetic".into(),
                    persona_epoch: 3,
                    summary: summary()
                })
            )
            .is_none()
    );
    assert!(panel.result.is_none());
    let (query, _) = request_id(panel.start().unwrap());
    let (inspect, _) = request_id(
        panel
            .receive(query, Ok(context(id, "synthetic", 4)))
            .unwrap(),
    );
    let mut invalid = summary();
    invalid.embedded_records = 3;
    panel.receive(
        inspect,
        Ok(ControlPayload::MemoryBackupInspection {
            instance_id: id,
            profile_id: "synthetic".into(),
            persona_epoch: 4,
            summary: invalid,
        }),
    );
    assert!(panel.result.is_none());
}
