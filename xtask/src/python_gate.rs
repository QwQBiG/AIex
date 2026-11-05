use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

#[path = "rust_command_gate.rs"]
mod rust_command_gate;

type Result<T> = std::result::Result<T, String>;

pub fn check(project: &Path, distribution: &Path) -> Result<(Value, bool)> {
    let project = project
        .canonicalize()
        .map_err(|e| format!("project: {e}"))?;
    let distribution_metadata =
        fs::symlink_metadata(distribution).map_err(|e| format!("distribution: {e}"))?;
    if is_link_or_reparse(&distribution_metadata) {
        return Err("distribution cannot be a link or reparse point".into());
    }
    let distribution = distribution
        .canonicalize()
        .map_err(|e| format!("distribution: {e}"))?;
    if !distribution.is_dir() {
        return Err("distribution must be an extracted directory".into());
    }
    validate_required_files(&distribution)?;
    let root = Command::new("git")
        .args([
            "-C",
            project.to_str().ok_or("project path is not UTF-8")?,
            "rev-parse",
            "--show-toplevel",
        ])
        .output()
        .map_err(|e| format!("git rev-parse: {e}"))?;
    if !root.status.success() {
        return Err(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&root.stderr)
        ));
    }
    let root = std::str::from_utf8(&root.stdout).map_err(|_| "git root is not UTF-8")?;
    let root = Path::new(root.trim_end_matches(['\r', '\n']))
        .canonicalize()
        .map_err(|e| format!("git root: {e}"))?;
    if root != project || !project.join("Cargo.toml").is_file() {
        return Err("project must be the repository root with Cargo.toml".into());
    }
    let output = Command::new("git")
        .args([
            "-C",
            project.to_str().ok_or("project path is not UTF-8")?,
            "ls-files",
            "-z",
        ])
        .output()
        .map_err(|e| format!("git ls-files: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let mut issues = Vec::new();
    let mut source_files = 0;
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let relative = std::str::from_utf8(raw).map_err(|_| "tracked path is not UTF-8")?;
        let path = project.join(relative);
        let missing = inspect_tracked_path(&path, relative, &mut issues)?;
        source_files += 1;
        if missing {
            continue;
        }
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if filename == "Cargo.toml" || filename == "Cargo.lock" {
            let text = read_text(&path)?;
            let document: toml::Value =
                toml::from_str(&text).map_err(|e| format!("{relative}: {e}"))?;
            check_toml(&document, &mut issues, relative);
        }
        if is_script(&path) {
            for (index, line) in read_text(&path)?.lines().enumerate() {
                if command_line(line) {
                    issue(&mut issues, "script_command", relative, Some(index + 1));
                }
            }
        }
        if path.extension().is_some_and(|extension| extension == "rs") {
            let source = read_text(&path)?;
            check_rust(&source, &mut issues, relative)?;
        }
    }
    let mut distribution_files = 0;
    scan_distribution(
        &distribution,
        &distribution,
        &mut distribution_files,
        &mut issues,
    )?;
    if distribution_files == 0 {
        return Err("distribution directory is empty".into());
    }
    let passed = issues.is_empty();
    Ok((
        json!({
            "status": if passed { "passed" } else { "failed" },
            "source_files_checked": source_files,
            "distribution_files_checked": distribution_files,
            "issues": issues,
            "scope": "tracked files, Cargo manifests and locks, literal Rust launches, script commands, extracted distribution filenames, script commands and static PE imports including renamed files",
            "limitations": "nested archives are not inspected; unrecognized or renamed archives may not be detected. Cross-file import aliases, dynamic or macro-generated commands, runtime library loading, embedded interpreters, remote services, and concurrent file changes are not inspected"
        }),
        passed,
    ))
}

fn validate_required_files(distribution: &Path) -> Result<()> {
    for relative in crate::portable::required_files() {
        let relative_path = Path::new(relative);
        let mut path = distribution.to_path_buf();
        for component in relative_path.components() {
            path.push(component);
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                format!("distribution is missing or cannot inspect {relative}: {error}")
            })?;
            if is_link_or_reparse(&metadata)
                || (path == distribution.join(relative_path) && !metadata.is_file())
                || (path != distribution.join(relative_path) && !metadata.is_dir())
            {
                return Err(format!("distribution has an invalid path for {relative}"));
            }
        }
    }
    Ok(())
}

fn inspect_tracked_path(path: &Path, relative: &str, issues: &mut Vec<Value>) -> Result<bool> {
    let artifact = artifact_name(relative);
    if artifact {
        issue(issues, "source_artifact", relative, None);
    }
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && artifact => Ok(true),
        Err(error) => Err(format!("tracked file {relative}: {error}")),
    }
}

fn read_text(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(format!("not a regular file: {}", path.display()));
    }
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn issue(issues: &mut Vec<Value>, kind: &str, path: &str, line: Option<usize>) {
    issues.push(json!({ "kind": kind, "path": path, "line": line }));
}

pub(crate) fn artifact_name(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("");
    matches!(extension, "py" | "pyw" | "pyc" | "pyo" | "pyd" | "whl")
        || matches!(
            name.as_str(),
            "pyproject.toml" | "pipfile" | "pipfile.lock" | "poetry.lock"
        )
        || (name.starts_with("requirements") && name.ends_with(".txt"))
        || interpreter_name(&name)
        || crate::pe_imports::python_dll(&name)
}

pub(crate) fn archive_name(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    [
        ".zip", ".7z", ".rar", ".tar", ".tgz", ".gz", ".bz2", ".xz", ".zst", ".npz", ".crate",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

pub(crate) fn archive_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
        || bytes.starts_with(b"\x1f\x8b")
        || bytes.starts_with(b"7z\xbc\xaf\x27\x1c")
        || bytes.starts_with(b"\xfd7zXZ\0")
        || bytes.starts_with(b"BZh")
        || bytes.starts_with(b"\x28\xb5\x2f\xfd")
        || bytes.starts_with(b"Rar!\x1a\x07")
        || bytes.get(257..262) == Some(b"ustar")
}

fn interpreter_name(name: &str) -> bool {
    let name = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".dll"))
        .unwrap_or(name);
    matches!(name, "python" | "pythonw" | "py" | "pip" | "pip3")
        || name.strip_prefix("python").is_some_and(|version| {
            !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
}

pub(crate) fn is_script(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "ps1" | "cmd" | "bat" | "sh" | "yml" | "yaml"
            )
        })
}

pub(crate) fn command_line(line: &str) -> bool {
    let line = line.trim_start();
    if line.starts_with('#') || line.starts_with("::") {
        return false;
    }
    line.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '\\')))
        .filter(|word| !word.is_empty())
        .any(|word| {
            interpreter_name(
                &word
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(word)
                    .to_ascii_lowercase(),
            )
        })
}

fn binding_name(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    ["pyo3", "cpython", "rustpython", "pythonize", "maturin"]
        .iter()
        .any(|name| {
            value == *name
                || value
                    .strip_prefix(name)
                    .is_some_and(|suffix| suffix.starts_with('-'))
        })
}

pub(crate) fn check_rust(source: &str, issues: &mut Vec<Value>, path: &str) -> Result<()> {
    let matches = rust_command_gate::scan(source)
        .map_err(|error| format!("cannot inspect Rust source {path}: {error}"))?;
    if matches > 0 {
        issues.push(json!({"kind": "rust_command", "path": path, "count": matches}));
    }
    Ok(())
}

pub(crate) fn check_toml(value: &toml::Value, issues: &mut Vec<Value>, path: &str) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                if binding_name(key)
                    || (matches!(key.as_str(), "name" | "package")
                        && value.as_str().is_some_and(binding_name))
                {
                    issue(issues, "cargo_binding", path, None);
                }
                check_toml(value, issues, path);
            }
        }
        toml::Value::Array(values) => {
            for value in values {
                check_toml(value, issues, path);
            }
        }
        _ => {}
    }
}

fn scan_distribution(
    root: &Path,
    directory: &Path,
    count: &mut usize,
    issues: &mut Vec<Value>,
) -> Result<()> {
    let entries = fs::read_dir(directory).map_err(|e| format!("{}: {e}", directory.display()))?;
    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.map(|value| value.path()).map_err(|e| e.to_string()))
        .collect::<Result<_>>()?;
    paths.sort();
    for path in paths {
        let metadata =
            fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if is_link_or_reparse(&metadata) {
            return Err(format!(
                "distribution contains a link or reparse point: {}",
                path.display()
            ));
        }
        if metadata.is_dir() {
            scan_distribution(root, &path, count, issues)?;
        } else if metadata.is_file() {
            *count += 1;
            let relative = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy();
            if artifact_name(&relative) {
                issue(issues, "distribution_artifact", &relative, None);
            }
            if is_script(&path) {
                for (index, line) in read_text(&path)?.lines().enumerate() {
                    if command_line(line) {
                        issue(issues, "distribution_command", &relative, Some(index + 1));
                    }
                }
            }
            let pe_extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("dll")
                });
            let mut file =
                fs::File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let mut header = Vec::with_capacity(512);
            file.by_ref()
                .take(512)
                .read_to_end(&mut header)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if archive_name(&relative) || archive_signature(&header) {
                issue(issues, "distribution_archive_uninspected", &relative, None);
            }
            if pe_extension || header.starts_with(b"MZ") {
                let bytes =
                    fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                for import in crate::pe_imports::inspect(&bytes)
                    .map_err(|error| format!("cannot inspect PE {relative}: {error}"))?
                {
                    if import.python_runtime {
                        issues.push(json!({"kind": "pe_import", "path": relative, "library": import.library, "delayed": import.delayed}));
                    }
                }
            }
        } else {
            return Err(format!(
                "distribution contains a link or non-file: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_runtime_artifacts_without_flagging_history() {
        for path in [
            "src/a.py",
            "cache/a.pyc",
            "bin/python311.dll",
            "bin/python311_d.dll",
            "bin/libpython3.11.dll",
            "bin/python.exe",
            "x/pkg.whl",
            "requirements-dev.txt",
            "pyproject.toml",
        ] {
            assert!(artifact_name(path), "{path}");
        }
        for path in [
            "docs/PYTHON_INVENTORY.tsv",
            "docs/python-migration.md",
            "python-notes.txt",
            "bin/python_adapter.exe",
        ] {
            assert!(!artifact_name(path), "{path}");
        }
    }

    #[test]
    fn detects_runtime_library_files_without_imports() {
        let root = std::env::temp_dir().join(format!("aiex-gate-libraries-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let names = ["python311_d.dll", "libpython3.11.dll"];
        for name in names {
            fs::write(
                root.join(name),
                crate::pe_imports::tests::fixture(None, None),
            )
            .unwrap();
        }
        let mut count = 0;
        let mut issues = Vec::new();
        scan_distribution(&root, &root, &mut count, &mut issues).unwrap();
        assert_eq!(count, 2);
        assert_eq!(issues.len(), 2);
        for name in names {
            assert!(
                issues
                    .iter()
                    .any(|item| item["kind"] == "distribution_artifact" && item["path"] == name)
            );
            fs::remove_file(root.join(name)).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn missing_tracked_artifact_is_not_hidden_by_worktree_deletion() {
        let path =
            std::env::temp_dir().join(format!("missing-gate-{}-legacy.py", std::process::id()));
        let mut issues = Vec::new();
        assert!(inspect_tracked_path(&path, "legacy.py", &mut issues).unwrap());
        assert_eq!(issues.len(), 1);
        assert!(inspect_tracked_path(&path, "Cargo.toml", &mut issues).is_err());
    }

    #[test]
    fn detects_commands_but_ignores_comment_lines() {
        for line in [
            "python script.py",
            "& 'C:\\Program Files\\Python311\\python.exe' a.py",
            "run: python3.11 -m pip install pkg",
            "pwsh -c python",
        ] {
            assert!(command_line(line), "{line}");
        }
        for line in [
            "# python script.py",
            ":: python script.py",
            "cargo test --locked",
        ] {
            assert!(!command_line(line), "{line}");
        }
    }

    #[test]
    fn detects_renamed_and_transitive_bindings() {
        let value: toml::Value = toml::from_str(
            r#"
[dependencies]
wrapper = { package = "pyo3", version = "0.25" }
[[package]]
name = "rustpython-vm"
version = "0.1"
"#,
        )
        .unwrap();
        let mut issues = Vec::new();
        check_toml(&value, &mut issues, "Cargo.toml");
        assert_eq!(issues.len(), 2);
        assert!(binding_name("cpython"));
        assert!(!binding_name("pythonless"));
    }

    #[test]
    fn examines_files_inside_extracted_distribution() {
        let root = std::env::temp_dir().join(format!("aiex-python-gate-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("AIex.exe"),
            crate::pe_imports::tests::fixture(Some("python311.dll"), Some("PYTHON3.DLL")),
        )
        .unwrap();
        fs::write(root.join("startup.cmd"), b"python.exe worker.py\n").unwrap();
        fs::write(root.join("extra.zip"), b"PK\x03\x04known archive").unwrap();
        fs::write(root.join("renamed.bin"), b"PK\x03\x04renamed archive").unwrap();
        fs::create_dir(root.join("runtime")).unwrap();
        fs::write(
            root.join("runtime").join("python311.dll"),
            crate::pe_imports::tests::fixture(Some("KERNEL32.dll"), None),
        )
        .unwrap();
        let mut count = 0;
        let mut issues = Vec::new();
        scan_distribution(&root, &root, &mut count, &mut issues).unwrap();
        assert_eq!(count, 5);
        assert_eq!(issues.len(), 6);
        assert_eq!(
            issues
                .iter()
                .filter(|item| item["kind"] == "distribution_archive_uninspected"
                    && item["path"] == "extra.zip")
                .count(),
            1
        );
        assert!(
            issues
                .iter()
                .any(|item| item["kind"] == "distribution_archive_uninspected"
                    && item["path"] == "renamed.bin")
        );
        assert!(
            issues
                .iter()
                .any(|item| item["kind"] == "distribution_archive_uninspected"
                    && item["path"] == "extra.zip")
        );
        assert!(
            issues
                .iter()
                .any(|item| item["kind"] == "distribution_command"
                    && item["path"] == "startup.cmd"
                    && item["line"] == 1)
        );
        assert!(
            issues
                .iter()
                .any(|item| item["kind"] == "distribution_artifact")
        );
        assert!(issues.iter().any(|item| item["kind"] == "pe_import"
            && item["library"] == "python311.dll"
            && item["delayed"] == false));
        assert!(issues.iter().any(|item| item["kind"] == "pe_import"
            && item["library"] == "PYTHON3.DLL"
            && item["delayed"] == true));
        fs::remove_file(root.join("runtime").join("python311.dll")).unwrap();
        fs::remove_dir(root.join("runtime")).unwrap();
        fs::remove_file(root.join("startup.cmd")).unwrap();
        fs::remove_file(root.join("extra.zip")).unwrap();
        fs::remove_file(root.join("renamed.bin")).unwrap();
        fs::remove_file(root.join("AIex.exe")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn distribution_rejects_corrupt_executable() {
        let root =
            std::env::temp_dir().join(format!("aiex-gate-corrupt-pe-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("broken.exe"), b"not a PE").unwrap();
        let mut count = 0;
        let mut issues = Vec::new();
        let error = scan_distribution(&root, &root, &mut count, &mut issues).unwrap_err();
        assert!(error.contains("cannot inspect PE broken.exe"), "{error}");
        fs::remove_file(root.join("broken.exe")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn distribution_inspects_renamed_and_extensionless_pe_files() {
        let root =
            std::env::temp_dir().join(format!("aiex-gate-renamed-pe-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("worker"),
            crate::pe_imports::tests::fixture(Some("python311.dll"), None),
        )
        .unwrap();
        fs::write(
            root.join("helper.dat"),
            crate::pe_imports::tests::fixture(None, Some("PYTHON3.DLL")),
        )
        .unwrap();
        fs::write(root.join("ordinary.dat"), b"ordinary data").unwrap();
        let mut count = 0;
        let mut issues = Vec::new();
        scan_distribution(&root, &root, &mut count, &mut issues).unwrap();
        assert_eq!(count, 3);
        assert_eq!(issues.len(), 2);
        assert!(
            issues
                .iter()
                .any(|issue| issue["path"] == "worker" && issue["library"] == "python311.dll")
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue["path"] == "helper.dat" && issue["delayed"] == true)
        );
        fs::write(root.join("worker"), b"MZbad").unwrap();
        assert!(scan_distribution(&root, &root, &mut 0, &mut Vec::new()).is_err());
        for name in ["worker", "helper.dat", "ordinary.dat"] {
            fs::remove_file(root.join(name)).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn rejects_missing_non_executable_portable_file() {
        let root =
            std::env::temp_dir().join(format!("aiex-gate-required-files-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("data")).unwrap();
        let files: Vec<_> = crate::portable::required_files().collect();
        assert_eq!(files.len(), 10);
        for file in files.iter().filter(|file| **file != "开始使用.txt") {
            fs::write(root.join(file), b"synthetic").unwrap();
        }
        let error = validate_required_files(&root).unwrap_err();
        assert!(error.contains("开始使用.txt"), "{error}");
        fs::write(root.join("开始使用.txt"), b"synthetic").unwrap();
        validate_required_files(&root).unwrap();
        for file in files {
            fs::remove_file(root.join(file)).unwrap();
        }
        fs::remove_dir(root.join("data")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn distribution_rejects_windows_junction_before_traversing_it() {
        let temp = std::env::temp_dir();
        let id = std::process::id();
        let root = temp.join(format!("aiex-gate-junction-root-{id}"));
        let outside = temp.join(format!("aiex-gate-junction-outside-{id}"));
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("python.exe"), b"must not be visited").unwrap();
        let link = root.join("junction");
        let command = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("cmd.exe");
        let result = Command::new(command)
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(is_link_or_reparse(&fs::symlink_metadata(&link).unwrap()));
        let mut count = 0;
        let mut issues = Vec::new();
        let error = scan_distribution(&root, &root, &mut count, &mut issues).unwrap_err();
        assert!(error.contains("reparse point"), "{error}");
        assert_eq!(count, 0);
        assert!(issues.is_empty());
        assert_eq!(
            fs::read(outside.join("python.exe")).unwrap(),
            b"must not be visited"
        );
        fs::remove_dir(&link).unwrap();
        fs::remove_file(outside.join("python.exe")).unwrap();
        fs::remove_dir(&outside).unwrap();
        fs::remove_dir(&root).unwrap();
    }
}
