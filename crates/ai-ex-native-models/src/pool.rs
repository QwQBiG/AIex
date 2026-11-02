use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ai_ex_config::native::ModelSelection;
use ai_ex_domain::AppError;
use ai_ex_inference::{InferenceOutput, InferenceRequest};
use ai_ex_model_manager::ModelManager;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::bundle::PreparedWorker;
use crate::files::{self, PrivateDirectory, RequestFiles};

mod lifecycle;
pub(crate) mod requests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NativeWorkerState {
    Cold,
    Loading,
    Ready,
    Error { message: String },
    Stopped,
}

#[derive(Debug, Clone)]
pub struct NativeWorkerOptions {
    pub ready_timeout: Duration,
    pub request_timeout: Duration,
}
impl Default for NativeWorkerOptions {
    fn default() -> Self {
        Self {
            ready_timeout: Duration::from_secs(120),
            request_timeout: Duration::from_secs(180),
        }
    }
}

#[derive(Clone)]
pub struct NativeWorkerPool {
    inner: Arc<Inner>,
}
struct Inner {
    closed: AtomicBool,
    manager: ModelManager,
    jobs: PathBuf,
    pub(crate) voices: PathBuf,
    options: NativeWorkerOptions,
    entries: Mutex<HashMap<String, Arc<Entry>>>,
}
struct Entry {
    requests: Arc<Mutex<HashMap<uuid::Uuid, requests::RequestRecord>>>,
    root: Arc<PrivateDirectory>,
    sender: mpsc::Sender<Call>,
    stop: watch::Sender<bool>,
    state: watch::Sender<NativeWorkerState>,
    task: tokio::sync::Mutex<Option<JoinHandle<Result<(), AppError>>>>,
}
pub(crate) struct Call {
    pub request: InferenceRequest,
    pub files: RequestFiles,
    pub response: oneshot::Sender<Result<CompletedCall, AppError>>,
    pub cancelled: watch::Receiver<bool>,
    pub active: Arc<AtomicBool>,
    pub _completion: requests::RequestDone,
}
pub(crate) struct CompletedCall {
    pub output: InferenceOutput,
    pub files: RequestFiles,
}

impl NativeWorkerPool {
    pub fn new(
        manager: ModelManager,
        jobs_root: PathBuf,
        voices_root: PathBuf,
    ) -> Result<Self, AppError> {
        Self::with_options(
            manager,
            jobs_root,
            voices_root,
            NativeWorkerOptions::default(),
        )
    }
    pub fn with_options(
        manager: ModelManager,
        jobs: PathBuf,
        voices: PathBuf,
        options: NativeWorkerOptions,
    ) -> Result<Self, AppError> {
        if options.ready_timeout.is_zero() || options.request_timeout.is_zero() {
            return Err(files::error("native worker timeouts must be positive"));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                closed: AtomicBool::new(false),
                manager,
                jobs: files::directory(&jobs)?,
                voices: files::directory(&voices)?,
                options,
                entries: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn warmup(&self, selection: &ModelSelection) -> Result<(), AppError> {
        selection.validate()?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| files::error("native warmup requires an asynchronous runtime"))?;
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| files::error("native worker registry is unavailable"))?;
        if self.inner.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AppError::unavailable("native worker pool is shut down"));
        }
        let key = key(selection);
        if let Some(entry) = entries.get(&key)
            && matches!(
                *entry.state.borrow(),
                NativeWorkerState::Loading | NativeWorkerState::Ready
            )
        {
            return Ok(());
        }
        if let Some(previous) = entries.remove(&key) {
            let _stop = previous.stop.send(true);
        }
        let prepared = PreparedWorker::prepare(
            &self.inner.manager,
            selection,
            &self.inner.jobs,
            self.inner.options.ready_timeout,
        )?;
        let (sender, receiver) = mpsc::channel(8);
        let (stop, cancellation) = watch::channel(false);
        let (state, _) = watch::channel(NativeWorkerState::Loading);
        let entry = Arc::new(Entry {
            requests: Arc::new(Mutex::new(HashMap::new())),
            root: prepared.root.clone(),
            sender,
            stop,
            state: state.clone(),
            task: tokio::sync::Mutex::new(None),
        });
        let timeout = self.inner.options.request_timeout;
        let task = runtime.spawn(crate::actor::run(
            prepared,
            receiver,
            cancellation,
            state,
            timeout,
        ));
        *entry
            .task
            .try_lock()
            .map_err(|_| files::error("cannot initialize native worker task"))? = Some(task);
        entries.insert(key, entry);
        Ok(())
    }

    pub fn state(&self, selection: &ModelSelection) -> Result<NativeWorkerState, AppError> {
        let entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| files::error("native worker registry is unavailable"))?;
        Ok(entries
            .get(&key(selection))
            .map(|entry| entry.state.borrow().clone())
            .unwrap_or(NativeWorkerState::Cold))
    }

    pub(crate) fn new_request(&self, selection: &ModelSelection) -> Result<RequestFiles, AppError> {
        RequestFiles::new(self.ready_entry(selection)?.root.clone())
    }

    fn ready_entry(&self, selection: &ModelSelection) -> Result<Arc<Entry>, AppError> {
        let entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| files::error("native worker registry is unavailable"))?;
        let entry = entries
            .get(&key(selection))
            .ok_or_else(|| AppError::unavailable("native model has not started loading"))?;
        if !matches!(*entry.state.borrow(), NativeWorkerState::Ready) {
            return Err(AppError::unavailable("native model is not ready"));
        }
        Ok(entry.clone())
    }

    pub(crate) async fn execute(
        &self,
        selection: &ModelSelection,
        request: InferenceRequest,
        files: RequestFiles,
    ) -> Result<CompletedCall, AppError> {
        request.validate()?;
        let entry = self.ready_entry(selection)?;
        if !files.matches_worker(&entry.root) {
            return Err(AppError::unavailable(
                "native model changed while preparing its request",
            ));
        }
        let (response, receive) = oneshot::channel();
        let (cancelled, active, completion) = requests::register(&entry, request.identity)?;
        let mut observe = cancelled.clone();
        let call = Call {
            request,
            files,
            response,
            cancelled,
            active,
            _completion: completion,
        };
        tokio::select! {
            biased;
            _ = requests::requested(&mut observe) => return Err(AppError::unavailable("native request cancelled")),
            sent = entry.sender.send(call) => sent.map_err(|_| AppError::unavailable("native worker has stopped"))?,
        }
        tokio::select! {
            biased;
            _ = requests::requested(&mut observe) => Err(AppError::unavailable("native request cancelled")),
            result = receive => result.map_err(|_| AppError::unavailable("native request ended without a result"))?,
        }
    }

    pub(crate) fn voices_root(&self) -> &Path {
        &self.inner.voices
    }
}

fn key(selection: &ModelSelection) -> String {
    format!(
        "{:?}:{}@{}",
        selection.backend, selection.model_id, selection.runtime_id
    )
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(entries) = self.entries.get_mut() {
            for entry in entries.values() {
                let _stop = entry.stop.send(true);
            }
        }
    }
}
