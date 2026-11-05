//! Project existing health observations without copying their potentially private detail.
use ai_ex_domain::{ComponentHealth, DiagnosticComponent, DiagnosticComponentObservation};

pub(crate) fn observations(health: &[ComponentHealth]) -> Vec<DiagnosticComponentObservation> {
    use DiagnosticComponent as Component;
    const ALLOWED: &[(&str, Component)] = &[
        ("native", Component::Native),
        ("ollama", Component::Ollama),
        ("ollama_native", Component::OllamaNative),
        ("deepseek", Component::Deepseek),
        ("koboldcpp", Component::Koboldcpp),
        ("memory", Component::Memory),
        ("audio-queue", Component::AudioQueue),
        ("audio-input", Component::AudioInput),
        ("audio-output", Component::AudioOutput),
        ("asr", Component::Asr),
        ("gpt-sovits", Component::GptSovits),
        ("vts", Component::Vts),
        ("vision", Component::Vision),
        ("safety", Component::Safety),
    ];
    ALLOWED
        .iter()
        .filter_map(|(name, component)| {
            health
                .iter()
                .find(|entry| entry.component == *name)
                .map(|entry| DiagnosticComponentObservation {
                    component: *component,
                    observed_ready: entry.ready,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_recorded_health_is_exposed_without_detail() {
        let health = vec![
            ComponentHealth::ready("native"),
            ComponentHealth::unavailable("audio-output", "private path or device name"),
            ComponentHealth::ready("plugin:private-name"),
        ];
        let result = observations(&health);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].component, DiagnosticComponent::Native);
        assert!(result[0].observed_ready);
        assert_eq!(result[1].component, DiagnosticComponent::AudioOutput);
        assert!(!result[1].observed_ready);
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("private"));
        assert!(!json.contains("plugin"));
        assert!(!json.contains("audio_input"));
    }

    #[test]
    fn earlier_reports_remain_readable_without_component_observations() {
        let old = serde_json::json!({
            "schema_version": 1,
            "application_version": "0.10",
            "desktop": {
                "instance_id": "00000000-0000-0000-0000-000000000000",
                "revision": 0,
                "settings": {},
                "hotkeys_registered": false,
                "hotkeys_error": false,
                "proactive_available": true,
                "vts_discovery_available": false
            },
            "uptime_seconds": 5
        });
        let report: ai_ex_domain::DesktopDiagnosticReport = serde_json::from_value(old).unwrap();
        assert!(report.component_observations.is_empty());
    }
}
