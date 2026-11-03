//! Pure command planning for the three development workspaces.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Workspace {
    Core,
    Desktop,
    Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Task {
    Check,
    Build,
    Test,
    Clippy,
    Verify,
}

impl Workspace {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "core" => Ok(Self::Core),
            "desktop" => Ok(Self::Desktop),
            "native" => Ok(Self::Native),
            _ => Err("Workspace must be core, desktop or native".into()),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Desktop => "desktop",
            Self::Native => "native",
        }
    }
}

impl Task {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "check" => Ok(Self::Check),
            "build" => Ok(Self::Build),
            "test" => Ok(Self::Test),
            "clippy" => Ok(Self::Clippy),
            "verify" => Ok(Self::Verify),
            _ => Err("Task must be check, build, test, clippy or verify".into()),
        }
    }
}

pub(crate) struct Options {
    pub(crate) workspace: Workspace,
    // Preserve the caller's validated spelling in the report, as PowerShell does.
    pub(crate) workspace_label: Option<String>,
    pub(crate) task: Task,
    pub(crate) packages: Vec<String>,
    pub(crate) test_filter: Option<String>,
    pub(crate) jobs: u32,
    pub(crate) minimum_free_gib: u32,
    pub(crate) keep_incremental: bool,
    pub(crate) release: bool,
    pub(crate) offline: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            workspace: Workspace::Core,
            workspace_label: None,
            task: Task::Check,
            packages: Vec::new(),
            test_filter: None,
            jobs: 2,
            minimum_free_gib: 10,
            keep_incremental: false,
            release: false,
            offline: false,
        }
    }
}

pub(crate) fn parse_args(arguments: &[OsString]) -> Result<(PathBuf, Options), String> {
    let mut result = Options::default();
    let mut project = None;
    let mut seen = BTreeSet::new();
    let mut index = 0;
    while index < arguments.len() {
        let option = arguments[index]
            .to_str()
            .ok_or("Option names must be Unicode")?;
        let flag = matches!(option, "--release" | "--offline" | "--keep-incremental");
        let valued = matches!(
            option,
            "--project"
                | "--workspace"
                | "--task"
                | "--package"
                | "--test-filter"
                | "--jobs"
                | "--minimum-free-gib"
        );
        if !flag && !valued {
            return Err(format!("Unknown option: {option}"));
        }
        if option != "--package" && !seen.insert(option.to_owned()) {
            return Err(format!("Repeated option: {option}"));
        }
        index += 1;
        if flag {
            match option {
                "--release" => result.release = true,
                "--offline" => result.offline = true,
                "--keep-incremental" => result.keep_incremental = true,
                _ => unreachable!(),
            }
            continue;
        }
        let value = arguments
            .get(index)
            .filter(|value| !value.is_empty() && !value.to_string_lossy().starts_with("--"))
            .ok_or_else(|| format!("Missing value for {option}"))?;
        index += 1;
        if option == "--project" {
            project = Some(PathBuf::from(value.as_os_str()));
            continue;
        }
        let text = value
            .to_str()
            .ok_or_else(|| format!("Value for {option} must be Unicode"))?;
        match option {
            "--workspace" => {
                result.workspace = Workspace::parse(text)?;
                result.workspace_label = Some(text.to_owned());
            }
            "--task" => result.task = Task::parse(text)?,
            "--package" => result.packages.push(text.to_owned()),
            "--test-filter" => result.test_filter = Some(text.to_owned()),
            "--jobs" => result.jobs = text.parse().map_err(|_| "Invalid jobs value")?,
            "--minimum-free-gib" => {
                result.minimum_free_gib = text.parse().map_err(|_| "Invalid minimum free space")?
            }
            _ => unreachable!(),
        }
    }
    let project = project.ok_or("Missing value for --project")?;
    validate_options(&result)?;
    Ok((project, result))
}

fn validate_options(options: &Options) -> Result<(), String> {
    if !(1..=16).contains(&options.jobs) || !(1..=1_048_576).contains(&options.minimum_free_gib) {
        return Err("Invalid jobs or minimum free space".into());
    }
    for name in &options.packages {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err("Invalid package name".into());
        }
    }
    if let Some(filter) = &options.test_filter
        && (options.task != Task::Test
            || filter.trim().is_empty()
            || filter.starts_with('-')
            || filter.chars().any(char::is_control))
    {
        return Err("TestFilter requires Task=test and a nonempty test name without control characters or a leading dash".into());
    }
    Ok(())
}

fn target_path(repository: &Path, workspace: Workspace) -> PathBuf {
    match workspace {
        Workspace::Desktop => repository
            .join("crates")
            .join("ai-ex-desktop")
            .join("target"),
        Workspace::Core | Workspace::Native => repository.join("target"),
    }
}

fn manifest_path(repository: &Path, workspace: Workspace) -> PathBuf {
    match workspace {
        Workspace::Core => repository.join("Cargo.toml"),
        Workspace::Desktop => repository
            .join("crates")
            .join("ai-ex-desktop")
            .join("Cargo.toml"),
        Workspace::Native => repository.join("native-workers").join("Cargo.toml"),
    }
}

fn nearest_directory(path: &Path) -> Result<PathBuf, String> {
    let mut candidate = path;
    loop {
        match std::fs::symlink_metadata(candidate) {
            Ok(_) => {
                let metadata = std::fs::metadata(candidate)
                    .map_err(|e| format!("Cannot inspect {}: {e}", candidate.display()))?;
                if !metadata.is_dir() {
                    return Err(format!("Not a directory: {}", candidate.display()));
                }
                return Ok(candidate.to_path_buf());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or("No existing target ancestor")?;
            }
            Err(e) => return Err(format!("Cannot inspect {}: {e}", candidate.display())),
        }
    }
}

fn prepare_with<F>(
    arguments: &[OsString],
    disk_space: F,
    inherited_incremental: Option<&str>,
) -> Result<(PathBuf, Value), String>
where
    F: FnOnce(&Path) -> std::io::Result<u64>,
{
    let (project, options) = parse_args(arguments)?;
    let root =
        std::path::absolute(&project).map_err(|e| format!("Cannot resolve project path: {e}"))?;
    if !root.is_dir() || !root.join("Cargo.toml").is_file() {
        return Err("Project must be an existing directory containing Cargo.toml".into());
    }
    let ancestor = nearest_directory(&target_path(&root, options.workspace))?;
    let free_bytes = disk_space(&ancestor)
        .map_err(|e| format!("Cannot query disk space for {}: {e}", ancestor.display()))?;
    let report = plan(&root, &options, free_bytes, inherited_incremental)?;
    Ok((root, report))
}

pub(crate) fn prepare(arguments: &[OsString]) -> Result<(PathBuf, Value), String> {
    let incremental = match std::env::var("CARGO_INCREMENTAL") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("CARGO_INCREMENTAL is not Unicode".into());
        }
    };
    prepare_with(
        arguments,
        ai_ex_platform_windows::available_disk_space,
        incremental.as_deref(),
    )
}

pub(crate) fn plan(
    repository: &Path,
    options: &Options,
    free_bytes: u64,
    inherited_incremental: Option<&str>,
) -> Result<Value, String> {
    if !repository.is_absolute() {
        return Err("Repository path must be absolute".into());
    }
    let workspace_label = match &options.workspace_label {
        Some(label) if Workspace::parse(label)? == options.workspace => label.as_str(),
        Some(_) => return Err("Workspace label does not match workspace".into()),
        None => options.workspace.name(),
    };
    validate_options(options)?;
    let minimum_free_bytes = u64::from(options.minimum_free_gib) * 1_073_741_824;
    if free_bytes < minimum_free_bytes {
        return Err(format!(
            "Insufficient free disk space: {free_bytes} bytes available, {minimum_free_bytes} required. Inspect tools/manage_build_cache.ps1 first."
        ));
    }
    let manifest = manifest_path(repository, options.workspace);
    let target = target_path(repository, options.workspace);
    let manifest = manifest.to_str().ok_or("Manifest path is not Unicode")?;
    let incremental = if options.keep_incremental {
        "1"
    } else if options.release || matches!(options.task, Task::Test | Task::Clippy | Task::Verify) {
        "0"
    } else {
        inherited_incremental
            .filter(|value| !value.is_empty())
            .unwrap_or("1")
    };
    let mut base = vec![
        "--manifest-path".to_owned(),
        manifest.to_owned(),
        "--all-features".into(),
        "--locked".into(),
    ];
    if options.packages.is_empty() {
        base.push("--workspace".into());
    } else {
        for name in &options.packages {
            base.extend(["--package".into(), name.clone()]);
        }
    }
    if options.release {
        base.push("--release".into());
    }
    if options.offline {
        base.push("--offline".into());
    }
    let steps: &[&str] = match options.task {
        Task::Check => &["check"],
        Task::Build => &["build"],
        Task::Test => &["test"],
        Task::Clippy => &["clippy"],
        Task::Verify => &["fmt", "test", "clippy"],
    };
    let commands: Vec<_> = steps
        .iter()
        .map(|step| {
            let mut args: Vec<String> = if *step == "fmt" {
                ["fmt", "--manifest-path", manifest, "--all", "--", "--check"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            } else {
                let mut args = vec![(*step).to_owned()];
                args.extend(base.clone());
                if *step == "clippy" {
                    args.extend(
                        ["--all-targets", "--", "-D", "warnings"]
                            .into_iter()
                            .map(str::to_owned),
                    );
                }
                args
            };
            if *step == "test"
                && let Some(filter) = &options.test_filter
            {
                args.extend(["--".into(), filter.clone()]);
            }
            json!({"program": "cargo", "arguments": args})
        })
        .collect();
    Ok(
        json!({"workspace": workspace_label, "target": target, "free_bytes": free_bytes, "minimum_free_bytes": minimum_free_bytes, "incremental": incremental, "jobs": options.jobs, "commands": commands}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"D:\项目 with spaces")
        } else {
            PathBuf::from("/tmp/项目 with spaces")
        }
    }

    fn options(task: Task, workspace: Workspace) -> Options {
        Options {
            task,
            workspace,
            ..Options::default()
        }
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(|value| OsString::from(*value)).collect()
    }

    fn fixture_project(with_manifest: bool) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aiex-dev-plan-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        if with_manifest {
            std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        }
        root
    }

    fn project_args(root: &Path, workspace: &str) -> Vec<OsString> {
        vec![
            "--project".into(),
            root.as_os_str().to_owned(),
            "--workspace".into(),
            workspace.into(),
        ]
    }

    #[test]
    fn preparation_queries_nearest_existing_target_without_creating_it() {
        let root = fixture_project(true);
        let crates = root.join("crates");
        let desktop = crates.join("ai-ex-desktop");
        std::fs::create_dir(&crates).unwrap();
        std::fs::create_dir(&desktop).unwrap();
        let (resolved, report) = prepare_with(
            &project_args(&root, "Desktop"),
            |directory| {
                assert_eq!(directory, desktop.as_path());
                Ok(40 * 1_073_741_824)
            },
            Some("0"),
        )
        .unwrap();
        assert_eq!(resolved, root);
        assert_eq!(report["workspace"], "Desktop");
        assert_eq!(report["incremental"], "0");
        assert_eq!(report["target"], desktop.join("target").to_str().unwrap());
        assert!(!desktop.join("target").exists());

        prepare_with(
            &project_args(&root, "native"),
            |directory| {
                assert_eq!(directory, root.as_path());
                Ok(40 * 1_073_741_824)
            },
            None,
        )
        .unwrap();
        let target = root.join("target");
        assert!(!target.exists());
        std::fs::create_dir(&target).unwrap();
        prepare_with(
            &project_args(&root, "core"),
            |directory| {
                assert_eq!(directory, target.as_path());
                Ok(40 * 1_073_741_824)
            },
            None,
        )
        .unwrap();
        assert!(
            prepare_with(&project_args(&root, "core"), |_| Ok(1), None)
                .unwrap_err()
                .contains("Insufficient")
        );
        assert!(
            prepare_with(
                &project_args(&root, "core"),
                |_| Err(std::io::Error::other("query failed")),
                None
            )
            .unwrap_err()
            .contains("query failed")
        );
        std::fs::remove_dir(&target).unwrap();
        std::fs::write(&target, "not a directory").unwrap();
        assert!(
            prepare_with(
                &project_args(&root, "core"),
                |_| panic!("must not query"),
                None
            )
            .is_err()
        );
        std::fs::remove_file(&target).unwrap();
        std::fs::remove_dir(desktop).unwrap();
        std::fs::remove_dir(crates).unwrap();
        std::fs::remove_file(root.join("Cargo.toml")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn preparation_rejects_missing_manifest_without_creating_target() {
        let root = fixture_project(false);
        let result = prepare_with(
            &project_args(&root, "core"),
            |_| panic!("must not query"),
            None,
        );
        assert!(result.unwrap_err().contains("Cargo.toml"));
        assert!(!root.join("target").exists());
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn parses_full_command_and_preserves_unicode_arguments() {
        let values = args(&[
            "--project",
            "项目 with spaces",
            "--workspace",
            "NATIVE",
            "--task",
            "TEST",
            "--package",
            "ai-ex-domain",
            "--package",
            "ai-ex-core",
            "--test-filter",
            "中文 过滤",
            "--jobs",
            "3",
            "--minimum-free-gib",
            "2",
            "--release",
            "--offline",
            "--keep-incremental",
        ]);
        let (project, parsed) = parse_args(&values).unwrap();
        assert_eq!(project, PathBuf::from("项目 with spaces"));
        assert_eq!(parsed.workspace, Workspace::Native);
        assert_eq!(parsed.workspace_label.as_deref(), Some("NATIVE"));
        assert_eq!(parsed.task, Task::Test);
        assert_eq!(
            parsed.packages,
            vec!["ai-ex-domain".to_owned(), "ai-ex-core".to_owned()]
        );
        assert_eq!(parsed.test_filter.as_deref(), Some("中文 过滤"));
        assert_eq!(parsed.jobs, 3);
        assert_eq!(parsed.minimum_free_gib, 2);
        assert!(parsed.release && parsed.offline && parsed.keep_incremental);
    }

    #[test]
    fn rejects_missing_unknown_or_duplicate_options() {
        for input in [
            vec![],
            vec!["--project"],
            vec!["--project", ""],
            vec!["--project", ".", "--task", "--offline"],
            vec!["--project", ".", "--unknown"],
            vec!["--project", ".", "--project", "other"],
            vec![
                "--project",
                ".",
                "--workspace",
                "core",
                "--workspace",
                "native",
            ],
            vec!["--project", ".", "--offline", "--offline"],
            vec!["--project", ".", "--jobs", "2", "--jobs", "3"],
        ] {
            assert!(parse_args(&args(&input)).is_err(), "{input:?}");
        }
    }

    #[test]
    fn rejects_invalid_values_without_running_the_plan() {
        for input in [
            vec!["--project", ".", "--jobs", "0"],
            vec!["--project", ".", "--jobs", "17"],
            vec!["--project", ".", "--jobs", "oops"],
            vec!["--project", ".", "--minimum-free-gib", "0"],
            vec!["--project", ".", "--workspace", "other"],
            vec!["--project", ".", "--package", "bad package"],
            vec!["--project", ".", "--test-filter", "test"],
            vec!["--project", ".", "--task", "test", "--test-filter", "-x"],
        ] {
            assert!(parse_args(&args(&input)).is_err(), "{input:?}");
        }
    }

    #[test]
    fn defaults_and_workspace_targets_match_development_entry() {
        let root = root();
        for (workspace, target, manifest) in [
            (Workspace::Core, "target", "Cargo.toml"),
            (Workspace::Native, "target", "native-workers/Cargo.toml"),
            (
                Workspace::Desktop,
                "crates/ai-ex-desktop/target",
                "crates/ai-ex-desktop/Cargo.toml",
            ),
        ] {
            let report = plan(
                &root,
                &options(Task::Check, workspace),
                11 * 1_073_741_824,
                None,
            )
            .unwrap();
            let target = target
                .split('/')
                .fold(root.clone(), |path, component| path.join(component));
            let manifest = manifest
                .split('/')
                .fold(root.clone(), |path, component| path.join(component));
            assert_eq!(report["target"], target.to_str().unwrap());
            assert_eq!(report["jobs"], 2);
            assert_eq!(report["minimum_free_bytes"], 10 * 1_073_741_824_u64);
            let args = report["commands"][0]["arguments"].as_array().unwrap();
            assert_eq!(
                args.as_slice(),
                &[
                    json!("check"),
                    json!("--manifest-path"),
                    json!(manifest),
                    json!("--all-features"),
                    json!("--locked"),
                    json!("--workspace")
                ]
            );
        }
    }

    #[test]
    fn unicode_filter_and_multiple_packages_remain_separate_arguments() {
        let mut settings = options(Task::Test, Workspace::Native);
        settings.packages = vec!["ai-ex-domain".into(), "ai-ex-core".into()];
        settings.test_filter = Some("中文 测试 with spaces".into());
        settings.offline = true;
        settings.release = true;
        settings.jobs = 3;
        let report = plan(&root(), &settings, u64::MAX, Some("1")).unwrap();
        let args = report["commands"][0]["arguments"].as_array().unwrap();
        assert_eq!(
            &args[5..],
            &[
                json!("--package"),
                json!("ai-ex-domain"),
                json!("--package"),
                json!("ai-ex-core"),
                json!("--release"),
                json!("--offline"),
                json!("--"),
                json!("中文 测试 with spaces")
            ]
        );
        assert_eq!(report["incremental"], "0");
        assert_eq!(report["jobs"], 3);
    }

    #[test]
    fn verify_orders_fmt_test_clippy_and_preserves_flags() {
        let mut settings = options(Task::Verify, Workspace::Desktop);
        settings.packages = vec!["ai-ex-desktop".into()];
        settings.offline = true;
        settings.release = true;
        let report = plan(&root(), &settings, u64::MAX, None).unwrap();
        let commands = report["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 3);
        let fmt = commands[0]["arguments"].as_array().unwrap();
        assert_eq!(fmt[0], "fmt");
        assert!(!fmt.contains(&json!("--package")));
        assert!(!fmt.contains(&json!("--release")));
        let tests = commands[1]["arguments"].as_array().unwrap();
        assert_eq!(tests[0], "test");
        assert!(tests.contains(&json!("--offline")));
        let clippy = commands[2]["arguments"].as_array().unwrap();
        assert_eq!(clippy[0], "clippy");
        assert_eq!(
            &clippy[clippy.len() - 4..],
            &[
                json!("--all-targets"),
                json!("--"),
                json!("-D"),
                json!("warnings")
            ]
        );
    }

    #[test]
    fn incremental_override_and_inherited_values_follow_existing_rules() {
        let root = root();
        let mut settings = options(Task::Build, Workspace::Core);
        assert_eq!(
            plan(&root, &settings, u64::MAX, Some("0")).unwrap()["incremental"],
            "0"
        );
        assert_eq!(
            plan(&root, &settings, u64::MAX, Some("")).unwrap()["incremental"],
            "1"
        );
        settings.release = true;
        assert_eq!(
            plan(&root, &settings, u64::MAX, Some("1")).unwrap()["incremental"],
            "0"
        );
        settings.keep_incremental = true;
        assert_eq!(
            plan(&root, &settings, u64::MAX, Some("0")).unwrap()["incremental"],
            "1"
        );
    }

    #[test]
    fn rejects_invalid_filters_packages_and_insufficient_space() {
        let root = root();
        for filter in ["", "  ", "--nocapture", "line\nother", "foo\u{7f}"] {
            let mut settings = options(Task::Test, Workspace::Core);
            settings.test_filter = Some(filter.into());
            assert!(
                plan(&root, &settings, u64::MAX, None).is_err(),
                "{filter:?}"
            );
        }
        let mut settings = options(Task::Check, Workspace::Core);
        settings.test_filter = Some("test".into());
        assert!(plan(&root, &settings, u64::MAX, None).is_err());
        settings.test_filter = None;
        settings.packages = vec!["not a package".into()];
        assert!(plan(&root, &settings, u64::MAX, None).is_err());
        settings.packages.clear();
        assert!(plan(&root, &settings, 10 * 1_073_741_824 - 1, None).is_err());
        assert!(plan(&root, &settings, 10 * 1_073_741_824, None).is_ok());
        settings.jobs = 0;
        assert!(plan(&root, &settings, u64::MAX, None).is_err());
        assert!(plan(Path::new("relative"), &Options::default(), u64::MAX, None).is_err());
        assert_eq!(Workspace::parse("NATIVE").unwrap(), Workspace::Native);
        assert_eq!(Task::parse("VERIFY").unwrap(), Task::Verify);
        let mut uppercase = options(Task::Check, Workspace::Native);
        uppercase.workspace_label = Some("NATIVE".into());
        assert_eq!(
            plan(&root, &uppercase, u64::MAX, None).unwrap()["workspace"],
            "NATIVE"
        );
    }
}
