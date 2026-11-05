//! Bounded inspection of project files inside a corresponding-sources ZIP.

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::Path;

use serde_json::{Value, json};
use zip::ZipArchive;

pub(super) const MAX_SOURCES_BYTES: u64 = 192 * 1024 * 1024;
const MAX_PROJECT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TEXT_TOTAL: u64 = 32 * 1024 * 1024;

pub(super) struct Budget {
    sources_left: u64,
    projects_left: u64,
    text_left: u64,
    entries_left: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            sources_left: MAX_SOURCES_BYTES,
            projects_left: MAX_PROJECT_BYTES,
            text_left: MAX_TEXT_TOTAL,
            entries_left: MAX_ENTRIES,
        }
    }
}

impl Budget {
    pub(super) fn take_source(&mut self, bytes: u64) -> bool {
        if let Some(remaining) = self.sources_left.checked_sub(bytes) {
            self.sources_left = remaining;
            true
        } else {
            false
        }
    }

    fn take_entries(&mut self, count: usize) -> Result<(), String> {
        self.entries_left = self
            .entries_left
            .checked_sub(count)
            .ok_or("source entry budget exhausted")?;
        Ok(())
    }
}

fn bounded_zip(data: Vec<u8>) -> Result<ZipArchive<Cursor<Vec<u8>>>, String> {
    let start = data.len().saturating_sub(65_557);
    let end = data.len().checked_sub(22).ok_or("ZIP is truncated")?;
    let eocd = (start..=end)
        .rev()
        .find(|&offset| {
            data.get(offset..offset + 4) == Some(b"PK\x05\x06")
                && data.get(offset + 20..offset + 22).is_some_and(|length| {
                    offset + 22 + usize::from(u16::from_le_bytes([length[0], length[1]]))
                        == data.len()
                })
        })
        .ok_or("ZIP end record is missing")?;
    let field = |offset| u16::from_le_bytes([data[eocd + offset], data[eocd + offset + 1]]);
    let entries = field(10);
    if field(4) != 0 || field(6) != 0 || field(8) != entries || entries == u16::MAX {
        return Err("multi-disk or ZIP64 source archives are not supported".into());
    }
    if usize::from(entries) > MAX_ENTRIES {
        return Err("source ZIP exceeds entry limit".into());
    }
    ZipArchive::new(Cursor::new(data)).map_err(|error| format!("invalid source ZIP: {error}"))
}

fn validate_entry(name: &str, directory: bool, names: &mut HashSet<String>) -> Result<(), String> {
    let path = if directory {
        name.strip_suffix('/').unwrap_or(name)
    } else {
        name
    };
    ai_ex_model_catalog::validate_relative_path(path)
        .map_err(|_| format!("invalid source ZIP path: {name}"))?;
    if !names.insert(path.to_uppercase()) {
        return Err(format!("duplicate source ZIP path: {name}"));
    }
    Ok(())
}

pub(super) fn inspect(
    archive_name: &str,
    data: Vec<u8>,
    budget: &mut Budget,
) -> Result<Value, String> {
    let mut sources = match bounded_zip(data) {
        Ok(zip) => zip,
        Err(error) if error == "ZIP end record is missing" || error == "ZIP is truncated" => {
            return Ok(json!({"archive":archive_name, "status":"not_checked", "reason":error}));
        }
        Err(error) => return Err(error),
    };
    if sources.len() > MAX_ENTRIES {
        return Err("source ZIP exceeds entry limit".into());
    }
    budget.take_entries(sources.len())?;
    let mut names = HashSet::new();
    let mut project_index = None;
    for index in 0..sources.len() {
        let entry = sources.by_index(index).map_err(|error| error.to_string())?;
        let name =
            std::str::from_utf8(entry.name_raw()).map_err(|_| "source ZIP path is not UTF-8")?;
        validate_entry(name, entry.is_dir(), &mut names)?;
        if entry
            .unix_mode()
            .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000 | 0o040000))
        {
            return Err(format!("source ZIP contains a non-file: {name}"));
        }
        if name == "project/source.zip" && entry.is_file() {
            project_index = Some(index);
        }
    }
    let Some(index) = project_index else {
        return Ok(json!({"archive":archive_name, "status":"not_checked",
            "reason":"project/source.zip is absent"}));
    };
    let entry = sources.by_index(index).map_err(|error| error.to_string())?;
    if entry.size() > budget.projects_left {
        return Ok(json!({"archive":archive_name, "status":"not_checked",
            "reason":"project source ZIP exceeds remaining inspection budget"}));
    }
    let expected_size = entry.size();
    budget.projects_left -= expected_size;
    let mut project = Vec::new();
    entry
        .take(expected_size.saturating_add(1))
        .read_to_end(&mut project)
        .map_err(|error| error.to_string())?;
    if project.len() as u64 != expected_size {
        return Err("project source ZIP size does not match its entry".into());
    }
    inspect_project(archive_name, project, budget)
}

fn inspect_project(
    archive_name: &str,
    data: Vec<u8>,
    budget: &mut Budget,
) -> Result<Value, String> {
    let mut project = bounded_zip(data).map_err(|error| format!("project ZIP: {error}"))?;
    if project.len() > MAX_ENTRIES {
        return Err("project ZIP exceeds entry limit".into());
    }
    budget.take_entries(project.len())?;
    let mut names = HashSet::new();
    let mut issues = Vec::new();
    let mut nested = Vec::new();
    let mut checked = 0usize;
    for index in 0..project.len() {
        let mut entry = project.by_index(index).map_err(|error| error.to_string())?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| "project ZIP path is not UTF-8")?
            .to_owned();
        validate_entry(&name, entry.is_dir(), &mut names)?;
        if entry
            .unix_mode()
            .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000 | 0o040000))
        {
            return Err(format!("project ZIP contains a non-file: {name}"));
        }
        if entry.is_dir() {
            continue;
        }
        checked += 1;
        if crate::python_gate::artifact_name(&name) {
            issues.push(json!({"kind":"source_artifact", "path":name}));
        }
        let mut header = vec![0; entry.size().min(512) as usize];
        entry
            .read_exact(&mut header)
            .map_err(|error| error.to_string())?;
        if crate::python_gate::archive_name(&name) || crate::python_gate::archive_signature(&header)
        {
            nested.push(name);
            continue;
        }
        let path = Path::new(&name);
        let is_script = crate::python_gate::is_script(path);
        let is_rust = path.extension().is_some_and(|extension| extension == "rs");
        let is_cargo = path
            .file_name()
            .is_some_and(|filename| filename == "Cargo.toml" || filename == "Cargo.lock");
        if is_script || is_rust || is_cargo {
            if entry.size() > MAX_TEXT_BYTES || entry.size() > budget.text_left {
                return Err("project source content exceeds inspection limit".into());
            }
            budget.text_left -= entry.size();
            let expected_size = entry.size();
            let mut bytes = header;
            entry
                .take(
                    expected_size
                        .saturating_sub(bytes.len() as u64)
                        .saturating_add(1),
                )
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() as u64 != expected_size {
                return Err("project source size does not match its entry".into());
            }
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| format!("project source is not UTF-8: {name}"))?;
            if is_script {
                for (index, line) in text.lines().enumerate() {
                    if crate::python_gate::command_line(line) {
                        issues
                            .push(json!({"kind":"source_command", "path":name, "line":index + 1}));
                    }
                }
            }
            if is_rust {
                crate::python_gate::check_rust(text, &mut issues, &name)?;
            }
            if is_cargo {
                let value: toml::Value = toml::from_str(text)
                    .map_err(|error| format!("invalid Cargo input {name}: {error}"))?;
                crate::python_gate::check_toml(&value, &mut issues, &name);
            }
        }
    }
    Ok(
        json!({"archive":archive_name, "status":"partial", "files_checked":checked,
        "issues":issues, "uninspected_nested_archives":nested,
        "scope":"project source filenames, script commands, literal Rust launches and Cargo bindings"}),
    )
}

#[cfg(test)]
#[path = "runtime_python_project_source_tests.rs"]
mod tests;
