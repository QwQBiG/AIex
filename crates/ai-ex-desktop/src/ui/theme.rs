use std::path::Path;

use eframe::egui;

pub(crate) const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(245, 242, 247);
pub(crate) const SURFACE: egui::Color32 = egui::Color32::from_rgb(255, 252, 250);
pub(crate) const SURFACE_ALT: egui::Color32 = egui::Color32::from_rgb(239, 234, 244);
pub(crate) const TEXT: egui::Color32 = egui::Color32::from_rgb(48, 42, 60);
pub(crate) const ACCENT: egui::Color32 = egui::Color32::from_rgb(148, 98, 136);
pub(crate) const ACCENT_SOFT: egui::Color32 = egui::Color32::from_rgb(236, 221, 237);
pub(crate) const MUTED: egui::Color32 = egui::Color32::from_rgb(117, 106, 128);
pub(crate) const BORDER: egui::Color32 = egui::Color32::from_rgb(223, 214, 229);
pub(crate) const STAGE: egui::Color32 = egui::Color32::from_rgb(238, 232, 243);
pub(crate) const SUCCESS: egui::Color32 = egui::Color32::from_rgb(45, 111, 88);

pub(crate) fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(18)
        .inner_margin(20)
}

pub(crate) fn primary_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
        .fill(ACCENT)
        .stroke(egui::Stroke::NONE)
        .corner_radius(10)
        .min_size(egui::vec2(88.0, 36.0))
}

pub(crate) fn configure_appearance(context: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = SURFACE;
    visuals.window_corner_radius = 16.into();
    visuals.window_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.extreme_bg_color = SURFACE;
    visuals.faint_bg_color = SURFACE_ALT;
    visuals.text_edit_bg_color = Some(SURFACE);
    visuals.override_text_color = Some(TEXT);
    visuals.weak_text_color = Some(MUTED);
    visuals.selection.bg_fill = ACCENT_SOFT;
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = egui::Color32::from_rgb(142, 94, 39);
    visuals.error_fg_color = egui::Color32::from_rgb(165, 59, 88);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = 9.into();
        widget.bg_stroke = egui::Stroke::new(1.0, BORDER);
        widget.fg_stroke = egui::Stroke::new(1.0, TEXT);
    }
    visuals.widgets.inactive.bg_fill = SURFACE;
    visuals.widgets.inactive.weak_bg_fill = SURFACE_ALT;
    visuals.widgets.hovered.bg_fill = ACCENT_SOFT;
    visuals.widgets.hovered.weak_bg_fill = ACCENT_SOFT;
    visuals.widgets.active.bg_fill = ACCENT_SOFT;
    visuals.widgets.active.weak_bg_fill = ACCENT_SOFT;
    visuals.widgets.open.bg_fill = SURFACE_ALT;
    context.set_theme(egui::Theme::Light);
    context.set_visuals(visuals);
    let mut style = (*context.style_of(egui::Theme::Light)).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 7.0);
    style.spacing.interact_size.y = 32.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(23.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
    context.set_style_of(egui::Theme::Light, style);

    let font_paths = [
        Path::new("C:/Windows/Fonts/msyh.ttc"),
        Path::new("C:/Windows/Fonts/simhei.ttf"),
    ];
    let Some(bytes) = font_paths.iter().find_map(|path| std::fs::read(path).ok()) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "ai-ex-cjk".to_owned(),
        egui::FontData::from_owned(bytes).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "ai-ex-cjk".to_owned());
    }
    context.set_fonts(fonts);
}
