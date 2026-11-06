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
    CheckVtsMappings,
    DiscoverVtsParameters {
        offset: usize,
    },
    DiscoverVtsParametersFiltered {
        filter: VtsParameterFilter,
        offset: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VtsParameterFilter {
    Mouth,
    Voice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredVtsHotkey {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VtsMappingState {
    Unconfigured,
    Matched,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VtsMappingStatus {
    pub emotion: crate::Emotion,
    pub status: VtsMappingState,
    pub hotkey_id: Option<String>,
    pub id_truncated: bool,
}

/// An observation of the active configuration against one VTS model snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VtsMappingCheck {
    pub model_loaded: bool,
    pub mappings: Vec<VtsMappingStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredVtsParameter {
    pub name: String,
    pub added_by: String,
    pub value: serde_json::Number,
    pub min: serde_json::Number,
    pub max: serde_json::Number,
    pub default_value: serde_json::Number,
    pub is_custom: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VtsParameterPage {
    pub model_loaded: bool,
    pub offset: usize,
    pub total: usize,
    pub parameters: Vec<DiscoveredVtsParameter>,
}

/// Stable names for the service's previously observed component health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticComponent {
    Native,
    Ollama,
    OllamaNative,
    Deepseek,
    Koboldcpp,
    Memory,
    AudioQueue,
    AudioInput,
    AudioOutput,
    Asr,
    GptSovits,
    Vts,
    Vision,
    Safety,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticComponentObservation {
    pub component: DiagnosticComponent,
    pub observed_ready: bool,
}

/// Deliberate allowlist: no messages, paths, window metadata, logs, or arbitrary detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopDiagnosticReport {
    pub schema_version: u32,
    pub application_version: String,
    pub desktop: DesktopRuntimeSnapshot,
    pub uptime_seconds: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_observations: Vec<DiagnosticComponentObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DesktopRuntimeReply {
    State(DesktopRuntimeSnapshot),
    Diagnostics(DesktopDiagnosticReport),
    VtsDiscovery(Vec<DiscoveredVtsHotkey>),
    VtsMappingCheck(VtsMappingCheck),
    VtsParameters(VtsParameterPage),
    VtsFilteredParameters {
        filter: VtsParameterFilter,
        page: VtsParameterPage,
    },
}
