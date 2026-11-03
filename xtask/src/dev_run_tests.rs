use super::*;
use crate::dev_plan::{Options, Task, Workspace};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
#[test]
fn identifies_managed_debug_release_and_target_triple_executables() {
    let target = Path::new(r"D:\项目 with spaces\target");
    for executable in [
        r"D:\项目 with spaces\target\debug\xtask.exe",
        r"d:\项目 with spaces\TARGET\release\XTASK.EXE",
        r"D:\项目 with spaces\target\x86_64-pc-windows-gnu\debug\xtask.exe",
        r"D:\项目 with spaces\target\x86_64-pc-windows-msvc\release\xtask.exe",
    ] {
        assert!(
            is_managed_xtask(Path::new(executable), target),
            "{executable}"
        );
    }
    for executable in [
        r"D:\项目 with spaces\target\xtask-dev-copy.exe",
        r"D:\项目 with spaces\target\debug\other.exe",
        r"D:\项目 with spaces\target\different\nested\debug\xtask.exe",
        r"D:\项目 with spaces\target-other\debug\xtask.exe",
        r"D:\other\target\debug\xtask.exe",
    ] {
        assert!(
            !is_managed_xtask(Path::new(executable), target),
            "{executable}"
        );
    }
    let desktop = Path::new(r"D:\项目 with spaces\crates\ai-ex-desktop\target");
    let worker = Path::new(
        r"D:\项目 with spaces\crates\ai-ex-desktop\target\x86_64-pc-windows-gnu\debug\xtask.exe",
    );
    assert!(is_managed_xtask(worker, desktop));
}

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "aiex-dev-run-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    root
}

fn report(root: &Path, task: Task) -> Value {
    let options = Options {
        task,
        workspace: Workspace::Core,
        ..Options::default()
    };
    crate::dev_plan::plan(root, &options, u64::MAX, None).unwrap()
}

fn clean(root: &Path) {
    let target = root.join("target");
    fs::remove_file(target.join(".development.lock")).unwrap();
    fs::remove_dir(&target).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn validates_targets_and_task_order_without_creating_files() {
    let root = fixture();
    let mut plan = report(&root, Task::Verify);
    assert!(!root.join("target").exists());
    assert!(parse_plan(&root, &plan).is_ok());
    plan["target"] = "elsewhere".into();
    assert!(parse_plan(&root, &plan).is_err());
    let mut plan = report(&root, Task::Verify);
    plan["commands"].as_array_mut().unwrap().swap(0, 1);
    assert!(parse_plan(&root, &plan).is_err());
    assert!(!root.join("target").exists());
    fs::remove_dir(root).unwrap();
}

#[test]
fn preserves_workspace_label_and_inherited_incremental_mode() {
    let root = fixture();
    let options = Options {
        task: Task::Check,
        workspace: Workspace::Native,
        workspace_label: Some("NATIVE".into()),
        ..Options::default()
    };
    let report = crate::dev_plan::plan(&root, &options, u64::MAX, Some("custom-value")).unwrap();
    let execution = parse_plan(&root, &report).unwrap();
    assert_eq!(execution.target, root.join("target"));
    assert_eq!(execution.incremental, "custom-value");
    assert!(!root.join("target").exists());
    fs::remove_dir(root).unwrap();
}

#[test]
fn desktop_target_uses_native_path_components() {
    let root = fixture();
    let options = Options {
        workspace: Workspace::Desktop,
        ..Options::default()
    };
    let report = crate::dev_plan::plan(&root, &options, u64::MAX, None).unwrap();
    let execution = parse_plan(&root, &report).unwrap();
    let expected = root.join("crates").join("ai-ex-desktop").join("target");
    assert_eq!(execution.target.as_os_str(), expected.as_os_str());
    assert!(!root.join("crates").exists());
    fs::remove_dir(root).unwrap();
}

#[cfg(windows)]
#[test]
fn executes_in_order_and_stops_on_failure() {
    let root = fixture();
    let plan = report(&root, Task::Verify);
    let before_dir = std::env::current_dir().unwrap();
    let before_env: Vec<_> = ["CARGO_TARGET_DIR", "CARGO_INCREMENTAL", "CARGO_BUILD_JOBS"]
        .iter()
        .map(std::env::var_os)
        .collect();
    let mut calls = Vec::new();
    let outcome = run_with(&root, &plan, |execution, command| {
        assert_eq!(execution.root, root);
        assert_eq!(execution.target, root.join("target"));
        assert_eq!(execution.incremental, "0");
        assert_eq!(execution.jobs, 2);
        calls.push(command.name.clone());
        Ok(if command.name == "test" { 42 } else { 0 })
    })
    .unwrap();
    assert_eq!(calls, ["fmt", "test"]);
    assert_eq!(
        outcome,
        Outcome {
            completed: 1,
            failed_step: Some("test".into()),
            exit_code: 42
        }
    );
    assert_eq!(std::env::current_dir().unwrap(), before_dir);
    let after_env: Vec<_> = ["CARGO_TARGET_DIR", "CARGO_INCREMENTAL", "CARGO_BUILD_JOBS"]
        .iter()
        .map(std::env::var_os)
        .collect();
    assert_eq!(before_env, after_env);
    calls.clear();
    let outcome = run_with(&root, &plan, |_, command| {
        calls.push(command.name.clone());
        Ok(0)
    })
    .unwrap();
    assert_eq!(calls, ["fmt", "test", "clippy"]);
    assert_eq!(
        outcome,
        Outcome {
            completed: 3,
            failed_step: None,
            exit_code: 0
        }
    );
    clean(&root);
}

#[cfg(windows)]
#[test]
fn releases_lock_after_executor_error() {
    let root = fixture();
    let plan = report(&root, Task::Check);
    assert!(run_with(&root, &plan, |_, _| Err("synthetic failure".into())).is_err());
    assert!(lock_target(&root.join("target")).is_ok());
    clean(&root);
}

#[cfg(windows)]
#[test]
fn child_probes_development_lock() {
    let Some(target) = std::env::var_os("AIEX_DEV_LOCK_TEST_TARGET") else {
        return;
    };
    let expect_available = std::env::var_os("AIEX_DEV_LOCK_TEST_AVAILABLE").unwrap() == "1";
    assert_eq!(lock_target(Path::new(&target)).is_ok(), expect_available);
}

#[cfg(windows)]
#[test]
fn child_checks_development_environment() {
    let Some(root) = std::env::var_os("AIEX_DEV_ENV_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert_eq!(std::env::current_dir().unwrap(), root);
    assert_eq!(
        PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").unwrap()),
        root.join("target")
    );
    assert_eq!(std::env::var("CARGO_INCREMENTAL").unwrap(), "0");
    assert_eq!(std::env::var("CARGO_BUILD_JOBS").unwrap(), "2");
}

#[cfg(windows)]
#[test]
fn child_receives_isolated_working_directory_and_environment() {
    let root = fixture();
    let execution = parse_plan(&root, &report(&root, Task::Test)).unwrap();
    let fake_command = TaskCommand {
        name: "test".into(),
        arguments: vec![
            "--exact".into(),
            "dev_run::tests::child_checks_development_environment".into(),
            "--nocapture".into(),
        ],
    };
    let exe = std::env::current_exe().unwrap();
    let output = child_command(&execution, &fake_command, exe.as_os_str())
        .env("AIEX_DEV_ENV_TEST_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert!(!root.join("target").exists());
    fs::remove_dir(root).unwrap();
}

#[cfg(windows)]
#[test]
fn lock_is_exclusive_across_processes_and_releases() {
    let root = fixture();
    let target = root.join("target");
    fs::create_dir(&target).unwrap();
    let lease = lock_target(&target).unwrap();
    let probe = |available: &str| {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dev_run::tests::child_probes_development_lock",
                "--nocapture",
            ])
            .env("AIEX_DEV_LOCK_TEST_TARGET", &target)
            .env("AIEX_DEV_LOCK_TEST_AVAILABLE", available)
            .output()
            .unwrap()
    };
    let blocked = probe("0");
    assert!(
        blocked.status.success(),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    assert!(String::from_utf8_lossy(&blocked.stdout).contains("1 passed"));
    drop(lease);
    let available = probe("1");
    assert!(
        available.status.success(),
        "{}",
        String::from_utf8_lossy(&available.stderr)
    );
    assert!(String::from_utf8_lossy(&available.stdout).contains("1 passed"));
    clean(&root);
}
