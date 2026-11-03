//! Execute a validated development plan without modifying the parent environment.

#[cfg(windows)]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

#[derive(Debug)]
struct TaskCommand {
    name: String,
    arguments: Vec<String>,
}

#[derive(Debug)]
struct ExecutionPlan {
    root: PathBuf,
    target: PathBuf,
    incremental: String,
    jobs: u64,
    commands: Vec<TaskCommand>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) completed: usize,
    pub(crate) failed_step: Option<String>,
    pub(crate) exit_code: i32,
}

fn is_managed_xtask(current: &Path, target: &Path) -> bool {
    let current: Vec<_> = current.components().map(|part| part.as_os_str()).collect();
    let target: Vec<_> = target.components().map(|part| part.as_os_str()).collect();
    if current.len() != target.len() + 2 && current.len() != target.len() + 3 {
        return false;
    }
    if !current.iter().zip(&target).all(|(left, right)| {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }) {
        return false;
    }
    let profile = current[current.len() - 2].to_string_lossy();
    let executable = current[current.len() - 1].to_string_lossy();
    (profile.eq_ignore_ascii_case("debug") || profile.eq_ignore_ascii_case("release"))
        && executable.eq_ignore_ascii_case("xtask.exe")
}

pub(crate) fn ensure_not_running_from_build_output(root: &Path) -> Result<(), String> {
    let current = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| format!("cannot inspect running development tool: {error}"))?;
    for target in [
        root.join("target"),
        root.join("crates").join("ai-ex-desktop").join("target"),
    ] {
        match fs::canonicalize(&target) {
            Ok(path) if is_managed_xtask(&current, &path) => {
                return Err("Run development tasks from a separate copy of xtask so Cargo can rebuild the original executable".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot inspect build output {}: {error}",
                    target.display()
                ));
            }
        }
    }
    Ok(())
}

fn parse_plan(root: &Path, plan: &Value) -> Result<ExecutionPlan, String> {
    if !root.is_absolute() {
        return Err("Repository path must be absolute".into());
    }
    let workspace = plan["workspace"].as_str().ok_or("Missing workspace")?;
    let expected = match workspace.to_ascii_lowercase().as_str() {
        "core" | "native" => root.join("target"),
        "desktop" => root.join("crates").join("ai-ex-desktop").join("target"),
        _ => return Err("Invalid workspace".into()),
    };
    let target = PathBuf::from(plan["target"].as_str().ok_or("Missing target")?);
    if target != expected {
        return Err("Target does not match workspace".into());
    }
    let incremental = plan["incremental"]
        .as_str()
        .ok_or("Missing incremental mode")?;
    if incremental.is_empty() || incremental.contains('\0') {
        return Err("Invalid incremental mode".into());
    }
    let jobs = plan["jobs"]
        .as_u64()
        .filter(|jobs| (1..=16).contains(jobs))
        .ok_or("Invalid jobs")?;
    let commands = plan["commands"]
        .as_array()
        .ok_or("Missing commands")?
        .iter()
        .map(|command| {
            if command["program"] != "cargo" {
                return Err("Invalid command program".into());
            }
            let arguments = command["arguments"]
                .as_array()
                .ok_or("Missing command arguments")?
                .iter()
                .map(|arg| {
                    arg.as_str()
                        .map(str::to_owned)
                        .ok_or("Invalid argument".to_owned())
                })
                .collect::<Result<Vec<_>, String>>()?;
            let name = arguments.first().ok_or("Missing task")?.to_owned();
            Ok(TaskCommand { name, arguments })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let names: Vec<_> = commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    if !matches!(
        names.as_slice(),
        ["check"] | ["build"] | ["test"] | ["clippy"] | ["fmt", "test", "clippy"]
    ) {
        return Err("Invalid task sequence".into());
    }
    Ok(ExecutionPlan {
        root: root.to_owned(),
        target: expected,
        incremental: incremental.into(),
        jobs,
        commands,
    })
}

/// Executes an already prepared plan. Creating this plan itself has no side effects.
pub(crate) fn run(root: &Path, report: &Value) -> Result<Outcome, String> {
    run_with(root, report, |plan, command| {
        let status = child_command(plan, command, std::ffi::OsStr::new("cargo"))
            .status()
            .map_err(|e| format!("cannot start Cargo: {e}"))?;
        status
            .code()
            .ok_or_else(|| "Cargo terminated without an exit code".into())
    })
}

fn child_command(
    plan: &ExecutionPlan,
    command: &TaskCommand,
    program: &std::ffi::OsStr,
) -> Command {
    let mut child = Command::new(program);
    child
        .args(&command.arguments)
        .current_dir(&plan.root)
        .env("CARGO_TARGET_DIR", &plan.target)
        .env("CARGO_INCREMENTAL", &plan.incremental)
        .env("CARGO_BUILD_JOBS", plan.jobs.to_string());
    child
}

fn run_with(
    root: &Path,
    report: &Value,
    mut execute: impl FnMut(&ExecutionPlan, &TaskCommand) -> Result<i32, String>,
) -> Result<Outcome, String> {
    let plan = parse_plan(root, report)?;
    if !cfg!(windows) {
        return Err("development execution currently requires Windows".into());
    }
    fs::create_dir_all(&plan.target).map_err(|e| format!("cannot create target directory: {e}"))?;
    let _lease = lock_target(&plan.target)?;
    let mut completed = 0;
    for command in &plan.commands {
        let exit_code = execute(&plan, command)?;
        if exit_code != 0 {
            return Ok(Outcome {
                completed,
                failed_step: Some(command.name.clone()),
                exit_code,
            });
        }
        completed += 1;
    }
    Ok(Outcome {
        completed,
        failed_step: None,
        exit_code: 0,
    })
}

#[cfg(windows)]
fn lock_target(target: &Path) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(target.join(".development.lock"))
        .map_err(|e| format!("development target is already in use or inaccessible: {e}"))
}

#[cfg(not(windows))]
fn lock_target(_target: &Path) -> Result<File, String> {
    Err("development execution currently requires Windows".into())
}

#[cfg(test)]
#[path = "dev_run_tests.rs"]
mod tests;
