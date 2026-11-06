use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

const FIXTURE: &[u8] = include_bytes!("fixtures/legacy-memory.json");

fn inspect(path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"))
        .args(["--inspect-memory-backup", "--input"])
        .arg(path)
        .output()
        .unwrap()
}

fn directory() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("private-backup-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn memory_backup_inspects_gzip_without_revealing_contents_or_writing_output() {
    let dir = directory();
    let path = dir.join("private-path-marker.json.gz");
    let mut encoder =
        flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::default());
    encoder.write_all(FIXTURE).unwrap();
    encoder.finish().unwrap();
    let original = fs::read(&path).unwrap();
    let output = inspect(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["records"], 1);
    assert_eq!(report["local_timestamp_records"], 1);
    assert_eq!(report["nonempty_embedding_records"], 1);
    assert_eq!(report["import_compatibility"], "not_checked");
    assert_eq!(report["authenticity"], "not_authenticated");
    assert_eq!(report["legacy_metadata_fields_present"], false);
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!rendered.contains("private-path-marker"));
    assert!(!rendered.contains("00000000-0000-4000-8000-000000000001"));
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    fs::remove_file(path).unwrap();
    fs::remove_dir(dir).unwrap();
}

#[test]
fn memory_backup_inspects_plain_json_and_preserves_legacy_metadata_presence() {
    let dir = directory();
    let path = dir.join("backup.json");
    let fixture = include_str!("fixtures/legacy-memory-unembedded.json");
    let content = fixture.replacen(
        '{',
        "{\"backup_timestamp\":\"private-time\",\"collection_name\":\"private-collection\",",
        1,
    );
    fs::write(&path, content.as_bytes()).unwrap();
    let output = inspect(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["legacy_metadata_fields_present"], true);
    assert_eq!(report["local_timestamp_records"], 0);
    assert_eq!(report["unclassified_timestamp_records"], 0);
    assert_eq!(report["nonempty_embedding_records"], 0);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains("private-time"));
    assert!(!text.contains("private-collection"));
    assert_eq!(fs::read(&path).unwrap(), content.as_bytes());
    fs::remove_file(path).unwrap();
    fs::remove_dir(dir).unwrap();
}

#[test]
fn memory_backup_rejects_bad_hash_count_truncation_and_extra_options_without_disclosure() {
    let dir = directory();
    let path = dir.join("private-path-marker.json");
    let fixture = std::str::from_utf8(FIXTURE).unwrap();
    for invalid in [
        fixture.replace("7ae7e7b2", "0ae7e7b2"),
        fixture.replace("\"total_memories\":1", "\"total_memories\":2"),
        "{\"private-record-marker\":".into(),
    ] {
        fs::write(&path, invalid).unwrap();
        let output = inspect(&path);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!error.contains("private-path-marker"));
        assert!(!error.contains("private-record-marker"));
    }
    fs::write(&path, FIXTURE).unwrap();
    let invalid = Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"))
        .args(["--inspect-memory-backup", "--input"])
        .arg(&path)
        .args(["--output", "private-output-marker"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("private-output-marker"));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    fs::remove_file(path).unwrap();
    fs::remove_dir(dir).unwrap();
}

#[test]
fn memory_backup_rejects_bounded_gzip_expansion_and_damaged_archive() {
    let dir = directory();
    let path = dir.join("large.json.gz");
    let mut encoder =
        flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::fast());
    let chunk = [b' '; 64 * 1024];
    for _ in 0..513 {
        encoder.write_all(&chunk).unwrap();
    }
    encoder.finish().unwrap();
    assert!(!inspect(&path).status.success());
    fs::write(&path, [0x1f, 0x8b, 0x00]).unwrap();
    assert!(!inspect(&path).status.success());
    fs::remove_file(path).unwrap();
    fs::remove_dir(dir).unwrap();
}
