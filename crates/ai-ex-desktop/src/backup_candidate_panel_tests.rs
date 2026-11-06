use super::*;

fn unpack(command: WorkerCommand) -> (u64, ControlCommand) {
    let WorkerCommand::BackupInspection {
        request_id,
        command,
        ..
    } = command
    else {
        panic!("backup request expected")
    };
    (request_id, command)
}

fn ready() -> (BackupInspectionPanel, Uuid, LegacyMemoryBackupInspection) {
    let instance = Uuid::new_v4();
    let summary = LegacyMemoryBackupInspection {
        records: 1,
        local_timestamp_records: 1,
        unclassified_timestamp_records: 0,
        nonempty_embedding_records: 1,
        unclassified_embedding_records: 0,
        legacy_metadata_fields_present: true,
    };
    let panel = BackupInspectionPanel {
        instance: Some(instance),
        profile: "synthetic".into(),
        ready: true,
        path: "C:\\synthetic\\old.json.gz".into(),
        format: BackupFormat::Legacy,
        result: Some(InspectionResult::Legacy(summary.clone())),
        ..Default::default()
    };
    (panel, instance, summary)
}

fn prepare(
    panel: &mut BackupInspectionPanel,
    instance: Uuid,
    summary: LegacyMemoryBackupInspection,
) {
    let (id, request) = unpack(panel.prepare_candidate().unwrap());
    assert!(matches!(request, ControlCommand::MemoryInspectionContext));
    let (id, request) = unpack(
        panel
            .receive(
                id,
                Ok(ControlPayload::MemoryInspectionContext {
                    instance_id: instance,
                    profile_id: "synthetic".into(),
                    persona_epoch: 3,
                }),
            )
            .unwrap(),
    );
    assert!(matches!(
        request,
        ControlCommand::PrepareLegacyCandidate { .. }
    ));
    assert!(
        panel
            .receive(
                id,
                Ok(ControlPayload::LegacyCandidatePreview {
                    instance_id: instance,
                    profile_id: "synthetic".into(),
                    persona_epoch: 3,
                    preview_id: Uuid::new_v4(),
                    expires_in_seconds: 300,
                    summary,
                })
            )
            .is_none()
    );
}

#[test]
fn candidate_requires_review_and_confirm_then_tracks_uncertain_result() {
    let (mut panel, instance, summary) = ready();
    prepare(&mut panel, instance, summary);
    panel.candidate.output_directory = std::env::temp_dir().to_string_lossy().into_owned();
    panel.review_candidate();
    assert!(panel.candidate.confirmation.is_none());
    panel.candidate.utc_offset = "480".into();
    panel.candidate.embedding_space = "synthetic-space".into();
    panel.review_candidate();
    assert!(panel.candidate.confirmation.is_some());
    let (id, command) = unpack(panel.confirm_candidate().unwrap());
    let ControlCommand::StartLegacyCandidate { operation_id, .. } = command else {
        panic!("candidate start expected")
    };
    assert!(panel.candidate_unresolved());
    panel.receive(id, Err(AppError::connectivity("synthetic timeout")));
    panel.synchronize(Some(instance), "other", 1, true);
    assert!(panel.candidate_unresolved());
    assert_eq!(
        panel.candidate.tracked.as_ref().unwrap().operation_id,
        operation_id
    );
    let (id, command) = unpack(panel.query_candidate().unwrap());
    assert!(
        matches!(command, ControlCommand::LegacyCandidateStatus { operation_id: actual, .. } if actual == operation_id)
    );
    let output = panel.candidate.tracked.as_ref().unwrap().output.clone();
    panel.receive(
        id,
        Ok(ControlPayload::LegacyCandidateStatus(
            LegacyCandidateStatus {
                instance_id: instance,
                operation_id,
                profile_id: Some("synthetic".into()),
                output: Some(SensitivePath::new(output)),
                state: LegacyCandidateState::Written {
                    temporary_retained: false,
                },
            },
        )),
    );
    assert!(!panel.candidate_unresolved());
}

#[test]
fn candidate_path_change_invalidates_preview_and_late_context() {
    let (mut panel, instance, _) = ready();
    let (id, _) = unpack(panel.prepare_candidate().unwrap());
    panel.path_edited();
    assert!(
        panel
            .receive(
                id,
                Ok(ControlPayload::MemoryInspectionContext {
                    instance_id: instance,
                    profile_id: "synthetic".into(),
                    persona_epoch: 3,
                })
            )
            .is_none()
    );
    assert!(panel.candidate.preview.is_none());
    assert!(panel.confirm_candidate().is_none());
}

#[test]
fn expired_candidate_preview_cannot_be_confirmed() {
    let (mut panel, instance, summary) = ready();
    prepare(&mut panel, instance, summary);
    panel.candidate.output_directory = std::env::temp_dir().to_string_lossy().into_owned();
    panel.candidate.utc_offset = "480".into();
    panel.candidate.embedding_space = "synthetic-space".into();
    panel.review_candidate();
    assert!(panel.candidate.confirmation.is_some());
    panel.candidate.preview.as_mut().unwrap().expires_at = Instant::now();
    assert!(panel.confirm_candidate().is_none());
    assert!(panel.candidate.tracked.is_none());
}

#[test]
fn confirmed_candidate_button_dispatches_the_reviewed_request() {
    let (mut panel, instance, summary) = ready();
    prepare(&mut panel, instance, summary);
    panel.candidate.output_directory = std::env::temp_dir().to_string_lossy().into_owned();
    panel.candidate.utc_offset = "480".into();
    panel.candidate.embedding_space = "synthetic-space".into();
    panel.review_candidate();
    let expected = panel.candidate.confirmation.as_ref().unwrap().operation_id;
    let context = egui::Context::default();
    let draw = |panel: &mut BackupInspectionPanel, events| {
        let mut command = None;
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| command = panel.show_candidate(ui),
        );
        (output, command)
    };
    let (output, _) = draw(&mut panel, vec![]);
    let button = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "确认生成候选" => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap();
    let mut sent = None;
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
        if command.is_some() {
            sent = command;
        }
    }
    let (_, command) = unpack(sent.unwrap());
    assert!(
        matches!(command, ControlCommand::StartLegacyCandidate { operation_id, .. } if operation_id == expected)
    );
    assert!(panel.candidate_unresolved());
}
