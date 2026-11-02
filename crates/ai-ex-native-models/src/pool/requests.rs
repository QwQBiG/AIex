use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use ai_ex_inference::RequestIdentity;
use uuid::Uuid;

use super::*;

#[derive(Clone)]
pub(super) struct RequestRecord {
    identity: RequestIdentity,
    cancellation: watch::Sender<bool>,
    finished: watch::Receiver<bool>,
    active: Arc<AtomicBool>,
}

pub(crate) struct RequestDone {
    requests: Arc<Mutex<HashMap<Uuid, RequestRecord>>>,
    identity: RequestIdentity,
    finished: watch::Sender<bool>,
    active: Arc<AtomicBool>,
}

pub(super) fn register(
    entry: &Entry,
    identity: RequestIdentity,
) -> Result<(watch::Receiver<bool>, Arc<AtomicBool>, RequestDone), AppError> {
    let (cancellation, receiver) = watch::channel(false);
    let (finished, done) = watch::channel(false);
    let active = Arc::new(AtomicBool::new(false));
    let mut requests = entry
        .requests
        .lock()
        .map_err(|_| files::error("native request registry unavailable"))?;
    if requests.contains_key(&identity.request_id) {
        return Err(AppError::invalid_transition(
            "native request ID is already in flight",
        ));
    }
    requests.insert(
        identity.request_id,
        RequestRecord {
            identity,
            cancellation,
            finished: done,
            active: active.clone(),
        },
    );
    Ok((
        receiver,
        active.clone(),
        RequestDone {
            requests: entry.requests.clone(),
            identity,
            finished,
            active,
        },
    ))
}

impl NativeWorkerPool {
    /// Cancels only this exact identity. It never unloads a model or affects a
    /// later session/generation, and active work is reaped before acknowledgement.
    pub async fn cancel(
        &self,
        selection: &ModelSelection,
        identity: RequestIdentity,
    ) -> Result<(), AppError> {
        identity.validate()?;
        let entry = self
            .inner
            .entries
            .lock()
            .map_err(|_| files::error("native worker registry unavailable"))?
            .get(&key(selection))
            .cloned();
        let Some(entry) = entry else {
            return Ok(());
        };
        let record = entry
            .requests
            .lock()
            .map_err(|_| files::error("native request registry unavailable"))?
            .get(&identity.request_id)
            .filter(|record| record.identity == identity)
            .cloned();
        let Some(mut record) = record else {
            return Ok(());
        };
        record.cancellation.send_replace(true);
        if !record.active.load(Ordering::SeqCst) {
            return Ok(());
        }
        loop {
            let finished = *record.finished.borrow_and_update();
            if finished {
                return Ok(());
            }
            record.finished.changed().await.map_err(|_| {
                AppError::unavailable("native cancellation ended without cleanup acknowledgement")
            })?;
        }
    }
}

impl Drop for RequestDone {
    fn drop(&mut self) {
        self.active.store(false, Ordering::SeqCst);
        if let Ok(mut requests) = self.requests.lock()
            && requests
                .get(&self.identity.request_id)
                .is_some_and(|record| record.identity == self.identity)
        {
            requests.remove(&self.identity.request_id);
        }
        self.finished.send_replace(true);
    }
}

pub(crate) async fn requested(receiver: &mut watch::Receiver<bool>) {
    loop {
        let cancelled = *receiver.borrow_and_update();
        if cancelled {
            return;
        }
        if receiver.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}
