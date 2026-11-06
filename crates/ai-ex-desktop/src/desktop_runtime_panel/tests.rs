use super::*;

fn snapshot(id: Uuid, revision: u64, captions: bool) -> DesktopRuntimeSnapshot {
    DesktopRuntimeSnapshot {
        instance_id: id,
        revision,
        settings: DesktopRuntimeSettings {
            captions_visible: captions,
            ..Default::default()
        },
        hotkeys_registered: false,
        hotkeys_error: false,
        proactive_available: true,
        vts_discovery_available: false,
    }
}

fn receive(panel: &mut DesktopRuntimePanel, state: DesktopRuntimeSnapshot) {
    let WorkerCommand::DesktopRuntime { request_id, .. } =
        panel.begin(DesktopRuntimeRequest::State).unwrap()
    else {
        unreachable!()
    };
    panel.receive(request_id, Ok(DesktopRuntimeReply::State(state)));
}

#[test]
fn captions_follow_confirmed_absolute_state_without_replaying_old_replies() {
    let id = Uuid::new_v4();
    let mut panel = DesktopRuntimePanel::default();
    panel.synchronize(Some(id), true);
    receive(&mut panel, snapshot(id, 2, true));
    assert!(panel.captions_visible());
    receive(&mut panel, snapshot(id, 1, false));
    assert!(panel.captions_visible());
    receive(&mut panel, snapshot(id, 3, false));
    assert!(!panel.captions_visible());
    panel.synchronize(Some(Uuid::new_v4()), true);
    receive(&mut panel, snapshot(id, 4, true));
    assert!(!panel.captions_visible());
    panel.synchronize(Some(id), false);
    assert!(panel.begin(DesktopRuntimeRequest::State).is_none());
}

#[test]
fn external_changes_preserve_dirty_draft_and_require_review_before_applying() {
    let id = Uuid::new_v4();
    let mut panel = DesktopRuntimePanel::default();
    panel.synchronize(Some(id), true);
    receive(&mut panel, snapshot(id, 0, false));
    panel.draft.cooldown_seconds = 600;
    receive(&mut panel, snapshot(id, 1, true));
    assert_eq!(panel.draft.cooldown_seconds, 600);
    assert!(panel.mutation(false).is_none());
    assert_eq!(panel.draft_revision, 0);
    let context = egui::Context::default();
    let output = context.run_ui(Default::default(), |ui| {
        assert!(panel.show(ui).is_empty());
    });
    assert!(!output.shapes.is_empty());
    assert!(panel.pending.is_empty());
}

#[test]
fn vts_parameter_pages_are_correlated_and_cleared_on_disconnect() {
    let id = Uuid::new_v4();
    let mut panel = DesktopRuntimePanel::default();
    panel.synchronize(Some(id), true);
    let WorkerCommand::DesktopRuntime { request_id, .. } = panel
        .begin(DesktopRuntimeRequest::DiscoverVtsParameters { offset: 16 })
        .unwrap()
    else {
        unreachable!()
    };
    let page = VtsParameterPage {
        model_loaded: true,
        offset: 16,
        total: 18,
        parameters: (0..2)
            .map(|index| ai_ex_domain::DiscoveredVtsParameter {
                name: format!("parameter-{index}"),
                added_by: "fixture".into(),
                value: 1.into(),
                min: 0.into(),
                max: 1.into(),
                default_value: 0.into(),
                is_custom: false,
            })
            .collect(),
    };
    let mut malformed = page.clone();
    malformed.parameters.clear();
    panel.receive(
        request_id,
        Ok(DesktopRuntimeReply::VtsParameters(malformed)),
    );
    assert!(panel.vts_parameters.is_none());
    let WorkerCommand::DesktopRuntime { request_id, .. } = panel
        .begin(DesktopRuntimeRequest::DiscoverVtsParameters { offset: 16 })
        .unwrap()
    else {
        unreachable!()
    };
    panel.receive(request_id, Ok(DesktopRuntimeReply::VtsParameters(page)));
    assert_eq!(panel.vts_parameters.as_ref().unwrap().offset, 16);
    panel.synchronize(Some(id), false);
    assert!(panel.vts_parameters.is_none());
}

#[test]
fn vts_filtered_parameter_pages_keep_filter_and_reject_mismatched_replies() {
    let id = Uuid::new_v4();
    let mut panel = DesktopRuntimePanel::default();
    panel.synchronize(Some(id), true);
    let page = VtsParameterPage {
        model_loaded: true,
        offset: 0,
        total: 0,
        parameters: vec![],
    };
    let request = parameter_request(Some(VtsParameterFilter::Mouth), 0);
    let WorkerCommand::DesktopRuntime { request_id, .. } = panel.begin(request.clone()).unwrap()
    else {
        unreachable!()
    };
    assert!(
        panel
            .begin(parameter_request(Some(VtsParameterFilter::Voice), 0))
            .is_none()
    );
    panel.receive(
        request_id,
        Ok(DesktopRuntimeReply::VtsFilteredParameters {
            filter: VtsParameterFilter::Voice,
            page: page.clone(),
        }),
    );
    assert!(panel.vts_parameters.is_none());
    let WorkerCommand::DesktopRuntime { request_id, .. } = panel.begin(request).unwrap() else {
        unreachable!()
    };
    panel.receive(
        request_id,
        Ok(DesktopRuntimeReply::VtsFilteredParameters {
            filter: VtsParameterFilter::Mouth,
            page,
        }),
    );
    assert_eq!(panel.vts_parameter_filter, Some(VtsParameterFilter::Mouth));
    assert_eq!(panel.vts_parameters.as_ref().unwrap().total, 0);
    assert!(matches!(
        parameter_request(panel.vts_parameter_filter, 16),
        DesktopRuntimeRequest::DiscoverVtsParametersFiltered {
            filter: VtsParameterFilter::Mouth,
            offset: 16
        }
    ));
    panel.synchronize(Some(id), false);
    assert!(panel.vts_parameters.is_none());
}

#[test]
fn vts_filtered_parameter_feedback_and_page_labels_explain_next_steps() {
    let mut panel = DesktopRuntimePanel::default();
    panel.synchronize(Some(Uuid::new_v4()), true);
    panel.vts_parameters = Some(VtsParameterPage {
        model_loaded: true,
        offset: 0,
        total: 0,
        parameters: vec![],
    });
    let WorkerCommand::DesktopRuntime { request_id, .. } = panel
        .begin(parameter_request(Some(VtsParameterFilter::Mouth), 16))
        .unwrap()
    else {
        unreachable!()
    };
    assert!(panel.vts_parameters.is_none());
    panel.receive(request_id, Err(AppError::connectivity("fixture")));
    let feedback = panel.feedback.as_deref().unwrap();
    assert!(feedback.contains("旧版服务") && feedback.contains("全部输入参数"));
    let WorkerCommand::DesktopRuntime { request_id, .. } = panel
        .begin(parameter_request(Some(VtsParameterFilter::Mouth), 16))
        .unwrap()
    else {
        unreachable!()
    };
    panel.receive(
        request_id,
        Ok(DesktopRuntimeReply::VtsFilteredParameters {
            filter: VtsParameterFilter::Mouth,
            page: VtsParameterPage {
                model_loaded: true,
                offset: 2,
                total: 2,
                parameters: vec![],
            },
        }),
    );
    let context = egui::Context::default();
    let render = |panel: &mut DesktopRuntimePanel| {
        context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 1600.0),
                )),
                ..Default::default()
            },
            |ui| {
                panel.show(ui);
            },
        )
    };
    let output = render(&mut panel);
    let labels: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(labels.iter().any(|label| label.contains("参数列表已变化")));
    assert!(labels.contains(&"回到首页"));
    panel.vts_parameters = Some(VtsParameterPage {
        model_loaded: true,
        offset: 0,
        total: 2,
        parameters: [false, true]
            .into_iter()
            .map(|is_custom| ai_ex_domain::DiscoveredVtsParameter {
                name: "fixture".into(),
                added_by: String::new(),
                value: 0.into(),
                min: 0.into(),
                max: 1.into(),
                default_value: 0.into(),
                is_custom,
            })
            .collect(),
    });
    let output = render(&mut panel);
    let labels: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(labels.iter().any(|label| label.contains("（默认参数）")));
    assert!(labels.iter().any(|label| label.contains("（自定义参数）")));
}

fn mappings(model_loaded: bool) -> VtsMappingCheck {
    use ai_ex_domain::{Emotion, VtsMappingStatus};
    VtsMappingCheck {
        model_loaded,
        mappings: [
            Emotion::Neutral,
            Emotion::Happy,
            Emotion::Sad,
            Emotion::Angry,
            Emotion::Surprised,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, emotion)| VtsMappingStatus {
            emotion,
            status: if index == 0 {
                VtsMappingState::Unconfigured
            } else if !model_loaded {
                VtsMappingState::Unknown
            } else if index == 1 {
                VtsMappingState::Matched
            } else {
                VtsMappingState::Missing
            },
            hotkey_id: (index != 0).then(|| format!("fixture-{index}")),
            id_truncated: index == 4,
        })
        .collect(),
    }
}

fn begin_mapping_check(panel: &mut DesktopRuntimePanel) -> u64 {
    let WorkerCommand::DesktopRuntime { request_id, .. } = panel
        .begin(DesktopRuntimeRequest::CheckVtsMappings)
        .unwrap()
    else {
        unreachable!()
    };
    request_id
}

#[test]
fn vts_mapping_check_displays_saved_ids_and_distinct_states() {
    let id = Uuid::new_v4();
    let mut panel = DesktopRuntimePanel::default();
    panel.synchronize(Some(id), true);
    let request = begin_mapping_check(&mut panel);
    assert!(
        panel
            .begin(DesktopRuntimeRequest::CheckVtsMappings)
            .is_none()
    );
    panel.receive(
        request,
        Ok(DesktopRuntimeReply::VtsMappingCheck(mappings(true))),
    );
    let context = egui::Context::default();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 1600.0),
            )),
            ..Default::default()
        },
        |ui| {
            panel.show(ui);
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(labels.contains(&"平静：未设置 · —"));
    assert!(labels.contains(&"开心：匹配 · fixture-1"));
    assert!(labels.contains(&"低落：未找到热键 · fixture-2"));
    assert!(labels.contains(&"惊讶：未找到热键 · fixture-4（已截短）"));
    assert!(labels.iter().any(|label| label.contains("快照")));
}

#[test]
fn vts_mapping_check_no_model_disconnect_and_old_reply_are_not_missing() {
    let id = Uuid::new_v4();
    let mut panel = DesktopRuntimePanel::default();
    panel.synchronize(Some(id), true);
    let request = begin_mapping_check(&mut panel);
    panel.receive(
        request,
        Ok(DesktopRuntimeReply::VtsMappingCheck(mappings(false))),
    );
    let result = panel.vts_mappings.as_ref().unwrap();
    assert!(result.mappings.iter().all(|item| matches!(
        item.status,
        VtsMappingState::Unconfigured | VtsMappingState::Unknown
    )));
    let request = begin_mapping_check(&mut panel);
    assert!(panel.vts_mappings.is_none());
    panel.synchronize(Some(id), false);
    panel.synchronize(Some(id), true);
    let newer = begin_mapping_check(&mut panel);
    panel.receive(
        request,
        Ok(DesktopRuntimeReply::VtsMappingCheck(mappings(true))),
    );
    assert!(panel.vts_mappings.is_none());
    panel.receive(
        newer,
        Ok(DesktopRuntimeReply::VtsMappingCheck(mappings(false))),
    );
    assert!(!panel.vts_mappings.as_ref().unwrap().model_loaded);
}

#[test]
fn vts_mapping_check_error_and_invalid_no_model_snapshot_clear_previous_result() {
    let mut panel = DesktopRuntimePanel::default();
    panel.synchronize(Some(Uuid::new_v4()), true);
    let request = begin_mapping_check(&mut panel);
    panel.receive(request, Err(AppError::connectivity("fixture")));
    assert!(panel.vts_mappings.is_none());
    assert!(panel.feedback.as_ref().unwrap().contains("无法核对"));
    let request = begin_mapping_check(&mut panel);
    assert!(panel.feedback.is_none());
    let mut malformed = mappings(false);
    malformed.mappings[1].status = VtsMappingState::Missing;
    panel.receive(request, Ok(DesktopRuntimeReply::VtsMappingCheck(malformed)));
    assert!(panel.vts_mappings.is_none());
    assert!(panel.feedback.is_some());
}

#[test]
fn diagnostic_observations_are_displayed_and_cleared_with_the_connection() {
    use ai_ex_domain::{DiagnosticComponent, DiagnosticComponentObservation};
    let id = Uuid::new_v4();
    let mut panel = DesktopRuntimePanel::default();
    panel.synchronize(Some(id), true);
    let WorkerCommand::DesktopRuntime { request_id, .. } =
        panel.begin(DesktopRuntimeRequest::Diagnostics).unwrap()
    else {
        unreachable!()
    };
    panel.receive(
        request_id,
        Ok(DesktopRuntimeReply::Diagnostics(
            ai_ex_domain::DesktopDiagnosticReport {
                schema_version: 1,
                application_version: "test".into(),
                desktop: snapshot(Uuid::new_v4(), 1, false),
                uptime_seconds: 5,
                component_observations: Vec::new(),
            },
        )),
    );
    assert!(panel.component_observations.is_none());
    panel.component_observations = Some(vec![
        DiagnosticComponentObservation {
            component: DiagnosticComponent::Memory,
            observed_ready: true,
        },
        DiagnosticComponentObservation {
            component: DiagnosticComponent::Vts,
            observed_ready: false,
        },
    ]);
    let context = egui::Context::default();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 1600.0),
            )),
            ..Default::default()
        },
        |ui| assert!(panel.show(ui).is_empty()),
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(labels.contains(&"记录为可用：记忆"));
    assert!(labels.contains(&"关闭或不可用：VTube Studio"));
    assert!(
        labels
            .iter()
            .any(|label| label.starts_with("暂无记录：") && label.contains("语音识别"))
    );
    assert!(labels.iter().any(|label| label.contains("可能已过期")));
    panel.synchronize(Some(id), false);
    assert!(panel.component_observations.is_none());
    panel.component_observations = Some(Vec::new());
    panel.synchronize(Some(Uuid::new_v4()), true);
    assert!(panel.component_observations.is_none());
}

#[test]
fn diagnostics_export_has_only_typed_allowlist_and_unique_create_new_files() {
    let report = ai_ex_domain::DesktopDiagnosticReport {
        schema_version: 1,
        application_version: "test".into(),
        desktop: snapshot(Uuid::new_v4(), 1, false),
        uptime_seconds: 5,
        component_observations: Vec::new(),
    };
    let first = export_report(&report).unwrap();
    let second = export_report(&report).unwrap();
    assert_ne!(first, second);
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&first).unwrap()).unwrap();
    let object = value.as_object().unwrap();
    assert_eq!(object.len(), 4);
    for key in ["logs", "messages", "reference_audio", "token", "path"] {
        assert!(!object.contains_key(key));
    }
    std::fs::remove_file(first).unwrap();
    std::fs::remove_file(second).unwrap();
}
