use super::*;
use ai_ex_domain::{ConversationState, SpeechPlaybackSnapshot};
use ai_ex_ui_model::ConnectionState;

#[derive(Default)]
struct Storage(std::collections::HashMap<String, String>);

impl eframe::Storage for Storage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

fn speaking() -> UiState {
    let mut state = UiState::new(8).unwrap();
    state.connection = ConnectionState::Connected;
    state.runtime.playback = SpeechPlaybackSnapshot {
        active: true,
        text: "Playing this cue.".into(),
        position_ms: 500,
        duration_ms: 1000,
        ..Default::default()
    };
    state
}

#[test]
fn only_confirmed_playback_is_shown_and_reconnection_cannot_reuse_a_stale_cue() {
    let mut overlay = SubtitleOverlay::default();
    let mut state = speaking();
    assert!(overlay.content(&state, "Test").is_none());
    overlay.preferences.enabled = true;
    let cue = overlay.content(&state, "Test").unwrap();
    assert_eq!(
        (cue.text, cue.progress, cue.preview),
        ("Playing this cue.", Some(0.5), false)
    );
    state.runtime.speech_cancelled = true;
    assert!(overlay.content(&state, "Test").is_none());
    state.runtime.speech_cancelled = false;
    state.needs_resync = true;
    assert!(overlay.content(&state, "Test").is_none());
    state.needs_resync = false;
    state.connection = ConnectionState::Disconnected;
    assert!(overlay.content(&state, "Test").is_none());
    overlay.preview = true;
    assert!(overlay.content(&state, "Test").is_none());
    state.connection = ConnectionState::Connected;
    assert!(overlay.content(&state, "Test").unwrap().preview);
    overlay.preview = false;
    state.runtime.state = ConversationState::Listening;
    assert!(overlay.content(&state, "Test").is_none());
    state.runtime.state = ConversationState::Idle;
    state.runtime.playback = Default::default();
    assert!(overlay.content(&state, "Test").is_none());
}

#[test]
fn display_preferences_round_trip_without_persisting_playback_or_preview() {
    let mut storage = Storage::default();
    let overlay = SubtitleOverlay {
        preferences: Preferences {
            enabled: true,
            passthrough: true,
            width: 720.0,
            font_size: 28.0,
            opacity: 120,
            position: Some(egui::pos2(-800.0, 80.0)),
        },
        preview: true,
        ..Default::default()
    };
    overlay.save(&mut storage);
    let loaded = SubtitleOverlay::load(Some(&storage));
    let mut expected = overlay.preferences.clone();
    expected.enabled = false;
    assert_eq!(loaded.preferences, expected);
    assert!(!loaded.preview && !loaded.showing);
    storage
        .0
        .insert("subtitle_overlay.font_size".into(), "NaN".into());
    storage
        .0
        .insert("subtitle_overlay.width".into(), "-20".into());
    storage
        .0
        .insert("subtitle_overlay.position".into(), "inf,0".into());
    let invalid = SubtitleOverlay::load(Some(&storage));
    assert_eq!(invalid.preferences.font_size, 24.0);
    assert_eq!(invalid.preferences.width, 360.0);
    assert!(invalid.preferences.position.is_none());
}

#[test]
fn viewport_does_not_take_focus_and_can_be_rendered_without_native_windows() {
    let mut overlay = SubtitleOverlay::default();
    overlay.preferences.enabled = true;
    overlay.preferences.passthrough = true;
    let builder = overlay.builder();
    assert_eq!(builder.transparent, Some(true));
    assert_eq!(builder.active, Some(false));
    assert_eq!(builder.taskbar, Some(false));
    assert_eq!(builder.mouse_passthrough, Some(true));
    let context = egui::Context::default();
    let state = speaking();
    let _measure = context.run_ui(egui::RawInput::default(), |ui| {
        overlay.show_viewport(ui.ctx(), &state, "Test");
    });
    let output = context.run_ui(egui::RawInput::default(), |ui| {
        overlay.show_viewport(ui.ctx(), &state, "Test");
    });
    assert!(overlay.showing);
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::epaint::Shape::Text(text) if text.galley.job.text.contains("Playing this cue."))));
    for primitive in context.tessellate(output.shapes, output.pixels_per_point) {
        if let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive {
            assert!(mesh.is_valid());
        }
    }
    let mut stopped = state;
    stopped.runtime.playback.active = false;
    let output = context.run_ui(egui::RawInput::default(), |ui| {
        overlay.show_viewport(ui.ctx(), &stopped, "Test");
    });
    assert!(!overlay.showing);
    assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::epaint::Shape::Text(text) if text.galley.job.text.contains("Playing this cue."))));
}
