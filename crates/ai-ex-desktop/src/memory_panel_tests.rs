use super::*;

fn note(profile_id: &str) -> MemoryRequest {
    MemoryRequest::Remember {
        profile_id: profile_id.to_owned(),
        text: "请叫我小林".to_owned(),
    }
}

fn begin(panel: &mut MemoryPanel, request: MemoryRequest) -> u64 {
    let crate::worker::WorkerCommand::Memory { request_id, .. } = panel.begin(request).unwrap()
    else {
        panic!("memory command");
    };
    request_id
}

fn page(profile_id: &str) -> MemoryResponse {
    MemoryResponse::Page(MemoryPage {
        profile_id: profile_id.to_owned(),
        enabled: true,
        total: 0,
        offset: 0,
        entries: vec![],
        truncated_ids: vec![],
        knowledge: Default::default(),
    })
}

#[test]
fn switching_roles_keeps_each_draft_and_discards_old_results() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.draft = "first private draft".to_owned();
    let first = begin(&mut panel, note("first"));
    panel.set_profile("second");
    assert!(panel.draft.is_empty());
    assert!(panel.page.is_none());
    panel.draft = "second private draft".to_owned();
    let request = panel.list_request(0);
    let second = begin(&mut panel, request);
    assert_ne!(first, second);
    assert!(!panel.receive(first, Ok(MemoryResponse::Changed)));
    assert!(panel.pending.is_some());
    assert_eq!(panel.draft, "second private draft");
    panel.receive(second, Ok(page("second")));
    assert_eq!(panel.page.as_ref().unwrap().profile_id, "second");
    panel.set_profile("first");
    assert_eq!(panel.draft, "first private draft");
    assert!(panel.page.is_none());
}

#[test]
fn only_confirmed_mutations_clear_the_draft_and_context() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.draft = "important draft".to_owned();
    let id = begin(&mut panel, note("first"));
    assert!(panel.mutation_pending());
    assert!(panel.begin(note("first")).is_none());
    assert!(!panel.receive(id, Err(AppError::connectivity("reply lost"))));
    assert_eq!(panel.draft, "important draft");
    assert!(panel.feedback.as_ref().unwrap().contains("结果尚未确认"));
    assert!(!panel.mutation_pending());
    assert!(panel.begin(note("first")).is_none());
    let request = panel.list_request(0);
    let refreshed = begin(&mut panel, request);
    panel.receive(refreshed, Ok(page("first")));
    let id = begin(&mut panel, note("first"));
    assert!(panel.receive(id, Ok(MemoryResponse::Changed)));
    assert!(panel.draft.is_empty());
    assert!(!panel.loaded);
    assert!(!panel.receive(id, Ok(MemoryResponse::Changed)));
}

#[test]
fn wrong_scope_or_payload_cannot_replace_visible_records() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    let request = panel.list_request(0);
    let id = begin(&mut panel, request);
    assert!(!panel.receive(id, Ok(page("second"))));
    assert!(panel.page.is_none());
    assert!(panel.feedback.is_some());
    let request = panel.list_request(0);
    let id = begin(&mut panel, request);
    assert!(!panel.receive(id, Ok(MemoryResponse::Changed)));
    assert!(panel.page.is_none());
}

#[test]
fn failed_initial_list_does_not_retry_every_frame() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    let request = panel.list_request(0);
    let id = begin(&mut panel, request);
    panel.receive(id, Err(AppError::invalid_transition("active turn")));
    assert!(panel.loaded);
    assert!(panel.pending.is_none());
    let context = eframe::egui::Context::default();
    let mut requested = None;
    let _output = context.run_ui(Default::default(), |ui| {
        requested = panel.show(ui, "伙伴", true, true);
    });
    assert!(requested.is_none());
}

#[test]
fn forgetting_does_not_clear_an_unrelated_note_draft() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    panel.draft = "keep this unrelated note".to_owned();
    let id = begin(
        &mut panel,
        MemoryRequest::Forget {
            profile_id: "first".to_owned(),
            id: uuid::Uuid::new_v4(),
            expected_revision: 1,
        },
    );
    assert!(panel.receive(id, Ok(MemoryResponse::Changed)));
    assert_eq!(panel.draft, "keep this unrelated note");
}

#[test]
fn back_navigation_remembers_actual_page_sizes() {
    let mut panel = MemoryPanel::default();
    panel.set_profile("first");
    for offset in [0, 2, 5] {
        let request = panel.list_request(offset);
        let id = begin(&mut panel, request);
        let MemoryResponse::Page(mut page) = page("first") else {
            unreachable!();
        };
        page.offset = offset;
        panel.receive(id, Ok(MemoryResponse::Page(page)));
    }
    assert_eq!(panel.previous_offset(), 2);
    let request = panel.list_request(panel.previous_offset());
    let id = begin(&mut panel, request);
    let MemoryResponse::Page(mut page) = page("first") else {
        unreachable!();
    };
    page.offset = 2;
    panel.receive(id, Ok(MemoryResponse::Page(page)));
    assert_eq!(panel.previous_offset(), 0);
}
