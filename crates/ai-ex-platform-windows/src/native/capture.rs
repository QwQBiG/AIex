use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::desktop::{DesktopSystem, InputEvent, OwnedInput, RgbaFrame, WindowSnapshot};
use crate::{OwnedJobProcess, ProcessLaunch};

const MAGIC: &[u8; 8] = b"AEXCAP01";
static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(1);

struct CaptureJob {
    process: OwnedJobProcess,
    directory: PathBuf,
}

pub struct WindowsDesktop {
    helper: PathBuf,
    jobs: PathBuf,
    pending: Option<CaptureJob>,
}

impl WindowsDesktop {
    pub fn new(helper: PathBuf, jobs: PathBuf) -> io::Result<Self> {
        if !helper.is_absolute() || !jobs.is_absolute() || !helper.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "absolute helper file and jobs directory required",
            ));
        }
        plain_ancestors(&helper)?;
        plain_ancestors(&jobs)?;
        fs::create_dir_all(&jobs)?;
        plain_ancestors(&jobs)?;
        Ok(Self {
            helper,
            jobs,
            pending: None,
        })
    }

    fn directory(&self) -> io::Result<PathBuf> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let sequence = NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed);
        let directory = self
            .jobs
            .join(format!("capture-{}-{now}-{sequence}", std::process::id()));
        fs::create_dir(&directory)?;
        Ok(directory)
    }

    fn stop_pending(&mut self) -> io::Result<()> {
        let Some(job) = &self.pending else {
            return Ok(());
        };
        if job.process.active_processes()? != 0 {
            job.process.terminate()?;
        }
        let started = Instant::now();
        // Job accounting may reach zero while the primary process still owns
        // its current-directory handle. Wait for the process handle as well
        // before removing the private worker directory.
        while job.process.active_processes()? != 0 || job.process.exit_code()?.is_none() {
            if started.elapsed() >= Duration::from_secs(1) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "owned capture worker is still stopping",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let job = self.pending.take().expect("capture job remains owned");
        drop(job.process);
        fs::remove_dir_all(job.directory)
    }

    fn capture_worker(
        &mut self,
        target: &WindowSnapshot,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> io::Result<RgbaFrame> {
        self.stop_pending()?;
        if cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "capture cancelled",
            ));
        }
        let directory = self.directory()?;
        let output = directory.join("frame.bin");
        let mut arguments: Vec<OsString> = super::helper::capture_arguments(target)
            .into_iter()
            .map(Into::into)
            .collect();
        arguments.push(output.as_os_str().to_owned());
        let mut environment = std::collections::BTreeMap::new();
        if let Some(system) = std::env::var_os("SystemRoot") {
            environment.insert("SystemRoot".into(), system.clone());
            environment.insert("WINDIR".into(), system.clone());
            environment.insert(
                "PATH".into(),
                format!(
                    "{}\\System32;{}",
                    system.to_string_lossy(),
                    system.to_string_lossy()
                )
                .into(),
            );
        }
        environment.insert("TEMP".into(), directory.as_os_str().to_owned());
        environment.insert("TMP".into(), directory.as_os_str().to_owned());
        let launch = ProcessLaunch {
            executable: self.helper.clone(),
            arguments,
            directory: directory.clone(),
            environment,
        };
        let process = match OwnedJobProcess::spawn(launch) {
            Ok(process) => process,
            Err(error) => {
                let _cleanup = fs::remove_dir(&directory);
                return Err(error);
            }
        };
        self.pending = Some(CaptureJob { process, directory });
        let result = self
            .wait_worker(timeout, cancelled)
            .and_then(|_| read_frame(&output, target));
        let cleanup = self.stop_pending();
        match (result, cleanup) {
            (Ok(_), Ok(())) if cancelled.load(Ordering::Acquire) => Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "capture cancelled before delivery",
            )),
            (Ok(frame), Ok(())) => Ok(frame),
            (Err(error), Ok(())) => Err(error),
            (result, Err(cleanup)) => Err(io::Error::other(format!(
                "capture result={}; cleanup: {cleanup}",
                result.as_ref().map(|_| "ready").unwrap_or("failed")
            ))),
        }
    }

    fn wait_worker(&self, timeout: Duration, cancelled: &AtomicBool) -> io::Result<()> {
        if timeout.is_zero() || timeout > Duration::from_secs(5) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "capture timeout must be in 1..=5000 ms",
            ));
        }
        let process = &self.pending.as_ref().expect("capture worker owned").process;
        let started = Instant::now();
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "capture cancelled",
                ));
            }
            if let Some(code) = process.exit_code()? {
                return if code == 0 {
                    Ok(())
                } else {
                    let path = self
                        .pending
                        .as_ref()
                        .expect("capture job owned")
                        .directory
                        .join("error.txt");
                    let detail = plain_ancestors(&path)
                        .and_then(|()| fs::File::open(&path))
                        .and_then(|file| {
                            let mut detail = String::new();
                            file.take(4096).read_to_string(&mut detail).map(|_| detail)
                        })
                        .unwrap_or_default();
                    Err(io::Error::other(format!(
                        "capture helper exited {code}; frame not delivered: {detail}"
                    )))
                };
            }
            if started.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "capture helper timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl DesktopSystem for WindowsDesktop {
    fn windows(&mut self) -> io::Result<Vec<WindowSnapshot>> {
        super::inspect::windows()
    }
    fn inspect(&mut self, handle: u64) -> io::Result<Option<WindowSnapshot>> {
        super::inspect::inspect(handle)
    }
    fn capture(
        &mut self,
        target: &WindowSnapshot,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> io::Result<RgbaFrame> {
        self.capture_worker(target, timeout, cancelled)
    }
    fn send_input(&mut self, events: &[InputEvent]) -> io::Result<usize> {
        super::input::send(events)
    }
    fn is_pressed(&mut self, input: OwnedInput) -> io::Result<bool> {
        super::input::pressed(input)
    }
    fn cursor(&mut self) -> io::Result<(i32, i32)> {
        super::input::cursor()
    }
    fn shutdown(&mut self) -> io::Result<()> {
        self.stop_pending()
    }
}

impl Drop for WindowsDesktop {
    fn drop(&mut self) {
        let _cleanup = self.stop_pending();
    }
}

fn plain_ancestors(path: &std::path::Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_attributes() & 0x400 != 0 => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "capture paths cannot use reparse points",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_frame(path: &std::path::Path, target: &WindowSnapshot) -> io::Result<RgbaFrame> {
    plain_ancestors(path)?;
    let length = u64::from(target.client.width) * u64::from(target.client.height) * 4;
    if length == 0 || length > 64 * 1024 * 1024 || fs::metadata(path)?.len() != length + 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture output exceeds its declared dimensions",
        ));
    }
    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 16];
    file.read_exact(&mut header)?;
    if &header[..8] != MAGIC
        || u32::from_le_bytes(header[8..12].try_into().expect("header width"))
            != target.client.width
        || u32::from_le_bytes(header[12..16].try_into().expect("header height"))
            != target.client.height
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid capture header",
        ));
    }
    let mut rgba = vec![0; length as usize];
    file.read_exact(&mut rgba)?;
    Ok(RgbaFrame {
        width: target.client.width,
        height: target.client.height,
        rgba,
    })
}
