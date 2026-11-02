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
fn diagnostics_export_has_only_typed_allowlist_and_unique_create_new_files() {
    let report = ai_ex_domain::DesktopDiagnosticReport {
        schema_version: 1,
        application_version: "test".into(),
        desktop: snapshot(Uuid::new_v4(), 1, false),
        uptime_seconds: 5,
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
