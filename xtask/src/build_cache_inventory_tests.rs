use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn fixture() -> PathBuf {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("aiex-cache-inventory-{}-{id}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::create_dir_all(root.join(TARGETS[0])).unwrap();
    root
}

fn clean_fixture(root: &Path) {
    fs::remove_dir(root.join(TARGETS[0])).unwrap();
    fs::remove_dir(root.join("target/debug")).unwrap();
    fs::remove_dir(root.join("target")).unwrap();
    fs::remove_dir(root).unwrap();
}

fn ignored(_: &str) -> Result<GitStatus> {
    Ok(GitStatus {
        tracked: Vec::new(),
        ignored: true,
    })
}

#[test]
fn reports_clean_inventory_without_mutating_cache() {
    let root = fixture();
    fs::write(root.join(TARGETS[0]).join("query-cache.bin"), b"fixture").unwrap();
    let (report, passed) = inventory(&root, ignored).unwrap();
    assert!(passed);
    assert_eq!(report["targets"][0]["files"], 1);
    assert!(report["issues"].as_array().unwrap().is_empty());
    assert!(root.join(TARGETS[0]).join("query-cache.bin").is_file());
    fs::remove_file(root.join(TARGETS[0]).join("query-cache.bin")).unwrap();
    clean_fixture(&root);
}

#[test]
fn reports_only_fixed_targets_and_unexpected_files() {
    let root = fixture();
    let cache = root.join(TARGETS[0]);
    fs::write(cache.join("work-products.bin"), b"fixture").unwrap();
    fs::write(cache.join("private-model.wav"), b"fixture").unwrap();
    let (report, passed) = inventory(&root, ignored).unwrap();
    assert!(!passed);
    assert_eq!(report["mode"], "read_only");
    assert_eq!(report["targets"].as_array().unwrap().len(), 5);
    assert_eq!(report["targets"][0]["files"], 2);
    assert_eq!(report["targets"][0]["bytes"], 14);
    assert_eq!(report["targets"][1]["state"], "missing");
    assert!(report["issues"].as_array().unwrap().iter().any(|issue| {
        issue["kind"] == "unexpected_file"
            && issue["path"]
                .as_str()
                .unwrap()
                .ends_with("private-model.wav")
    }));
    assert!(cache.join("work-products.bin").is_file());
    assert!(cache.join("private-model.wav").is_file());
    fs::remove_file(cache.join("work-products.bin")).unwrap();
    fs::remove_file(cache.join("private-model.wav")).unwrap();
    clean_fixture(&root);
}

#[test]
fn refuses_tracked_or_not_ignored_targets() {
    let root = fixture();
    let (report, passed) = inventory(&root, |relative| {
        Ok(GitStatus {
            tracked: if relative == TARGETS[0] {
                vec!["target/debug/incremental/tracked.rs".into()]
            } else {
                Vec::new()
            },
            ignored: relative != TARGETS[0],
        })
    })
    .unwrap();
    assert!(!passed);
    let issues = report["issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| issue["kind"] == "tracked"));
    assert!(issues.iter().any(|issue| issue["kind"] == "not_ignored"));
    clean_fixture(&root);
}

#[cfg(windows)]
#[test]
fn reports_junction_without_visiting_its_target() {
    let root = fixture();
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("private.wav"), b"fixture").unwrap();
    let link = root
        .join("target")
        .join("debug")
        .join("incremental")
        .join("junction");
    let command = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe");
    let output = Command::new(command)
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (report, passed) = inventory(&root, ignored).unwrap();
    assert!(!passed);
    assert_eq!(report["targets"][0]["files"], 0);
    assert!(
        report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["kind"] == "link")
    );
    assert!(outside.join("private.wav").is_file());
    fs::remove_dir(&link).unwrap();
    fs::remove_file(outside.join("private.wav")).unwrap();
    fs::remove_dir(&outside).unwrap();
    clean_fixture(&root);
}

#[cfg(unix)]
#[test]
fn reports_symlink_without_visiting_its_target() {
    use std::os::unix::fs::symlink;
    let root = fixture();
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("private.wav"), b"fixture").unwrap();
    let link = root.join(TARGETS[0]).join("link");
    symlink(&outside, &link).unwrap();
    let (report, passed) = inventory(&root, ignored).unwrap();
    assert!(!passed);
    assert_eq!(report["targets"][0]["files"], 0);
    assert!(
        report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["kind"] == "link")
    );
    assert!(outside.join("private.wav").is_file());
    fs::remove_file(&link).unwrap();
    fs::remove_file(outside.join("private.wav")).unwrap();
    fs::remove_dir(&outside).unwrap();
    clean_fixture(&root);
}
