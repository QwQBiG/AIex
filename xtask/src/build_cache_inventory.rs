//! Read-only inventory of the known incremental build directories.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

const TARGETS: [&str; 5] = [
    "target/debug/incremental",
    "crates/ai-ex-desktop/target/debug/incremental",
    "target/m0-fetch-native-20261029/target/debug/incremental",
    "target/native-ui-checks-20261101/debug/incremental",
    "target/native-model-manager-20261101/debug/incremental",
];

type Result<T> = std::result::Result<T, String>;

#[derive(Default)]
struct GitStatus {
    tracked: Vec<String>,
    ignored: bool,
}

pub(crate) fn inspect(project: &Path) -> Result<(Value, bool)> {
    let root_metadata = fs::symlink_metadata(project).map_err(|e| format!("project: {e}"))?;
    if reparse(&root_metadata) || !root_metadata.is_dir() {
        return Err("project must be a directory, not a link".into());
    }
    let root = project
        .canonicalize()
        .map_err(|e| format!("project: {e}"))?;
    let output = git(&root, &["rev-parse", "--show-toplevel"])?;
    if !output.status.success() {
        return Err(format!(
            "cannot find Git root: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let output = std::str::from_utf8(&output.stdout).map_err(|_| "Git root is not UTF-8")?;
    let git_root = Path::new(output.trim_end_matches(['\r', '\n']))
        .canonicalize()
        .map_err(|e| format!("Git root: {e}"))?;
    if root != git_root {
        return Err("project must be the Git repository root".into());
    }
    inventory(&root, |relative| git_status(&root, relative))
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("cannot execute Git: {e}"))
}

fn git_status(root: &Path, relative: &str) -> Result<GitStatus> {
    let tracked = git(root, &["ls-files", "-z", "--", relative])?;
    if !tracked.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&tracked.stderr)
        ));
    }
    let tracked = tracked
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| {
            std::str::from_utf8(name)
                .map(str::to_owned)
                .map_err(|_| "tracked path is not UTF-8".to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    let ignored = git(root, &["check-ignore", "--quiet", "--", relative])?;
    let ignored = match ignored.status.code() {
        Some(0) => true,
        Some(1) => false,
        _ => {
            return Err(format!(
                "git check-ignore failed: {}",
                String::from_utf8_lossy(&ignored.stderr)
            ));
        }
    };
    Ok(GitStatus { tracked, ignored })
}

fn inventory(root: &Path, git_status: impl Fn(&str) -> Result<GitStatus>) -> Result<(Value, bool)> {
    let mut targets = Vec::new();
    let mut issues = Vec::new();
    for relative in TARGETS {
        let status = git_status(relative)?;
        for path in status.tracked {
            issues.push(json!({ "kind": "tracked", "path": path }));
        }
        if !status.ignored {
            issues.push(json!({ "kind": "not_ignored", "path": relative }));
        }
        let mut count = 0_u64;
        let mut bytes = 0_u64;
        let state = inspect_ancestry(root, relative, &mut issues)?;
        if state == TargetState::Directory {
            walk(
                root,
                &root.join(relative),
                &mut count,
                &mut bytes,
                &mut issues,
            )?;
        }
        targets.push(
            json!({ "path": relative, "state": state.label(), "files": count, "bytes": bytes }),
        );
    }
    let passed = issues.is_empty();
    Ok((
        json!({
            "status": if passed { "passed" } else { "issues_found" },
            "mode": "read_only",
            "targets": targets,
            "issues": issues,
            "limitations": "inspection is not a deletion plan and does not prove file ownership or exclusion of concurrent builds or filesystem changes"
        }),
        passed,
    ))
}

#[derive(PartialEq, Eq)]
enum TargetState {
    Missing,
    Directory,
    Rejected,
}

impl TargetState {
    fn label(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Directory => "inspected",
            Self::Rejected => "rejected",
        }
    }
}

fn inspect_ancestry(root: &Path, relative: &str, issues: &mut Vec<Value>) -> Result<TargetState> {
    let mut current = root.to_path_buf();
    for component in Path::new(relative).components() {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(TargetState::Missing);
            }
            Err(error) => return Err(format!("{}: {error}", current.display())),
        };
        if reparse(&metadata) || !metadata.is_dir() {
            issues.push(
                json!({ "kind": "invalid_ancestor", "path": relative_name(root, &current)? }),
            );
            return Ok(TargetState::Rejected);
        }
    }
    Ok(TargetState::Directory)
}

fn relative_name(root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned())
}

fn walk(
    root: &Path,
    directory: &Path,
    count: &mut u64,
    bytes: &mut u64,
    issues: &mut Vec<Value>,
) -> Result<()> {
    let mut paths: Vec<PathBuf> = fs::read_dir(directory)
        .map_err(|e| format!("{}: {e}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()).map_err(|e| e.to_string()))
        .collect::<Result<_>>()?;
    paths.sort();
    for path in paths {
        let metadata =
            fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let relative = relative_name(root, &path)?;
        if reparse(&metadata) {
            issues.push(json!({ "kind": "link", "path": relative }));
        } else if metadata.is_dir() {
            walk(root, &path, count, bytes, issues)?;
        } else if metadata.is_file() {
            *count = count.checked_add(1).ok_or("file count overflow")?;
            *bytes = bytes
                .checked_add(metadata.len())
                .ok_or("byte count overflow")?;
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            if !["lock", "bin", "rmeta", "o"]
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
            {
                issues.push(json!({ "kind": "unexpected_file", "path": relative }));
            }
            if metadata.permissions().readonly() {
                issues.push(json!({ "kind": "read_only_file", "path": relative }));
            }
        } else {
            issues.push(json!({ "kind": "special_file", "path": relative }));
        }
    }
    Ok(())
}

fn reparse(metadata: &fs::Metadata) -> bool {
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

#[cfg(test)]
#[path = "build_cache_inventory_tests.rs"]
mod tests;
