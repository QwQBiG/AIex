use ai_ex_domain::AppError;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtsHotkey {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtsHotkeyList {
    pub model_loaded: bool,
    pub hotkeys: Vec<VtsHotkey>,
}

/// One available VTube Studio input parameter and its current value.
#[derive(Debug, Clone, PartialEq)]
pub struct VtsParameter {
    pub name: String,
    pub added_by: String,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub default_value: f64,
    pub is_custom: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VtsParameterList {
    pub model_loaded: bool,
    pub parameters: Vec<VtsParameter>,
}

pub(super) fn parameters(response: &Value) -> Result<VtsParameterList, AppError> {
    let invalid = || AppError::protocol("invalid or rejected VTS parameter discovery response");
    if response.get("messageType").and_then(Value::as_str) != Some("InputParameterListResponse") {
        return Err(invalid());
    }
    let data = response.get("data").ok_or_else(invalid)?;
    let model_loaded = data
        .get("modelLoaded")
        .and_then(Value::as_bool)
        .ok_or_else(invalid)?;
    let defaults = data
        .get("defaultParameters")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let custom = data
        .get("customParameters")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if defaults.len().saturating_add(custom.len()) > 1024 {
        return Err(invalid());
    }
    let mut parameters = Vec::with_capacity(defaults.len() + custom.len());
    for (items, is_custom) in [(defaults, false), (custom, true)] {
        for item in items {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            let added_by = item
                .get("addedBy")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            if name.trim().is_empty()
                || name.chars().count() > 256
                || added_by.chars().count() > 256
            {
                return Err(invalid());
            }
            let number = |key| {
                item.get(key)
                    .and_then(Value::as_f64)
                    .filter(|n| n.is_finite())
                    .ok_or_else(invalid)
            };
            parameters.push(VtsParameter {
                name: name.into(),
                added_by: added_by.into(),
                value: number("value")?,
                min: number("min")?,
                max: number("max")?,
                default_value: number("defaultValue")?,
                is_custom,
            });
        }
    }
    Ok(VtsParameterList {
        model_loaded,
        parameters,
    })
}

pub(super) fn hotkeys(response: &Value) -> Result<VtsHotkeyList, AppError> {
    let invalid = || AppError::protocol("invalid or rejected VTS hotkey discovery response");
    if response.get("messageType").and_then(Value::as_str) != Some("HotkeysInCurrentModelResponse")
    {
        return Err(invalid());
    }
    let data = response.get("data").ok_or_else(invalid)?;
    let loaded = data
        .get("modelLoaded")
        .and_then(Value::as_bool)
        .ok_or_else(invalid)?;
    let list = data
        .get("availableHotkeys")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if list.len() > 128 || (!loaded && !list.is_empty()) {
        return Err(invalid());
    }
    let mut ids = std::collections::BTreeSet::new();
    let hotkeys = list
        .iter()
        .map(|item| {
            let id = item
                .get("hotkeyID")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            if id.trim().is_empty()
                || id.chars().count() > 256
                || name.chars().count() > 256
                || !ids.insert(id)
            {
                return Err(invalid());
            }
            Ok(VtsHotkey {
                id: id.into(),
                name: name.into(),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(VtsHotkeyList {
        model_loaded: loaded,
        hotkeys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_bounded_well_formed_identifiers_are_exposed() {
        let response = |items| {
            json!({"messageType":"HotkeysInCurrentModelResponse",
            "data":{"modelLoaded":true,"availableHotkeys":items}})
        };
        for items in [
            json!([{"hotkeyID":"","name":"empty"}]),
            json!([{"hotkeyID":"x"}]),
            json!([{"hotkeyID":"x","name":"n"},{"hotkeyID":"x","name":"n"}]),
            json!([{"hotkeyID":"x".repeat(257),"name":"n"}]),
            json!([{"hotkeyID":"x","name":"n".repeat(257)}]),
            json!(
                (0..129)
                    .map(|index| json!({"hotkeyID":index.to_string(),"name":"n"}))
                    .collect::<Vec<_>>()
            ),
        ] {
            assert!(hotkeys(&response(items)).is_err());
        }
        let boundary = json!(
            (0..128)
                .map(|index| json!({"hotkeyID":index.to_string(),"name":"n"}))
                .collect::<Vec<_>>()
        );
        assert_eq!(hotkeys(&response(boundary)).unwrap().hotkeys.len(), 128);
        let mut unloaded = response(json!([]));
        unloaded["data"]["modelLoaded"] = json!(false);
        let result = hotkeys(&unloaded).unwrap();
        assert!(!result.model_loaded && result.hotkeys.is_empty());
        assert!(hotkeys(&json!({"messageType":"APIError"})).is_err());
    }

    #[test]
    fn input_parameters_preserve_sources_values_and_model_state() {
        let response = json!({"messageType":"InputParameterListResponse", "data": {
            "modelLoaded": false,
            "defaultParameters": [{"name":"MouthOpen","addedBy":"VTube Studio", "value":1.3, "min":0, "max":1, "defaultValue":0}],
            "customParameters": [{"name":"VoiceVolume","addedBy":"工具", "value":0.5, "min":-1, "max":1, "defaultValue":0}]
        }});
        let list = parameters(&response).unwrap();
        assert!(!list.model_loaded);
        assert_eq!(list.parameters.len(), 2);
        assert_eq!(list.parameters[0].value, 1.3);
        assert!(!list.parameters[0].is_custom);
        assert!(list.parameters[1].is_custom);
        assert_eq!(list.parameters[1].added_by, "工具");
    }

    #[test]
    fn input_parameters_reject_invalid_or_oversized_responses() {
        let valid = json!({"name":"MouthOpen","addedBy":"VTube Studio", "value":0.5, "min":0, "max":1, "defaultValue":0});
        let response = |items| json!({"messageType":"InputParameterListResponse", "data":{"modelLoaded":true,"defaultParameters":items,"customParameters":[]}});
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove("min");
        for item in [
            missing,
            json!({"name":"","addedBy":"x","value":0,"min":0,"max":1,"defaultValue":0}),
            json!({"name":"x".repeat(257),"addedBy":"x","value":0,"min":0,"max":1,"defaultValue":0}),
        ] {
            assert!(parameters(&response(json!([item]))).is_err());
        }
        assert_eq!(
            parameters(&response(json!(vec![valid.clone(); 1024])))
                .unwrap()
                .parameters
                .len(),
            1024
        );
        assert!(parameters(&response(json!(vec![valid; 1025]))).is_err());
        assert!(parameters(&json!({"messageType":"APIError"})).is_err());
    }
}
