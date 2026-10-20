#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use ai_ex_domain::AppError;
use serde::{Deserialize, Serialize};

/// The existing assets/games/<game>/profile.json format, without a new wrapper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameProfile {
    pub display_name: String,
    pub description: String,
    pub vlm_prompts: Vec<String>,
    pub default_templates: BTreeMap<String, String>,
    #[serde(default)]
    pub action_cooldowns: BTreeMap<String, f64>,
}

impl GameProfile {
    pub fn parse(input: &str) -> Result<Self, AppError> {
        let profile: Self = serde_json::from_str(input).map_err(|error| {
            AppError::configuration(format!("invalid game profile JSON: {error}"))
        })?;
        profile.validate()?;
        Ok(profile)
    }

    /// Resolve an exact profile key; file-name aliases are deliberately absent.
    pub fn template_file(&self, key: &str) -> Result<&str, AppError> {
        if key.trim().is_empty() {
            return Err(AppError::configuration("game template key is empty"));
        }
        let file = self.default_templates.get(key).ok_or_else(|| {
            AppError::configuration(format!("game template key is not present: {key}"))
        })?;
        validate_template_file(file)?;
        Ok(file)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.display_name.trim().is_empty() {
            return Err(AppError::configuration("game display name is empty"));
        }
        if self.default_templates.is_empty() {
            return Err(AppError::configuration("game profile has no templates"));
        }
        for (key, file) in &self.default_templates {
            if key.trim().is_empty() {
                return Err(AppError::configuration("game template key is empty"));
            }
            validate_template_file(file)?;
        }
        for (key, seconds) in &self.action_cooldowns {
            if key.trim().is_empty() || !seconds.is_finite() || *seconds < 0.0 {
                return Err(AppError::configuration(
                    "game action cooldowns require a name and finite nonnegative seconds",
                ));
            }
        }
        Ok(())
    }
}

fn validate_template_file(file: &str) -> Result<(), AppError> {
    // Validate both separator conventions, regardless of the host operating system.
    if file.is_empty()
        || file.trim() != file
        || file.ends_with('.')
        || file.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
                )
        })
    {
        return Err(AppError::configuration(
            "game template must be a single relative image file name",
        ));
    }
    let (stem, extension) = file.rsplit_once('.').ok_or_else(|| {
        AppError::configuration("game template file requires a PNG or JPEG extension")
    })?;
    if stem.is_empty()
        || stem == "."
        || stem == ".."
        || !(extension.eq_ignore_ascii_case("png")
            || extension.eq_ignore_ascii_case("jpg")
            || extension.eq_ignore_ascii_case("jpeg"))
    {
        return Err(AppError::configuration(
            "game template file requires a PNG or JPEG extension",
        ));
    }
    let device_stem = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(device_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (device_stem.len() == 4
            && (device_stem.starts_with("COM") || device_stem.starts_with("LPT"))
            && matches!(device_stem.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(AppError::configuration(
            "game template file name is a reserved device name",
        ));
    }
    Ok(())
}
