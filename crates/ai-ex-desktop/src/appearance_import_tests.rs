use super::*;

#[test]
fn dropping_during_import_keeps_current_request_and_explains_retry() {
    let (sender, receiver) = mpsc::channel();
    let mut importer = AppearanceImport {
        input: "D:\\synthetic\\first.toml".into(),
        pending: Some(receiver),
        ..Default::default()
    };
    let context = egui::Context::default();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            dropped_files: vec![egui::DroppedFile {
                path: Some(PathBuf::from("D:\\synthetic\\second.toml")),
                ..Default::default()
            }],
            ..Default::default()
        },
        |ui| {
            importer.controls(ui);
            assert!(ui.input(|input| input.raw.dropped_files.is_empty()));
        },
    );
    assert_eq!(importer.input, "D:\\synthetic\\first.toml");
    assert!(importer.is_loading());
    assert!(output.shapes.iter().any(|shape| matches!(
        &shape.shape, egui::Shape::Text(text) if text.galley.job.text == "正在读取外形，请完成后重新拖入其他文件。"
    )));
    sender
        .send(Err(AppError::configuration("synthetic")))
        .unwrap();
    assert!(!importer.poll(&context));
    assert!(!importer.is_loading());
    assert!(importer.drop_notice);
    assert_eq!(importer.input, "D:\\synthetic\\first.toml");
    let output = context.run_ui(egui::RawInput::default(), |ui| importer.controls(ui));
    assert!(output.shapes.iter().any(|shape| matches!(
        &shape.shape, egui::Shape::Text(text) if text.galley.job.text == "上次拖入的文件未导入，请重新拖入。"
    )));
}
