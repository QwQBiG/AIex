use serde_json::{Value, json};
use uuid::Uuid;

pub fn authentication(token: &str, plugin_name: &str, developer: &str) -> Value {
    envelope(
        "AuthenticationRequest",
        json!({
            "pluginName": plugin_name,
            "pluginDeveloper": developer,
            "authenticationToken": token,
        }),
    )
}

pub fn mouth_open(value: f64) -> Value {
    envelope(
        "InjectParameterDataRequest",
        json!({
            "faceFound": false,
            "mode": "set",
            "parameterValues": [{
                "id": "MouthOpen",
                "value": value.clamp(0.0, 1.0),
                "weight": 1.0,
            }],
        }),
    )
}

pub fn trigger_hotkey(hotkey_id: &str) -> Value {
    envelope("HotkeyTriggerRequest", json!({ "hotkeyID": hotkey_id }))
}

pub fn discover_hotkeys() -> Value {
    envelope("HotkeysInCurrentModelRequest", json!({}))
}

pub fn discover_parameters() -> Value {
    envelope("InputParameterListRequest", json!({}))
}

pub fn authenticated(response: &Value) -> bool {
    response
        .get("data")
        .and_then(|data| data.get("authenticated"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn envelope(message_type: &str, data: Value) -> Value {
    json!({
        "apiName": "VTubeStudioPublicAPI",
        "apiVersion": "1.0",
        "requestID": Uuid::new_v4().to_string(),
        "messageType": message_type,
        "data": data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_mouth_parameter() {
        let request = mouth_open(2.0);
        assert_eq!(request["data"]["parameterValues"][0]["value"], 1.0);
    }
}
