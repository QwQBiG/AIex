//! Bounded observation worker; no automation/control authority reaches a provider.
use ai_ex_automation::{FrameGeometry, PixelRect, ReflexControlSnapshot, ScreenFrame};
use ai_ex_domain::AppError;
use ai_ex_vision::{VisionAnalyzerPort, VisionObservation, VisionRequest, VisualFrame};
use image::ImageEncoder;
use serde::Deserialize;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

const MAX_SAMPLE_PIXELS: u64 = 4_194_304;
const MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const SAMPLE_EDGE: u32 = 640;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StrategyOptions {
    pub(crate) prompt: String,
    #[serde(default = "default_interval")]
    pub(crate) interval_ms: u64,
    #[serde(default = "default_timeout")]
    pub(crate) timeout_ms: u64,
    #[serde(default = "default_max_age")]
    pub(crate) max_age_ms: u64,
    #[serde(default = "default_max_requests")]
    pub(crate) max_requests: u16,
}
fn default_interval() -> u64 {
    1_000
}
fn default_timeout() -> u64 {
    5_000
}
fn default_max_age() -> u64 {
    5_000
}
fn default_max_requests() -> u16 {
    16
}
impl StrategyOptions {
    pub(crate) fn validate(&self) -> Result<(), AppError> {
        if self.prompt.trim().is_empty()
            || self.prompt.chars().count() > 2_048
            || !(100..=60_000).contains(&self.interval_ms)
            || !(1..=30_000).contains(&self.timeout_ms)
            || !(1..=60_000).contains(&self.max_age_ms)
            || !(1..=64).contains(&self.max_requests)
        {
            return Err(AppError::configuration(
                "invalid bounded template strategy options",
            ));
        }
        Ok(())
    }
}
/// Factories configure clients without performing requests.
pub(crate) type StrategyProviderFactory =
    Arc<dyn Fn() -> Result<Box<dyn VisionAnalyzerPort>, AppError> + Send + Sync>;
#[derive(Debug, Clone, Copy)]
pub(crate) struct StrategyFrameContext {
    pub(crate) instance_id: Uuid,
    pub(crate) session_id: Uuid,
    pub(crate) operation_id: Uuid,
    pub(crate) control: ReflexControlSnapshot,
    pub(crate) frame_sequence: u64,
    pub(crate) captured_ms: u64,
    pub(crate) geometry: FrameGeometry,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct StrategyTicket {
    pub(crate) context: StrategyFrameContext,
    pub(crate) request_id: u64,
    pub(crate) max_age_ms: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StrategyDecision {
    Keep,
    FocusRegion(PixelRect),
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct StrategyProposal {
    pub(crate) ticket: StrategyTicket,
    pub(crate) decision: StrategyDecision,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct StrategyStatus {
    pub(crate) requests_started: u16,
    pub(crate) requests_finished: u16,
    pub(crate) proposals_applied: u16,
    pub(crate) proposals_rejected: u16,
    pub(crate) in_flight: bool,
    #[cfg(test)]
    pub(crate) queued: bool,
    #[cfg(test)]
    pub(crate) proposal_pending: bool,
    pub(crate) closed: bool,
    pub(crate) last_error: Option<String>,
}
struct Sample {
    ticket: StrategyTicket,
    frame: ScreenFrame,
}
struct StagedSample {
    context: StrategyFrameContext,
    frame: ScreenFrame,
}
struct WorkerState {
    armed: Option<StrategyFrameContext>,
    staged: Option<StagedSample>,
    latest: Option<Sample>,
    proposal: Option<StrategyProposal>,
    next_due_ms: Option<u64>,
    request_sequence: u64,
    status: StrategyStatus,
}
#[derive(Clone)]
pub(crate) struct StrategyHandle {
    state: Arc<Mutex<WorkerState>>,
    wake: Arc<Notify>,
    cancel: watch::Sender<bool>,
    options: StrategyOptions,
}
impl StrategyHandle {
    fn lock(&self) -> Option<MutexGuard<'_, WorkerState>> {
        self.state.lock().ok()
    }
    #[cfg(test)]
    pub(crate) fn status(&self) -> StrategyStatus {
        self.lock().map_or_else(
            || StrategyStatus {
                closed: true,
                ..StrategyStatus::default()
            },
            |state| {
                let mut status = state.status.clone();
                status.queued = state.latest.is_some();
                status.proposal_pending = state.proposal.is_some();
                status
            },
        )
    }
    pub(crate) fn arm(&self, context: StrategyFrameContext) {
        if let Some(mut state) = self.lock() {
            state.armed = None;
            state.staged = None;
            if !state.status.closed
                && state.status.requests_started < self.options.max_requests
                && state
                    .next_due_ms
                    .is_none_or(|due| context.captured_ms >= due)
            {
                state.armed = Some(context);
            }
        }
    }
    /// Only due frames are copied. The host has not signed an observation yet.
    /// Staged and queued images share the one latest-image memory slot.
    pub(crate) fn stage_capture(&self, frame: &ScreenFrame) {
        let Some(mut state) = self.lock() else {
            return;
        };
        let Some(context) = state.armed.take() else {
            return;
        };
        if state.status.closed
            || u64::from(frame.width) * u64::from(frame.height) > MAX_SAMPLE_PIXELS
            || frame.rgba.len() > MAX_SAMPLE_PIXELS as usize * 4
        {
            return;
        }
        state.latest = None;
        state.staged = Some(StagedSample {
            context,
            frame: frame.clone(),
        });
    }
    /// Call only after successful capture completion audit, under host arbitration.
    pub(crate) fn commit_capture(&self) {
        let Some(mut state) = self.lock() else {
            return;
        };
        let Some(staged) = state.staged.take() else {
            return;
        };
        if state.status.closed || state.status.requests_started >= self.options.max_requests {
            return;
        }
        let Some(request_id) = state.request_sequence.checked_add(1) else {
            return;
        };
        state.request_sequence = request_id;
        state.next_due_ms = staged
            .context
            .captured_ms
            .checked_add(self.options.interval_ms);
        let ticket = StrategyTicket {
            context: staged.context,
            request_id,
            max_age_ms: self.options.max_age_ms,
            width: staged.frame.width,
            height: staged.frame.height,
        };
        state.latest = Some(Sample {
            ticket,
            frame: staged.frame,
        });
        drop(state);
        self.wake.notify_one();
    }
    pub(crate) fn discard_capture(&self) {
        if let Some(mut state) = self.lock() {
            state.armed = None;
            state.staged = None;
        }
    }
    pub(crate) fn invalidate_pending(&self) {
        if let Some(mut state) = self.lock() {
            state.armed = None;
            state.staged = None;
            state.latest = None;
            state.proposal = None;
        }
    }
    pub(crate) fn take_proposal(&self) -> Option<StrategyProposal> {
        self.lock()?.proposal.take()
    }
    pub(crate) fn applied(&self) {
        if let Some(mut state) = self.lock() {
            state.status.proposals_applied = state.status.proposals_applied.saturating_add(1);
        }
    }
    pub(crate) fn rejected(&self) {
        if let Some(mut state) = self.lock() {
            state.status.proposals_rejected = state.status.proposals_rejected.saturating_add(1);
        }
    }
    pub(crate) fn cancel(&self) {
        if let Some(mut state) = self.lock() {
            state.status.closed = true;
            state.armed = None;
            state.staged = None;
            state.latest = None;
            state.proposal = None;
        }
        self.cancel.send_replace(true);
        self.wake.notify_waiters();
    }
}
pub(crate) struct StrategyRuntime {
    handle: StrategyHandle,
    worker: Option<JoinHandle<()>>,
}
impl StrategyRuntime {
    pub(crate) fn start(
        factory: StrategyProviderFactory,
        options: StrategyOptions,
        encoding: Arc<Semaphore>,
    ) -> Result<Self, AppError> {
        options.validate()?;
        let (cancel, receiver) = watch::channel(false);
        let handle = StrategyHandle {
            state: Arc::new(Mutex::new(WorkerState {
                armed: None,
                staged: None,
                latest: None,
                proposal: None,
                next_due_ms: None,
                request_sequence: 0,
                status: StrategyStatus::default(),
            })),
            wake: Arc::new(Notify::new()),
            cancel,
            options,
        };
        let worker_handle = handle.clone();
        let worker = tokio::spawn(async move {
            run_worker(factory, worker_handle, receiver, encoding).await;
        });
        Ok(Self {
            handle,
            worker: Some(worker),
        })
    }
    pub(crate) fn handle(&self) -> StrategyHandle {
        self.handle.clone()
    }
    pub(crate) async fn shutdown(mut self) -> Result<(), AppError> {
        self.handle.cancel();
        if let Some(worker) = self.worker.take() {
            worker
                .await
                .map_err(|_| AppError::unavailable("template strategy worker failed"))?;
        }
        Ok(())
    }
}
impl Drop for StrategyRuntime {
    fn drop(&mut self) {
        // Owners explicitly join on all normal/error exits. Unwinding still revokes
        // observations and wakes the worker; a running encoder is never aborted.
        self.handle.cancel();
    }
}
async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
async fn run_worker(
    factory: StrategyProviderFactory,
    handle: StrategyHandle,
    mut cancel: watch::Receiver<bool>,
    encoding: Arc<Semaphore>,
) {
    let mut provider = match factory() {
        Ok(provider) => provider,
        Err(error) => {
            finish_request(&handle, Some(&error), false);
            return;
        }
    };
    loop {
        let notified = handle.wake.notified();
        let sample = match handle.lock() {
            Some(mut state)
                if !state.status.closed
                    && state.status.requests_started < handle.options.max_requests =>
            {
                let sample = state.latest.take();
                if sample.is_some() {
                    state.status.in_flight = true;
                }
                sample
            }
            _ => break,
        };
        let Some(sample) = sample else {
            tokio::select! { () = cancelled(&mut cancel) => break, () = notified => {} }
            continue;
        };
        let ticket = sample.ticket;
        let permit = tokio::select! {
            () = cancelled(&mut cancel) => break,
            permit = Arc::clone(&encoding).acquire_owned() => match permit { Ok(permit) => permit, Err(_) => break },
        };
        let encoder = tokio::task::spawn_blocking(move || {
            // The host-wide slot is owned by the blocking closure, even if its
            // caller is revoked. It bounds CPU work across session replacements.
            let _permit = permit;
            encode_frame(sample.frame)
        });
        // Blocking work cannot be cancelled. Drain this finite, size-bounded
        // operation before cancellation and before creating another encoder.
        let encoded = encoder.await;
        if *cancel.borrow() {
            break;
        }
        let frame = match encoded {
            Ok(Ok(frame)) => frame,
            Ok(Err(error)) => {
                finish_request(&handle, Some(&error), false);
                continue;
            }
            Err(_) => {
                finish_request(
                    &handle,
                    Some(&AppError::unavailable("template strategy encoder failed")),
                    false,
                );
                continue;
            }
        };
        let prompt = format!(
            "Observe only the configured game target. Return exactly {{\"type\":\"keep\"}} or {{\"type\":\"focus_region\",\"region\":{{\"x\":0,\"y\":0,\"width\":1,\"height\":1}}}}. Use integer ORIGINAL frame-local pixels for {}x{}; the image may be downsampled. No actions or other fields. Objective: {}",
            ticket.width, ticket.height, handle.options.prompt,
        );
        let request = match VisionRequest::new(prompt, frame) {
            Ok(request) => request,
            Err(error) => {
                finish_request(&handle, Some(&error), false);
                continue;
            }
        };
        if let Some(mut state) = handle.lock() {
            if state.status.closed {
                break;
            }
            state.status.requests_started += 1;
        } else {
            break;
        }
        let analyzed = tokio::select! {
            () = cancelled(&mut cancel) => break,
            response = tokio::time::timeout(Duration::from_millis(handle.options.timeout_ms), provider.analyze(request)) => {
                response.unwrap_or_else(|_| Err(AppError::unavailable("template strategy request timed out")))
            }
        };
        match analyzed.and_then(|observation| parse_decision(observation, &ticket)) {
            Ok(decision) => {
                finish_request(&handle, None, true);
                if let Some(mut state) = handle.lock()
                    && !state.status.closed
                {
                    state.proposal = Some(StrategyProposal { ticket, decision });
                }
            }
            Err(error) => finish_request(&handle, Some(&error), true),
        }
    }
    if let Some(mut state) = handle.lock() {
        state.status.in_flight = false;
    }
}
fn finish_request(handle: &StrategyHandle, error: Option<&AppError>, analyzed: bool) {
    if let Some(mut state) = handle.lock() {
        state.status.in_flight = false;
        if analyzed {
            state.status.requests_finished = state.status.requests_finished.saturating_add(1);
        }
        state.status.last_error = error.map(|error| error.to_string().chars().take(256).collect());
    }
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum RawDecision {
    Keep {},
    FocusRegion { region: RawRegion },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegion {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}
fn parse_decision(
    observation: VisionObservation,
    ticket: &StrategyTicket,
) -> Result<StrategyDecision, AppError> {
    if observation.text.len() > MAX_RESPONSE_BYTES {
        return Err(AppError::protocol(
            "template strategy response exceeds its byte limit",
        ));
    }
    let raw: RawDecision = serde_json::from_str(&observation.text).map_err(|_| {
        AppError::protocol("template strategy requires strict Keep/FocusRegion JSON")
    })?;
    match raw {
        RawDecision::Keep {} => Ok(StrategyDecision::Keep),
        RawDecision::FocusRegion { region } => {
            if region.width == 0
                || region.height == 0
                || region
                    .x
                    .checked_add(region.width)
                    .is_none_or(|right| right > ticket.width)
                || region
                    .y
                    .checked_add(region.height)
                    .is_none_or(|bottom| bottom > ticket.height)
            {
                return Err(AppError::protocol(
                    "template strategy region is outside its source frame",
                ));
            }
            Ok(StrategyDecision::FocusRegion(PixelRect {
                x: region.x,
                y: region.y,
                width: region.width,
                height: region.height,
            }))
        }
    }
}
struct BoundedPng(Vec<u8>);
impl Write for BoundedPng {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|len| len > MAX_ENCODED_BYTES)
        {
            return Err(io::Error::other("bounded strategy PNG exceeds byte limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encode_frame(frame: ScreenFrame) -> Result<VisualFrame, AppError> {
    if u64::from(frame.width) * u64::from(frame.height) > MAX_SAMPLE_PIXELS {
        return Err(AppError::configuration(
            "template strategy frame exceeds 4194304 pixels",
        ));
    }
    let image = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
        .ok_or_else(|| AppError::protocol("invalid template strategy RGBA frame"))?;
    let edge = frame.width.max(frame.height);
    let image = if edge > SAMPLE_EDGE {
        let width =
            ((u64::from(frame.width) * u64::from(SAMPLE_EDGE)) / u64::from(edge)).max(1) as u32;
        let height =
            ((u64::from(frame.height) * u64::from(SAMPLE_EDGE)) / u64::from(edge)).max(1) as u32;
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Nearest)
    } else {
        image
    };
    let mut encoded = BoundedPng(Vec::new());
    image::codecs::png::PngEncoder::new(&mut encoded)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|_| AppError::protocol("cannot encode bounded template strategy PNG"))?;
    VisualFrame::detect(encoded.0)
}

#[cfg(test)]
#[path = "strategy_loop_tests.rs"]
mod tests;
