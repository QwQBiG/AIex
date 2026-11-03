//! Build a portable Windows archive from already-built release binaries.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ring::digest::{Context, SHA256};
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;

const TEST_WINDOW: &[u8] =
    b"@echo off\r\nsetlocal\r\nstart \"\" \"%~dp0ai-ex-platform-helper.exe\" --test-window\r\n";
const BINARIES: [(&str, &str, bool); 4] = [
    (
        "AIex.exe",
        "crates/ai-ex-desktop/target/release/ai-ex-desktop.exe",
        true,
    ),
    (
        "ai-ex-service.exe",
        "target/release/ai-ex-service.exe",
        false,
    ),
    (
        "ai-ex-migrate.exe",
        "target/release/ai-ex-migrate.exe",
        false,
    ),
    (
        "ai-ex-platform-helper.exe",
        "target/release/ai-ex-platform-helper.exe",
        false,
    ),
];
const ASSETS: [(&str, &str); 4] = [
    (
        "config/ai-ex.portable.example.toml",
        "data/ai-ex.local.toml",
    ),
    ("docs/PORTABLE_START.txt", "开始使用.txt"),
    ("LICENSE", "LICENSE.txt"),
    ("docs/PORTABLE_THIRD_PARTY.txt", "THIRD-PARTY-LICENSES.txt"),
];
const GENERATED: [&str; 2] = ["AIex.portable", "AIex-Test-Window.cmd"];

pub(crate) fn required_files() -> impl Iterator<Item = &'static str> {
    BINARIES
        .iter()
        .map(|(name, _, _)| *name)
        .chain(ASSETS.iter().map(|(_, name)| *name))
        .chain(GENERATED)
}

#[cfg(test)]
#[path = "portable_tests.rs"]
mod tests;

pub(crate) fn package(
    repository: &Path,
    output_root: &Path,
    offline: bool,
) -> Result<(Value, bool), String> {
    if !cfg!(windows) {
        return Err("Portable packaging requires Windows".into());
    }
    let output_root = std::path::absolute(output_root)
        .map_err(|e| format!("cannot resolve output directory: {e}"))?;
    let version = versions(repository, offline)?;
    let name = format!("AIex-Windows-x64-{version}");
    let destination = output_root.join(&name);
    let archive = output_root.join(format!("{name}.zip"));
    let checksum = output_root.join(format!("{name}.zip.sha256"));
    reject_existing(&[&destination, &archive, &checksum])?;

    for (_, path, gui) in BINARIES {
        check_pe(&repository.join(path), gui)?;
    }

    fs::create_dir_all(&output_root).map_err(|e| format!("cannot create output directory: {e}"))?;
    fs::create_dir(&destination)
        .map_err(|e| format!("cannot create new package directory: {e}"))?;
    fs::create_dir(destination.join("data")).map_err(|e| e.to_string())?;
    for (name, source, _) in BINARIES {
        copy_new(&repository.join(source), &destination.join(name))?;
    }
    for (source, target) in ASSETS {
        copy_new(&repository.join(source), &destination.join(target))?;
    }
    write_new(&destination.join(GENERATED[0]), b"1\n")?;
    write_new(&destination.join(GENERATED[1]), TEST_WINDOW)?;

    verify_executables(&destination, &version)?;
    let hash = write_archive(&destination, &archive, &name)?;
    write_new(&checksum, format!("{hash}  {name}.zip\n").as_bytes())?;
    Ok((
        json!({"version": version, "directory": destination, "archive": archive, "sha256": hash, "ready": true}),
        true,
    ))
}

fn versions(repository: &Path, offline: bool) -> Result<String, String> {
    let core = metadata(&repository.join("Cargo.toml"), offline)?;
    let desktop = metadata(&repository.join("crates/ai-ex-desktop/Cargo.toml"), offline)?;
    let service = package_version(&core, "ai-ex-service")?;
    if service != package_version(&core, "ai-ex-migrate")?
        || service != package_version(&desktop, "ai-ex-desktop")?
    {
        return Err("Desktop, service and migrator versions must match".into());
    }
    // Cargo validates package versions, but filenames must never contain path separators.
    if service.is_empty()
        || !service
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))
    {
        return Err("Invalid package version".into());
    }
    Ok(service.to_owned())
}

fn metadata(manifest: &Path, offline: bool) -> Result<Value, String> {
    let mut command = Command::new("cargo");
    command
        .args([
            "metadata",
            "--no-deps",
            "--locked",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(manifest);
    if offline {
        command.arg("--offline");
    }
    let output = command
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot read metadata for {}: {}",
            manifest.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("invalid cargo metadata: {e}"))
}

fn package_version<'a>(metadata: &'a Value, name: &str) -> Result<&'a str, String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("missing metadata packages")?;
    let mut matches = packages.iter().filter(|p| p["name"].as_str() == Some(name));
    let version = matches
        .next()
        .and_then(|p| p["version"].as_str())
        .ok_or_else(|| format!("missing package version: {name}"))?;
    if matches.next().is_some() {
        return Err(format!("duplicate package metadata: {name}"));
    }
    Ok(version)
}

fn reject_existing(paths: &[&Path]) -> Result<(), String> {
    for path in paths {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(format!(
                    "Output already exists; nothing overwritten: {}",
                    path.display()
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot inspect output {}: {e}", path.display())),
        }
    }
    Ok(())
}

fn check_pe(path: &Path, gui: bool) -> Result<(), String> {
    let mut file = File::open(path).map_err(|e| {
        format!(
            "Release executable missing or unreadable {}: {e}",
            path.display()
        )
    })?;
    let mut bytes = [0_u8; 4];
    file.seek(SeekFrom::Start(0x3c))
        .and_then(|_| file.read_exact(&mut bytes))
        .map_err(|e| format!("invalid executable {}: {e}", path.display()))?;
    let header = u64::from(u32::from_le_bytes(bytes));
    let mut signature = [0_u8; 6];
    file.seek(SeekFrom::Start(header))
        .and_then(|_| file.read_exact(&mut signature))
        .map_err(|e| format!("invalid executable {}: {e}", path.display()))?;
    if signature != [b'P', b'E', 0, 0, 0x64, 0x86] {
        return Err(format!(
            "Expected a Windows x64 executable: {}",
            path.display()
        ));
    }
    if gui {
        let mut subsystem = [0_u8; 2];
        file.seek(SeekFrom::Start(header + 24 + 68))
            .and_then(|_| file.read_exact(&mut subsystem))
            .map_err(|e| format!("invalid executable {}: {e}", path.display()))?;
        if u16::from_le_bytes(subsystem) != 2 {
            return Err("Desktop must use the Windows GUI subsystem".into());
        }
    }
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn copy_new(source: &Path, destination: &Path) -> Result<(), String> {
    let mut input =
        File::open(source).map_err(|e| format!("cannot open {}: {e}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| format!("cannot create {}: {e}", destination.display()))?;
    io::copy(&mut input, &mut output)
        .map_err(|e| format!("cannot copy {}: {e}", source.display()))?;
    Ok(())
}

fn verify_executables(destination: &Path, version: &str) -> Result<(), String> {
    verify_with(destination, version, run_check)
}

fn verify_with<F>(destination: &Path, version: &str, check: F) -> Result<(), String>
where
    F: Fn(&Path, &str) -> Result<Vec<u8>, String>,
{
    let output = check(&destination.join("AIex.exe"), "--check-install")?;
    let status: Value = serde_json::from_slice(&output)
        .map_err(|e| format!("invalid desktop check output: {e}"))?;
    if status["ready"].as_bool() != Some(true) || status["version"].as_str() != Some(version) {
        return Err("Packaged desktop version check failed".into());
    }
    for binary in ["ai-ex-service", "ai-ex-migrate", "ai-ex-platform-helper"] {
        let output = check(&destination.join(format!("{binary}.exe")), "--version")?;
        let output =
            std::str::from_utf8(&output).map_err(|e| format!("invalid version output: {e}"))?;
        if output.trim() != format!("{binary} {version}") {
            return Err(format!("Packaged {binary} version check failed"));
        }
    }
    Ok(())
}

fn run_check(executable: &Path, argument: &str) -> Result<Vec<u8>, String> {
    let mut command = Command::new(executable);
    command
        .arg(argument)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start package check {}: {e}", executable.display()))?;
    let mut stdout = child.stdout.take().ok_or("missing check stdout")?;
    let mut stderr = child.stderr.take().ok_or("missing check stderr")?;
    let output_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let error_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let start = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if start.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("Package check timed out".to_string());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("cannot wait for package check: {e}"));
            }
        }
    };
    let output = output_thread
        .join()
        .map_err(|_| "cannot read check stdout")?
        .map_err(|e| e.to_string())?;
    let errors = error_thread
        .join()
        .map_err(|_| "cannot read check stderr")?
        .map_err(|e| e.to_string())?;
    let status = result?;
    if !status.success() {
        return Err(format!(
            "Package check failed ({status}): {}",
            String::from_utf8_lossy(&errors)
        ));
    }
    Ok(output)
}

fn write_archive(destination: &Path, archive: &Path, name: &str) -> Result<String, String> {
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(archive)
        .map_err(|e| format!("cannot create archive {}: {e}", archive.display()))?;
    let mut zip = zip::ZipWriter::new(output);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in required_files() {
        let mut input = File::open(destination.join(entry))
            .map_err(|e| format!("cannot open package file {entry}: {e}"))?;
        zip.start_file(format!("{name}/{entry}"), options)
            .map_err(|e| e.to_string())?;
        io::copy(&mut input, &mut zip)
            .map_err(|e| format!("cannot add package file {entry}: {e}"))?;
    }
    zip.finish()
        .map_err(|e| format!("cannot finish archive: {e}"))?;
    let mut input = File::open(archive).map_err(|e| e.to_string())?;
    let mut context = Context::new(&SHA256);
    let mut buffer = [0_u8; 65536];
    loop {
        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        context.update(&buffer[..count]);
    }
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
