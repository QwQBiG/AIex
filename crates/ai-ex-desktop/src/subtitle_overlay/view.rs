use super::*;

impl SubtitleOverlay {
    pub fn show_controls(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.strong("悬浮字幕");
            ui.weak("只跟随实际播放；断线、打断和同步期间自动隐藏。");
            ui.weak("显示开关由上方行为设置应用后生效；快捷键也会同步到这里。");
            if !self.preferences.enabled {
                self.preview = false;
                return;
            }
            ui.checkbox(&mut self.preferences.passthrough, "允许鼠标穿过字幕");
            ui.weak("开启穿透后，在本页取消勾选即可继续调整位置。");
            ui.checkbox(&mut self.preview, "预览字幕位置（不发声）");
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Slider::new(&mut self.preferences.font_size, 16.0..=40.0).text("字号"),
                );
                ui.add(egui::Slider::new(&mut self.preferences.width, 360.0..=1200.0).text("宽度"));
                ui.add(egui::Slider::new(&mut self.preferences.opacity, 32..=255).text("底色浓度"));
            });
            if ui.button("将字幕位置恢复到左上方").clicked() {
                let position = egui::pos2(80.0, 80.0);
                self.preferences.position = Some(position);
                self.viewport_position = Some(position);
            }
        });
    }

    pub fn show_viewport(&mut self, context: &egui::Context, state: &UiState, speaker: &str) {
        let Some(content) = self.content(state, speaker) else {
            self.showing = false;
            return;
        };
        if !self.showing {
            self.viewport_position = self.preferences.position;
        }
        self.showing = true;
        let builder = self.builder();
        let mut close = false;
        context.show_viewport_immediate(Self::viewport_id(), builder, |ui, _class| {
            close |= ui.input(|input| input.viewport().close_requested());
            if let Some(rect) = ui.input(|input| input.viewport().outer_rect) {
                self.preferences.position = Some(rect.min);
            }
            let frame = egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(
                    28,
                    24,
                    38,
                    self.preferences.opacity,
                ))
                .corner_radius(16)
                .inner_margin(16);
            frame.show(ui, |ui| {
                ui.set_min_width((self.preferences.width - 32.0).max(1.0));
                ui.horizontal(|ui| {
                    let heading = if content.preview {
                        "字幕位置预览"
                    } else {
                        content.speaker
                    };
                    let heading = ui.label(
                        egui::RichText::new(heading)
                            .color(egui::Color32::LIGHT_GRAY)
                            .size(13.0),
                    );
                    if !self.preferences.passthrough
                        && heading.interact(egui::Sense::drag()).drag_started()
                    {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if !self.preferences.passthrough && ui.small_button("关闭字幕").clicked() {
                        close = true;
                    }
                });
                egui::ScrollArea::vertical()
                    .max_height(self.preferences.font_size * 3.5)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(content.text)
                                    .size(self.preferences.font_size)
                                    .color(egui::Color32::WHITE),
                            )
                            .wrap(),
                        );
                    });
                if let Some(progress) = content.progress {
                    ui.add(egui::ProgressBar::new(progress).desired_height(3.0));
                }
            });
        });
        if close {
            self.close_requested = true;
        }
    }

    pub(super) fn builder(&self) -> egui::ViewportBuilder {
        let height = self.preferences.font_size * 3.5 + 62.0;
        let mut builder = egui::ViewportBuilder::default()
            .with_title("AIex · 悬浮字幕")
            .with_inner_size([self.preferences.width, height])
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_has_shadow(false)
            .with_mouse_passthrough(self.preferences.passthrough)
            .with_active(false)
            .with_taskbar(false)
            .with_window_level(egui::viewport::WindowLevel::AlwaysOnTop);
        if let Some(position) = self.viewport_position {
            builder = builder.with_position(position);
        }
        builder
    }
}
