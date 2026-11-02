use super::*;

impl NativeWorkerPool {
    pub async fn wait_ready(
        &self,
        selection: &ModelSelection,
        timeout: Duration,
    ) -> Result<(), AppError> {
        self.warmup(selection)?;
        let mut state = {
            let entries = self
                .inner
                .entries
                .lock()
                .map_err(|_| files::error("native worker registry is unavailable"))?;
            entries
                .get(&key(selection))
                .ok_or_else(|| AppError::unavailable("native worker disappeared"))?
                .state
                .subscribe()
        };
        tokio::time::timeout(timeout, async {
            loop {
                let current = state.borrow_and_update().clone();
                match current {
                    NativeWorkerState::Ready => return Ok(()),
                    NativeWorkerState::Error { message } => {
                        return Err(AppError::unavailable(message));
                    }
                    NativeWorkerState::Stopped => {
                        return Err(AppError::unavailable("native worker stopped"));
                    }
                    _ => {}
                }
                state
                    .changed()
                    .await
                    .map_err(|_| AppError::unavailable("native worker status closed"))?;
            }
        })
        .await
        .map_err(|_| AppError::unavailable("native warmup wait timed out"))?
    }

    pub async fn unload(&self, selection: &ModelSelection) -> Result<(), AppError> {
        let entry = self
            .inner
            .entries
            .lock()
            .map_err(|_| files::error("native worker registry is unavailable"))?
            .remove(&key(selection));
        if let Some(entry) = entry {
            stop(entry).await?;
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), AppError> {
        self.inner
            .closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let entries = {
            let mut registry = self
                .inner
                .entries
                .lock()
                .map_err(|_| files::error("native worker registry is unavailable"))?;
            std::mem::take(&mut *registry)
        };
        for entry in entries.values() {
            let _stop = entry.stop.send(true);
        }
        let mut failure = None;
        for entry in entries.into_values() {
            if let Err(error) = stop(entry).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

async fn stop(entry: Arc<Entry>) -> Result<(), AppError> {
    let _signal = entry.stop.send(true);
    if let Some(task) = entry.task.lock().await.take() {
        task.await.map_err(|error| {
            AppError::unavailable(format!("native worker task failed: {error}"))
        })??;
    }
    Ok(())
}
