use std::collections::BTreeMap;

use ai_ex_config::{AppConfig, ModelBackend};
use ai_ex_domain::Emotion;
use serde::Deserialize;
use serde_json::value::RawValue;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LegacyConfig {
    ollama: Option<Box<RawValue>>,
    vts: Option<Box<RawValue>>,
    ollama_url: Option<String>,
    ollama_model: Option<String>,
    llm_backend: Option<String>,
    koboldcpp_url: Option<String>,
    koboldcpp_max_context_length: Option<u64>,
    koboldcpp_max_length: Option<u64>,
    koboldcpp_temperature: Option<f64>,
    vts_port: Option<u16>,
    enable_expression_control: Option<bool>,
    enable_voice_cloning: Option<bool>,
    emotion_hotkey_map: Option<BTreeMap<String, String>>,
    sovits_url: Option<String>,
    sovits_timeout: Option<f64>,
    sovits_language: Option<String>,
    sovits_ref_audio_path: Option<String>,
    sovits_prompt_text: Option<String>,
    sovits_prompt_lang: Option<String>,
    enable_memory_features: Option<bool>,
    #[serde(default, deserialize_with = "default_if_null")]
    performance: LegacyPerformance,
    #[serde(default, deserialize_with = "default_if_null")]
    agent: LegacyAgent,
}

#[derive(Deserialize)]
struct NestedOllama {
    #[serde(default = "legacy_ollama_url")]
    base_url: String,
    #[serde(default = "legacy_ollama_model")]
    model: String,
}

#[derive(Deserialize)]
struct NestedVts {
    #[serde(default = "legacy_vts_port")]
    port: u16,
}

// The legacy loader copies only these fields when the nested format is used.
#[derive(Default, Deserialize)]
#[serde(default)]
struct NestedLegacyInput {
    ollama: Option<Box<RawValue>>,
    vts: Option<Box<RawValue>>,
    enable_expression_control: Option<bool>,
    enable_voice_cloning: Option<bool>,
    emotion_hotkey_map: Option<BTreeMap<String, String>>,
    sovits_url: Option<String>,
    sovits_timeout: Option<f64>,
    sovits_language: Option<String>,
    sovits_ref_audio_path: Option<String>,
    sovits_prompt_text: Option<String>,
    sovits_prompt_lang: Option<String>,
    #[serde(default, deserialize_with = "default_if_null")]
    performance: LegacyPerformance,
    #[serde(default, deserialize_with = "default_if_null")]
    agent: LegacyAgent,
}

fn default_if_null<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn legacy_ollama_url() -> String {
    "http://localhost:11434".into()
}

fn legacy_ollama_model() -> String {
    "llama3".into()
}

fn legacy_vts_port() -> u16 {
    8001
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyPerformance {
    max_queue_size: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyAgent {
    enabled: Option<bool>,
    vision: LegacyVision,
    safety: LegacySafety,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyVision {
    vision_model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacySafety {
    emergency_key: Option<String>,
}

pub struct Migration {
    pub config: AppConfig,
    pub warnings: Vec<String>,
}

/// Report keys that this migration does not interpret without printing their values.
pub fn unmapped_fields(value: &serde_json::Value) -> Vec<String> {
    const ROOT: &[&str] = &[
        "ollama_url",
        "ollama_model",
        "llm_backend",
        "koboldcpp_url",
        "koboldcpp_max_context_length",
        "koboldcpp_max_length",
        "koboldcpp_temperature",
        "vts_port",
        "enable_expression_control",
        "enable_voice_cloning",
        "emotion_hotkey_map",
        "sovits_url",
        "sovits_timeout",
        "sovits_language",
        "sovits_ref_audio_path",
        "sovits_prompt_text",
        "sovits_prompt_lang",
        "enable_memory_features",
        "performance",
        "agent",
    ];
    fn collect(value: &serde_json::Value, prefix: &str, known: &[&str], out: &mut Vec<String>) {
        if let Some(object) = value.as_object() {
            for key in object.keys() {
                if !known.contains(&key.as_str()) {
                    out.push(format!("{prefix}{key}"));
                }
            }
        }
    }
    let mut fields = Vec::new();
    let nested = value
        .get("ollama")
        .is_some_and(serde_json::Value::is_object);
    if nested {
        const IGNORED_FLAT: &[&str] = &[
            "ollama_url",
            "ollama_model",
            "vts_port",
            "llm_backend",
            "koboldcpp_url",
            "koboldcpp_max_context_length",
            "koboldcpp_max_length",
            "koboldcpp_temperature",
            "enable_memory_features",
        ];
        for key in value
            .as_object()
            .into_iter()
            .flat_map(|object| object.keys())
        {
            if IGNORED_FLAT.contains(&key.as_str())
                || (!ROOT.contains(&key.as_str()) && key != "ollama" && key != "vts")
            {
                fields.push(key.clone());
            }
        }
        collect(
            &value["ollama"],
            "ollama.",
            &["base_url", "model"],
            &mut fields,
        );
        collect(&value["vts"], "vts.", &["port"], &mut fields);
    } else {
        collect(value, "", ROOT, &mut fields);
    }
    collect(
        &value["performance"],
        "performance.",
        &["max_queue_size"],
        &mut fields,
    );
    let agent = &value["agent"];
    collect(
        agent,
        "agent.",
        &["enabled", "vision", "safety"],
        &mut fields,
    );
    collect(
        &agent["vision"],
        "agent.vision.",
        &["vision_model"],
        &mut fields,
    );
    collect(
        &agent["safety"],
        "agent.safety.",
        &["emergency_key"],
        &mut fields,
    );
    fields.sort();
    fields
}

impl LegacyConfig {
    pub fn parse_nested(content: &[u8]) -> serde_json::Result<Self> {
        let nested: NestedLegacyInput = serde_json::from_slice(content)?;
        Ok(Self {
            ollama: nested.ollama,
            vts: nested.vts,
            enable_expression_control: nested.enable_expression_control,
            enable_voice_cloning: nested.enable_voice_cloning,
            emotion_hotkey_map: nested.emotion_hotkey_map,
            sovits_url: nested.sovits_url,
            sovits_timeout: nested.sovits_timeout,
            sovits_language: nested.sovits_language,
            sovits_ref_audio_path: nested.sovits_ref_audio_path,
            sovits_prompt_text: nested.sovits_prompt_text,
            sovits_prompt_lang: nested.sovits_prompt_lang,
            performance: nested.performance,
            agent: nested.agent,
            ..Self::default()
        })
    }

    pub fn apply_nested_format(&mut self, source: &serde_json::Value) -> Result<(), String> {
        if !source
            .get("ollama")
            .is_some_and(serde_json::Value::is_object)
        {
            return Ok(());
        }
        let ollama: NestedOllama =
            serde_json::from_str(self.ollama.as_ref().ok_or("missing nested ollama")?.get())
                .map_err(|_| "invalid nested ollama settings".to_owned())?;
        if ollama.base_url.trim().is_empty() || ollama.model.trim().is_empty() {
            return Err("invalid nested ollama settings".to_owned());
        }
        let port = if source.get("vts").is_some() {
            let raw = self.vts.as_ref().ok_or("invalid nested vts settings")?;
            let nested: NestedVts = serde_json::from_str(raw.get())
                .map_err(|_| "invalid nested vts settings".to_owned())?;
            nested.port
        } else {
            legacy_vts_port()
        };
        self.ollama_url = Some(ollama.base_url);
        self.ollama_model = Some(ollama.model);
        self.vts_port = Some(port);
        self.llm_backend = None;
        self.koboldcpp_url = None;
        self.koboldcpp_max_context_length = None;
        self.koboldcpp_max_length = None;
        self.koboldcpp_temperature = None;
        self.enable_memory_features = None;
        self.enable_expression_control.get_or_insert(false);
        self.sovits_timeout.get_or_insert(10.0);
        Ok(())
    }

    pub fn migrate(self) -> Migration {
        let mut config = AppConfig::default();
        let mut warnings = Vec::new();
        assign_nonempty(&mut config.ollama.base_url, self.ollama_url);
        assign_nonempty(&mut config.ollama.model, self.ollama_model);
        assign_nonempty(&mut config.koboldcpp.base_url, self.koboldcpp_url);
        assign_positive_usize(
            &mut config.koboldcpp.max_context_length,
            self.koboldcpp_max_context_length,
            "koboldcpp_max_context_length",
            &mut warnings,
        );
        assign_positive_usize(
            &mut config.koboldcpp.max_length,
            self.koboldcpp_max_length,
            "koboldcpp_max_length",
            &mut warnings,
        );
        if let Some(temperature) = self.koboldcpp_temperature {
            if temperature.is_finite() && (0.0..=2.0).contains(&temperature) {
                config.koboldcpp.temperature = temperature as f32;
            } else {
                warnings.push("legacy koboldcpp_temperature was invalid".to_owned());
            }
        }
        match self.llm_backend.as_deref().map(str::trim) {
            Some("koboldcpp") => config.model.backend = ModelBackend::KoboldCpp,
            Some("ollama") | None | Some("") => {}
            Some(value) => warnings.push(format!(
                "legacy llm_backend '{value}' is unsupported; using Ollama",
            )),
        }
        if let Some(port) = self.vts_port {
            config.vts.port = port;
        }
        if let Some(enabled) = self.enable_expression_control {
            config.vts.enabled = enabled;
        }
        if let Some(mappings) = self.emotion_hotkey_map {
            for (emotion, hotkey) in mappings {
                if Emotion::parse(&emotion).is_some() && !hotkey.trim().is_empty() {
                    config.vts.expression_hotkeys.insert(emotion, hotkey);
                } else {
                    warnings.push(format!("legacy emotion mapping '{emotion}' was invalid",));
                }
            }
        }
        if let Some(capacity) = self.performance.max_queue_size {
            match usize::try_from(capacity) {
                Ok(capacity) if capacity > 0 => config.audio.queue_capacity = capacity,
                _ => warnings.push("legacy performance.max_queue_size was invalid".to_owned()),
            }
        }
        assign_nonempty(&mut config.tts.base_url, self.sovits_url);
        assign_nonempty(&mut config.tts.text_lang, self.sovits_language);
        assign_nonempty(&mut config.tts.ref_audio_path, self.sovits_ref_audio_path);
        assign_nonempty(&mut config.tts.prompt_text, self.sovits_prompt_text);
        assign_nonempty(&mut config.tts.prompt_lang, self.sovits_prompt_lang);
        if let Some(timeout) = positive_seconds(self.sovits_timeout) {
            config.tts.timeout_seconds = timeout;
        } else if self.sovits_timeout.is_some() {
            warnings.push("legacy sovits_timeout was invalid; using the default".to_owned());
        }
        if let Some(enabled) = self.enable_voice_cloning {
            config.tts.enabled = enabled;
        }
        if let Some(enabled) = self.enable_memory_features {
            config.memory.enabled = enabled;
        }
        assign_nonempty(
            &mut config.safety.emergency_hotkey,
            self.agent.safety.emergency_key,
        );
        assign_nonempty(&mut config.vision.model, self.agent.vision.vision_model);
        if self.agent.enabled.unwrap_or(false) {
            warnings.push(
                "legacy agent.enabled was true; automation and vision remain disabled until policy review"
                    .to_owned(),
            );
        }
        config.safety.automation_enabled = false;
        config.vision.enabled = false;
        config.duplex.enabled = false;
        config.control.enabled = false;
        Migration { config, warnings }
    }
}

fn assign_nonempty(target: &mut String, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        *target = value;
    }
}

fn positive_seconds(value: Option<f64>) -> Option<u64> {
    value
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value.ceil().min(u64::MAX as f64) as u64)
}

fn assign_positive_usize(
    target: &mut usize,
    value: Option<u64>,
    name: &str,
    warnings: &mut Vec<String>,
) {
    if let Some(value) = value {
        match usize::try_from(value) {
            Ok(value) if value > 0 => *target = value,
            _ => warnings.push(format!("legacy {name} was invalid")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_format_preserves_legacy_precedence_and_reports_ignored_fields() {
        let text = br#"{
            "ollama": {"base_url":"http://127.0.0.1:15000", "model":"nested-model", "other":1},
            "vts": {"port":8123, "extra":true},
            "ollama_model":"ignored-model", "vts_port":"ignored-invalid-value",
            "llm_backend":"koboldcpp", "enable_voice_cloning":true
        }"#;
        let source: serde_json::Value = serde_json::from_slice(text).unwrap();
        let mut legacy = LegacyConfig::parse_nested(text).unwrap();
        legacy.apply_nested_format(&source).unwrap();
        let result = legacy.migrate();
        assert_eq!(result.config.ollama.base_url, "http://127.0.0.1:15000");
        assert_eq!(result.config.ollama.model, "nested-model");
        assert_eq!(result.config.vts.port, 8123);
        assert_eq!(result.config.model.backend, ModelBackend::Ollama);
        assert!(result.config.tts.enabled);
        assert_eq!(
            unmapped_fields(&source),
            [
                "llm_backend",
                "ollama.other",
                "ollama_model",
                "vts.extra",
                "vts_port"
            ]
        );
    }

    #[test]
    fn nested_format_uses_old_defaults_and_rejects_duplicate_mapped_fields() {
        let text = br#"{"ollama":{},"ollama_model":"ignored","performance":null,"agent":null}"#;
        let source = serde_json::from_slice(text).unwrap();
        let mut legacy = LegacyConfig::parse_nested(text).unwrap();
        legacy.apply_nested_format(&source).unwrap();
        let result = legacy.migrate();
        assert_eq!(result.config.ollama.base_url, "http://localhost:11434");
        assert_eq!(result.config.ollama.model, "llama3");
        assert_eq!(result.config.vts.port, 8001);
        assert!(!result.config.vts.enabled);
        assert_eq!(result.config.tts.timeout_seconds, 10);
        let duplicate = br#"{"ollama":{"model":"one","model":"two"}}"#;
        let mut legacy = LegacyConfig::parse_nested(duplicate).unwrap();
        let source = serde_json::from_slice(duplicate).unwrap();
        assert!(legacy.apply_nested_format(&source).is_err());
        let empty = br#"{"ollama":{"model":""}}"#;
        let mut legacy = LegacyConfig::parse_nested(empty).unwrap();
        let source = serde_json::from_slice(empty).unwrap();
        assert!(legacy.apply_nested_format(&source).is_err());
    }

    #[test]
    fn nested_vts_without_an_ollama_object_does_not_change_flat_settings() {
        let text = br#"{"ollama":null,"vts":{"port":9001},"vts_port":8123}"#;
        let source = serde_json::from_slice(text).unwrap();
        let mut legacy: LegacyConfig = serde_json::from_slice(text).unwrap();
        legacy.apply_nested_format(&source).unwrap();
        assert_eq!(legacy.migrate().config.vts.port, 8123);
        assert_eq!(unmapped_fields(&source), ["ollama", "vts"]);
    }

    #[test]
    fn maps_safe_fields_but_does_not_enable_automation() {
        let legacy: LegacyConfig = serde_json::from_str(
            r#"{
                "ollama_url": "http://127.0.0.1:9999",
                "ollama_model": "model-a",
                "enable_memory_features": false,
                "emotion_hotkey_map": { "happy": "hotkey-happy" },
                "agent": {
                    "enabled": true,
                    "vision": { "vision_model": "vision-a" },
                    "safety": { "emergency_key": "F10" }
                }
            }"#,
        )
        .expect("legacy JSON");
        let migration = legacy.migrate();

        assert_eq!(migration.config.ollama.model, "model-a");
        assert_eq!(migration.config.vision.model, "vision-a");
        assert_eq!(migration.config.safety.emergency_hotkey, "F10");
        assert_eq!(
            migration
                .config
                .vts
                .expression_hotkeys
                .get("happy")
                .map(String::as_str),
            Some("hotkey-happy"),
        );
        assert!(!migration.config.memory.enabled);
        assert!(!migration.config.safety.automation_enabled);
        assert!(!migration.config.vision.enabled);
        assert_eq!(migration.warnings.len(), 1);
    }

    #[test]
    fn rounds_positive_timeout_up() {
        assert_eq!(positive_seconds(Some(1.2)), Some(2));
        assert_eq!(positive_seconds(Some(0.0)), None);
        assert_eq!(positive_seconds(Some(f64::NAN)), None);
    }

    #[test]
    fn reports_invalid_timeout_instead_of_silently_using_the_default() {
        let legacy: LegacyConfig = serde_json::from_str(r#"{"sovits_timeout":0}"#).unwrap();
        let migration = legacy.migrate();
        assert_eq!(
            migration.config.tts.timeout_seconds,
            AppConfig::default().tts.timeout_seconds
        );
        assert!(
            migration
                .warnings
                .iter()
                .any(|warning| warning.contains("sovits_timeout"))
        );
    }

    #[test]
    fn migrates_koboldcpp_backend_and_limits() {
        let legacy: LegacyConfig = serde_json::from_str(
            r#"{
                "llm_backend": "koboldcpp",
                "koboldcpp_url": "http://127.0.0.1:5002",
                "koboldcpp_max_context_length": 4096,
                "koboldcpp_max_length": 512,
                "koboldcpp_temperature": 0.4
            }"#,
        )
        .expect("legacy JSON");
        let migration = legacy.migrate();

        assert_eq!(migration.config.model.backend, ModelBackend::KoboldCpp);
        assert_eq!(migration.config.koboldcpp.max_context_length, 4_096);
        assert_eq!(migration.config.koboldcpp.max_length, 512);
        assert_eq!(migration.config.koboldcpp.temperature, 0.4);
        assert!(migration.warnings.is_empty());
    }
}
