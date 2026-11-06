use super::*;
use ai_ex_domain::MemoryBackupStatus;
use eframe::egui;

fn panel() -> MemoryPanel {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.synchronize_backup_instance(Some(uuid::Uuid::new_v4()));
    panel.storage_enabled = Some(true);
    panel.loaded = true;
    panel
}

fn begin(panel: &mut MemoryPanel, request: MemoryRequest) -> u64 {
    let crate::worker::WorkerCommand::Memory { request_id, .. } = panel.begin(request).unwrap()
    else {
        unreachable!()
    };
    request_id
}

fn backup(id: uuid::Uuid) -> MemoryRequest {
    MemoryRequest::Backup {
        profile_id: "first".into(),
        backup_id: id,
    }
}

#[test]
fn saved_entity_conflict_explains_failed_backup_without_changing_other_errors() {
    let mut panel = panel();
    panel.draft = "synthetic draft".into();
    for message in [
        ai_ex_domain::MEMORY_SAVED_ENTITIES_CHANGED,
        "another synthetic failure",
    ] {
        let id = uuid::Uuid::new_v4();
        let request = begin(&mut panel, backup(id));
        panel.receive(
            request,
            Ok(status(
                id,
                MemoryBackupState::Failed {
                    message: message.into(),
                },
            )),
        );
    }
    let context = egui::Context::default();
    let output = context.run_ui(Default::default(), |ui| {
        let mut action = None;
        panel.show_backup_contents(ui, true, true, &mut action);
        assert!(action.is_none());
    });
    let labels: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(labels.contains(&"角色备份未生成，请先保存完整原始副本并核对。"));
    assert!(labels.contains(&"备份未完成：another synthetic failure"));
    assert_eq!(panel.draft, "synthetic draft");
}

fn status(id: uuid::Uuid, state: MemoryBackupState) -> MemoryResponse {
    MemoryResponse::BackupStatus(MemoryBackupStatus {
        profile_id: "first".into(),
        backup_id: id,
        state,
    })
}

#[test]
fn backup_requires_explicit_confirmation_and_completion_preserves_draft() {
    let mut panel = panel();
    panel.draft = "未提交的合成笔记".into();
    let context = egui::Context::default();
    let render = |panel: &mut MemoryPanel, events| {
        let mut action = None;
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 1600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| panel.show_backup(ui, true, true, &mut action),
        );
        (output, action)
    };
    let click = |panel: &mut MemoryPanel, label: &str| {
        let (output, action) = render(panel, Vec::new());
        assert!(action.is_none());
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("backup button");
        let mut result = None;
        for pressed in [true, false] {
            let (_, action) = render(
                panel,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
            if action.is_some() {
                result = action;
            }
        }
        result
    };
    assert!(click(&mut panel, "备份当前角色的记忆").is_none());
    for _ in 0..20 {
        let _ = render(&mut panel, Vec::new());
    }
    assert!(click(&mut panel, "准备备份记忆").is_none());
    let request = click(&mut panel, "确认生成备份").expect("explicit backup request");
    let MemoryRequest::Backup { backup_id, .. } = request else {
        panic!("backup request")
    };
    let request_id = begin(&mut panel, backup(backup_id));
    assert!(panel.backup_pending());
    assert!(!panel.receive(
        request_id,
        Ok(status(
            backup_id,
            MemoryBackupState::Completed {
                path: "D:\\synthetic\\backup.jsonl".into(),
                memory_count: 1,
                temporary_retained: false,
            }
        ))
    ));
    assert!(!panel.backup_pending());
    assert!(!panel.backup_unresolved());
    assert_eq!(panel.draft, "未提交的合成笔记");
    assert!(matches!(
        panel.backups["first"][0].state,
        MemoryBackupState::Completed { .. }
    ));
    panel.synchronize_backup_instance(Some(uuid::Uuid::new_v4()));
    assert!(
        matches!(&panel.backups["first"][0].state, MemoryBackupState::Completed { path, .. } if path.contains("synthetic"))
    );
}

#[test]
fn backup_unknown_preserves_ids_across_disconnect_profiles_and_new_requests() {
    let mut panel = panel();
    let first = uuid::Uuid::new_v4();
    let request = begin(&mut panel, backup(first));
    panel.connection_changed();
    assert!(panel.backup_unresolved());
    assert!(panel.has_unsaved());
    assert!(matches!(
        panel.backups["first"][0].state,
        MemoryBackupState::Unknown
    ));
    panel.receive(
        request,
        Ok(status(
            first,
            MemoryBackupState::Completed {
                path: "late".into(),
                memory_count: 1,
                temporary_retained: false,
            },
        )),
    );
    assert!(matches!(
        panel.backups["first"][0].state,
        MemoryBackupState::Unknown
    ));
    panel.set_profile("second");
    assert!(!panel.backups.contains_key("second"));
    panel.set_profile("first");
    let query = MemoryRequest::BackupStatus {
        profile_id: "first".into(),
        backup_id: first,
    };
    let query_id = begin(&mut panel, query);
    panel.receive(query_id, Err(AppError::connectivity("synthetic timeout")));
    assert!(matches!(
        panel.backups["first"][0].state,
        MemoryBackupState::Unknown
    ));
    assert!(panel.begin(backup(first)).is_none());
    let second = uuid::Uuid::new_v4();
    let request = begin(&mut panel, backup(second));
    assert_eq!(panel.backups["first"].len(), 2);
    panel.receive(
        request,
        Ok(status(
            second,
            MemoryBackupState::Failed {
                message: "synthetic failure".into(),
            },
        )),
    );
    panel.synchronize_backup_instance(Some(uuid::Uuid::new_v4()));
    assert!(matches!(
        panel.backups["first"][0].state,
        MemoryBackupState::Unknown
    ));
    assert!(matches!(
        panel.backups["first"][1].state,
        MemoryBackupState::Failed { .. }
    ));
    assert!(
        panel
            .begin(MemoryRequest::BackupStatus {
                profile_id: "first".into(),
                backup_id: first
            })
            .is_none()
    );
}

#[test]
fn backup_mismatched_response_is_not_accepted() {
    let mut panel = panel();
    let id = uuid::Uuid::new_v4();
    let request = begin(&mut panel, backup(id));
    panel.receive(
        request,
        Ok(status(
            uuid::Uuid::new_v4(),
            MemoryBackupState::Completed {
                path: "wrong".into(),
                memory_count: 1,
                temporary_retained: false,
            },
        )),
    );
    assert!(matches!(
        panel.backups["first"][0].state,
        MemoryBackupState::Unknown
    ));
    assert!(panel.feedback.is_some());
}

#[test]
fn acknowledged_unknown_keeps_identifier_without_repeated_exit_warning() {
    let mut panel = panel();
    let id = uuid::Uuid::new_v4();
    let request = begin(&mut panel, backup(id));
    panel.receive(request, Err(AppError::connectivity("synthetic timeout")));
    assert!(panel.backup_unresolved());
    let context = egui::Context::default();
    let render = |panel: &mut MemoryPanel, events| {
        context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 1600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| panel.show_backup_contents(ui, true, true, &mut None),
        )
    };
    let output = render(&mut panel, Vec::new());
    let pos = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "已核对，不再提醒" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .expect("acknowledgement button");
    for pressed in [true, false] {
        let _ = render(
            &mut panel,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
    }
    assert!(!panel.backup_unresolved());
    assert_eq!(panel.backups["first"][0].id, id);
    assert!(matches!(
        panel.backups["first"][0].state,
        MemoryBackupState::Unknown
    ));
}

#[test]
fn backup_reply_cannot_confirm_a_different_pending_memory_write() {
    let mut panel = panel();
    panel.draft = "保留的笔记".into();
    let text = panel.draft.clone();
    let request = begin(
        &mut panel,
        MemoryRequest::Remember {
            profile_id: "first".into(),
            text,
        },
    );
    assert!(!panel.receive(
        request,
        Ok(status(uuid::Uuid::new_v4(), MemoryBackupState::Unknown))
    ));
    assert!(panel.requires_review);
    assert_eq!(panel.draft, "保留的笔记");
}
