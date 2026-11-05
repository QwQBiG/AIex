use super::*;
use std::fs::{self, File};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::bundle_io::hash_bytes;
use zip::write::SimpleFileOptions;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: std::path::PathBuf,
    archive: std::path::PathBuf,
    catalog: std::path::PathBuf,
}

impl Fixture {
    fn new(import: Option<&str>, source: bool, script: bool, nested: bool) -> Self {
        let source = source.then(|| {
            let project = zip(&[("README.md", b"documentation")]);
            zip(&[
                ("project/source.zip", &project),
                ("rust/upstream.crate", b"opaque"),
            ])
        });
        Self::with_source(import, source, script, nested)
    }

    fn with_source(
        import: Option<&str>,
        source: Option<Vec<u8>>,
        script: bool,
        nested: bool,
    ) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "runtime-gate-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let archive = directory.join("runtime.zip");
        let catalog = directory.join("catalog.json");
        let mut files = vec![
            (
                "ollama/ollama.exe",
                "native_runtime",
                crate::pe_imports::tests::fixture(import, None),
            ),
            ("LICENSE.txt", "license", b"fixture license".to_vec()),
        ];
        if let Some(source) = source {
            files.push(("sources/source.zip", "source_archive", source));
        }
        if script {
            files.push((
                "tool.yaml",
                "configuration",
                b"run: python3 worker.py\n".to_vec(),
            ));
            files.push((
                "start",
                "configuration",
                b"#!/usr/bin/env python3\nprint('ok')\n".to_vec(),
            ));
            files.push((
                "history",
                "configuration",
                b"# python3 was previously used\n".to_vec(),
            ));
            files.push((
                "history.md",
                "configuration",
                b"#!/usr/bin/python3\nAn old example.\n".to_vec(),
            ));
        }
        if nested {
            files.push(("weights.npz", "model", b"model container".to_vec()));
            files.push(("renamed.bin", "model", b"PK\x03\x04zip contents".to_vec()));
        }
        let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
        let mut artifacts = Vec::new();
        for (name, kind, data) in files {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(&data).unwrap();
            artifacts.push(
                json!({"path":name,"kind":kind,"bytes":data.len(),"sha256":hash_bytes(&data)}),
            );
        }
        zip.finish().unwrap();
        let data = fs::read(&archive).unwrap();
        let value = json!({"schema_version":1,"models":[],"runtimes":[{
            "name":"fixture",
            "manifest":{"id":"native-cpu","revision":"0.1.0","protocol_version":1,
                "backends":["ollama"],"artifacts":artifacts,
                "licenses":[{"id":"MIT","source_url":"https://example.com/license"}],
                "workers":[],"ollama_executable":"ollama/ollama.exe"},
            "source":{"type":"archive","url":"https://example.com/v1/runtime.zip",
                "bytes":data.len(),"sha256":hash_bytes(&data)}
        }]});
        fs::write(&catalog, serde_json::to_vec(&value).unwrap()).unwrap();
        Self {
            directory,
            archive,
            catalog,
        }
    }
}

fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_file(&self.archive).unwrap();
        fs::remove_file(&self.catalog).unwrap();
        fs::remove_dir(&self.directory).unwrap();
    }
}

#[test]
fn clean_runtime_is_scoped_and_uninspected_source_blocks_success() {
    let clean = Fixture::new(None, false, false, false);
    let (report, passed) = check(&clean.catalog, &clean.archive).unwrap();
    assert!(passed);
    assert_eq!(report["status"], "passed_for_scope");
    let source = Fixture::new(None, true, false, false);
    let (report, passed) = check(&source.catalog, &source.archive).unwrap();
    assert!(!passed);
    assert_eq!(report["status"], "incomplete");
    assert_eq!(
        report["uninspected_source_archives"][0],
        "sources/source.zip"
    );
    assert_eq!(report["project_source_audits"][0]["status"], "partial");
}

#[test]
fn runtime_reports_python_in_its_project_source_without_clearing_the_source_gate() {
    let project = zip(&[("src/legacy.py", b"pass")]);
    let sources = zip(&[
        ("project/source.zip", &project),
        ("rust/upstream.crate", b"opaque"),
    ]);
    let fixture = Fixture::with_source(None, Some(sources), false, false);
    let (report, passed) = check(&fixture.catalog, &fixture.archive).unwrap();
    assert!(!passed);
    assert_eq!(report["status"], "failed");
    assert_eq!(
        report["project_source_audits"][0]["issues"][0]["path"],
        "src/legacy.py"
    );
    assert_eq!(
        report["uninspected_source_archives"][0],
        "sources/source.zip"
    );
}

#[test]
fn model_containers_and_renamed_archives_block_success() {
    let fixture = Fixture::new(None, false, false, true);
    let (report, passed) = check(&fixture.catalog, &fixture.archive).unwrap();
    assert!(!passed);
    assert_eq!(report["status"], "incomplete");
    let nested = report["uninspected_nested_archives"].as_array().unwrap();
    assert!(nested.iter().any(|value| value == "weights.npz"));
    assert!(nested.iter().any(|value| value == "renamed.bin"));
}

#[test]
fn detects_static_imports_and_script_commands() {
    let fixture = Fixture::new(Some("python311.dll"), true, true, false);
    let (report, passed) = check(&fixture.catalog, &fixture.archive).unwrap();
    assert!(!passed);
    assert_eq!(report["status"], "failed");
    let issues = report["issues"].as_array().unwrap();
    assert!(issues.iter().any(|entry| entry["kind"] == "pe_import"));
    assert!(
        issues
            .iter()
            .any(|entry| entry["kind"] == "runtime_command")
    );
    assert!(
        issues
            .iter()
            .any(|entry| entry["path"] == "start" && entry["line"] == 1)
    );
    assert!(!issues.iter().any(|entry| entry["path"] == "history"));
    assert!(!issues.iter().any(|entry| entry["path"] == "history.md"));
    assert_eq!(
        report["uninspected_source_archives"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn rejects_wrong_archive_and_entry_hashes() {
    let fixture = Fixture::new(None, false, false, false);
    let mut value: Value = serde_json::from_slice(&fs::read(&fixture.catalog).unwrap()).unwrap();
    value["runtimes"][0]["source"]["sha256"] = json!("0".repeat(64));
    fs::write(&fixture.catalog, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(check(&fixture.catalog, &fixture.archive).is_err());
    value["runtimes"][0]["source"]["sha256"] =
        json!(hash_bytes(&fs::read(&fixture.archive).unwrap()));
    value["runtimes"][0]["manifest"]["artifacts"][0]["sha256"] = json!("0".repeat(64));
    fs::write(&fixture.catalog, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(check(&fixture.catalog, &fixture.archive).is_err());
}
