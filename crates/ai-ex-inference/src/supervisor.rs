use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use ai_ex_domain::AppError;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;

use crate::{
    InferenceOutput, InferenceRequest, WORKER_PROTOCOL_VERSION, WorkerCommand, WorkerReply,
    read_frame, write_frame,
};

const CANCELLATION_GRACE: Duration = Duration::from_secs(1);
const STDERR_TAIL_BYTES: usize = 8192;
const STDERR_DRAIN_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancellationOutcome {
    Idle,
    Acknowledged,
    Terminated,
}

/// Paths come from the host's verified runtime package, never a PATH lookup.
pub struct WorkerLaunch {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub job_directory: PathBuf,
    pub ready_timeout: Duration,
}

/// One model worker and at most one in-flight request. Dropping it kills its child.
pub struct InferenceWorker {
    child: Child,
    input: ChildStdin,
    replies: mpsc::Receiver<Result<WorkerReply, AppError>>,
    stdout_task: JoinHandle<()>,
    stderr_task: Option<JoinHandle<()>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    active: Option<InferenceRequest>,
    cancel_requested: bool,
    terminated: bool,
    exit_status: Option<ExitStatus>,
    forced_termination: bool,
}

impl InferenceWorker {
    pub async fn spawn(launch: WorkerLaunch) -> Result<Self, AppError> {
        let (_source, cancellation) = watch::channel(false);
        Self::spawn_cancellable(launch, cancellation).await
    }

    /// Cancelling a cold model load explicitly terminates and reaps the child
    /// before returning, so its package leases can be released safely.
    pub async fn spawn_cancellable(
        launch: WorkerLaunch,
        mut cancellation: watch::Receiver<bool>,
    ) -> Result<Self, AppError> {
        if *cancellation.borrow() {
            return Err(AppError::unavailable("native worker startup cancelled"));
        }
        if !launch.executable.is_absolute()
            || !launch.executable.is_file()
            || !launch.job_directory.is_absolute()
            || !launch.job_directory.is_dir()
            || launch.ready_timeout.is_zero()
        {
            return Err(AppError::configuration(
                "invalid native worker launch paths or timeout",
            ));
        }
        let mut command = Command::new(&launch.executable);
        command
            .args(&launch.arguments)
            .envs(&launch.environment)
            .current_dir(&launch.job_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.as_std_mut().creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().map_err(|error| {
            AppError::unavailable(format!("cannot start native worker: {error}"))
        })?;
        let input = child.stdin.take().expect("worker stdin was piped");
        let stdout = child.stdout.take().expect("worker stdout was piped");
        let stderr_pipe = child.stderr.take().expect("worker stderr was piped");
        let (sender, replies) = mpsc::channel(8);
        let stdout_task = tokio::spawn(read_replies(stdout, sender));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let stderr_task = tokio::spawn(drain_stderr(stderr_pipe, stderr.clone()));
        let mut worker = Self {
            child,
            input,
            replies,
            stdout_task,
            stderr_task: Some(stderr_task),
            stderr,
            active: None,
            cancel_requested: false,
            terminated: false,
            exit_status: None,
            forced_termination: false,
        };
        let ready = tokio::select! {
            biased;
            _ = startup_cancelled(&mut cancellation) => None,
            ready = tokio::time::timeout(launch.ready_timeout, worker.receive()) => Some(ready),
        };
        let mut error = match ready {
            Some(Ok(Ok(WorkerReply::Ready { protocol_version })))
                if protocol_version == WORKER_PROTOCOL_VERSION =>
            {
                return Ok(worker);
            }
            Some(Ok(Ok(_))) => AppError::protocol("native worker handshake is incompatible"),
            Some(Ok(Err(error))) => error,
            Some(Err(_)) => AppError::unavailable("native worker startup timed out"),
            None => AppError::unavailable("native worker startup cancelled"),
        };
        if let Err(cleanup) = worker.terminate().await {
            error
                .message
                .push_str(&format!("; cleanup failed: {}", cleanup.message));
        }
        // Preserve the original kind and handshake/load failure. Launch arguments,
        // environment and protocol input are not included in this diagnostic.
        let diagnostics = serde_json::json!({
            "exit_code": worker.exit_status().and_then(|status| status.code()),
            "exit_status": worker.exit_status().map(|status| status.to_string()),
            "forced": worker.was_force_terminated(),
            "stderr_tail": worker.diagnostics().await,
        });
        error
            .message
            .push_str(&format!("; native startup diagnostics: {diagnostics}"));
        Err(error)
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.id()
    }

    /// The observed status is retained after the owned child has been reaped.
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit_status
    }

    /// Distinguishes a host-requested kill from an observed natural exit.
    pub fn was_force_terminated(&self) -> bool {
        self.forced_termination
    }

    pub async fn diagnostics(&self) -> String {
        let tail = self.stderr.lock().await;
        let text = String::from_utf8_lossy(&tail);
        let mut start = text.len().saturating_sub(STDERR_TAIL_BYTES);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text[start..].to_owned()
    }

    async fn finish_stderr(&mut self) {
        if let Some(mut task) = self.stderr_task.take()
            && tokio::time::timeout(STDERR_DRAIN_GRACE, &mut task)
                .await
                .is_err()
        {
            task.abort();
        }
    }

    pub async fn start(&mut self, request: InferenceRequest) -> Result<(), AppError> {
        if self.terminated || self.active.is_some() {
            return Err(AppError::invalid_transition(
                "native worker is stopped or busy",
            ));
        }
        request.validate()?;
        self.active = Some(request.clone());
        if let Err(error) = write_frame(&mut self.input, &WorkerCommand::Run { request }).await {
            self.terminate().await?;
            return Err(error);
        }
        Ok(())
    }

    /// A timed-out send may have written only part of its frame. Stop the child
    /// instead of sending another command through that incomplete stream.
    pub async fn start_until(
        &mut self,
        request: InferenceRequest,
        deadline: tokio::time::Instant,
    ) -> Result<(), AppError> {
        match tokio::time::timeout_at(deadline, self.start(request)).await {
            Ok(result) => result,
            Err(_) => {
                self.terminate().await?;
                Err(AppError::unavailable("native request send timed out"))
            }
        }
    }

    /// Safe to select against cancellation: a dedicated task owns frame assembly.
    pub async fn wait(&mut self) -> Result<InferenceOutput, AppError> {
        if self.cancel_requested {
            return Err(AppError::invalid_transition(
                "native request cancellation is pending",
            ));
        }
        let request = self
            .active
            .clone()
            .ok_or_else(|| AppError::invalid_transition("native worker has no active request"))?;
        let reply = match self.receive().await {
            Ok(reply) => reply,
            Err(error) => {
                self.terminate().await?;
                return Err(error);
            }
        };
        match reply {
            WorkerReply::Completed { identity, output }
                if identity == request.identity && output.matches_task(&request.task) =>
            {
                self.active = None;
                Ok(output)
            }
            WorkerReply::Failed { identity, error } if identity == request.identity => {
                self.active = None;
                Err(error)
            }
            _ => {
                self.terminate().await?;
                Err(AppError::protocol(
                    "native reply does not match the active request",
                ))
            }
        }
    }

    pub async fn cancel(&mut self) -> Result<CancellationOutcome, AppError> {
        let Some(identity) = self.active.as_ref().map(|request| request.identity) else {
            return Ok(CancellationOutcome::Idle);
        };
        self.cancel_requested = true;
        let acknowledgement = tokio::time::timeout(CANCELLATION_GRACE, async {
            write_frame(&mut self.input, &WorkerCommand::Cancel { identity }).await?;
            loop {
                match self.receive().await? {
                    WorkerReply::Cancelled { identity: reply } if reply == identity => {
                        return Ok(());
                    }
                    WorkerReply::Completed {
                        identity: reply, ..
                    }
                    | WorkerReply::Failed {
                        identity: reply, ..
                    } if reply == identity => {}
                    _ => return Err(AppError::protocol("native cancellation reply mismatch")),
                }
            }
        })
        .await;
        if matches!(acknowledgement, Ok(Ok(()))) {
            self.active = None;
            self.cancel_requested = false;
            Ok(CancellationOutcome::Acknowledged)
        } else {
            self.terminate().await?;
            Ok(CancellationOutcome::Terminated)
        }
    }

    async fn receive(&mut self) -> Result<WorkerReply, AppError> {
        self.replies.recv().await.unwrap_or_else(|| {
            Err(AppError::unavailable(
                "native worker closed its reply stream",
            ))
        })
    }

    /// Give the owned worker a bounded opportunity to release native resources.
    /// An unsuccessful shutdown is reported after reaping the child.
    pub async fn shutdown(&mut self) -> Result<(), AppError> {
        if self.terminated {
            return Ok(());
        }
        if self.active.is_some() {
            self.cancel().await?;
            if self.terminated {
                return Ok(());
            }
        }
        let stopped = tokio::time::timeout(CANCELLATION_GRACE, async {
            write_frame(&mut self.input, &WorkerCommand::Shutdown).await?;
            self.child.wait().await.map_err(|error| {
                AppError::unavailable(format!("cannot wait for native worker: {error}"))
            })
        })
        .await;
        match stopped {
            Ok(Ok(status)) => {
                self.exit_status = Some(status);
                self.terminated = true;
            }
            _ => self.terminate().await?,
        }
        self.finish_stderr().await;
        if let Some(status) = self.exit_status
            && !status.success()
        {
            return Err(AppError::unavailable(format!(
                "native worker exited unsuccessfully during shutdown: {status}"
            )));
        }
        Ok(())
    }

    pub async fn terminate(&mut self) -> Result<(), AppError> {
        self.active = None;
        self.cancel_requested = false;
        if !self.terminated {
            let exited = self.child.try_wait().map_err(|error| {
                AppError::unavailable(format!("cannot query native worker: {error}"))
            })?;
            self.exit_status = Some(match exited {
                Some(status) => status,
                None => {
                    self.child.kill().await.map_err(|error| {
                        AppError::unavailable(format!("cannot stop native worker: {error}"))
                    })?;
                    self.forced_termination = true;
                    self.child.wait().await.map_err(|error| {
                        AppError::unavailable(format!("cannot reap native worker: {error}"))
                    })?
                }
            });
            self.terminated = true;
        }
        self.finish_stderr().await;
        Ok(())
    }
}

async fn startup_cancelled(cancellation: &mut watch::Receiver<bool>) {
    loop {
        let cancelled = *cancellation.borrow_and_update();
        if cancelled || cancellation.changed().await.is_err() {
            return;
        }
    }
}

impl Drop for InferenceWorker {
    fn drop(&mut self) {
        self.stdout_task.abort();
        if let Some(task) = &self.stderr_task {
            task.abort();
        }
    }
}

async fn read_replies(
    stdout: tokio::process::ChildStdout,
    sender: mpsc::Sender<Result<WorkerReply, AppError>>,
) {
    let mut stdout = BufReader::new(stdout);
    loop {
        let reply = match read_frame::<_, WorkerReply>(&mut stdout).await {
            Ok(Some(reply)) => reply,
            Ok(None) => break,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                break;
            }
        };
        if let Err(error) = reply.validate() {
            let _ = sender.send(Err(error)).await;
            break;
        }
        if sender.send(Ok(reply)).await.is_err() {
            break;
        }
    }
}

async fn drain_stderr(mut pipe: tokio::process::ChildStderr, tail: Arc<Mutex<Vec<u8>>>) {
    let mut buffer = [0_u8; 2048];
    while let Ok(count) = pipe.read(&mut buffer).await {
        if count == 0 {
            break;
        }
        let mut tail = tail.lock().await;
        let discard = (tail.len() + count).saturating_sub(STDERR_TAIL_BYTES);
        tail.drain(..discard);
        tail.extend_from_slice(&buffer[..count]);
    }
}
