use super::*;

fn command(command: WorkerCommand) -> (u64, ControlCommand) {
    let WorkerCommand::BackupInspection {
        request_id,
        command,
        ..
    } = command
    else {
        panic!("backup command")
    };
    (request_id, command)
}

fn ready() -> (BackupInspectionPanel, Uuid) {
    let instance = Uuid::new_v4();
    let mut panel = BackupInspectionPanel::default();
    panel.synchronize(Some(instance), "person-one", 1, true);
    (panel, instance)
}

fn prepare(panel: &mut BackupInspectionPanel, instance: Uuid) -> (Uuid, Uuid, String) {
    let (request, cmd) = command(panel.prepare_raw().unwrap());
    let ControlCommand::PrepareRawMemoryBackup {
        instance_id,
        operation_id,
    } = cmd
    else {
        panic!("prepare")
    };
    assert_eq!(instance_id, instance);
    let plan_id = Uuid::new_v4();
    let output = std::env::temp_dir()
        .join("synthetic-raw-backup.jsonl")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        panel
            .receive(
                request,
                Ok(ControlPayload::RawMemoryBackupPlan {
                    instance_id,
                    operation_id,
                    plan_id,
                    expires_in_seconds: 300,
                    output: SensitivePath::new(&output),
                })
            )
            .is_none()
    );
    (operation_id, plan_id, output)
}

#[test]
fn confirmation_is_required_and_unknown_preserves_original_operation() {
    let (mut panel, instance) = ready();
    assert!(panel.confirm_raw().is_none());
    let (operation_id, plan_id, output) = prepare(&mut panel, instance);
    let (request, cmd) = command(panel.confirm_raw().unwrap());
    assert!(
        matches!(cmd, ControlCommand::StartRawMemoryBackup { instance_id, operation_id: op, plan_id: plan }
        if instance_id == instance && op == operation_id && plan == plan_id)
    );
    assert!(panel.raw_unresolved());
    assert!(panel.prepare_raw().is_none());
    panel.receive(
        request,
        Err(AppError::unavailable("synthetic connection failure")),
    );
    let tracked = panel.raw.tracked.as_ref().unwrap();
    assert_eq!(tracked.operation_id, operation_id);
    assert_eq!(tracked.output, output);
    assert!(matches!(tracked.state, RawMemoryBackupState::Unknown));
    let (query, cmd) = command(panel.query_raw().unwrap());
    assert!(
        matches!(cmd, ControlCommand::RawMemoryBackupStatus { instance_id, operation_id: op }
        if instance_id == instance && op == operation_id)
    );
    panel.receive(
        query,
        Ok(ControlPayload::RawMemoryBackupStatus(
            RawMemoryBackupStatus {
                instance_id: instance,
                operation_id,
                output: Some(SensitivePath::new(&output)),
                state: RawMemoryBackupState::Written {
                    temporary_retained: false,
                },
            },
        )),
    );
    assert!(!panel.raw_unresolved());
    assert!(matches!(
        panel.raw.tracked.as_ref().unwrap().state,
        RawMemoryBackupState::Written { .. }
    ));
}

#[test]
fn stale_plans_are_discarded_and_submitted_operations_survive_role_changes() {
    let (mut panel, instance) = ready();
    prepare(&mut panel, instance);
    panel.raw.plan.as_mut().unwrap().expires_at = Instant::now() - Duration::from_secs(1);
    assert!(panel.confirm_raw().is_none());
    prepare(&mut panel, instance);
    panel.synchronize(Some(instance), "person-two", 2, true);
    assert!(panel.confirm_raw().is_none());
    let (operation_id, _, output) = prepare(&mut panel, instance);
    let (request, _) = command(panel.confirm_raw().unwrap());
    panel.synchronize(Some(instance), "person-one", 3, true);
    assert!(panel.raw_unresolved());
    assert_eq!(
        panel.raw.tracked.as_ref().unwrap().operation_id,
        operation_id
    );
    assert!(
        panel
            .receive(
                request,
                Ok(ControlPayload::RawMemoryBackupStatus(
                    RawMemoryBackupStatus {
                        instance_id: instance,
                        operation_id,
                        output: Some(SensitivePath::new(&output)),
                        state: RawMemoryBackupState::Written {
                            temporary_retained: false
                        },
                    }
                ))
            )
            .is_none()
    );
    assert!(panel.raw_unresolved());
    assert!(panel.query_raw().is_some());
    panel.synchronize(Some(Uuid::new_v4()), "person-one", 4, true);
    assert!(panel.query_raw().is_none());
    assert_eq!(panel.raw.tracked.as_ref().unwrap().output, output);
    assert!(panel.raw_unresolved());
}

#[test]
fn mismatched_status_never_confirms_a_different_file() {
    let (mut panel, instance) = ready();
    let (operation_id, _, output) = prepare(&mut panel, instance);
    let (request, _) = command(panel.confirm_raw().unwrap());
    panel.receive(
        request,
        Ok(ControlPayload::RawMemoryBackupStatus(
            RawMemoryBackupStatus {
                instance_id: instance,
                operation_id,
                output: Some(SensitivePath::new("different-file")),
                state: RawMemoryBackupState::Written {
                    temporary_retained: false,
                },
            },
        )),
    );
    assert!(panel.raw_unresolved());
    assert_eq!(panel.raw.tracked.as_ref().unwrap().output, output);
}

#[test]
fn late_prepare_reply_cannot_replace_a_new_plan() {
    let (mut panel, instance) = ready();
    let (old_request, old_command) = command(panel.prepare_raw().unwrap());
    let ControlCommand::PrepareRawMemoryBackup {
        operation_id: old_operation,
        ..
    } = old_command
    else {
        panic!("prepare");
    };
    panel.synchronize(Some(instance), "person-two", 2, true);
    let (new_request, new_command) = command(panel.prepare_raw().unwrap());
    let ControlCommand::PrepareRawMemoryBackup { operation_id, .. } = new_command else {
        panic!("prepare");
    };
    let output = std::env::temp_dir()
        .join("synthetic-raw-backup.jsonl")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        panel
            .receive(
                old_request,
                Ok(ControlPayload::RawMemoryBackupPlan {
                    instance_id: instance,
                    operation_id: old_operation,
                    plan_id: Uuid::new_v4(),
                    expires_in_seconds: 300,
                    output: SensitivePath::new(&output),
                })
            )
            .is_none()
    );
    let new_plan = Uuid::new_v4();
    assert!(
        panel
            .receive(
                new_request,
                Ok(ControlPayload::RawMemoryBackupPlan {
                    instance_id: instance,
                    operation_id,
                    plan_id: new_plan,
                    expires_in_seconds: 300,
                    output: SensitivePath::new(&output),
                })
            )
            .is_none()
    );
    let (_, cmd) = command(panel.confirm_raw().unwrap());
    assert!(
        matches!(cmd, ControlCommand::StartRawMemoryBackup { operation_id: op, plan_id, .. }
        if op == operation_id && plan_id == new_plan)
    );
}

#[test]
fn revoking_queued_work_never_loses_a_submitted_backup_identifier() {
    let (mut panel, instance) = ready();
    let (request, _) = command(panel.prepare_raw().unwrap());
    panel.path_edited();
    assert!(panel.raw.plan.is_none());
    assert!(panel.raw.feedback.is_some());
    assert!(
        panel
            .receive(request, Err(AppError::unavailable("revoked")))
            .is_none()
    );

    let (operation_id, _, output) = prepare(&mut panel, instance);
    let (request, _) = command(panel.confirm_raw().unwrap());
    panel.path_edited();
    assert!(
        panel
            .receive(request, Err(AppError::unavailable("revoked")))
            .is_none()
    );
    assert!(panel.raw_unresolved());
    let tracked = panel.raw.tracked.as_ref().unwrap();
    assert_eq!(tracked.operation_id, operation_id);
    assert_eq!(tracked.output, output);
    assert!(panel.query_raw().is_some());
}

#[test]
fn plan_lifetime_includes_time_spent_waiting_for_response() {
    let (mut panel, instance) = ready();
    let (request, cmd) = command(panel.prepare_raw().unwrap());
    let ControlCommand::PrepareRawMemoryBackup { operation_id, .. } = cmd else {
        panic!("prepare")
    };
    let Some(Pending::RawPrepare { requested_at, .. }) = &mut panel.pending else {
        panic!("pending")
    };
    *requested_at = Instant::now() - Duration::from_secs(301);
    let output = std::env::temp_dir()
        .join("synthetic-raw-backup.jsonl")
        .to_str()
        .unwrap()
        .to_owned();
    panel.receive(
        request,
        Ok(ControlPayload::RawMemoryBackupPlan {
            instance_id: instance,
            operation_id,
            plan_id: Uuid::new_v4(),
            expires_in_seconds: 300,
            output: SensitivePath::new(output),
        }),
    );
    assert!(panel.confirm_raw().is_none());
    assert!(!panel.raw_unresolved());
}
