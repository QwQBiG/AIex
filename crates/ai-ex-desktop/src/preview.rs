use ai_ex_domain::{AppError, ConversationState, Emotion};
use ai_ex_ui_model::PresentationState;
use eframe::egui;

use crate::appearance::AppearancePanel;
use crate::navigation::{Destination, Navigation};
use crate::ui::theme;

#[path = "oc_gallery.rs"]
mod gallery;

pub fn run(package: Option<std::path::PathBuf>) -> Result<Option<Destination>, AppError> {
    let decoded = package
        .as_deref()
        .map(crate::image_appearance::DecodedAppearance::load)
        .transpose()?;
    let options = crate::navigation::companion_window([980.0, 720.0], [640.0, 520.0]);
    let navigation = Navigation::default();
    let result = navigation.clone();
    eframe::run_native(
        "AIex 外形工作室",
        options,
        Box::new(move |context| {
            theme::configure_appearance(&context.egui_ctx);
            let mut appearance = AppearancePanel::load(context.storage);
            if let Some(decoded) = decoded {
                appearance.use_images(&context.egui_ctx, decoded);
            }
            Ok(Box::new(PreviewApp {
                navigation,
                appearance,
                gallery: gallery::OcGallery::load(context.storage),
                name: "AIex".to_owned(),
                state: PresentationState {
                    connected: true,
                    synchronized: true,
                    activity: ConversationState::Idle,
                    emotion: Emotion::Neutral,
                    mouth_level: None,
                },
            }))
        }),
    )
    .map_err(|error| AppError::unavailable(error.to_string()))?;
    Ok(result.take())
}

struct PreviewApp {
    navigation: Navigation,
    appearance: AppearancePanel,
    gallery: gallery::OcGallery,
    name: String,
    state: PresentationState,
}

impl PreviewApp {
    fn show_contents(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("外形工作室")
                .color(theme::ACCENT)
                .strong(),
        );
        ui.heading("陪伴，也可以有你的风格");
        ui.weak("在这里试试表情与外形，按自己的喜好慢慢调整。");
        ui.add_space(16.0);
        if ui.available_width() >= 560.0 {
            let wide = ui.available_width() >= 740.0;
            let stage_width = (ui.available_width() * if wide { 0.55 } else { 0.39 }).min(580.0);
            let portrait_height = if wide { 400.0 } else { 210.0 };
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(stage_width, portrait_height + 90.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.show_stage(ui, portrait_height),
                );
                ui.add_space(8.0);
                ui.vertical(|ui| self.show_editor(ui));
            });
        } else {
            self.show_stage(ui, 180.0);
            ui.add_space(12.0);
            self.show_editor(ui);
        }
    }

    fn show_stage(&mut self, ui: &mut egui::Ui, height: f32) {
        self.appearance.poll(ui.ctx());
        theme::card().fill(theme::SURFACE_ALT).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.strong("人物舞台");
                ui.weak("离线预览");
            });
            self.appearance
                .show_portrait(ui, self.state, &self.name, height);
        });
    }

    fn show_editor(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("此刻的状态");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = ui.label("预览称呼");
                ui.add(
                    egui::TextEdit::singleline(&mut self.name)
                        .hint_text("角色称呼")
                        .desired_width(ui.available_width()),
                )
                .labelled_by(label.id);
            });
            ui.add_space(6.0);
            self.show_states(ui);
        });
        ui.add_space(12.0);
        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::CollapsingHeader::new("外形与自定义图片")
                .id_salt("preview_appearance_options")
                .show(ui, |ui| self.appearance.show_controls(ui));
            ui.weak("外形选择会自动保存，并沿用到对话中。");
        });
        if self.appearance.builtin_character() == crate::builtin_character::BuiltinCharacter::Oc01 {
            ui.add_space(12.0);
            theme::card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                egui::CollapsingHeader::new("OC 素材册")
                    .id_salt("preview_oc_gallery")
                    .show(ui, |ui| self.gallery.show(ui));
            });
        }
        ui.add_space(8.0);
        egui::CollapsingHeader::new("关于预览")
            .id_salt("preview_help")
            .show(ui, |ui| {
                ui.weak("这里不连接模型，也不会执行角色动作。");
                ui.weak("表达状态使用预览口型；正式对话中口型随声音变化。");
                ui.weak("称呼与状态仅用于预览，不会修改正式人格。");
            });
    }

    fn show_states(&mut self, ui: &mut egui::Ui) {
        ui.small("互动状态");
        ui.horizontal_wrapped(|ui| {
            for (activity, label) in [
                (ConversationState::Idle, "陪伴"),
                (ConversationState::Listening, "倾听"),
                (ConversationState::Thinking, "思考"),
                (ConversationState::Speaking, "表达"),
                (ConversationState::Interrupted, "打断"),
                (ConversationState::Stopped, "停止"),
            ] {
                ui.selectable_value(&mut self.state.activity, activity, label);
            }
        });
        ui.add_space(4.0);
        ui.small("情绪");
        ui.horizontal_wrapped(|ui| {
            for (emotion, label) in [
                (Emotion::Neutral, "平静"),
                (Emotion::Happy, "开心"),
                (Emotion::Sad, "低落"),
                (Emotion::Angry, "生气"),
                (Emotion::Surprised, "惊讶"),
            ] {
                ui.selectable_value(&mut self.state.emotion, emotion, label);
            }
        });
    }

    fn show_navigation(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("返回首页").clicked() {
                self.navigation.request(ui.ctx(), Destination::Home);
            }
            if ui.add(theme::primary_button("开始对话")).clicked() {
                self.navigation.request(ui.ctx(), Destination::Connect);
            }
            ui.weak("准备好了，就开始聊聊。");
        });
    }
}

impl eframe::App for PreviewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::bottom("preview_navigation")
            .resizable(false)
            .show(ui, |ui| {
                self.show_navigation(ui);
            });
        egui::Frame::new().inner_margin(20.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.show_contents(ui));
        });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.appearance.save(storage);
        self.gallery.save(storage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support as review;

    fn app() -> PreviewApp {
        PreviewApp {
            navigation: Navigation::default(),
            appearance: AppearancePanel::default(),
            gallery: gallery::OcGallery::default(),
            name: "AIex".to_owned(),
            state: PresentationState {
                connected: true,
                synchronized: true,
                activity: ConversationState::Idle,
                emotion: Emotion::Neutral,
                mouth_level: None,
            },
        }
    }

    #[test]
    fn studio_controls_change_preview_and_navigation_survives_narrow_layout() {
        use eframe::App;
        let context = egui::Context::default();
        theme::configure_appearance(&context);
        let mut app = app();
        let mut frame = eframe::Frame::_new_kittest();
        let size = [980, 720];
        let mut draw = |ui: &mut egui::Ui| app.ui(ui, &mut frame);
        review::render(&context, size, Vec::new(), &mut draw);
        let output = review::render(&context, size, Vec::new(), &mut draw);
        let target = review::position(&output, "开心");
        for pressed in [true, false] {
            review::render(&context, size, review::pointer(target, pressed), &mut draw);
        }
        assert_eq!(app.state.emotion, Emotion::Happy);
        let mut draw = |ui: &mut egui::Ui| app.ui(ui, &mut frame);
        let narrow = [640, 520];
        review::render(&context, narrow, Vec::new(), &mut draw);
        let output = review::render(&context, narrow, Vec::new(), &mut draw);
        review::position(&output, "开心");
        review::position(&output, "表达");
        review::position(&output, "返回首页");
        let target = review::position(&output, "开始对话");
        for pressed in [true, false] {
            review::render(
                &context,
                narrow,
                review::pointer(target, pressed),
                &mut draw,
            );
        }
        assert_eq!(app.navigation.take(), Some(Destination::Connect));
    }

    #[test]
    fn oc_gallery_is_reachable_without_changing_the_stage_or_hiding_navigation() {
        use eframe::App;

        fn find(output: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    let center = text.pos + text.galley.size() * 0.5;
                    shape.clip_rect.contains(center).then_some(center)
                }
                _ => None,
            })
        }

        for size in [[640, 520], [480, 520]] {
            let context = egui::Context::default();
            theme::configure_appearance(&context);
            context.global_style_mut(|style| style.animation_time = 0.0);
            let mut app = app();
            app.appearance.builtin = crate::builtin_character::BuiltinCharacter::Oc01;
            app.state.activity = ConversationState::Speaking;
            app.state.emotion = Emotion::Happy;
            app.name = "预览称呼".to_owned();
            let mut frame = eframe::Frame::_new_kittest();
            let mut draw = |ui: &mut egui::Ui| app.ui(ui, &mut frame);
            for label in ["OC 素材册", "完成庆祝"] {
                let mut output = review::render(&context, size, Vec::new(), &mut draw);
                for _ in 0..16 {
                    if find(&output, label).is_some() {
                        break;
                    }
                    output = review::render(
                        &context,
                        size,
                        vec![
                            egui::Event::PointerMoved(egui::pos2(size[0] as f32 - 60.0, 300.0)),
                            egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point,
                                delta: egui::vec2(0.0, -100.0),
                                phase: egui::TouchPhase::Move,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                        &mut draw,
                    );
                }
                let target = review::position(&output, label);
                for pressed in [true, false] {
                    review::render(&context, size, review::pointer(target, pressed), &mut draw);
                }
            }
            let output = review::render(&context, size, Vec::new(), &mut draw);
            review::position(&output, "返回首页");
            let target = review::position(&output, "开始对话");
            for pressed in [true, false] {
                review::render(&context, size, review::pointer(target, pressed), &mut draw);
            }
            assert_eq!(app.state.activity, ConversationState::Speaking);
            assert_eq!(app.state.emotion, Emotion::Happy);
            assert_eq!(app.gallery.selected_id(), "success");
            assert_eq!(app.name, "预览称呼");
            assert_eq!(
                app.appearance.builtin_character(),
                crate::builtin_character::BuiltinCharacter::Oc01
            );
            assert_eq!(app.navigation.take(), Some(Destination::Connect));
        }
    }

    #[test]
    fn original_character_does_not_offer_the_oc_gallery() {
        let mut app = app();
        app.appearance.builtin = crate::builtin_character::BuiltinCharacter::Original;
        let context = egui::Context::default();
        let output = context.run_ui(Default::default(), |ui| app.show_editor(ui));
        assert!(!output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(text)
                if text.galley.job.text == "OC 素材册")
        }));
    }

    #[test]
    #[ignore = "exports studio screenshots without opening a native window"]
    fn export_preview_review() {
        use eframe::App;
        for size in [[640, 520], [980, 720]] {
            let mut app = app();
            let mut frame = eframe::Frame::_new_kittest();
            review::export("preview", size, |ui| app.ui(ui, &mut frame));
        }
    }

    #[test]
    #[ignore = "exports the expanded OC gallery without opening a native window"]
    fn export_oc_gallery_review() {
        use eframe::App;
        for size in [[640, 520], [980, 720]] {
            let context = egui::Context::default();
            theme::configure_appearance(&context);
            context.global_style_mut(|style| style.animation_time = 0.0);
            let mut app = app();
            app.appearance.builtin = crate::builtin_character::BuiltinCharacter::Oc01;
            let mut frame = eframe::Frame::_new_kittest();
            let mut draw = |ui: &mut egui::Ui| app.ui(ui, &mut frame);
            let mut output = review::render(&context, size, Vec::new(), &mut draw);
            output.append(review::render(&context, size, Vec::new(), &mut draw));
            let scroll = |delta| {
                vec![
                    egui::Event::PointerMoved(egui::pos2(size[0] as f32 - 60.0, 300.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, delta),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            };
            for label in ["OC 素材册", "表情参考"] {
                for _ in 0..16 {
                    if output.shapes.iter().any(|shape| {
                        matches!(&shape.shape, egui::Shape::Text(text)
                            if text.galley.job.text == label && shape.clip_rect.contains(
                                text.pos + text.galley.size() * 0.5))
                    }) {
                        break;
                    }
                    output.append(review::render(&context, size, scroll(-100.0), &mut draw));
                }
                let target = review::position(&output, label);
                for pressed in [true, false] {
                    output.append(review::render(
                        &context,
                        size,
                        review::pointer(target, pressed),
                        &mut draw,
                    ));
                }
                output.append(review::render(&context, size, Vec::new(), &mut draw));
            }
            output.append(review::render(&context, size, scroll(-2000.0), &mut draw));
            for _ in 0..20 {
                output.append(review::render(&context, size, Vec::new(), &mut draw));
            }
            assert_eq!(app.gallery.selected_id(), "expressions");
            review::position(&output, "开始对话");
            review::export_output("preview-oc-gallery", size, &context, output);
        }
    }

    #[test]
    fn preview_actions_navigate_without_starting_services() {
        for (label, expected) in [
            ("返回首页", Destination::Home),
            ("开始对话", Destination::Connect),
        ] {
            let context = egui::Context::default();
            let app = PreviewApp {
                navigation: Navigation::default(),
                appearance: AppearancePanel::default(),
                gallery: gallery::OcGallery::default(),
                name: "伙伴".to_owned(),
                state: PresentationState {
                    connected: true,
                    synchronized: true,
                    activity: ConversationState::Idle,
                    emotion: Emotion::Neutral,
                    mouth_level: None,
                },
            };
            let input = || egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(480.0, 480.0),
                )),
                ..Default::default()
            };
            let output = context.run_ui(input(), |ui| app.show_navigation(ui));
            let position = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("navigation button is visible");
            for pressed in [true, false] {
                let mut input = input();
                input.events = vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ];
                let _output = context.run_ui(input, |ui| app.show_navigation(ui));
            }
            assert_eq!(app.navigation.take(), Some(expected));
        }
    }
}
