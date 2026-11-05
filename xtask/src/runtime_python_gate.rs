//! Inspect an explicitly supplied runtime archive without executing or extracting it.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use ai_ex_model_catalog::{Artifact, ArtifactKind};
use ai_ex_model_manager::{DistributionCatalog, PackageSource};
use serde_json::{Value, json};
use zip::ZipArchive;

use crate::bundle_io::{hash_reader, read_json, regular_file};

#[path = "runtime_python_project_source.rs"]
mod project_source;

const MAX_INSPECTED_FILE_BYTES: u64 = 128 * 1024 * 1024;

struct ScanResult {
    issues: Vec<Value>,
    sources: Vec<String>,
    nested: Vec<String>,
    project_sources: Vec<Value>,
}

pub(crate) fn check(catalog: &Path, archive: &Path) -> Result<(Value, bool), String> {
    let catalog: DistributionCatalog = read_json(catalog)?;
    catalog.validate().map_err(|error| error.to_string())?;
    let mut input = regular_file(archive)?;
    let (bytes, sha256) = hash_reader(&mut input)?;
    let matches: Vec<_> = catalog
        .runtimes
        .iter()
        .filter(|runtime| {
            matches!(&runtime.source, PackageSource::Archive { bytes: expected, sha256: hash, .. }
            if *expected == bytes && hash.eq_ignore_ascii_case(&sha256))
        })
        .collect();
    if matches.len() != 1 {
        return Err("runtime ZIP does not match exactly one catalog size and SHA256".into());
    }
    let runtime = matches[0];
    input
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(input).map_err(|error| error.to_string())?;
    let ScanResult {
        issues,
        sources,
        nested,
        project_sources,
    } = inspect(&mut archive, &runtime.manifest.artifacts)?;
    let passed = issues.is_empty() && sources.is_empty() && nested.is_empty();
    Ok((
        json!({
            "status": if !issues.is_empty() { "failed" } else if !sources.is_empty() || !nested.is_empty() { "incomplete" } else { "passed_for_scope" },
            "runtime_id": runtime.manifest.id,
            "runtime_revision": runtime.manifest.revision,
            "archive_bytes": bytes,
            "archive_sha256": sha256,
            "files_checked": runtime.manifest.artifacts.len() - sources.len() - nested.len(),
            "issues": issues,
            "uninspected_source_archives": sources,
            "uninspected_nested_archives": nested,
            "project_source_audits": project_sources,
            "scope": "supplied catalog and runtime archive; names, script commands, common file-header shebangs, static PE imports and partial project source checks",
            "limitations": "catalog authenticity is not established; upstream or other nested archives, cross-file command aliases, dynamic or macro-generated commands, dynamic loading, embedded interpreters, other packages, concurrent file changes and remote services are not inspected"
        }),
        passed,
    ))
}

fn inspect(archive: &mut ZipArchive<File>, artifacts: &[Artifact]) -> Result<ScanResult, String> {
    if archive.len() != artifacts.len() {
        return Err("ZIP entry count differs from runtime manifest".into());
    }
    let mut issues = Vec::new();
    let mut sources = Vec::new();
    let mut nested = Vec::new();
    let mut project_sources = Vec::new();
    let mut source_budget = project_source::Budget::default();
    for artifact in artifacts {
        let mut entry = archive
            .by_name(&artifact.path)
            .map_err(|error| error.to_string())?;
        if !entry.is_file()
            || entry.size() != artifact.bytes
            || entry
                .unix_mode()
                .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000))
        {
            return Err(format!("invalid ZIP entry: {}", artifact.path));
        }
        let (bytes, sha256) =
            hash_reader(&mut entry.by_ref().take(artifact.bytes.saturating_add(1)))?;
        if bytes != artifact.bytes || !sha256.eq_ignore_ascii_case(&artifact.sha256) {
            return Err(format!("ZIP hash mismatch: {}", artifact.path));
        }
        drop(entry);
        if artifact.kind == ArtifactKind::SourceArchive {
            sources.push(artifact.path.clone());
            let mut entry = archive
                .by_name(&artifact.path)
                .map_err(|error| error.to_string())?;
            if source_budget.take_source(artifact.bytes) {
                let mut data = Vec::new();
                entry
                    .by_ref()
                    .take(artifact.bytes.saturating_add(1))
                    .read_to_end(&mut data)
                    .map_err(|error| error.to_string())?;
                if data.len() as u64 != artifact.bytes {
                    return Err(format!("source archive size changed: {}", artifact.path));
                }
                let audit = project_source::inspect(&artifact.path, data, &mut source_budget)?;
                let finding_count = audit["issues"].as_array().map_or(0, Vec::len);
                if finding_count > 0 {
                    issues.push(
                        json!({"kind":"project_source_findings", "path":artifact.path,
                        "count":finding_count}),
                    );
                }
                project_sources.push(audit);
            } else {
                project_sources.push(json!({"archive":artifact.path, "status":"not_checked",
                    "reason":"source archives exceed remaining inspection budget"}));
            }
            continue;
        }
        let name = &artifact.path;
        if crate::python_gate::artifact_name(name) {
            issues.push(json!({"kind": "runtime_artifact", "path": name}));
        }
        let mut entry = archive.by_name(name).map_err(|error| error.to_string())?;
        let mut header = vec![0; artifact.bytes.min(512) as usize];
        entry
            .read_exact(&mut header)
            .map_err(|error| error.to_string())?;
        if crate::python_gate::python_shebang_for_path(name, &header) {
            issues.push(json!({"kind": "runtime_command", "path": name, "line": 1}));
        }
        if crate::python_gate::archive_name(name) || crate::python_gate::archive_signature(&header)
        {
            nested.push(name.clone());
            continue;
        }
        let path = Path::new(name);
        let is_pe_name = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("dll"));
        let is_script = crate::python_gate::is_script(path);
        if !is_pe_name && !header.starts_with(b"MZ") && !is_script {
            continue;
        }
        if artifact.bytes > MAX_INSPECTED_FILE_BYTES {
            return Err(format!("runtime file exceeds inspection limit: {name}"));
        }
        let count = header.len();
        let mut data = header;
        entry
            .take(
                artifact
                    .bytes
                    .saturating_sub(count as u64)
                    .saturating_add(1),
            )
            .read_to_end(&mut data)
            .map_err(|error| error.to_string())?;
        if data.len() as u64 != artifact.bytes {
            return Err(format!(
                "runtime file size changed during inspection: {name}"
            ));
        }
        if is_script {
            let text =
                std::str::from_utf8(&data).map_err(|_| format!("script is not UTF-8: {name}"))?;
            for (index, line) in text.lines().enumerate() {
                if crate::python_gate::command_line(line) {
                    issues
                        .push(json!({"kind": "runtime_command", "path": name, "line": index + 1}));
                }
            }
        }
        if is_pe_name || data.starts_with(b"MZ") {
            for import in crate::pe_imports::inspect(&data)
                .map_err(|error| format!("cannot inspect PE {name}: {error}"))?
            {
                if import.python_runtime {
                    issues.push(json!({"kind": "pe_import", "path": name, "library": import.library, "delayed": import.delayed}));
                }
            }
        }
    }
    Ok(ScanResult {
        issues,
        sources,
        nested,
        project_sources,
    })
}

#[cfg(test)]
#[path = "runtime_python_gate_tests.rs"]
mod tests;
