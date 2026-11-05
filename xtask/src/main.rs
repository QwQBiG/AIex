#![forbid(unsafe_code)]

mod architecture;
mod build_cache_inventory;
mod bundle;
mod bundle_io;
#[cfg(test)]
mod bundle_tests;
mod bundle_zip;
mod catalog_build;
mod control_token;
mod dev_plan;
mod dev_run;
mod native;
mod pe_imports;
mod portable;
mod portable_build;
mod portable_verify;
mod portable_verify_archive;
mod python_gate;
mod runtime_python_gate;
mod sources_build;
mod spec;
mod wav;

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let [command, options @ ..] = arguments.as_slice()
        && command == "dev"
    {
        return run_development(options);
    }
    let result = match arguments.as_slice() {
        [command] if command == "architecture" => architecture::check(false),
        [command, probe] if command == "architecture" && probe == "--probe-violation" => {
            architecture::check(true)
        }
        [command, options @ ..] if command == "cache-inventory" => {
            command_paths(options, &["--project"])
                .and_then(|paths| build_cache_inventory::inspect(&paths[0]))
        }
        [command, options @ ..] if command == "control-token" => {
            let force_count = options.iter().filter(|option| *option == "--force").count();
            if force_count > 1 {
                Err("repeated option: --force".into())
            } else {
                let paths: Vec<_> = options.iter().filter(|option| *option != "--force").cloned().collect();
                command_paths(&paths, &["--path"])
                    .and_then(|paths| control_token::create(&paths[0], force_count == 1))
            }
        }
        [command, options @ ..] if command == "dev-plan" => {
            dev_plan::prepare(options).map(|(_, report)| (report, true))
        }
        [command, options @ ..] if command == "portable-build" => {
            portable_build::execute(options)
        }
        [command, options @ ..] if command == "portable" => {
            let offline_count = options.iter().filter(|option| *option == "--offline").count();
            if offline_count > 1 {
                Err("repeated option: --offline".into())
            } else {
                let paths: Vec<_> = options.iter().filter(|option| *option != "--offline").cloned().collect();
                command_paths(&paths, &["--project", "--output"])
                    .and_then(|paths| portable::package(&paths[0], &paths[1], offline_count == 1))
            }
        }
        [command, manifest, directory] if command == "native-verify" => {
            native::verify(Path::new(manifest), Path::new(directory))
        }
        [command, specification] if command == "native-probe" => {
            native::probe(Path::new(specification)).await
        }
        [command, archive] if command == "portable-inspect" => {
            portable_verify_archive::inspect_path(Path::new(archive))
        }
        [command, options @ ..] if command == "portable-verify" => {
            command_paths(options, &["--archive", "--evidence-directory"])
                .and_then(|paths| portable_verify::verify(&paths[0], &paths[1]))
        }
        [command, options @ ..] if command == "native-bundle" => {
            command_paths(options, &["--spec", "--output", "--manifest"])
                .and_then(|paths| bundle::build(&paths[0], &paths[1], &paths[2]))
        }
        [command, options @ ..] if command == "native-catalog" => {
            command_paths(options, &["--distribution", "--runtime-manifest", "--runtime-zip", "--output"])
                .and_then(|paths| catalog_build::build(&paths[0], &paths[1], &paths[2], &paths[3]))
        }
        [command, options @ ..] if command == "native-sources" => {
            command_paths(options, &["--spec", "--output"])
                .and_then(|paths| sources_build::build(&paths[0], &paths[1]))
        }
        [command, options @ ..] if command == "zero-python" => {
            command_paths(options, &["--project", "--distribution"])
                .and_then(|paths| python_gate::check(&paths[0], &paths[1]))
        }
        [command, options @ ..] if command == "zero-python-runtime" => {
            command_paths(options, &["--catalog", "--archive"])
                .and_then(|paths| runtime_python_gate::check(&paths[0], &paths[1]))
        }
        _ => Err("usage: cargo xtask architecture [--probe-violation] | dev-plan --project <directory> [options] | dev --project <directory> [options] | cache-inventory --project <directory> | control-token --path <file> [--force] | portable-inspect <archive.zip> | portable-verify --archive <zip> --evidence-directory <directory> | portable --project <directory> --output <directory> [--offline] | portable-build --project <directory> --output <directory> [--offline] [--skip-build] [--jobs N] [--minimum-free-gib N] | zero-python --project <directory> --distribution <extracted-directory> | zero-python-runtime --catalog <json> --archive <zip> | native-verify <manifest.json> <directory> | native-probe <spec.json> | native-bundle --spec <input.json> --output <new.zip> --manifest <new.json> | native-catalog --distribution <directory> --runtime-manifest <json> --runtime-zip <zip> --output <new.json> | native-sources --spec <input.json> --output <new.zip>".into()),
    };
    let (report, passed) = match result {
        Ok(result) => result,
        Err(error) => (json!({"status": "failed", "error": error}), false),
    };
    match serde_json::to_string_pretty(&report) {
        Ok(report) => println!("{report}"),
        Err(error) => {
            eprintln!("cannot serialize verification report: {error}");
            return ExitCode::FAILURE;
        }
    }
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_development(options: &[OsString]) -> ExitCode {
    let result = dev_plan::prepare(options).and_then(|(root, report)| {
        dev_run::ensure_not_running_from_build_output(&root)?;
        dev_run::run(&root, &report)
    });
    let (report, exit_code) = match result {
        Ok(outcome) => (
            json!({"status": if outcome.exit_code == 0 { "passed" } else { "failed" }, "completed": outcome.completed, "failed_step": outcome.failed_step, "exit_code": outcome.exit_code}),
            outcome.exit_code,
        ),
        Err(error) => (json!({"status": "failed", "error": error}), 1),
    };
    println!("{report}");
    ExitCode::from(u8::try_from(exit_code).unwrap_or(1))
}

fn command_paths(
    arguments: &[OsString],
    names: &[&str],
) -> Result<Vec<std::path::PathBuf>, String> {
    let mut paths = vec![None; names.len()];
    let mut pairs = arguments.chunks_exact(2);
    for pair in &mut pairs {
        let index = names
            .iter()
            .position(|name| pair[0] == *name)
            .ok_or_else(|| format!("unknown option: {}", pair[0].to_string_lossy()))?;
        if pair[1].is_empty()
            || paths[index]
                .replace(std::path::PathBuf::from(&pair[1]))
                .is_some()
        {
            return Err(format!("empty or repeated option: {}", names[index]));
        }
    }
    if !pairs.remainder().is_empty() || paths.iter().any(Option::is_none) {
        return Err(format!("required path options: {}", names.join(" ")));
    }
    paths
        .into_iter()
        .map(|path| path.ok_or_else(|| "missing option".into()))
        .collect()
}
