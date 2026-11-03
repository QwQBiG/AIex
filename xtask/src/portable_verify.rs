//! Verify a specified portable archive without accessing personal configuration.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ring::digest::{Context, SHA256};
use serde_json::{Value, json};

use crate::portable_verify_archive::{self, ArchiveLayout};

const CONFIG: &[u8] = include_bytes!("../../config/ai-ex.portable.example.toml");
const REPLAY: &[u8] = include_bytes!("../../config_examples/stage-replay.jsonl");
static NEXT_EVIDENCE: AtomicU64 = AtomicU64::new(0);

struct CheckResult {
    code: i32,
    stdout: String,
    stderr: String,
}

fn digest(reader: &mut File) -> Result<String, String> {
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut context = Context::new(&SHA256);
    let mut buffer = [0; 65536];
    loop {
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        context.update(&buffer[..count]);
    }
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn file_digest(path: &Path) -> Result<String, String> {
    digest(&mut File::open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?)
}

fn new_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("Cannot create new file {}: {e}", path.display()))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    new_file(path)?
        .write_all(bytes)
        .map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

fn create_evidence(parent: &Path) -> Result<PathBuf, String> {
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("Cannot resolve evidence directory: {e}"))?;
    if !parent.is_dir() {
        return Err("Evidence parent is not a directory".into());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    for _ in 0..32 {
        let sequence = NEXT_EVIDENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            "portable-verification-{}-{now}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Cannot create evidence directory: {e}")),
        }
    }
    Err("Cannot allocate a new evidence directory".into())
}

fn extract(reader: &mut File, layout: &ArchiveLayout, evidence: &Path) -> Result<PathBuf, String> {
    let relocated = evidence.join("中文目录 with spaces");
    fs::create_dir(&relocated).map_err(|e| e.to_string())?;
    let package = relocated.join(&layout.name);
    fs::create_dir(&package).map_err(|e| e.to_string())?;
    fs::create_dir(package.join("data")).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| format!("Invalid ZIP: {e}"))?;
    let mut extracted = std::collections::BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| format!("Invalid ZIP entry: {e}"))?;
        let path = entry.name().replace('\\', "/");
        if entry.is_symlink() {
            return Err("ZIP entries changed during verification".into());
        }
        if entry.is_dir() || path.ends_with('/') {
            if path != format!("{}/", layout.name) && path != format!("{}/data/", layout.name) {
                return Err("ZIP entries changed during verification".into());
            }
            continue;
        }
        let relative = path
            .strip_prefix(&format!("{}/", layout.name))
            .ok_or("ZIP root changed")?;
        // Explicit paths ensure a concurrently changed archive cannot escape this directory.
        let allowed = matches!(
            relative,
            "AIex.exe"
                | "ai-ex-service.exe"
                | "AIex.portable"
                | "开始使用.txt"
                | "data/ai-ex.local.toml"
        ) || (layout.requires_migrator && relative == "ai-ex-migrate.exe")
            || (layout.requires_license
                && matches!(relative, "LICENSE.txt" | "THIRD-PARTY-LICENSES.txt"))
            || (layout.requires_platform
                && matches!(
                    relative,
                    "ai-ex-platform-helper.exe" | "AIex-Test-Window.cmd"
                ));
        if !allowed || !extracted.insert(relative.to_owned()) {
            return Err("ZIP entries changed during verification".into());
        }
        io::copy(&mut entry, &mut new_file(&package.join(relative))?)
            .map_err(|e| format!("Cannot extract {relative}: {e}"))?;
    }
    if extracted.len() != layout.file_count {
        return Err("ZIP entries changed during extraction".into());
    }
    Ok(package)
}

fn run_check(
    executable: &Path,
    arguments: &[&std::ffi::OsStr],
    evidence: &Path,
    expected: i32,
) -> Result<CheckResult, String> {
    let system_root = std::env::var_os("SystemRoot").ok_or("SystemRoot is not set")?;
    let system_root = PathBuf::from(system_root);
    let path = std::env::join_paths([system_root.join("System32"), system_root.clone()])
        .map_err(|e| e.to_string())?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .current_dir(evidence)
        .env_clear()
        .env("SystemRoot", &system_root)
        .env("WINDIR", &system_root)
        .env("PATH", path)
        .env("TEMP", evidence)
        .env("TMP", evidence)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start {}: {e}", executable.display()))?;
    let start = Instant::now();
    let stdout = child.stdout.take().ok_or("Missing stdout")?;
    let stderr = child.stderr.take().ok_or("Missing stderr")?;
    let collect = |pipe: Box<dyn Read + Send>| {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut data = Vec::new();
            let result = pipe.take(1_048_577).read_to_end(&mut data).map(|_| data);
            let _ = sender.send(result);
        });
        receiver
    };
    let output = collect(Box::new(stdout));
    let errors = collect(Box::new(stderr));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if start.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("Package check timed out".to_owned());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("Cannot wait for package check: {e}"));
            }
        }
    };
    let code = status?
        .code()
        .ok_or("Package check terminated without an exit code")?;
    let receive = |receiver: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>| {
        let data = receiver
            .recv_timeout(Duration::from_secs(30).saturating_sub(start.elapsed()))
            .map_err(|e| format!("Cannot collect package check output: {e}"))?
            .map_err(|e| e.to_string())?;
        if data.len() > 1_048_576 {
            return Err("Package check output exceeds limit".to_owned());
        }
        Ok(data)
    };
    let stdout = receive(output)?;
    let stderr = receive(errors)?;
    let result = CheckResult {
        code,
        stdout: String::from_utf8(stdout).map_err(|e| format!("Invalid UTF-8 stdout: {e}"))?,
        stderr: String::from_utf8(stderr).map_err(|e| format!("Invalid UTF-8 stderr: {e}"))?,
    };
    if result.code != expected {
        return Err(format!(
            "Unexpected package check exit code {}: {}",
            result.code, result.stderr
        ));
    }
    Ok(result)
}

fn check_version(output: &str, name: &str, version: &str) -> Result<(), String> {
    if output.trim() != format!("{name} {version}") {
        return Err(format!("Packaged {name} version check failed"));
    }
    Ok(())
}

fn check_install(output: &str, version: &str) -> Result<(), String> {
    let status: Value =
        serde_json::from_str(output).map_err(|e| format!("Invalid installation status: {e}"))?;
    if status["ready"].as_bool() != Some(true) || status["version"].as_str() != Some(version) {
        return Err("Packaged desktop version check failed".into());
    }
    Ok(())
}

fn check_replay(output: &str) -> Result<(), String> {
    let fixture = std::str::from_utf8(REPLAY).map_err(|e| e.to_string())?;
    let records: Vec<Value> = fixture
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|e| format!("Invalid replay fixture: {e}")))
        .collect::<Result<_, _>>()?;
    let lines: std::collections::BTreeSet<_> = output.lines().collect();
    if !lines.contains(format!("OBS dry-run replay complete: records={}", records.len()).as_str()) {
        return Err("Packaged service replay count differs".into());
    }
    for record in records {
        let sequence = record["sequence"]
            .as_u64()
            .ok_or("Invalid replay sequence")?;
        let action = record["action"]["type"]
            .as_str()
            .ok_or("Invalid replay action")?;
        if !lines.contains(format!("stage action #{sequence}: {action}").as_str()) {
            return Err("Packaged service replay action differs".into());
        }
    }
    Ok(())
}

pub(crate) fn verify(archive_path: &Path, evidence_parent: &Path) -> Result<(Value, bool), String> {
    if !cfg!(windows) {
        return Err("Portable verification requires Windows".into());
    }
    let archive_path = archive_path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve archive: {e}"))?;
    let filename = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Invalid archive filename")?;
    let mut archive = File::open(&archive_path).map_err(|e| format!("Cannot open archive: {e}"))?;
    let archive_size = archive.metadata().map_err(|e| e.to_string())?.len();
    let original_hash = digest(&mut archive)?;
    let layout = portable_verify_archive::inspect(&mut archive, filename)?;
    let evidence = create_evidence(evidence_parent)?;
    let package = extract(&mut archive, &layout, &evidence)?;
    if digest(&mut archive)? != original_hash || file_digest(&archive_path)? != original_hash {
        return Err("Archive changed during extraction".into());
    }

    let desktop = package.join("AIex.exe");
    let service = package.join("ai-ex-service.exe");
    let checked = run_check(&desktop, &["--check-install".as_ref()], &evidence, 0)?;
    check_install(&checked.stdout, &layout.version)?;
    let checked = run_check(&service, &["--version".as_ref()], &evidence, 0)?;
    check_version(&checked.stdout, "ai-ex-service", &layout.version)?;
    if layout.requires_migrator {
        let checked = run_check(
            &package.join("ai-ex-migrate.exe"),
            &["--version".as_ref()],
            &evidence,
            0,
        )?;
        check_version(&checked.stdout, "ai-ex-migrate", &layout.version)?;
    }
    if layout.requires_platform {
        let helper = package.join("ai-ex-platform-helper.exe");
        let checked = run_check(&helper, &["--version".as_ref()], &evidence, 0)?;
        check_version(&checked.stdout, "ai-ex-platform-helper", &layout.version)?;
        run_check(&helper, &["--help".as_ref()], &evidence, 0)?;
        run_check(&helper, &["--invalid".as_ref()], &evidence, 1)?;
    }

    let fixture = evidence.join("stage-replay.jsonl");
    write_new(&fixture, REPLAY)?;
    let config = package.join("data/ai-ex.local.toml");
    let replay = run_check(
        &service,
        &[
            "--config".as_ref(),
            config.as_os_str(),
            "--replay-stage".as_ref(),
            fixture.as_os_str(),
        ],
        &evidence,
        0,
    )?;
    check_replay(&replay.stdout)?;
    let data = package.join("data");
    let contents = fs::read_dir(&data)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if contents.len() != 1
        || contents[0].file_name() != "ai-ex.local.toml"
        || fs::read(&config).map_err(|e| e.to_string())? != CONFIG
    {
        return Err(
            "Package checks changed distributed configuration or initialized user data".into(),
        );
    }

    let incomplete = evidence.join("incomplete");
    fs::create_dir(&incomplete).map_err(|e| e.to_string())?;
    fs::create_dir(incomplete.join("data")).map_err(|e| e.to_string())?;
    for name in ["AIex.exe", "AIex.portable", "data/ai-ex.local.toml"] {
        io::copy(
            &mut File::open(package.join(name)).map_err(|e| e.to_string())?,
            &mut new_file(&incomplete.join(name))?,
        )
        .map_err(|e| e.to_string())?;
    }
    let missing = run_check(
        &incomplete.join("AIex.exe"),
        &["--check-install".as_ref()],
        &evidence,
        1,
    )?;
    if !missing.stderr.contains("ai-ex-service.exe") {
        return Err("Missing service executable was not reported".into());
    }
    if digest(&mut archive)? != original_hash || file_digest(&archive_path)? != original_hash {
        return Err("Archive changed during verification".into());
    }

    let report_path = evidence.join("verification.json");
    let report = json!({
        "ready": true, "version": layout.version, "archive": archive_path, "archiveBytes": archive_size,
        "sha256": original_hash, "fileCount": layout.file_count, "migratorIncluded": layout.requires_migrator,
        "platformHelperIncluded": layout.requires_platform, "noDevelopmentPath": true, "crossDirectory": true,
        "unicodePath": true, "serviceReplay": true, "incompletePackageRejected": true,
        "evidenceDirectory": evidence, "reportPath": report_path,
    });
    let mut json = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    json.push(b'\n');
    write_new(&report_path, &json)?;
    Ok((report, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    #[test]
    fn installation_and_versions_require_the_exact_release() {
        assert!(
            check_install(
                r#"{"ready":true,"version":"0.10.0-alpha.1"}"#,
                "0.10.0-alpha.1"
            )
            .is_ok()
        );
        assert!(
            check_install(
                r#"{"ready":false,"version":"0.10.0-alpha.1"}"#,
                "0.10.0-alpha.1"
            )
            .is_err()
        );
        assert!(check_install(r#"{"ready":true,"version":"0.9.0"}"#, "0.10.0-alpha.1").is_err());
        assert!(
            check_version(
                "ai-ex-service 0.10.0-alpha.1\r\n",
                "ai-ex-service",
                "0.10.0-alpha.1"
            )
            .is_ok()
        );
        assert!(check_version("ai-ex-service 0.9.0", "ai-ex-service", "0.10.0-alpha.1").is_err());
    }

    #[test]
    fn replay_requires_count_and_every_expected_action() {
        let output = "stage action #1: scene\nstage action #2: subtitle\nstage action #3: hotkey\nOBS dry-run replay complete: records=3\n";
        assert!(check_replay(output).is_ok());
        assert!(check_replay(&output.replace("records=3", "records=2")).is_err());
        assert!(check_replay(&output.replace("stage action #3: hotkey\n", "")).is_err());
    }

    #[test]
    fn extraction_uses_a_fresh_directory_and_never_overwrites() {
        const FILES: [&str; 10] = [
            "AIex.exe",
            "ai-ex-service.exe",
            "AIex.portable",
            "开始使用.txt",
            "data/ai-ex.local.toml",
            "ai-ex-migrate.exe",
            "LICENSE.txt",
            "THIRD-PARTY-LICENSES.txt",
            "ai-ex-platform-helper.exe",
            "AIex-Test-Window.cmd",
        ];
        let evidence = create_evidence(&std::env::temp_dir()).unwrap();
        let filename = "AIex-Windows-x64-0.10.0-alpha.1.zip";
        let name = filename.strip_suffix(".zip").unwrap();
        let archive_path = evidence.join(filename);
        let mut writer = zip::ZipWriter::new(new_file(&archive_path).unwrap());
        for entry in FILES {
            writer
                .start_file(format!("{name}/{entry}"), SimpleFileOptions::default())
                .unwrap();
            writer.write_all(entry.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        let mut input = File::open(&archive_path).unwrap();
        let layout = portable_verify_archive::inspect(&mut input, filename).unwrap();
        let package = extract(&mut input, &layout, &evidence).unwrap();
        assert_eq!(
            fs::read(package.join("data/ai-ex.local.toml")).unwrap(),
            b"data/ai-ex.local.toml"
        );
        assert!(extract(&mut input, &layout, &evidence).is_err());
        drop(input);
        for entry in FILES {
            fs::remove_file(package.join(entry)).unwrap();
        }
        fs::remove_dir(package.join("data")).unwrap();
        fs::remove_dir(&package).unwrap();
        fs::remove_dir(evidence.join("中文目录 with spaces")).unwrap();
        fs::remove_file(archive_path).unwrap();
        fs::remove_dir(evidence).unwrap();
    }
}
