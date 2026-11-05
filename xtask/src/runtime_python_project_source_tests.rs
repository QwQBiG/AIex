use super::*;
use std::io::Write;
use zip::write::SimpleFileOptions;

fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn project_python_and_launches_are_reported_but_upstream_is_not_claimed() {
    let project = zip(&[
        ("src/old.py", b"pass"),
        ("tools/start.ps1", b"python worker.py\n"),
        (
            "src/main.rs",
            b"use std::process::Command; fn main() { Command::new(\"python\"); }",
        ),
        (
            "Cargo.toml",
            b"[dependencies]\nwrapper = { package = 'pyo3', version = '0.25' }\n",
        ),
        (
            "Cargo.lock",
            b"[[package]]\nname = 'rustpython-vm'\nversion = '0.1'\n",
        ),
        (
            "src/clean.rs",
            b"// Command::new(\"python\");\nfn main() {}",
        ),
        ("README.md", b"historic Python references are documentation"),
        ("nested.zip", b"PK\x03\x04opaque"),
    ]);
    let sources = zip(&[
        ("project/source.zip", &project),
        ("rust/upstream.crate", b"opaque"),
    ]);
    let report = inspect("sources.zip", sources, &mut Budget::default()).unwrap();
    assert_eq!(report["status"], "partial");
    let issues = report["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 5);
    assert!(issues.iter().any(|issue| issue["path"] == "src/old.py"));
    assert!(
        issues
            .iter()
            .any(|issue| issue["path"] == "tools/start.ps1" && issue["line"] == 1)
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue["kind"] == "rust_command" && issue["path"] == "src/main.rs")
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue["kind"] == "cargo_binding" && issue["path"] == "Cargo.toml")
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue["kind"] == "cargo_binding" && issue["path"] == "Cargo.lock")
    );
    assert!(
        !issues
            .iter()
            .any(|issue| issue["path"] == "src/clean.rs" || issue["path"] == "README.md")
    );
    assert_eq!(report["uninspected_nested_archives"][0], "nested.zip");
}

#[test]
fn rejects_unsafe_names_duplicates_and_excessive_work() {
    for project in [
        zip(&[("../escape.py", b"x")]),
        zip(&[("Readme.md", b"x"), ("README.md", b"x")]),
        zip(&[("large.rs", &vec![b'x'; MAX_TEXT_BYTES as usize + 1])]),
        zip(&[("Cargo.toml", b"[broken")]),
    ] {
        let sources = zip(&[("project/source.zip", &project)]);
        assert!(inspect("sources.zip", sources, &mut Budget::default()).is_err());
    }
    let mut sources = zip(&[("project/source.zip", b"dummy")]);
    let eocd = sources.len() - 22;
    let count = (MAX_ENTRIES as u16 + 1).to_le_bytes();
    sources[eocd + 8..eocd + 10].copy_from_slice(&count);
    sources[eocd + 10..eocd + 12].copy_from_slice(&count);
    assert!(
        inspect("sources.zip", sources, &mut Budget::default())
            .unwrap_err()
            .contains("entry limit")
    );
}

#[test]
fn cumulative_budgets_span_multiple_source_archives() {
    let project = zip(&[("start.ps1", b"cargo test")]);
    let source = zip(&[("project/source.zip", &project)]);
    let mut budget = Budget {
        text_left: 15,
        ..Budget::default()
    };
    assert!(inspect("first.zip", source.clone(), &mut budget).is_ok());
    assert!(inspect("second.zip", source, &mut budget).is_err());
    assert!(budget.take_source(MAX_SOURCES_BYTES));
    assert!(!budget.take_source(1));
}
