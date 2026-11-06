//! Owns at most one isolated, cancellable backup inspection process.
//!
//! The service must await `shutdown` before dropping its runtime. Termination
//! affects only the spawned child; failed or unconfirmed reaping disables later
//! inspections. A forced parent exit does not guarantee `kill_on_drop` runs,
//! and an unresponsive OS driver may delay child termination and cleanup.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ai_ex_control::LegacyMemoryBackupInspection;
use ai_ex_domain::AppError;
use ai_ex_memory::BackupInspection;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{Semaphore, oneshot, watch};

use crate::backup_inspection_worker::{MAX_FRAME, Reply, Request, WORKER_MODE};

const REAP_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(crate) struct BackupInspector {
    inner: Arc<Inner>,
}

pub(crate) struct PendingInspection(oneshot::Receiver<Result<Reply, AppError>>);

pub(crate) enum CandidateOutcome {
    Written { temporary_retained: bool },
    Rejected,
    Uncertain,
}

impl PendingInspection {
    pub(crate) async fn wait(self) -> Result<BackupInspection, AppError> {
        match self.0.await.map_err(|_| unavailable())?? {
            Reply::Ready { summary } => Ok(summary),
            _ => Err(invalid()),
        }
    }

    pub(crate) async fn wait_legacy(self) -> Result<LegacyMemoryBackupInspection, AppError> {
        match self.0.await.map_err(|_| unavailable())?? {
            Reply::LegacyReady { summary, .. } => Ok(summary),
            _ => Err(invalid()),
        }
    }

    pub(crate) async fn wait_legacy_digest(
        self,
    ) -> Result<(LegacyMemoryBackupInspection, [u8; 32]), AppError> {
        match self.0.await.map_err(|_| unavailable())?? {
            Reply::LegacyReady { summary, digest } => Ok((summary, digest)),
            _ => Err(invalid()),
        }
    }

    pub(crate) async fn wait_candidate(self) -> Result<CandidateOutcome, AppError> {
        match self.0.await.map_err(|_| unavailable())?? {
            Reply::CandidateWritten { temporary_retained } => {
                Ok(CandidateOutcome::Written { temporary_retained })
            }
            Reply::CandidateRejected => Ok(CandidateOutcome::Rejected),
            Reply::CandidateUncertain => Ok(CandidateOutcome::Uncertain),
            _ => Err(invalid()),
        }
    }
}

struct Inner {
    executable: PathBuf,
    arguments: Vec<String>,
    slots: Arc<Semaphore>,
    stopping: watch::Sender<bool>,
    closed: AtomicBool,
    poisoned: AtomicBool,
    #[cfg(test)]
    reaped: AtomicBool,
    #[cfg(test)]
    started: AtomicBool,
}

impl BackupInspector {
    pub(crate) fn new(executable: PathBuf) -> Result<Self, AppError> {
        if !executable.is_absolute() {
            return Err(AppError::configuration(
                "backup worker requires an absolute executable",
            ));
        }
        let (stopping, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(Inner {
                executable,
                arguments: vec![WORKER_MODE.to_owned()],
                slots: Arc::new(Semaphore::new(1)),
                stopping,
                closed: AtomicBool::new(false),
                poisoned: AtomicBool::new(false),
                #[cfg(test)]
                reaped: AtomicBool::new(false),
                #[cfg(test)]
                started: AtomicBool::new(false),
            }),
        })
    }

    pub(crate) fn start(
        &self,
        request: Request,
        deadline: Duration,
    ) -> Result<PendingInspection, AppError> {
        if self.inner.closed.load(Ordering::Acquire)
            || self.inner.poisoned.load(Ordering::Acquire)
            || deadline.is_zero()
        {
            return Err(unavailable());
        }
        let encoded = serde_json::to_vec(&request).map_err(|_| invalid())?;
        if encoded.is_empty() || encoded.len() > MAX_FRAME {
            return Err(invalid());
        }
        let permit = self
            .inner
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::unavailable("backup inspection is busy"))?;
        if self.inner.closed.load(Ordering::Acquire) || self.inner.poisoned.load(Ordering::Acquire)
        {
            return Err(unavailable());
        }
        let inner = self.inner.clone();
        let mut stop = inner.stopping.subscribe();
        let (mut response, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let result = run_owned(&inner, &encoded, deadline, &mut stop, &mut response).await;
            drop(permit);
            let _delivered = response.send(result);
        });
        Ok(PendingInspection(receiver))
    }

    #[cfg(test)]
    pub(crate) async fn inspect(
        &self,
        request: Request,
        deadline: Duration,
    ) -> Result<BackupInspection, AppError> {
        self.start(request, deadline)?.wait().await
    }

    pub(crate) async fn shutdown(&self) -> Result<(), AppError> {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.stopping.send_replace(true);
        let permit = tokio::time::timeout(REAP_TIMEOUT + REAP_TIMEOUT, self.inner.slots.acquire())
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
        drop(permit);
        if self.inner.poisoned.load(Ordering::Acquire) {
            Err(unavailable())
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn for_test(executable: PathBuf, arguments: Vec<String>) -> Self {
        let (stopping, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                executable,
                arguments,
                slots: Arc::new(Semaphore::new(1)),
                stopping,
                closed: AtomicBool::new(false),
                poisoned: AtomicBool::new(false),
                reaped: AtomicBool::new(false),
                started: AtomicBool::new(false),
            }),
        }
    }
}

fn unavailable() -> AppError {
    AppError::unavailable("backup inspection worker unavailable")
}

fn invalid() -> AppError {
    AppError::configuration("backup inspection request or response is invalid")
}

async fn run_owned(
    inner: &Inner,
    encoded: &[u8],
    deadline: Duration,
    stop: &mut watch::Receiver<bool>,
    response: &mut oneshot::Sender<Result<Reply, AppError>>,
) -> Result<Reply, AppError> {
    if *stop.borrow() || response.is_closed() {
        return Err(unavailable());
    }
    let mut command = Command::new(&inner.executable);
    command
        .args(&inner.arguments)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command
            .env("SystemRoot", &system_root)
            .env("WINDIR", system_root);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.as_std_mut().creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|_| unavailable())?;
    #[cfg(test)]
    inner.started.store(true, Ordering::Release);
    let outcome = tokio::select! {
        biased;
        _ = stopped(stop) => Err(unavailable()),
        _ = response.closed() => Err(unavailable()),
        result = tokio::time::timeout(deadline, exchange(&mut child, encoded)) => {
            result.unwrap_or_else(|_| Err(unavailable()))
        }
    };
    if reap(&mut child).await.is_err() {
        inner.poisoned.store(true, Ordering::Release);
        return Err(unavailable());
    }
    #[cfg(test)]
    inner.reaped.store(true, Ordering::Release);
    outcome
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() || stop.changed().await.is_err() {
            return;
        }
    }
}

async fn exchange(child: &mut Child, encoded: &[u8]) -> Result<Reply, AppError> {
    let mut input = child.stdin.take().ok_or_else(unavailable)?;
    input
        .write_u32(encoded.len() as u32)
        .await
        .map_err(|_| unavailable())?;
    input.write_all(encoded).await.map_err(|_| unavailable())?;
    input.shutdown().await.map_err(|_| unavailable())?;
    drop(input);
    let mut output = child.stdout.take().ok_or_else(unavailable)?;
    let length = output.read_u32().await.map_err(|_| unavailable())? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(invalid());
    }
    let mut frame = vec![0; length];
    output
        .read_exact(&mut frame)
        .await
        .map_err(|_| unavailable())?;
    let mut extra = [0; 1];
    if output.read(&mut extra).await.map_err(|_| unavailable())? != 0 {
        return Err(invalid());
    }
    let status = child.wait().await.map_err(|_| unavailable())?;
    if !status.success() {
        return Err(unavailable());
    }
    match serde_json::from_slice::<Reply>(&frame).map_err(|_| invalid())? {
        Reply::Ready { summary } if valid_summary(&summary) => Ok(Reply::Ready { summary }),
        Reply::LegacyReady { summary, digest } if valid_legacy_summary(&summary) => {
            Ok(Reply::LegacyReady { summary, digest })
        }
        Reply::CandidateWritten { temporary_retained } => {
            Ok(Reply::CandidateWritten { temporary_retained })
        }
        Reply::CandidateRejected => Ok(Reply::CandidateRejected),
        Reply::CandidateUncertain => Ok(Reply::CandidateUncertain),
        _ => Err(invalid()),
    }
}

fn valid_legacy_summary(summary: &LegacyMemoryBackupInspection) -> bool {
    summary.records <= 50_000
        && summary
            .local_timestamp_records
            .saturating_add(summary.unclassified_timestamp_records)
            <= summary.records
        && summary
            .nonempty_embedding_records
            .saturating_add(summary.unclassified_embedding_records)
            <= summary.records
}

fn valid_summary(summary: &BackupInspection) -> bool {
    summary.memory_records <= 50_000
        && summary.entity_candidates <= 50_000
        && summary.source_associations <= 50_000
        && summary.inactive_source_associations <= summary.source_associations
        && summary.embedded_records <= summary.memory_records
        && summary.distinct_embedding_spaces <= 16
        && summary
            .matching_embeddings
            .is_none_or(|count| count <= summary.embedded_records)
}

async fn reap(child: &mut Child) -> Result<(), AppError> {
    if child.try_wait().map_err(|_| unavailable())?.is_some() {
        return Ok(());
    }
    if child.start_kill().is_err() && child.try_wait().map_err(|_| unavailable())?.is_none() {
        return Err(unavailable());
    }
    tokio::time::timeout(REAP_TIMEOUT, child.wait())
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    Ok(())
}

#[cfg(test)]
#[path = "backup_inspection_coordinator_tests.rs"]
mod tests;
