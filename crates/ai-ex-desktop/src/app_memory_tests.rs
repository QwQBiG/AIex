use super::*;
use ai_ex_domain::{
    MemoryEntry, MemoryKind, MemoryPage, MemoryRequest, MemoryResponse, MemorySource, TurnId,
};
use ai_ex_observability::{RuntimeSnapshot, SequencedEvent};

fn pending(app: &mut DesktopApp, request: MemoryRequest) -> u64 {
    app.memory.set_profile("default");
    let WorkerCommand::Memory { request_id, .. } = app.memory.begin(request).unwrap() else {
        panic!("memory request");
    };
    request_id
}

fn remember() -> MemoryRequest {
    MemoryRequest::Remember {
        profile_id: "default".to_owned(),
        text: "remember this".to_owned(),
    }
}

#[test]
fn confirmed_memory_updates_discard_delayed_old_chat_events_but_allow_new_turns() {
    let (mut app, events, _sent) = app();
    let request_id = pending(&mut app, remember());
    app.input = "next draft".to_owned();
    assert!(!app.can_submit());
    events
        .send(WorkerEvent::Memory {
            request_id,
            result: Ok(Box::new(ai_ex_control::MemoryReply {
                response: MemoryResponse::Changed,
                snapshot: RuntimeSnapshot {
                    last_sequence: 9,
                    ..Default::default()
                },
            })),
        })
        .unwrap();
    events
        .send(WorkerEvent::Snapshot(RuntimeSnapshot {
            last_sequence: 4,
            ..Default::default()
        }))
        .unwrap();
    events
        .send(WorkerEvent::HistoryGap(RuntimeSnapshot {
            last_sequence: 4,
            ..Default::default()
        }))
        .unwrap();
    let old = TurnId::new();
    events
        .send(WorkerEvent::Events(vec![
            SequencedEvent {
                sequence: 5,
                event: SystemEvent::TurnStarted {
                    turn_id: old,
                    user_text: "forgotten".to_owned(),
                },
            },
            SequencedEvent {
                sequence: 6,
                event: SystemEvent::TurnFinished {
                    turn_id: old,
                    full_text: "old reply".to_owned(),
                },
            },
        ]))
        .unwrap();
    app.drain_events();
    assert!(app.state.turns.is_empty());
    assert!(!app.state.needs_resync);
    assert!(app.can_submit());
    assert_eq!(app.input, "next draft");
    let next = TurnId::new();
    events
        .send(WorkerEvent::Events(vec![
            SequencedEvent {
                sequence: 10,
                event: SystemEvent::TurnStarted {
                    turn_id: next,
                    user_text: "new turn".to_owned(),
                },
            },
            SequencedEvent {
                sequence: 11,
                event: SystemEvent::TurnFinished {
                    turn_id: next,
                    full_text: "new reply".to_owned(),
                },
            },
        ]))
        .unwrap();
    app.drain_events();
    assert_eq!(app.state.turns.len(), 1);
    assert_eq!(app.state.turns[0].assistant_text, "new reply");
}

#[test]
fn a_previous_service_acknowledgement_cannot_clear_the_new_service_chat() {
    let (mut app, events, _sent) = app();
    let request_id = pending(&mut app, remember());
    app.state.runtime.instance_id = Some(uuid::Uuid::new_v4());
    app.state.turns.push(ai_ex_ui_model::UiTurn {
        turn_id: TurnId::new(),
        user_text: "new session".to_owned(),
        assistant_text: "keep this".to_owned(),
        status: ai_ex_ui_model::TurnStatus::Completed,
    });
    events
        .send(WorkerEvent::Memory {
            request_id,
            result: Ok(Box::new(ai_ex_control::MemoryReply {
                response: MemoryResponse::Changed,
                snapshot: RuntimeSnapshot::default(),
            })),
        })
        .unwrap();
    app.drain_events();
    assert_eq!(app.state.turns[0].assistant_text, "keep this");
    assert!(app.memory.begin(remember()).is_none());
}

fn show_sample_page(app: &mut DesktopApp, long_text: bool) {
    let request_id = pending(
        app,
        MemoryRequest::List {
            profile_id: "default".to_owned(),
            query: String::new(),
            kind: None,
            offset: 0,
            limit: 12,
        },
    );
    app.memory.receive(
        request_id,
        Ok(MemoryResponse::Page(MemoryPage {
            profile_id: "default".to_owned(),
            enabled: true,
            total: 1,
            offset: 0,
            truncated_ids: vec![],
            knowledge: Default::default(),
            entries: vec![MemoryEntry {
                id: uuid::Uuid::new_v4(),
                profile_id: "default".to_owned(),
                turn_id: TurnId::new(),
                created_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis(),
                updated_ms: None,
                kind: MemoryKind::Persona,
                revision: 1,
                source: MemorySource::UserNote,
                user_text: if long_text {
                    "\n".repeat(200)
                } else {
                    "请叫我小林。学习时希望你简短回应，休息时可以多聊一会儿。".to_owned()
                },
                assistant_text: String::new(),
            }],
        })),
    );
    app.page = layout::Page::Memory;
}

#[test]
fn reading_memory_does_not_skip_conversation_events() {
    let (mut app, events, _sent) = app();
    let request_id = pending(
        &mut app,
        MemoryRequest::List {
            profile_id: "default".to_owned(),
            query: String::new(),
            kind: None,
            offset: 0,
            limit: 12,
        },
    );
    events
        .send(WorkerEvent::Memory {
            request_id,
            result: Ok(Box::new(ai_ex_control::MemoryReply {
                response: MemoryResponse::Page(MemoryPage {
                    profile_id: "default".to_owned(),
                    enabled: true,
                    total: 0,
                    offset: 0,
                    entries: vec![],
                    truncated_ids: vec![],
                    knowledge: Default::default(),
                }),
                snapshot: RuntimeSnapshot {
                    last_sequence: 100,
                    ..Default::default()
                },
            })),
        })
        .unwrap();
    app.drain_events();
    assert_eq!(app.state.runtime.last_sequence, 0);
}

fn click_at_size(app: &mut DesktopApp, context: &egui::Context, size: [f32; 2], pos: egui::Pos2) {
    for pressed in [true, false] {
        render(
            app,
            context,
            size,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

#[test]
fn forgetting_long_multiline_memory_keeps_confirmation_buttons_visible() {
    for size in [[720.0, 520.0], [1120.0, 720.0]] {
        let (mut app, _events, mut commands) = app();
        show_sample_page(&mut app, true);
        let context = egui::Context::default();
        configure_appearance(&context);
        render(&mut app, &context, size, vec![]);
        let output = render(&mut app, &context, size, vec![]);
        let button = label_position(&output, "遗忘");
        click_at_size(&mut app, &context, size, button);
        assert!(
            commands.try_recv().is_err(),
            "opening confirmation cannot delete"
        );
        render(&mut app, &context, size, vec![]);
        let output = render(&mut app, &context, size, vec![]);
        for label in ["保留记忆", "确认遗忘"] {
            let position = label_position(&output, label);
            assert!(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]))
                    .contains(position),
                "{label} stays in the window"
            );
        }
        let button = label_position(&output, "确认遗忘");
        click_at_size(&mut app, &context, size, button);
        assert!(matches!(
            commands.try_recv().unwrap(),
            WorkerCommand::Memory {
                request: MemoryRequest::Forget {
                    expected_revision: 1,
                    ..
                },
                ..
            }
        ));
    }
}

#[test]
#[ignore = "writes memory page review images without opening windows"]
fn write_memory_layout_snapshots() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/memory-ui-review-{stamp}"));
    std::fs::create_dir(&directory).unwrap();
    for (name, size) in [
        ("memory-wide.png", [1120, 720]),
        ("memory-narrow.png", [720, 520]),
    ] {
        let (mut app, _events, _sent) = app();
        app.active_persona.name = "澄".to_owned();
        show_sample_page(&mut app, false);
        let context = egui::Context::default();
        configure_appearance(&context);
        let points = [size[0] as f32, size[1] as f32];
        let mut output = render(&mut app, &context, points, vec![]);
        output.append(render(&mut app, &context, points, vec![]));
        let path = directory.join(name);
        headless_snapshot::save(&context, output, size, &path).unwrap();
        println!("{}", path.display());
    }
}
