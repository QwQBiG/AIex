use super::*;

use crate::ui::test_support::{export, pointer, position, render};

#[test]
fn narrow_welcome_keeps_the_first_steps_visible_and_selectable() {
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    let mut app = Welcome {
        configured: false,
        error: None,
        choice: Arc::new(Mutex::new(None)),
        appearance: AppearancePanel::default(),
    };
    let size = [640, 520];
    let mut draw = |ui: &mut egui::Ui| app.contents(ui);
    render(&context, size, Vec::new(), &mut draw);
    let output = render(&context, size, Vec::new(), &mut draw);
    position(&output, "先体验外形");
    let target = position(&output, "连接模型并开始对话");
    for pressed in [true, false] {
        render(&context, size, pointer(target, pressed), &mut draw);
    }
    assert_eq!(*app.choice.lock().unwrap(), Some(Choice::Setup));
}

#[test]
#[ignore = "exports onboarding screenshots without opening a native window"]
fn export_welcome_review() {
    for size in [[640, 520], [860, 580]] {
        let mut app = Welcome {
            configured: false,
            error: None,
            choice: Arc::new(Mutex::new(None)),
            appearance: AppearancePanel::default(),
        };
        export("welcome", size, |ui| app.contents(ui));
    }
}
