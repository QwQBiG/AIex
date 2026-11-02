use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ai_ex_domain::AppError;

pub struct ManagedService {
    child: Child,
    exit_reported: bool,
}

impl ManagedService {
    pub fn spawn_native(config_path: &Path, native_path: &Path) -> Result<Self, AppError> {
        let mut command = Command::new(service_executable()?);
        command
            .arg("--managed")
            .arg("--config")
            .arg(config_path)
            .arg("--native-setup")
            .arg("--native-config")
            .arg(native_path);
        if let Some(directory) = std::env::current_exe()
            .ok()
            .and_then(|path| crate::portable::root(&path))
        {
            let log = crate::portable::service_log(&directory)?;
            let errors = log
                .try_clone()
                .map_err(|error| AppError::unavailable(error.to_string()))?;
            command.stdout(Stdio::from(log)).stderr(Stdio::from(errors));
        }
        Self::start_command(&mut command)
    }

    pub fn spawn(
        config_path: &Path,
        api_key: Option<&str>,
        api_key_env: &str,
    ) -> Result<Self, AppError> {
        let executable = service_executable()?;
        let mut command = Command::new(executable);
        command.args(["--managed", "--config"]).arg(config_path);
        if let Some(directory) = std::env::current_exe()
            .ok()
            .and_then(|path| crate::portable::root(&path))
        {
            let log = crate::portable::service_log(&directory)?;
            let errors = log
                .try_clone()
                .map_err(|error| AppError::unavailable(error.to_string()))?;
            command.stdout(Stdio::from(log)).stderr(Stdio::from(errors));
        }
        if let Some(api_key) = api_key {
            command.env(api_key_env, api_key);
        }
        let service = Self::start_command(&mut command)?;
        eprintln!("AIex: managed service started (pid={})", service.child.id());
        Ok(service)
    }

    fn start_command(command: &mut Command) -> Result<Self, AppError> {
        hide_console(command);
        let child = command.stdin(Stdio::piped()).spawn().map_err(|error| {
            AppError::unavailable(format!("cannot start ai-ex-service: {error}"))
        })?;
        Ok(Self {
            child,
            exit_reported: false,
        })
    }

    /// Observe only our own child, without blocking the window or restarting it.
    pub fn take_failure(&mut self) -> Option<String> {
        if self.exit_reported {
            return None;
        }
        let detail = match self.child.try_wait() {
            Ok(None) => return None,
            Ok(Some(status)) => format!("后台服务已退出（{status}）。"),
            Err(error) => format!("无法检查后台服务状态：{error}。"),
        };
        self.exit_reported = true;
        let mut message = format!("{detail} 请打开“连接设置”检查后重试。");
        if let Some(root) = std::env::current_exe()
            .ok()
            .and_then(|path| crate::portable::root(&path))
        {
            message.push_str(&format!(
                " 日志：{}",
                root.join("data/logs/service.log").display()
            ));
        }
        Some(message)
    }

    fn shutdown(&mut self) -> Result<(), AppError> {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    eprintln!("AIex: owned service {} exited ({status})", self.child.id());
                    return Ok(());
                }
                Ok(Some(status)) => {
                    return Err(AppError::unavailable(format!(
                        "ai-ex-service exited: {status}"
                    )));
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(AppError::unavailable(format!(
                        "cannot inspect owned service: {error}"
                    )));
                }
            }
            if Instant::now() >= deadline {
                // Only terminate the exact process owned by this desktop.
                self.child.kill().map_err(|error| {
                    AppError::unavailable(format!("cannot stop owned service: {error}"))
                })?;
                self.child.wait().map_err(|error| {
                    AppError::unavailable(format!("cannot reap owned service: {error}"))
                })?;
                return Err(AppError::unavailable(
                    "owned service exceeded graceful shutdown deadline; process stopped",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ManagedService {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("AIex service shutdown: {error}");
        }
    }
}

fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ignored = command;
}

fn service_executable() -> Result<PathBuf, AppError> {
    let desktop = std::env::current_exe().map_err(|error| {
        AppError::unavailable(format!("cannot locate desktop executable: {error}"))
    })?;
    let name = format!("ai-ex-service{}", std::env::consts::EXE_SUFFIX);
    if let Some(sibling) = desktop
        .parent()
        .map(|parent| parent.join(&name))
        .filter(|path| path.is_file())
    {
        return Ok(sibling);
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| AppError::unavailable("cannot locate service workspace"))?;
    if !workspace.join("Cargo.toml").is_file() {
        return Err(AppError::unavailable(
            "ai-ex-service executable is missing; install it beside the desktop executable",
        ));
    }
    // Build first, then own the service directly. Owning `cargo run` would not
    // let us reliably reap its service child on shutdown.
    eprintln!("AIex: preparing the local service executable...");
    let mut build = Command::new("cargo");
    hide_console(&mut build);
    let status = build
        .args([
            "build",
            "-p",
            "ai-ex-service",
            "--locked",
            "--all-features",
            "--manifest-path",
        ])
        .arg(workspace.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", workspace.join("target"))
        .stdin(Stdio::null())
        .status()
        .map_err(|error| AppError::unavailable(format!("cannot build ai-ex-service: {error}")))?;
    let executable = workspace.join("target").join("debug").join(name);
    if !status.success() || !executable.is_file() {
        return Err(AppError::unavailable(format!(
            "ai-ex-service build failed: {status}"
        )));
    }
    Ok(executable)
}

#[cfg(test)]
#[path = "service_process_tests.rs"]
mod tests;
