use std::sync::{Arc, Mutex};

use ai_ex_domain::AppError;
use eframe::egui;

use crate::appearance::AppearancePanel;
use crate::ui::theme;

#[cfg(test)]
#[path = "onboarding_ui_tests.rs"]
mod review;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Preview,
    Connect,
    Setup,
}

pub fn run(configured: bool) -> Result<Option<Choice>, AppError> {
    window(configured, None)
}

pub fn show_error(message: String) -> Result<(), AppError> {
    window(false, Some(message)).map(|_| ())
}

pub fn recover(message: String) -> Result<Option<Choice>, AppError> {
    window(false, Some(message))
}

fn window(configured: bool, error: Option<String>) -> Result<Option<Choice>, AppError> {
    let choice = Arc::new(Mutex::new(None));
    let result = choice.clone();
    eframe::run_native(
        "AIex 数字伙伴",
        crate::navigation::companion_window([860.0, 580.0], [520.0, 380.0]),
        Box::new(move |context| {
            theme::configure_appearance(&context.egui_ctx);
            Ok(Box::new(Welcome {
                configured,
                error,
                choice,
                appearance: AppearancePanel::load(context.storage),
            }))
        }),
    )
    .map_err(|error| AppError::unavailable(error.to_string()))?;
    let selected = *result
        .lock()
        .map_err(|_| AppError::unavailable("welcome result lock poisoned"))?;
    Ok(selected)
}

struct Welcome {
    configured: bool,
    error: Option<String>,
    choice: Arc<Mutex<Option<Choice>>>,
    appearance: AppearancePanel,
}

impl Welcome {
    fn contents(&mut self, ui: &mut egui::Ui) {
        self.appearance.poll(ui.ctx());
        egui::Frame::new().inner_margin(24.0).show(ui, |ui| {
            ui.label(
                egui::RichText::new("AIex · 数字伙伴")
                    .color(theme::ACCENT)
                    .strong(),
            );
            ui.add_space(18.0);
            if ui.available_width() >= 650.0
                && self.error.is_none()
                && self.appearance.kind != crate::appearance::AppearanceKind::Hidden
            {
                let stage_width = ui.available_width() * 0.42;
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(stage_width, 390.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            theme::card().fill(theme::SURFACE_ALT).show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                self.appearance.show_portrait(
                                    ui,
                                    ai_ex_ui_model::PresentationState {
                                        connected: true,
                                        synchronized: true,
                                        activity: ai_ex_domain::ConversationState::Idle,
                                        emotion: ai_ex_domain::Emotion::Neutral,
                                        mouth_level: None,
                                    },
                                    "很高兴见到你",
                                    340.0,
                                );
                            });
                        },
                    );
                    ui.add_space(12.0);
                    ui.vertical(|ui| self.show_choices(ui));
                });
            } else {
                self.show_choices(ui);
            }
            ui.add_space(14.0);
            ui.small(format!("版本 {}", env!("CARGO_PKG_VERSION")));
        });
    }

    fn show_choices(&mut self, ui: &mut egui::Ui) {
        ui.heading("把陪伴，变成日常");
        ui.add_space(6.0);
        if let Some(error) = &self.error {
            theme::card().show(ui, |ui| {
                ui.strong("暂时无法启动");
                egui::ScrollArea::vertical()
                    .max_height(130.0)
                    .show(ui, |ui| {
                        ui.label(error);
                    });
            });
            ui.label("可以修改连接设置，或先离线体验外形。文件缺失时，请在新目录完整解压程序包。");
            if ui.button("复制错误信息").clicked() {
                ui.ctx().copy_text(error.clone());
            }
            self.button(ui, "连接设置", Choice::Setup);
            self.button(ui, "先体验外形", Choice::Preview);
            if ui.button("关闭").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        ui.weak("认识一个角色，留下一段对话。\n从你喜欢的样子开始。");
        ui.add_space(16.0);
        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            self.button(ui, "先体验外形", Choice::Preview);
            ui.small("立即认识伙伴，试试表情与自定义外形。");
        });
        ui.add_space(12.0);
        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            self.button(
                ui,
                if self.configured {
                    "开始对话"
                } else {
                    "连接模型并开始对话"
                },
                if self.configured {
                    Choice::Connect
                } else {
                    Choice::Setup
                },
            );
            ui.small("连接你选择的模型，开启对话与记忆。");
        });
        ui.add_space(12.0);
        if self.configured {
            self.button(ui, "连接设置", Choice::Setup);
        }
        ui.add_space(8.0);
        ui.small("外形预览可以离线使用。语音可稍后配置。");
    }

    fn button(&self, ui: &mut egui::Ui, label: &str, choice: Choice) {
        if ui
            .add_sized(
                [ui.available_width().min(320.0), 40.0],
                if choice == Choice::Preview {
                    theme::primary_button(label)
                } else {
                    egui::Button::new(label)
                },
            )
            .clicked()
        {
            if let Ok(mut selected) = self.choice.lock() {
                *selected = Some(choice);
            }
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for Welcome {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::ScrollArea::vertical().show(ui, |ui| self.contents(ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_buttons_select_preview_setup_or_conversation_without_opening_windows() {
        for (configured, label, expected) in [
            (false, "先体验外形", Choice::Preview),
            (false, "连接模型并开始对话", Choice::Setup),
            (true, "开始对话", Choice::Connect),
            (true, "连接设置", Choice::Setup),
        ] {
            let context = egui::Context::default();
            let choice = Arc::new(Mutex::new(None));
            let mut app = Welcome {
                configured,
                error: None,
                choice: choice.clone(),
                appearance: AppearancePanel::default(),
            };
            let output = context.run_ui(egui::RawInput::default(), |ui| app.contents(ui));
            let position = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("welcome action is visible");
            for pressed in [true, false] {
                let _output = context.run_ui(
                    egui::RawInput {
                        events: vec![
                            egui::Event::PointerMoved(position),
                            egui::Event::PointerButton {
                                pos: position,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                        ..Default::default()
                    },
                    |ui| app.contents(ui),
                );
            }
            assert_eq!(*choice.lock().unwrap(), Some(expected));
        }
    }

    #[test]
    fn welcome_and_startup_errors_are_readable_without_a_console() {
        for configured in [false, true] {
            let context = egui::Context::default();
            let mut app = Welcome {
                configured,
                error: None,
                choice: Arc::new(Mutex::new(None)),
                appearance: AppearancePanel::default(),
            };
            let output = context.run_ui(egui::RawInput::default(), |ui| app.contents(ui));
            let texts: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                    _ => None,
                })
                .collect();
            assert!(texts.contains(&"先体验外形"));
            assert!(texts.contains(&if configured {
                "开始对话"
            } else {
                "连接模型并开始对话"
            }));
            app.error = Some("service package is incomplete".to_owned());
            let output = context.run_ui(egui::RawInput::default(), |ui| app.contents(ui));
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.job.text == "service package is incomplete")));
        }
    }
}
