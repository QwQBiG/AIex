use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "aiex-token-fixture-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).expect("create isolated fixture directory");
    path
}

fn assert_token_file(path: &Path) -> String {
    let content = fs::read_to_string(path).unwrap();
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
    let token = content.strip_suffix(newline).unwrap();
    assert_eq!(token.len(), 44);
    assert_eq!(STANDARD.decode(token).unwrap().len(), 32);
    token.to_owned()
}

#[test]
fn creates_base64_token_and_never_returns_it_in_report() {
    let root = directory();
    let parent = root.join("中文 空格");
    let path = parent.join("control.token");
    let (report, passed) = create(&path, false).unwrap();
    assert!(passed);
    let token = assert_token_file(&path);
    assert!(!report.to_string().contains(&token));
    assert_eq!(report["allow_rotation"], false);
    fs::remove_file(path).unwrap();
    fs::remove_dir(parent).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn existing_token_is_preserved_until_rotation_is_requested() {
    let root = directory();
    let path = root.join("control.token");
    fs::write(&path, b"existing credential").unwrap();
    assert!(create(&path, false).unwrap_err().contains("already exists"));
    assert_eq!(fs::read(&path).unwrap(), b"existing credential");
    let (report, passed) = create(&path, true).unwrap();
    assert!(passed);
    assert_eq!(report["allow_rotation"], true);
    assert_token_file(&path);
    fs::remove_file(path).unwrap();
    fs::remove_dir(root).unwrap();
}

#[test]
fn concurrent_creation_allows_exactly_one_writer() {
    let root = directory();
    let path = root.join("control.token");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                create(&path, false).is_ok()
            })
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|passed| *passed)
            .count(),
        1
    );
    assert_token_file(&path);
    fs::remove_file(path).unwrap();
    fs::remove_dir(root).unwrap();
}
