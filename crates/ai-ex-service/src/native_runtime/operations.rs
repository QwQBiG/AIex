use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use ai_ex_domain::{NativeOperation, NativeOperationPhase};
use tokio::sync::watch;
use uuid::Uuid;

use super::*;

#[derive(Default)]
pub(super) struct Operations {
    inner: Arc<Mutex<Book>>,
    changed: Arc<Notify>,
}

#[derive(Default)]
struct Book {
    entries: BTreeMap<Uuid, Entry>,
    closed: bool,
}
struct Entry {
    snapshot: NativeOperation,
    cancel: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub(super) struct Context {
    pub id: Uuid,
    operations: Arc<Mutex<Book>>,
    changed: Arc<Notify>,
    cancel: watch::Receiver<bool>,
}

impl Operations {
    pub fn has_active(&self) -> Result<bool, AppError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| failure())?
            .entries
            .values()
            .any(|entry| !entry.snapshot.phase.is_terminal()))
    }
    pub fn reserve(
        &self,
        package_id: &str,
        phase: NativeOperationPhase,
    ) -> Result<Context, AppError> {
        let mut book = self.inner.lock().map_err(|_| failure())?;
        if book.closed {
            return Err(AppError::unavailable("原生设置任务已停止"));
        }
        if book
            .entries
            .values()
            .filter(|entry| !entry.snapshot.phase.is_terminal())
            .count()
            >= 4
        {
            return Err(AppError::unavailable("请等待当前原生操作完成"));
        }
        if book.entries.len() >= 64 {
            let completed: Vec<_> = book
                .entries
                .iter()
                .filter(|(_, entry)| entry.snapshot.phase.is_terminal())
                .take(32)
                .map(|(id, _)| *id)
                .collect();
            for id in completed {
                book.entries.remove(&id);
            }
        }
        let id = Uuid::new_v4();
        let (cancel, cancellation) = watch::channel(false);
        book.entries.insert(
            id,
            Entry {
                snapshot: NativeOperation {
                    id,
                    package_id: package_id.into(),
                    phase,
                    completed_bytes: 0,
                    total_bytes: 0,
                    error: None,
                },
                cancel,
                task: None,
            },
        );
        Ok(Context {
            id,
            operations: self.inner.clone(),
            changed: self.changed.clone(),
            cancel: cancellation,
        })
    }

    pub fn launch<F>(&self, context: Context, future: F) -> Result<(), AppError>
    where
        F: Future<Output = Result<(), AppError>> + Send + 'static,
    {
        let mut book = self.inner.lock().map_err(|_| failure())?;
        let entry = book.entries.get_mut(&context.id).ok_or_else(failure)?;
        entry.task = Some(tokio::spawn(async move {
            let result = future.await;
            context.finish(result);
        }));
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<NativeOperation>, AppError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| failure())?
            .entries
            .get(&id)
            .map(|entry| entry.snapshot.clone()))
    }

    pub fn cancel(&self, id: Uuid) -> Result<Option<bool>, AppError> {
        let mut book = self.inner.lock().map_err(|_| failure())?;
        let Some(entry) = book.entries.get_mut(&id) else {
            return Ok(None);
        };
        if entry.snapshot.phase.is_terminal()
            || entry.snapshot.phase == NativeOperationPhase::Committing
        {
            return Ok(Some(false));
        }
        entry.snapshot.phase = NativeOperationPhase::Cancelling;
        let _signal = entry.cancel.send(true);
        self.changed.notify_waiters();
        Ok(Some(true))
    }

    pub async fn wait(&self, id: Uuid) -> Result<(), AppError> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .get(id)?
                .is_none_or(|operation| operation.phase.is_terminal())
            {
                return Ok(());
            }
            notified.await;
        }
    }

    pub async fn shutdown(&self) -> Result<(), AppError> {
        let tasks: Vec<_> = {
            let mut book = self.inner.lock().map_err(|_| failure())?;
            book.closed = true;
            for entry in book.entries.values_mut() {
                if !entry.snapshot.phase.is_terminal()
                    && entry.snapshot.phase != NativeOperationPhase::Committing
                {
                    let _signal = entry.cancel.send(true);
                }
            }
            book.entries
                .values_mut()
                .filter_map(|entry| entry.task.take())
                .collect()
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            for task in tasks {
                task.await
                    .map_err(|error| AppError::unavailable(format!("原生任务退出：{error}")))?;
            }
            Ok::<_, AppError>(())
        })
        .await
        .map_err(|_| AppError::unavailable("原生任务尚未完成取消与回收"))?
    }
}

impl Context {
    pub fn cancel(&self) -> Result<(), AppError> {
        let mut book = self.operations.lock().map_err(|_| failure())?;
        let entry = book.entries.get_mut(&self.id).ok_or_else(failure)?;
        if !entry.snapshot.phase.is_terminal()
            && entry.snapshot.phase != NativeOperationPhase::Committing
        {
            entry.snapshot.phase = NativeOperationPhase::Cancelling;
            let _signal = entry.cancel.send(true);
        }
        Ok(())
    }
    pub fn check(&self) -> Result<(), AppError> {
        if *self.cancel.borrow() {
            Err(AppError::invalid_transition("原生操作已取消"))
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&self) {
        let mut cancel = self.cancel.clone();
        if *cancel.borrow_and_update() {
            return;
        }
        let _changed = cancel.changed().await;
    }
    pub fn committing(&self) -> Result<(), AppError> {
        let mut book = self.operations.lock().map_err(|_| failure())?;
        self.check()?;
        book.entries
            .get_mut(&self.id)
            .ok_or_else(failure)?
            .snapshot
            .phase = NativeOperationPhase::Committing;
        Ok(())
    }
    fn finish(&self, result: Result<(), AppError>) {
        if let Ok(mut book) = self.operations.lock()
            && let Some(entry) = book.entries.get_mut(&self.id)
        {
            let cancelled = *self.cancel.borrow()
                && result
                    .as_ref()
                    .is_err_and(|error| error.kind == ai_ex_domain::ErrorKind::InvalidTransition);
            entry.snapshot.phase = match &result {
                Ok(()) => NativeOperationPhase::Completed,
                Err(_) if cancelled => NativeOperationPhase::Cancelled,
                Err(_) => NativeOperationPhase::Failed,
            };
            entry.snapshot.error = if cancelled {
                None
            } else {
                result.err().map(|error| bounded(error.message))
            };
        }
        self.changed.notify_waiters();
    }
}

fn failure() -> AppError {
    AppError::unavailable("原生操作记录不可用")
}
pub(super) fn bounded(mut text: String) -> String {
    let mut end = text.len().min(ai_ex_domain::NATIVE_MAX_ERROR_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}
