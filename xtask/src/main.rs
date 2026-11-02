#![forbid(unsafe_code)]

mod bundle;
mod bundle_io;
#[cfg(test)]
mod bundle_tests;
mod bundle_zip;
mod catalog_build;
mod native;
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
    let result = match arguments.as_slice() {
        [command, manifest, directory] if command == "native-verify" => {
            native::verify(Path::new(manifest), Path::new(directory))
        }
        [command, specification] if command == "native-probe" => {
            native::probe(Path::new(specification)).await
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
        _ => Err("usage: cargo xtask native-verify <manifest.json> <directory> | native-probe <spec.json> | native-bundle --spec <input.json> --output <new.zip> --manifest <new.json> | native-catalog --distribution <directory> --runtime-manifest <json> --runtime-zip <zip> --output <new.json> | native-sources --spec <input.json> --output <new.zip>".into()),
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
