use super::*;
use crate::ui::test_support as review;

#[test]
fn setup_keeps_save_and_errors_visible_without_discarding_fields() {
    let context = egui::Context::default();
    crate::ui::theme::configure_appearance(&context);
    let mut app = SetupApp::new(
        PathBuf::from("unused-config.toml"),
        Arc::new(Mutex::new(None)),
        None,
    );
    app.persona_name.clear();
    app.api_key = "test-secret".to_owned();
    let size = [640, 520];
    let mut draw = |ui: &mut egui::Ui| app.show_window(ui);
    review::render(&context, size, Vec::new(), &mut draw);
    let output = review::render(&context, size, Vec::new(), &mut draw);
    assert!(
        output.shapes.iter().any(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "API Key" => shape
                .clip_rect
                .contains_rect(egui::Rect::from_min_size(text.pos, text.galley.size())),
            _ => false,
        }),
        "the credential field label is fully visible in the smallest setup window"
    );
    let target = review::position(&output, "保存并进入 AIex");
    for pressed in [true, false] {
        review::render(&context, size, review::pointer(target, pressed), &mut draw);
    }
    assert!(app.status_error);
    assert!(app.result.lock().unwrap().is_none());
    assert_eq!(app.api_key, "test-secret");
    let output = review::render(&context, size, Vec::new(), |ui| app.show_window(ui));
    review::position(&output, "请填写角色名、模型地址和模型名称。");
    review::position(&output, "保存并进入 AIex");
    review::position(&output, "返回首页（不保存修改）");
}

#[test]
#[ignore = "exports setup screenshots without opening a native window"]
fn export_setup_review() {
    for size in [[640, 520], [760, 620]] {
        let mut app = SetupApp::new(
            PathBuf::from("data/ai-ex.local.toml"),
            Arc::new(Mutex::new(None)),
            None,
        );
        review::export("setup", size, |ui| app.show_window(ui));
    }
}
