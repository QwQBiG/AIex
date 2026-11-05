//! Coordinate release builds before constructing the portable archive.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::Value;

struct Options {
    project: PathBuf,
    output: PathBuf,
    jobs: u32,
    minimum_free_gib: u32,
    offline: bool,
    skip_build: bool,
}

fn parse(arguments: &[OsString]) -> Result<Options, String> {
    let mut project = None;
    let mut output = None;
    let mut jobs = 2;
    let mut minimum_free_gib = 10;
    let mut offline = false;
    let mut skip_build = false;
    let mut seen = BTreeSet::new();
    let mut index = 0;
    while index < arguments.len() {
        let option = arguments[index].to_str().ok_or("Invalid option name")?;
        if !seen.insert(option.to_owned()) {
            return Err(format!("Repeated option: {option}"));
        }
        index += 1;
        match option {
            "--offline" => offline = true,
            "--skip-build" => skip_build = true,
            "--project" | "--output" | "--jobs" | "--minimum-free-gib" => {
                let value = arguments
                    .get(index)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| format!("Missing value: {option}"))?;
                index += 1;
                match option {
                    "--project" => project = Some(PathBuf::from(value)),
                    "--output" => output = Some(PathBuf::from(value)),
                    "--jobs" => {
                        jobs = value
                            .to_str()
                            .ok_or("Invalid jobs")?
                            .parse()
                            .map_err(|_| "Invalid jobs")?
                    }
                    "--minimum-free-gib" => {
                        minimum_free_gib = value
                            .to_str()
                            .ok_or("Invalid minimum free space")?
                            .parse()
                            .map_err(|_| "Invalid minimum free space")?
                    }
                    _ => unreachable!(),
                }
            }
            _ => return Err(format!("Unknown option: {option}")),
        }
    }
    if !(1..=16).contains(&jobs) || !(1..=1_048_576).contains(&minimum_free_gib) {
        return Err("Invalid jobs or minimum free space".into());
    }
    Ok(Options {
        project: project.ok_or("Missing --project")?,
        output: output.ok_or("Missing --output")?,
        jobs,
        minimum_free_gib,
        offline,
        skip_build,
    })
}

pub(crate) fn execute(arguments: &[OsString]) -> Result<(Value, bool), String> {
    if !cfg!(windows) {
        return Err("Portable packaging requires Windows".into());
    }
    let options = parse(arguments)?;
    orchestrate(
        options.skip_build,
        || {
            crate::portable::preflight(&options.project, &options.output, options.offline)
                .map(|_| ())
        },
        |workspace| build(&options, workspace),
        || crate::portable::package(&options.project, &options.output, options.offline),
    )
}

fn orchestrate<T>(
    skip_build: bool,
    preflight: impl FnOnce() -> Result<(), String>,
    mut build: impl FnMut(&str) -> Result<(), String>,
    package: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    preflight()?;
    if !skip_build {
        build("core")?;
        build("desktop")?;
    }
    package()
}

fn build(options: &Options, workspace: &str) -> Result<(), String> {
    let mut args: Vec<OsString> = vec![
        "--project".into(),
        options.project.as_os_str().into(),
        "--workspace".into(),
        workspace.into(),
        "--task".into(),
        "build".into(),
        "--jobs".into(),
        options.jobs.to_string().into(),
        "--minimum-free-gib".into(),
        options.minimum_free_gib.to_string().into(),
        "--release".into(),
    ];
    if workspace == "core" {
        for name in ["ai-ex-service", "ai-ex-migrate", "ai-ex-platform-windows"] {
            args.extend(["--package".into(), name.into()]);
        }
    }
    if options.offline {
        args.push("--offline".into());
    }
    let (root, plan) = crate::dev_plan::prepare(&args)?;
    let result = crate::dev_run::run(&root, &plan)?;
    if result.exit_code != 0 {
        return Err(format!(
            "{workspace} release build failed with exit code {}",
            result.exit_code
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "portable_build_tests.rs"]
mod tests;
