//! A display-only viewport of confirmed playback; it never owns an audio device.

mod preferences;
#[cfg(test)]
mod tests;
mod view;

use ai_ex_ui_model::{PresentationState, UiState};
use eframe::egui;
use preferences::Preferences;

#[derive(Default)]
pub struct SubtitleOverlay {
    preferences: Preferences,
    preview: bool,
    showing: bool,
    viewport_position: Option<egui::Pos2>,
    close_requested: bool,
}

struct Content<'a> {
    speaker: &'a str,
    text: &'a str,
    progress: Option<f32>,
    preview: bool,
}

impl SubtitleOverlay {
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        Self {
            preferences: Preferences::load(storage),
            ..Default::default()
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        self.preferences.save(storage);
    }

    pub fn hide_until_synchronized(&mut self) {
        self.showing = false;
    }

    pub fn set_confirmed_visible(&mut self, visible: bool) {
        self.preferences.enabled = visible;
        if !visible {
            self.preview = false;
        }
    }

    pub fn take_close_request(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    fn content<'a>(&self, state: &'a UiState, speaker: &'a str) -> Option<Content<'a>> {
        if !self.preferences.enabled
            || state.connection != ai_ex_ui_model::ConnectionState::Connected
            || state.needs_resync
        {
            return None;
        }
        if self.preview {
            return Some(Content {
                speaker,
                text: "这是字幕位置预览。可以拖动上方标题，调整到喜欢的位置。",
                progress: None,
                preview: true,
            });
        }
        let text = PresentationState::subtitle(state)?;
        let playback = &state.runtime.playback;
        Some(Content {
            speaker,
            text,
            progress: (playback.duration_ms > 0).then(|| {
                (playback.position_ms as f64 / playback.duration_ms as f64).clamp(0.0, 1.0) as f32
            }),
            preview: false,
        })
    }

    fn viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("confirmed-playback-subtitle-overlay")
    }
}
