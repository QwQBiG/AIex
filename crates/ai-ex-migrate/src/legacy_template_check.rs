use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use ai_ex_domain::AppError;
use serde::Serialize;
use serde_json::Value;

const MAX_INPUT: u64 = 4 * 1024 * 1024;
const MAX_TEMPLATES: usize = 10_000;

#[derive(Default, Serialize)]
pub(crate) struct Inspection {
    format: &'static str,
    templates: usize,
    invalid_entries: usize,
    duplicate_ids: usize,
    unknown_root_fields: usize,
    unknown_entry_fields: usize,
    opaque_data_objects: usize,
    offsets_absent: usize,
    offsets_invalid: usize,
    offsets_nonzero: usize,
}

pub(crate) fn inspect(path: &Path) -> Result<Inspection, AppError> {
    let file =
        File::open(path).map_err(|_| AppError::configuration("cannot open template export"))?;
    let mut bytes = Vec::new();
    file.take(MAX_INPUT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::configuration("cannot read template export"))?;
    if bytes.len() as u64 > MAX_INPUT {
        return Err(AppError::configuration("template export exceeds 4 MiB"));
    }
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::configuration("invalid template export JSON"))?;
    let root = document
        .as_object()
        .ok_or_else(|| AppError::configuration("template export must be an object"))?;
    let templates = root
        .get("templates")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::configuration("template export needs a templates array"))?;
    if templates.len() > MAX_TEMPLATES {
        return Err(AppError::configuration(
            "template export exceeds 10000 entries",
        ));
    }
    let mut report = Inspection {
        format: "legacy_template_export",
        templates: templates.len(),
        unknown_root_fields: root
            .keys()
            .filter(|key| !matches!(key.as_str(), "templates" | "timestamp"))
            .count(),
        ..Inspection::default()
    };
    let mut ids = HashSet::new();
    for entry in templates {
        let Some(row) = entry.as_object() else {
            report.invalid_entries += 1;
            continue;
        };
        report.unknown_entry_fields += row
            .keys()
            .filter(|key| {
                !matches!(
                    key.as_str(),
                    "template_id"
                        | "game_name"
                        | "type"
                        | "data"
                        | "confidence"
                        | "adaptive_offset"
                )
            })
            .count();
        let id = row.get("template_id").and_then(Value::as_str);
        if let Some(id) = id
            && !ids.insert(id)
        {
            report.duplicate_ids += 1;
        }
        if row.get("data").is_some_and(Value::is_object) {
            report.opaque_data_objects += 1;
        }
        let valid_fields = id.is_some_and(|id| !id.is_empty())
            && row.get("game_name").is_some_and(Value::is_string)
            && row.get("type").is_some_and(Value::is_string)
            && row.get("data").is_some_and(Value::is_object)
            && row
                .get("confidence")
                .is_none_or(|value| value.as_f64().is_some_and(f64::is_finite));
        if !valid_fields {
            report.invalid_entries += 1;
        }
        match row.get("adaptive_offset") {
            None => report.offsets_absent += 1,
            Some(value) => match value.as_array().map(Vec::as_slice) {
                Some([x, y]) => match (x.as_f64(), y.as_f64()) {
                    (Some(x), Some(y)) if x.is_finite() && y.is_finite() => {
                        if x != 0.0 || y != 0.0 {
                            report.offsets_nonzero += 1;
                        }
                    }
                    _ => report.offsets_invalid += 1,
                },
                _ => report.offsets_invalid += 1,
            },
        }
    }
    Ok(report)
}
