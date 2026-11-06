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

fn drop_files(panel: &mut BackupInspectionPanel, paths: Vec<Option<&str>>) -> usize {
    let context = egui::Context::default();
    let mut remaining = usize::MAX;
    let _ = context.run_ui(
        egui::RawInput {
            dropped_files: paths
                .into_iter()
                .map(|path| egui::DroppedFile {
                    path: path.map(std::path::PathBuf::from),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
        |ui| {
            panel.receive_drop(ui);
            remaining = ui.input(|input| input.raw.dropped_files.len());
        },
    );
    remaining
}

#[test]
fn explicit_backup_drop_only_fills_path_and_invalidates_older_request() {
    let (mut panel, id) = ready();
    let before = panel.path.clone();
    assert_eq!(
        drop_files(&mut panel, vec![Some("C:\\synthetic\\other.jsonl")]),
        1
    );
    assert_eq!(panel.path, before);
    let (old_query, _) = request_id(panel.start().unwrap());
    let generation = panel.path_generation;
    panel.arm_drop();
    assert!(
        panel
            .receive(old_query, Ok(context(id, "synthetic", 1)))
            .is_none()
    );
    assert_eq!(
        drop_files(&mut panel, vec![Some("C:\\synthetic\\memory.jsonl")]),
        0
    );
    assert!(!panel.waiting_for_drop);
    assert!(panel.pending.is_none());
    assert!(panel.path_generation > generation);
    assert!(panel.result.is_none());
    assert!(panel.drop_message.unwrap().contains("点击“检查备份”"));
}

#[test]
fn backup_drop_invalid_or_cancelled_preserves_path_and_disconnect_disarms() {
    let (mut panel, id) = ready();
    let before = panel.path.clone();
    for files in [
        vec![None],
        vec![Some("C:\\synthetic\\one"), Some("C:\\synthetic\\two")],
    ] {
        panel.waiting_for_drop = true;
        assert_eq!(drop_files(&mut panel, files), 0);
        assert!(!panel.waiting_for_drop);
        assert_eq!(panel.path, before);
        assert!(panel.drop_message.is_some());
    }
    panel.waiting_for_drop = true;
    panel.cancel_drop();
    assert_eq!(
        drop_files(&mut panel, vec![Some("C:\\synthetic\\other")]),
        1
    );
    panel.waiting_for_drop = true;
    panel.synchronize(Some(id), "synthetic", 1, false);
    assert!(!panel.waiting_for_drop);
    assert_eq!(panel.path, before);
}

#[test]
fn backup_drop_waiting_disables_check_button_for_previous_path() {
    let (mut panel, _) = ready();
    let context = egui::Context::default();
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        context.style_mut_of(theme, |style| style.animation_time = 0.0);
    }
    let draw = |panel: &mut BackupInspectionPanel, events| {
        let mut command = None;
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| command = panel.show_actions(ui),
        );
        (output, command)
    };
    let position = |output: &egui::FullOutput, label: &str| {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                let labels: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                        _ => None,
                    })
                    .collect();
                panic!("missing {label}: {labels:?}")
            })
    };
    draw(&mut panel, vec![]);
    panel.arm_drop();
    assert!(panel.start().is_none());
    let (output, _) = draw(&mut panel, vec![]);
    let button = position(&output, "检查备份");
    for pressed in [true, false] {
        let (_, command) = draw(
            &mut panel,
            vec![
                egui::Event::PointerMoved(button),
                egui::Event::PointerButton {
                    pos: button,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(command.is_none());
    }
    assert!(panel.pending.is_none());
    assert!(panel.waiting_for_drop());
}

#[test]
fn inspection_accepts_windows_copy_path_without_mutating_the_draft() {
    let (mut panel, id) = ready();
    panel.path = "\"C:\\synthetic folder\\memory.jsonl\"".into();
    panel.path_edited();
    let generation = panel.path_generation;
    let (query, _) = request_id(panel.start().unwrap());
    let (_, command) = request_id(
        panel
            .receive(query, Ok(context(id, "synthetic", 1)))
            .unwrap(),
    );
    let ControlCommand::InspectMemoryBackup { path, .. } = command else {
        panic!("inspection command")
    };
    let expected = if cfg!(windows) {
        "C:\\synthetic folder\\memory.jsonl"
    } else {
        "\"C:\\synthetic folder\\memory.jsonl\""
    };
    assert_eq!(path.as_str(), expected);
    assert_eq!(panel.path, "\"C:\\synthetic folder\\memory.jsonl\"");
    assert_eq!(panel.path_generation, generation);
}

#[test]
fn selected_path_preserves_unquoted_mismatched_and_surrounding_whitespace() {
    for value in [
        "C:\\synthetic\\memory.jsonl",
        "\"C:\\synthetic\\memory.jsonl",
        "C:\\synthetic\\memory.jsonl\"",
        " \"C:\\synthetic\\memory.jsonl\" ",
    ] {
        assert_eq!(selected_path(value), value);
    }
    let unc = "\"\\\\server\\share\\backup.jsonl\"";
    assert_eq!(
        selected_path(unc),
        if cfg!(windows) {
            "\\\\server\\share\\backup.jsonl"
        } else {
            unc
        }
    );
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
    assert!(matches!(
        panel.result.as_ref(),
        Some(InspectionResult::Current(result)) if result.memory_records == 2
    ));
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

#[test]
fn legacy_inspection_requires_explicit_format_and_discards_old_requests() {
    let (mut panel, id) = ready();
    let (old_query, _) = request_id(panel.start().unwrap());
    panel.set_format(BackupFormat::Legacy);
    assert!(
        panel
            .receive(old_query, Ok(context(id, "synthetic", 1)))
            .is_none()
    );
    assert!(panel.pending.is_none());
    assert!(panel.result.is_none());

    let (query, _) = request_id(panel.start().unwrap());
    let (inspect, command) = request_id(
        panel
            .receive(query, Ok(context(id, "synthetic", 7)))
            .unwrap(),
    );
    assert!(
        matches!(command, ControlCommand::InspectLegacyMemoryBackup {
        instance_id, ref profile_id, persona_epoch: 7, ref path
    } if instance_id == id && profile_id == "synthetic" && path.as_str() == "C:\\synthetic\\memory.jsonl")
    );
    let legacy = LegacyMemoryBackupInspection {
        records: 2,
        local_timestamp_records: 1,
        unclassified_timestamp_records: 1,
        nonempty_embedding_records: 1,
        unclassified_embedding_records: 0,
        legacy_metadata_fields_present: true,
    };
    panel.receive(
        inspect,
        Ok(ControlPayload::LegacyMemoryBackupInspection {
            instance_id: id,
            profile_id: "synthetic".into(),
            persona_epoch: 7,
            summary: legacy.clone(),
        }),
    );
    assert!(
        matches!(panel.result.as_ref(), Some(InspectionResult::Legacy(result)) if result.records == 2)
    );
    let (query, _) = request_id(panel.start().unwrap());
    let (inspect, _) = request_id(
        panel
            .receive(query, Ok(context(id, "synthetic", 8)))
            .unwrap(),
    );
    panel.set_format(BackupFormat::Current);
    panel.receive(
        inspect,
        Ok(ControlPayload::LegacyMemoryBackupInspection {
            instance_id: id,
            profile_id: "synthetic".into(),
            persona_epoch: 8,
            summary: legacy,
        }),
    );
    assert!(panel.pending.is_none());
    assert!(panel.result.is_none());
}

#[test]
fn legacy_inspection_rejects_mismatched_epoch_and_invalid_counts() {
    let (mut panel, id) = ready();
    panel.set_format(BackupFormat::Legacy);
    let (query, _) = request_id(panel.start().unwrap());
    let (inspect, _) = request_id(
        panel
            .receive(query, Ok(context(id, "synthetic", 1)))
            .unwrap(),
    );
    let mut legacy = LegacyMemoryBackupInspection {
        records: 2,
        local_timestamp_records: 1,
        unclassified_timestamp_records: 1,
        nonempty_embedding_records: 1,
        unclassified_embedding_records: 0,
        legacy_metadata_fields_present: false,
    };
    panel.receive(
        inspect,
        Ok(ControlPayload::LegacyMemoryBackupInspection {
            instance_id: id,
            profile_id: "synthetic".into(),
            persona_epoch: 2,
            summary: legacy.clone(),
        }),
    );
    assert!(panel.result.is_none());
    let (query, _) = request_id(panel.start().unwrap());
    let (inspect, _) = request_id(
        panel
            .receive(query, Ok(context(id, "synthetic", 3)))
            .unwrap(),
    );
    legacy.unclassified_timestamp_records = 2;
    panel.receive(
        inspect,
        Ok(ControlPayload::LegacyMemoryBackupInspection {
            instance_id: id,
            profile_id: "synthetic".into(),
            persona_epoch: 3,
            summary: legacy,
        }),
    );
    assert!(panel.result.is_none());
}
