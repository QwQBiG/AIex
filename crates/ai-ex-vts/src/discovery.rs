use ai_ex_domain::AppError;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtsHotkey {
    pub id: String,
    pub name: String,
}

pub(super) fn hotkeys(response: &Value) -> Result<Vec<VtsHotkey>, AppError> {
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
    list.iter()
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
        .collect()
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
        assert_eq!(hotkeys(&response(boundary)).unwrap().len(), 128);
        let mut unloaded = response(json!([]));
        unloaded["data"]["modelLoaded"] = json!(false);
        assert!(hotkeys(&unloaded).unwrap().is_empty());
        assert!(hotkeys(&json!({"messageType":"APIError"})).is_err());
    }
}
