use eframe::egui;

const PREFIX: &str = "subtitle_overlay";

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Preferences {
    pub enabled: bool,
    pub passthrough: bool,
    pub font_size: f32,
    pub width: f32,
    pub opacity: u8,
    pub position: Option<egui::Pos2>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: false,
            passthrough: false,
            font_size: 24.0,
            width: 680.0,
            opacity: 190,
            position: None,
        }
    }
}

impl Preferences {
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        let Some(storage) = storage else {
            return Self::default();
        };
        let get = |name: &str| storage.get_string(&format!("{PREFIX}.{name}"));
        let number = |name: &str, default: f32, minimum: f32, maximum: f32| {
            get(name)
                .and_then(|value| value.parse::<f32>().ok())
                .filter(|value| value.is_finite())
                .unwrap_or(default)
                .clamp(minimum, maximum)
        };
        let defaults = Self::default();
        let position = get("position").and_then(|value| {
            let (x, y) = value.split_once(',')?;
            let position = egui::pos2(x.parse().ok()?, y.parse().ok()?);
            (position.is_finite() && position.x.abs() < 100_000.0 && position.y.abs() < 100_000.0)
                .then_some(position)
        });
        Self {
            enabled: false,
            passthrough: get("passthrough").as_deref() == Some("true"),
            font_size: number("font_size", defaults.font_size, 16.0, 40.0),
            width: number("width", defaults.width, 360.0, 1200.0),
            opacity: get("opacity")
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(defaults.opacity)
                .clamp(32, 255),
            position,
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        for (name, value) in [
            ("passthrough", self.passthrough.to_string()),
            ("font_size", self.font_size.to_string()),
            ("width", self.width.to_string()),
            ("opacity", self.opacity.to_string()),
        ] {
            storage.set_string(&format!("{PREFIX}.{name}"), value);
        }
        if let Some(position) = self.position {
            storage.set_string(
                &format!("{PREFIX}.position"),
                format!("{},{}", position.x, position.y),
            );
        } else {
            storage.remove_string(&format!("{PREFIX}.position"));
        }
    }
}
