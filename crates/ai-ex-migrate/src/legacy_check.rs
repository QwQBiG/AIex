use std::path::PathBuf;

use ai_ex_domain::AppError;
use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;

#[cfg(test)]
#[path = "legacy_check_tests.rs"]
mod tests;

pub async fn run(arguments: Vec<String>) -> Result<(), AppError> {
    let input = parse_input(arguments)?;
    let file = tokio::fs::File::open(input)
        .await
        .map_err(|_| AppError::configuration("cannot open legacy configuration"))?;
    let mut bytes = Vec::new();
    file.take((ai_ex_config::MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| AppError::configuration("cannot read legacy configuration"))?;
    let report = inspect(&bytes)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|_| AppError::protocol("cannot encode legacy check report"))?
    );
    if report["issues"]
        .as_array()
        .is_some_and(|issues| !issues.is_empty())
    {
        return Err(AppError::configuration(
            "legacy check found invalid selected fields",
        ));
    }
    Ok(())
}

fn parse_input(arguments: Vec<String>) -> Result<PathBuf, AppError> {
    if let [flag, value] = arguments.as_slice()
        && flag == "--input"
        && !value.is_empty()
        && !value.starts_with("--")
    {
        return Ok(PathBuf::from(value));
    }
    Err(AppError::configuration(
        "legacy check requires exactly one --input path and no other options",
    ))
}

fn nested_field<'a>(
    object: &'a Map<String, Value>,
    section: &'static str,
    field: &str,
    issues: &mut Vec<Value>,
) -> Option<&'a Value> {
    match object.get(section) {
        Some(Value::Object(values)) => values.get(field),
        Some(_) => {
            issues.push(json!({"field": section, "kind": "expected_object"}));
            None
        }
        None => None,
    }
}

fn nonempty_string(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

fn url_with_authority(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        reqwest::Url::parse(text)
            .ok()
            .is_some_and(|url| url.has_host())
    })
}

fn positive_number(value: &Value) -> bool {
    value == &Value::Bool(true)
        || value
            .as_f64()
            .is_some_and(|number| number.is_finite() && number > 0.0)
}

fn inspect(bytes: &[u8]) -> Result<Value, AppError> {
    if bytes.len() > ai_ex_config::MAX_CONFIG_BYTES {
        return Err(AppError::configuration(
            "legacy configuration exceeds 1 MiB",
        ));
    }
    let source: Value = serde_json::from_slice(bytes)
        .map_err(|_| AppError::configuration("invalid legacy JSON"))?;
    let object = source
        .as_object()
        .ok_or_else(|| AppError::configuration("legacy configuration must be an object"))?;
    let nested = object.get("ollama").is_some_and(Value::is_object);
    let mut issues = Vec::new();
    let (ollama_url, ollama_model, tts_voice, vts_port) = if nested {
        (
            nested_field(object, "ollama", "base_url", &mut issues),
            nested_field(object, "ollama", "model", &mut issues),
            nested_field(object, "tts", "voice", &mut issues),
            nested_field(object, "vts", "port", &mut issues),
        )
    } else {
        (
            object.get("ollama_url"),
            object.get("ollama_model"),
            object.get("tts_voice"),
            object.get("vts_port"),
        )
    };
    for (field, value) in [
        ("ollama_model", ollama_model),
        ("tts_voice", tts_voice),
        ("sovits_url", object.get("sovits_url")),
        ("sovits_language", object.get("sovits_language")),
    ] {
        if value.is_some_and(|value| !nonempty_string(value)) {
            issues.push(json!({"field": field, "kind": "expected_nonempty_string"}));
        }
    }
    if ollama_url.is_some_and(|value| !url_with_authority(value)) {
        issues.push(json!({"field":"ollama_url", "kind":"invalid_url"}));
    }
    if vts_port.is_some_and(|value| {
        value != &Value::Bool(true) && value.as_u64().is_none_or(|number| number == 0)
    }) {
        issues.push(json!({"field":"vts_port", "kind":"expected_positive_integer"}));
    }
    for field in [
        "enable_emotional_intelligence",
        "enable_voice_cloning",
        "enable_expression_control",
        "fallback_to_edge_tts",
    ] {
        if object.get(field).is_some_and(|value| !value.is_boolean()) {
            issues.push(json!({"field":field, "kind":"expected_boolean"}));
        }
    }
    if object
        .get("sovits_timeout")
        .is_some_and(|value| !positive_number(value))
    {
        issues.push(json!({"field":"sovits_timeout", "kind":"expected_positive_number"}));
    }
    if object.get("enable_emotional_intelligence") == Some(&Value::Bool(true))
        && object.get("enable_voice_cloning") == Some(&Value::Bool(true))
        && object
            .get("sovits_url")
            .is_some_and(|value| !url_with_authority(value))
    {
        issues.push(json!({"field":"sovits_url", "kind":"invalid_url"}));
    }
    let log_level = if nested {
        nested_field(object, "logging", "level", &mut issues)
    } else {
        object.get("log_level")
    };
    if log_level.is_some_and(|value| {
        !matches!(
            value.as_str(),
            Some("DEBUG" | "INFO" | "WARNING" | "ERROR" | "CRITICAL")
        )
    }) {
        issues.push(json!({"field":"log_level", "kind":"invalid_value"}));
    }
    if object.get("default_emotion").is_some_and(|value| {
        !matches!(
            value.as_str(),
            Some("neutral" | "happy" | "angry" | "sad" | "surprised")
        )
    }) {
        issues.push(json!({"field":"default_emotion", "kind":"invalid_value"}));
    }
    if object.get("expression_timeout").is_some_and(|value| {
        value
            .as_f64()
            .is_none_or(|number| !number.is_finite() || number <= 0.0)
    }) {
        issues.push(json!({"field":"expression_timeout", "kind":"expected_positive_number"}));
    }
    match object.get("emotion_hotkey_map") {
        None | Some(Value::Null) => {}
        Some(Value::Object(mappings)) => {
            if mappings.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "neutral" | "happy" | "angry" | "sad" | "surprised"
                )
            }) {
                issues.push(json!({"field":"emotion_hotkey_map", "kind":"invalid_emotion_key"}));
            }
        }
        Some(_) => issues.push(json!({"field":"emotion_hotkey_map", "kind":"expected_object"})),
    }
    Ok(json!({
        "status": if issues.is_empty() { "selected_fields_ok" } else { "selected_fields_invalid" },
        "format": if nested { "nested" } else { "flat" },
        "checked_fields": ["ollama_url", "ollama_model", "tts_voice", "vts_port", "enable_emotional_intelligence", "enable_voice_cloning", "enable_expression_control", "fallback_to_edge_tts", "sovits_url", "sovits_timeout", "sovits_language", "log_level", "default_emotion", "expression_timeout", "emotion_hotkey_map keys"],
        "issues": issues,
        "limitations": "Partial static check only; URL syntax uses Rust URL parsing, other fields, hotkey values, connectivity, devices and migration are not validated."
    }))
}
