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

fn ready() -> (BackupInspectionPanel, Uuid) {
    let instance = Uuid::new_v4();
    let mut panel = BackupInspectionPanel {
        instance: Some(instance),
        profile: "synthetic".into(),
        ready: true,
        ..Default::default()
    };
    let directory = std::env::temp_dir();
    panel.merge.base = directory
        .join("synthetic-base.jsonl")
        .to_string_lossy()
        .into_owned();
    panel.merge.incoming = directory
        .join("synthetic-incoming.jsonl")
        .to_string_lossy()
        .into_owned();
    panel.merge.output_directory = directory.to_string_lossy().into_owned();
    (panel, instance)
}

fn summary() -> MemoryMergeSummary {
    MemoryMergeSummary {
        base_memory_records: 1,
        incoming_memory_records: 2,
        entity_candidates: 1,
        source_associations: 1,
        inactive_source_associations: 1,
        distinct_embedding_spaces: 0,
        base_empty: false,
        incoming_empty: false,
    }
}

fn prepare(panel: &mut BackupInspectionPanel, instance: Uuid) {
    let (id, command) = unpack(panel.prepare_merge().unwrap());
    assert!(matches!(command, ControlCommand::MemoryInspectionContext));
    let (id, command) = unpack(
        panel
            .receive(
                id,
                Ok(ControlPayload::MemoryInspectionContext {
                    instance_id: instance,
                    profile_id: "synthetic".into(),
                    persona_epoch: 7,
                }),
            )
            .unwrap(),
    );
    assert!(matches!(
        command,
        ControlCommand::PrepareMergeCandidate { .. }
    ));
    assert!(
        panel
            .receive(
                id,
                Ok(ControlPayload::MergeCandidatePreview {
                    instance_id: instance,
                    profile_id: "synthetic".into(),
                    persona_epoch: 7,
                    preview_id: Some(Uuid::new_v4()),
                    expires_in_seconds: 300,
                    result: MergeCandidatePreviewResult::Ready { summary: summary() },
                })
            )
            .is_none()
    );
}

#[test]
fn merge_requires_review_and_tracks_original_role_after_switch() {
    let (mut panel, instance) = ready();
    assert!(panel.confirm_merge().is_none());
    prepare(&mut panel, instance);
    panel.review_merge();
    let (id, command) = unpack(panel.confirm_merge().unwrap());
    let ControlCommand::StartMergeCandidate { operation_id, .. } = command else {
        panic!("merge start expected")
    };
    assert!(panel.candidate_unresolved());
    panel.receive(id, Err(AppError::connectivity("synthetic timeout")));
    panel.synchronize(Some(instance), "other", 1, true);
    assert!(panel.candidate_unresolved());
    assert!(panel.merge.base.is_empty() && panel.merge.incoming.is_empty());
    let (id, command) = unpack(panel.query_merge().unwrap());
    assert!(
        matches!(command, ControlCommand::MergeCandidateStatus { operation_id: value, .. } if value == operation_id)
    );
    let output = panel.merge.tracked.as_ref().unwrap().output.clone();
    panel.receive(
        id,
        Ok(ControlPayload::MergeCandidateStatus(
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
fn edited_sources_revoke_old_context_and_expired_preview() {
    let (mut panel, instance) = ready();
    let (id, command) = unpack(panel.prepare_merge().unwrap());
    assert!(matches!(command, ControlCommand::MemoryInspectionContext));
    panel.merge_sources_edited();
    assert!(
        panel
            .receive(
                id,
                Ok(ControlPayload::MemoryInspectionContext {
                    instance_id: instance,
                    profile_id: "synthetic".into(),
                    persona_epoch: 7,
                })
            )
            .is_none()
    );
    assert!(panel.merge.preview.is_none());
    prepare(&mut panel, instance);
    panel.review_merge();
    assert!(panel.merge.confirmation.is_some());
    panel.merge.preview.as_mut().unwrap().expires_at = Instant::now();
    assert!(panel.confirm_merge().is_none());
    assert!(panel.merge.tracked.is_none());
}

#[test]
fn typed_conflict_cannot_create_a_preview_or_trigger_writes() {
    let (mut panel, instance) = ready();
    let (id, _) = unpack(panel.prepare_merge().unwrap());
    let (id, _) = unpack(
        panel
            .receive(
                id,
                Ok(ControlPayload::MemoryInspectionContext {
                    instance_id: instance,
                    profile_id: "synthetic".into(),
                    persona_epoch: 7,
                }),
            )
            .unwrap(),
    );
    panel.receive(
        id,
        Ok(ControlPayload::MergeCandidatePreview {
            instance_id: instance,
            profile_id: "synthetic".into(),
            persona_epoch: 7,
            preview_id: None,
            expires_in_seconds: 0,
            result: MergeCandidatePreviewResult::Conflict {
                reason: MemoryMergeConflict::EmbeddingSpaceOverlap,
            },
        }),
    );
    assert!(panel.merge.preview.is_none());
    assert!(panel.merge.feedback.unwrap().contains("向量空间"));
    panel.review_merge();
    assert!(panel.confirm_merge().is_none());
    assert!(panel.merge.tracked.is_none());
}

#[test]
fn mismatched_status_cannot_claim_a_file_was_written() {
    let (mut panel, instance) = ready();
    prepare(&mut panel, instance);
    panel.review_merge();
    let (id, command) = unpack(panel.confirm_merge().unwrap());
    let ControlCommand::StartMergeCandidate { operation_id, .. } = command else {
        panic!("merge start expected")
    };
    panel.receive(
        id,
        Ok(ControlPayload::MergeCandidateStatus(
            LegacyCandidateStatus {
                instance_id: instance,
                operation_id,
                profile_id: Some("synthetic".into()),
                output: Some(SensitivePath::new("wrong-output")),
                state: LegacyCandidateState::Written {
                    temporary_retained: false,
                },
            },
        )),
    );
    assert!(matches!(
        panel.merge.tracked.as_ref().unwrap().state,
        LegacyCandidateState::Unknown
    ));
    assert!(panel.candidate_unresolved());
}

#[test]
fn same_source_and_unbounded_summaries_are_not_accepted() {
    let (mut panel, _) = ready();
    panel.merge.incoming = panel.merge.base.clone();
    assert!(panel.prepare_merge().is_none());
    let mut invalid = summary();
    invalid.base_empty = true;
    assert!(!valid_summary(&invalid));
    invalid = summary();
    invalid.inactive_source_associations = 2;
    assert!(!valid_summary(&invalid));
    invalid = summary();
    invalid.distinct_embedding_spaces = 17;
    assert!(!valid_summary(&invalid));
}

#[test]
fn visible_merge_flow_sends_only_after_check_review_and_confirmation() {
    let (mut panel, instance) = ready();
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
                    egui::vec2(1400.0, 1100.0),
                )),
                events,
                ..Default::default()
            },
            |ui| command = panel.show_merge(ui),
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
            .unwrap_or_else(|| panic!("missing {label}"))
    };
    let click = |panel: &mut BackupInspectionPanel, point| {
        let mut result = None;
        for pressed in [true, false] {
            let (_, command) = draw(
                panel,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            if command.is_some() {
                result = command;
            }
        }
        result
    };
    let (output, command) = draw(&mut panel, vec![]);
    assert!(command.is_none());
    assert!(click(&mut panel, position(&output, "合并两份备份为独立候选")).is_none());
    let (output, _) = draw(&mut panel, vec![]);
    let (id, command) = unpack(click(&mut panel, position(&output, "检查两份备份")).unwrap());
    assert!(matches!(command, ControlCommand::MemoryInspectionContext));
    let (id, command) = unpack(
        panel
            .receive(
                id,
                Ok(ControlPayload::MemoryInspectionContext {
                    instance_id: instance,
                    profile_id: "synthetic".into(),
                    persona_epoch: 7,
                }),
            )
            .unwrap(),
    );
    assert!(matches!(
        command,
        ControlCommand::PrepareMergeCandidate { .. }
    ));
    panel.receive(
        id,
        Ok(ControlPayload::MergeCandidatePreview {
            instance_id: instance,
            profile_id: "synthetic".into(),
            persona_epoch: 7,
            preview_id: Some(Uuid::new_v4()),
            expires_in_seconds: 300,
            result: MergeCandidatePreviewResult::Ready { summary: summary() },
        }),
    );
    let (output, _) = draw(&mut panel, vec![]);
    assert!(click(&mut panel, position(&output, "审阅合并候选")).is_none());
    assert!(panel.merge.tracked.is_none());
    let operation = panel.merge.confirmation.as_ref().unwrap().operation_id;
    let (output, _) = draw(&mut panel, vec![]);
    let (_, command) = unpack(click(&mut panel, position(&output, "确认生成合并候选")).unwrap());
    assert!(
        matches!(command, ControlCommand::StartMergeCandidate { operation_id, .. } if operation_id == operation)
    );
    assert!(panel.candidate_unresolved());
}
