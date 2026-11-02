use std::sync::{Arc, Mutex};

use eframe::egui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Home,
    Preview,
    Connect,
    Setup,
    Native,
    NativeChat,
}

/// Window transitions are requests; each conversation owns its managed service.
#[derive(Clone, Default)]
pub struct Navigation(Arc<Mutex<Option<Destination>>>);

impl Navigation {
    pub fn request(&self, context: &egui::Context, destination: Destination) {
        if let Ok(mut next) = self.0.lock() {
            *next = Some(destination);
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub fn take(&self) -> Option<Destination> {
        self.0.lock().ok()?.take()
    }
}

/// Preview and conversation share appearance preferences despite their titles.
pub fn companion_window(size: [f32; 2], minimum: [f32; 2]) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("AIex")
            .with_inner_size(size)
            .with_min_inner_size(minimum),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_navigation_closes_only_the_current_window_and_is_consumed_once() {
        for destination in [
            Destination::Home,
            Destination::Preview,
            Destination::Connect,
            Destination::Setup,
            Destination::Native,
            Destination::NativeChat,
        ] {
            let context = egui::Context::default();
            let navigation = Navigation::default();
            assert_eq!(navigation.take(), None);
            let output = context.run_ui(egui::RawInput::default(), |ui| {
                navigation.clone().request(ui.ctx(), destination);
            });
            assert!(
                output
                    .viewport_output
                    .values()
                    .any(|viewport| viewport.commands.contains(&egui::ViewportCommand::Close))
            );
            assert_eq!(navigation.take(), Some(destination));
            assert_eq!(navigation.take(), None);
        }
    }
}
