//! Explicitly applied desktop behavior. These settings never grant input authority.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuietHours {
    pub start_minute: u16,
    pub end_minute: u16,
}

impl QuietHours {
    pub fn contains(&self, minute: u16) -> bool {
        if self.start_minute < self.end_minute {
            (self.start_minute..self.end_minute).contains(&minute)
        } else {
            minute >= self.start_minute || minute < self.end_minute
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopRuntimeSettings {
    pub proactive_enabled: bool,
    pub cooldown_seconds: u32,
    pub do_not_disturb: bool,
    pub quiet_hours: Option<QuietHours>,
    pub hotkeys_enabled: bool,
    pub captions_visible: bool,
}

impl Default for DesktopRuntimeSettings {
    fn default() -> Self {
        Self {
            proactive_enabled: false,
            cooldown_seconds: 300,
            do_not_disturb: false,
            quiet_hours: None,
            hotkeys_enabled: false,
            captions_visible: false,
        }
    }
}

impl DesktopRuntimeSettings {
    pub fn validate(&self) -> Result<(), AppError> {
        if !(10..=86400).contains(&self.cooldown_seconds)
            || self.quiet_hours.as_ref().is_some_and(|hours| {
                hours.start_minute >= 1440
                    || hours.end_minute >= 1440
                    || hours.start_minute == hours.end_minute
            })
        {
            return Err(AppError::configuration(
                "invalid desktop cooldown or quiet hours",
            ));
        }
        Ok(())
    }

    pub fn allows_proactive(&self, local_minute: Option<u16>) -> bool {
        self.proactive_enabled
            && !self.do_not_disturb
            && match &self.quiet_hours {
                None => true,
                Some(hours) => {
                    local_minute.is_some_and(|minute| minute < 1440 && !hours.contains(minute))
                }
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopRuntimeSnapshot {
    pub instance_id: Uuid,
    pub revision: u64,
    pub settings: DesktopRuntimeSettings,
    /// False after restart until the user applies the settings again.
    pub hotkeys_registered: bool,
    pub hotkeys_error: bool,
    pub proactive_available: bool,
    pub vts_discovery_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopRuntimeRequest {
    State,
    Apply {
        instance_id: Uuid,
        expected_revision: u64,
        settings: DesktopRuntimeSettings,
    },
    /// Reloads only the separate desktop behavior settings file.
    Reload {
        instance_id: Uuid,
        expected_revision: u64,
    },
    Diagnostics,
    DiscoverVts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredVtsHotkey {
    pub id: String,
    pub name: String,
}

/// Deliberate allowlist: no messages, paths, window metadata, logs, or arbitrary detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopDiagnosticReport {
    pub schema_version: u32,
    pub application_version: String,
    pub desktop: DesktopRuntimeSnapshot,
    pub uptime_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DesktopRuntimeReply {
    State(DesktopRuntimeSnapshot),
    Diagnostics(DesktopDiagnosticReport),
    VtsDiscovery(Vec<DiscoveredVtsHotkey>),
}
