use ai_ex_platform_windows::desktop::{
    DesktopSystem, InputEvent, OwnedInput, RgbaFrame, WindowSnapshot,
};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Lazily constructs the platform adapter on an explicit window operation.
pub(crate) struct SystemDesktop {
    helper: PathBuf,
    jobs: PathBuf,
    #[cfg(windows)]
    inner: Option<ai_ex_platform_windows::native::WindowsDesktop>,
}
impl SystemDesktop {
    pub(crate) fn new(helper: PathBuf, jobs: PathBuf) -> Self {
        Self {
            helper,
            jobs,
            #[cfg(windows)]
            inner: None,
        }
    }
    #[cfg(windows)]
    fn get(&mut self) -> io::Result<&mut ai_ex_platform_windows::native::WindowsDesktop> {
        if self.inner.is_none() {
            self.inner = Some(ai_ex_platform_windows::native::WindowsDesktop::new(
                self.helper.clone(),
                self.jobs.clone(),
            )?);
        }
        Ok(self.inner.as_mut().expect("adapter initialized"))
    }
}
#[cfg(windows)]
impl DesktopSystem for SystemDesktop {
    fn windows(&mut self) -> io::Result<Vec<WindowSnapshot>> {
        self.get()?.windows()
    }
    fn inspect(&mut self, handle: u64) -> io::Result<Option<WindowSnapshot>> {
        self.get()?.inspect(handle)
    }
    fn capture(
        &mut self,
        target: &WindowSnapshot,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> io::Result<RgbaFrame> {
        self.get()?.capture(target, timeout, cancelled)
    }
    fn send_input(&mut self, events: &[InputEvent]) -> io::Result<usize> {
        self.get()?.send_input(events)
    }
    fn is_pressed(&mut self, input: OwnedInput) -> io::Result<bool> {
        self.get()?.is_pressed(input)
    }
    fn cursor(&mut self) -> io::Result<(i32, i32)> {
        self.get()?.cursor()
    }
    fn shutdown(&mut self) -> io::Result<()> {
        match &mut self.inner {
            Some(inner) => inner.shutdown(),
            None => Ok(()),
        }
    }
}
#[cfg(not(windows))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "native desktop requires Windows",
    )
}
#[cfg(not(windows))]
impl DesktopSystem for SystemDesktop {
    fn windows(&mut self) -> io::Result<Vec<WindowSnapshot>> {
        let _paths = (&self.helper, &self.jobs);
        Err(unsupported())
    }
    fn inspect(&mut self, _: u64) -> io::Result<Option<WindowSnapshot>> {
        Err(unsupported())
    }
    fn capture(
        &mut self,
        _: &WindowSnapshot,
        _: Duration,
        _: &AtomicBool,
    ) -> io::Result<RgbaFrame> {
        Err(unsupported())
    }
    fn send_input(&mut self, _: &[InputEvent]) -> io::Result<usize> {
        Err(unsupported())
    }
    fn is_pressed(&mut self, _: OwnedInput) -> io::Result<bool> {
        Err(unsupported())
    }
    fn cursor(&mut self) -> io::Result<(i32, i32)> {
        Err(unsupported())
    }
}

pub(crate) fn create(
    instance: uuid::Uuid,
    config: &ai_ex_config::AppConfig,
    speech: ai_ex_audio::SpeechActivity,
    safety: std::sync::Arc<ai_ex_safety::SafetyGate>,
    vision: Option<crate::strategy_loop::StrategyProviderFactory>,
) -> Result<super::NativeDesktopHost, ai_ex_domain::AppError> {
    let executable = std::env::current_exe().map_err(super::io_error)?;
    let helper = executable
        .parent()
        .ok_or_else(|| ai_ex_domain::AppError::unavailable("service directory unavailable"))?
        .join("ai-ex-platform-helper.exe");
    let audit = std::path::absolute(&config.safety.audit_path).map_err(super::io_error)?;
    let jobs = audit
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("capture-jobs");
    // A distinct durable stream avoids concurrent append ownership with plugins.
    let native_audit = audit.with_file_name(format!(
        "{}.native.jsonl",
        audit.file_name().unwrap_or_default().to_string_lossy()
    ));
    Ok(super::NativeHost::new(
        instance,
        SystemDesktop::new(helper, jobs),
        safety,
        speech,
        native_audit,
        vision,
    ))
}
