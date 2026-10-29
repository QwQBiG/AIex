use ai_ex_domain::AppError;

/// Validate portable relative syntax using Windows filename rules on every host.
/// This does not access files or check whether their parents are links.
pub fn validate_relative_path(path: &str) -> Result<(), AppError> {
    if path.is_empty()
        || path.len() > 4_096
        || path.contains(['\\', ':', '<', '>', '"', '|', '?', '*'])
        || path.chars().any(char::is_control)
    {
        return Err(AppError::configuration("invalid relative resource path"));
    }
    for component in path.split('/') {
        if component.is_empty()
            || matches!(component, "." | "..")
            || component.ends_with(['.', ' '])
            || is_device_name(component)
        {
            return Err(AppError::configuration("invalid relative resource path"));
        }
    }
    Ok(())
}

fn is_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || matches!(
        stem.strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT")),
        Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
    )
}

#[cfg(test)]
mod tests {
    use super::validate_relative_path;

    #[test]
    fn accepts_relative_wav_and_unicode_resources() {
        for path in ["reference.wav", "voices/人物 voice.wav", "模型/model.onnx"] {
            validate_relative_path(path).unwrap();
        }
    }

    #[test]
    fn rejects_windows_escape_and_alias_paths_on_every_host() {
        for path in [
            "",
            "/model.onnx",
            "../model.onnx",
            "x/../model.onnx",
            "./model.onnx",
            "C:/model.onnx",
            "C:model.onnx",
            "//server/share/file",
            "x\\model.onnx",
            "x//model.onnx",
            "x/",
            "x/file:stream",
            "x/model.",
            "x/model ",
            "NUL",
            "con.json",
            "x/COM1.bin",
            "x/lpt9",
            "x/a?b",
            "x/\0file",
        ] {
            assert!(validate_relative_path(path).is_err(), "accepted {path:?}");
        }
    }
}
