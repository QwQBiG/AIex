use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ai_ex_platform_windows::ProcessLaunch;

static NEXT: AtomicU64 = AtomicU64::new(0);
pub static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub struct Fixture(pub PathBuf);

impl Fixture {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "owned job 中文 {}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::copy(
            env!("CARGO_BIN_EXE_owned-job-test-worker"),
            root.join("程序 空格.exe"),
        )
        .unwrap();
        Self(root)
    }

    pub fn launch(&self, mode: &str) -> ProcessLaunch {
        let mut environment = BTreeMap::new();
        if let Some(value) = std::env::var_os("SystemRoot") {
            environment.insert("SystemRoot".into(), value);
        }
        ProcessLaunch {
            executable: self.0.join("程序 空格.exe"),
            arguments: vec![mode.into(), self.0.as_os_str().into()],
            directory: self.0.clone(),
            environment,
        }
    }

    pub fn wait_file(&self, name: &str) -> String {
        let path = self.0.join(name);
        let timer = Instant::now();
        while !std::fs::read_to_string(&path).is_ok_and(|s| !s.is_empty()) {
            assert!(
                timer.elapsed() < Duration::from_secs(5),
                "fixture file deadline: {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::read_to_string(self.0.join(name)).unwrap()
    }

    pub fn external(&self) -> External {
        let mut command = Command::new(self.0.join("程序 空格.exe"));
        command.arg("child").arg(&self.0);
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000 | 0x4000);
        External(command.spawn().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for name in [
            "程序 空格.exe",
            "child.pid",
            "child.ready",
            "port.txt",
            "arguments.txt",
            "environment.txt",
            "invalid.exe",
        ] {
            let file = self.0.join(name);
            if file.exists() {
                let result = std::fs::remove_file(file);
                if !std::thread::panicking() {
                    result.unwrap();
                }
            }
        }
        let result = std::fs::remove_dir(&self.0);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

pub struct External(pub Child);
impl Drop for External {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn wait(mut ready: impl FnMut() -> bool) {
    let timer = Instant::now();
    while !ready() {
        assert!(
            timer.elapsed() < Duration::from_secs(5),
            "owned fixture deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn text(value: &str) -> OsString {
    value.into()
}
