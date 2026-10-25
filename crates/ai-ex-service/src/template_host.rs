//! Service-owned synthetic sessions; no native input or capture adapter is constructed.
//! The arbitration lock covers admission, pause ownership and conditional resume.
//! Owners drain started SDK steps before replacement and shutdown.
use std::collections::{BTreeSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ai_ex_audio::{SpeechActivity, SpeechActivitySnapshot};
use ai_ex_automation::{
    ActionResult, AuditRecord, AuditSink, AutomationAction, AutomationCoordinator, AutomationPort,
    ChatPauseOptions, ChatPauseOutcome, ChatPausePolicy, ChatPauseReport, DryRunAutomationPort,
    ReflexControl, ReflexControlMode, ReflexFailure, ReflexOutcome, ReflexSession,
    ReflexSessionOutcome, ReflexSessionReport, ReflexState, ResumeEligibility,
};
use ai_ex_domain::{
    AppError, AutomationControlMode, AutomationPauseReason, AutomationRequest,
    AutomationResumeEligibility, AutomationSearchIncompleteReason, AutomationSearchRegion,
    AutomationSearchSnapshot, AutomationSessionPhase, AutomationSessionSnapshot,
    AutomationSnapshot, AutomationStepOutcome, AutomationTemplateMode,
};
use ai_ex_safety::{Capability, Permit, SafetyGate, SafetyPolicy};
use async_trait::async_trait;
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::strategy_loop::{
    StrategyDecision, StrategyFrameContext, StrategyHandle, StrategyProviderFactory,
    StrategyRuntime, StrategyTicket,
};
use crate::template_game::PreparedTemplate;

const ARCHIVE_CAPACITY: usize = 2;
const OWNER_POLL_MS: u64 = 50;
const MAX_OWNER_TASKS: usize = 2;
const MAX_PENDING_CHAT: usize = 128;

trait HostClock: Send + Sync {
    fn now_ms(&self) -> Result<u64, AppError>;
}

struct ElapsedClock(Instant);

impl HostClock for ElapsedClock {
    fn now_ms(&self) -> Result<u64, AppError> {
        u64::try_from(self.0.elapsed().as_millis())
            .map_err(|_| AppError::unavailable("automation monotonic clock exhausted"))
    }
}

/// Cloneable command/admission boundary; no mutable controller is exposed.
#[derive(Clone)]
pub(crate) struct TemplateHost {
    inner: Arc<Mutex<HostState>>,
    speech: SpeechActivity,
    global_safety: Arc<SafetyGate>,
    clock: Arc<dyn HostClock>,
    wake: Arc<Notify>,
    execution: Arc<AsyncMutex<()>>,
    strategy_factory: Option<StrategyProviderFactory>,
    strategy_encoding: Arc<Semaphore>,
}

struct HostState {
    snapshot: AutomationSnapshot,
    current: Option<CurrentSession>,
    pending_chat: usize,
    last_chat_ms: Option<u64>,
    last_chat_deadline_ms: Option<u64>,
    closed: bool,
    tasks: Vec<JoinHandle<()>>,
    archives: VecDeque<SessionArchive>,
    strategy: Option<StrategyHandle>,
}

struct CurrentSession {
    manifest: String,
    manual_resume: bool,
    control: Option<ReflexControl>,
    policy: Option<ChatPausePolicy>,
}

// Full receipts and audits remain bounded and private; the wire carries summaries.
struct SessionArchive {
    _session_id: Uuid,
    _reports: Vec<ReflexSessionReport>,
    _failure: Option<ReflexFailure>,
    _audit: Arc<Mutex<Vec<AuditRecord>>>,
}

/// Owned by the actual admission/completion task, independently of its caller.
pub(crate) struct ChatActivityGuard {
    host: TemplateHost,
}

impl Drop for ChatActivityGuard {
    fn drop(&mut self) {
        match self.host.inner.lock() {
            Ok(mut state) => {
                if let Some(remaining) = state.pending_chat.checked_sub(1) {
                    state.pending_chat = remaining;
                    update_eligibility(&mut state, self.host.eligibility(remaining));
                } else {
                    fail_closed(&mut state);
                }
            }
            Err(poisoned) => fail_closed(&mut poisoned.into_inner()),
        }
        self.host.wake.notify_waiters();
    }
}

impl TemplateHost {
    pub(crate) fn new(
        instance_id: Uuid,
        speech: SpeechActivity,
        global_safety: Arc<SafetyGate>,
    ) -> Result<Self, AppError> {
        Self::with_clock(
            instance_id,
            speech,
            global_safety,
            Arc::new(ElapsedClock(Instant::now())),
        )
    }

    pub(crate) fn with_strategy_factory(mut self, factory: StrategyProviderFactory) -> Self {
        self.strategy_factory = Some(factory);
        self
    }

    fn with_clock(
        instance_id: Uuid,
        speech: SpeechActivity,
        global_safety: Arc<SafetyGate>,
        clock: Arc<dyn HostClock>,
    ) -> Result<Self, AppError> {
        if instance_id.is_nil() {
            return Err(AppError::configuration(
                "automation host instance must not be nil",
            ));
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(HostState {
                snapshot: AutomationSnapshot::new(instance_id, true),
                current: None,
                pending_chat: 0,
                last_chat_ms: None,
                last_chat_deadline_ms: None,
                closed: false,
                tasks: Vec::new(),
                archives: VecDeque::with_capacity(ARCHIVE_CAPACITY),
                strategy: None,
            })),
            speech,
            global_safety,
            clock,
            wake: Arc::new(Notify::new()),
            execution: Arc::new(AsyncMutex::new(())),
            strategy_factory: None,
            strategy_encoding: Arc::new(Semaphore::new(1)),
        })
    }

    /// Clock/policy errors refuse admission before its count changes.
    pub(crate) fn begin_chat(&self) -> Result<ChatActivityGuard, AppError> {
        let mut state = self.lock()?;
        let now_ms = self.clock.now_ms()?;
        if state.closed || !state.snapshot.available {
            return Err(AppError::unavailable("automation/chat arbitration stopped"));
        }
        if state.pending_chat >= MAX_PENDING_CHAT {
            return Err(AppError::unavailable(
                "conversation admission capacity is full",
            ));
        }
        if state.last_chat_ms.is_some_and(|previous| now_ms < previous) {
            return Err(AppError::invalid_transition("chat clock regressed"));
        }
        let deadline_ms = now_ms
            .checked_add(ChatPauseOptions::default().timeout_ms)
            .ok_or_else(|| AppError::invalid_transition("chat pause deadline exhausted"))?;
        let pending = state
            .pending_chat
            .checked_add(1)
            .ok_or_else(|| AppError::unavailable("chat admission count exhausted"))?;
        ensure_revision(&mut state)?;
        let report = state
            .current
            .as_mut()
            .and_then(|current| current.policy.as_mut())
            .map(|policy| policy.on_chat(now_ms))
            .transpose()?;
        if let Some(strategy) = state.strategy.as_ref() {
            strategy.invalidate_pending();
        }
        state.pending_chat = pending;
        state.last_chat_ms = Some(now_ms);
        state.last_chat_deadline_ms = Some(deadline_ms);
        if let Some(current) = state.current.as_mut() {
            current.manual_resume = false;
        }
        if let Some(report) = report {
            apply_policy(&mut state, &report);
        }
        update_eligibility(&mut state, self.eligibility(pending));
        bump_revision(&mut state);
        drop(state);
        self.wake.notify_waiters();
        Ok(ChatActivityGuard { host: self.clone() })
    }

    pub(crate) async fn execute(
        &self,
        request: AutomationRequest,
    ) -> Result<AutomationSnapshot, AppError> {
        request.validate()?;
        match request {
            AutomationRequest::Status => self.snapshot(),
            AutomationRequest::StartTemplate {
                instance_id,
                expected_revision,
                operation_id,
                manifest,
            } => self.start(instance_id, expected_revision, operation_id, manifest),
            AutomationRequest::Pause {
                instance_id,
                session_id,
                expected_revision,
            } => self.mutate(instance_id, session_id, expected_revision, Mutation::Pause),
            AutomationRequest::Resume {
                instance_id,
                session_id,
                expected_revision,
            } => self.mutate(instance_id, session_id, expected_revision, Mutation::Resume),
            AutomationRequest::Stop {
                instance_id,
                session_id,
                expected_revision,
            } => self.mutate(instance_id, session_id, expected_revision, Mutation::Stop),
        }
    }

    /// Revoke synchronously before awaiting conversation interruption.
    pub(crate) fn emergency_stop(&self) -> Result<(), AppError> {
        self.global_safety.trigger_emergency_stop();
        let mut state = self.lock()?;
        stop_current(&mut state)?;
        drop(state);
        self.wake.notify_waiters();
        Ok(())
    }

    /// Stop and join owners/loaders without aborting an SDK step or its audit.
    pub(crate) async fn shutdown(&self) -> Result<(), AppError> {
        let (tasks, mut failure) = {
            // Poison or cursor exhaustion cannot skip draining owned tasks.
            // Recover only to revoke control and take task handles, never to infer Idle.
            let (mut state, mut failure) = match self.inner.lock() {
                Ok(state) => (state, None),
                Err(poisoned) => (
                    poisoned.into_inner(),
                    Some(AppError::unavailable(
                        "automation arbitration lock failed during shutdown",
                    )),
                ),
            };
            state.closed = true;
            if let Err(error) = stop_current(&mut state) {
                failure.get_or_insert(error);
            }
            if failure.is_some() {
                fail_closed(&mut state);
            }
            (std::mem::take(&mut state.tasks), failure)
        };
        self.wake.notify_waiters();
        for task in tasks {
            if let Err(error) = task.await {
                failure.get_or_insert_with(|| {
                    AppError::unavailable(format!(
                        "automation owner failed while draining: {error}"
                    ))
                });
            }
        }
        failure.map_or(Ok(()), Err)
    }

    #[cfg(test)]
    pub(crate) fn pending_chat_count(&self) -> Result<usize, AppError> {
        Ok(self.lock()?.pending_chat)
    }

    fn snapshot(&self) -> Result<AutomationSnapshot, AppError> {
        let mut state = self.lock()?;
        let eligibility = self.eligibility(state.pending_chat);
        update_eligibility(&mut state, eligibility);
        Ok(state.snapshot.clone())
    }

    fn start(
        &self,
        instance_id: Uuid,
        expected_revision: u64,
        operation_id: Uuid,
        manifest: String,
    ) -> Result<AutomationSnapshot, AppError> {
        let mut state = self.lock()?;
        check_instance(&state, instance_id)?;
        if state.closed || !state.snapshot.available || self.global_safety.emergency_stop_active() {
            return Err(AppError::safety(
                "synthetic automation is stopped or unavailable",
            ));
        }
        if state
            .snapshot
            .session
            .as_ref()
            .is_some_and(|session| session.operation_id == operation_id)
        {
            if state
                .current
                .as_ref()
                .is_some_and(|current| current.manifest == manifest)
            {
                return Ok(state.snapshot.clone());
            }
            return Err(AppError::invalid_transition(
                "automation operation already names another manifest",
            ));
        }
        check_revision(&state, expected_revision)?;
        if state
            .snapshot
            .session
            .as_ref()
            .is_some_and(|session| session.phase == AutomationSessionPhase::Loading)
        {
            return Err(AppError::invalid_transition(
                "an automation manifest is already loading",
            ));
        }
        state.tasks.retain(|task| !task.is_finished());
        if state.tasks.len() >= MAX_OWNER_TASKS {
            return Err(AppError::unavailable(
                "previous automation owners are still draining",
            ));
        }
        ensure_revision(&mut state)?;
        if let Some(control) = state
            .current
            .as_ref()
            .and_then(|current| current.control.as_ref())
        {
            control.stop()?;
        }
        if let Some(strategy) = state.strategy.take() {
            strategy.cancel();
        }
        let session_id = Uuid::new_v4();
        let mut session = AutomationSessionSnapshot::loading(session_id, operation_id);
        session.resume_eligibility = wire_eligibility(self.eligibility(state.pending_chat));
        state.snapshot.session = Some(session);
        state.current = Some(CurrentSession {
            manifest: manifest.clone(),
            manual_resume: false,
            control: None,
            policy: None,
        });
        bump_revision(&mut state);
        state.tasks.retain(|task| !task.is_finished());
        let host = self.clone();
        state.tasks.push(tokio::spawn(async move {
            host.load_and_run(session_id, operation_id, manifest).await;
        }));
        let snapshot = state.snapshot.clone();
        drop(state);
        self.wake.notify_waiters();
        Ok(snapshot)
    }

    fn mutate(
        &self,
        instance_id: Uuid,
        session_id: Uuid,
        expected_revision: u64,
        mutation: Mutation,
    ) -> Result<AutomationSnapshot, AppError> {
        let mut state = self.lock()?;
        check_instance(&state, instance_id)?;
        if matches!(mutation, Mutation::Stop) {
            if expected_revision > state.snapshot.revision {
                return Err(AppError::invalid_transition(
                    "automation revision is from the future",
                ));
            }
        } else {
            check_revision(&state, expected_revision)?;
        }
        if state.closed || !state.snapshot.available {
            return Err(AppError::unavailable("automation host is unavailable"));
        }
        let session = state
            .snapshot
            .session
            .as_ref()
            .filter(|session| session.session_id == session_id)
            .ok_or_else(|| AppError::invalid_transition("automation session is stale"))?;
        if session.phase == AutomationSessionPhase::Finished {
            return match mutation {
                Mutation::Stop => Ok(state.snapshot.clone()),
                _ => Err(AppError::invalid_transition(
                    "finished automation needs a new session",
                )),
            };
        }
        if session.phase == AutomationSessionPhase::Stopping && !matches!(mutation, Mutation::Stop)
        {
            return Err(AppError::invalid_transition(
                "stopped automation cannot resume",
            ));
        }
        let eligibility = self.eligibility(state.pending_chat);
        if matches!(mutation, Mutation::Resume)
            && (eligibility != ResumeEligibility::Idle
                || self.global_safety.emergency_stop_active())
        {
            return Err(AppError::invalid_transition(
                "automation cannot resume while chat or speech is busy or unknown",
            ));
        }
        ensure_revision(&mut state)?;
        if matches!(mutation, Mutation::Stop) {
            stop_current(&mut state)?;
        } else {
            let control = state
                .current
                .as_ref()
                .and_then(|current| current.control.as_ref());
            let observed = match (mutation, control) {
                (Mutation::Pause, Some(control)) => Some(control.pause_for_chat()?),
                (Mutation::Resume, Some(control)) => Some(control.resume()?),
                _ => None,
            };
            if let Some(current) = state.current.as_mut() {
                current.manual_resume = matches!(mutation, Mutation::Resume);
            }
            if let Some(session) = state.snapshot.session.as_mut() {
                session.control_mode = if matches!(mutation, Mutation::Pause) {
                    AutomationControlMode::PausedForChat
                } else {
                    AutomationControlMode::Running
                };
                session.control_generation =
                    observed.map_or(session.control_generation, |value| value.generation);
                session.pause_reason =
                    matches!(mutation, Mutation::Pause).then_some(AutomationPauseReason::Manual);
                session.chat_deadline_ms = None;
                session.resume_eligibility = wire_eligibility(eligibility);
            }
            bump_revision(&mut state);
        }
        let snapshot = state.snapshot.clone();
        drop(state);
        self.wake.notify_waiters();
        Ok(snapshot)
    }

    async fn load_and_run(&self, session_id: Uuid, operation_id: Uuid, manifest: String) {
        // Bound heavy decoding and execution together. Superseded queued owners
        // recheck their reservation before touching any manifest or image file.
        let _execution = self.execution.lock().await;
        match self.lock() {
            Ok(state)
                if is_current(&state, session_id, operation_id)
                    && !state.closed
                    && state.snapshot.session.as_ref().is_some_and(|session| {
                        session.phase == AutomationSessionPhase::Loading
                    }) => {}
            _ => return,
        }
        let prepared = tokio::task::spawn_blocking(move || {
            crate::template_game::load_session(Path::new(&manifest))
        })
        .await;
        let prepared = match prepared {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(error)) => {
                self.fail_reservation(session_id, operation_id, &error);
                return;
            }
            Err(error) => {
                self.fail_reservation(
                    session_id,
                    operation_id,
                    &AppError::unavailable(format!("template loader failed: {error}")),
                );
                return;
            }
        };

        if let Err(error) = self.run_prepared(session_id, operation_id, prepared).await {
            self.fail_reservation(session_id, operation_id, &error);
        }
    }

    async fn run_prepared(
        &self,
        session_id: Uuid,
        operation_id: Uuid,
        prepared: PreparedTemplate,
    ) -> Result<(), AppError> {
        let strategy_factory = if let Some(options) = prepared.strategy.as_ref() {
            options.validate()?;
            Some(self.strategy_factory.clone().ok_or_else(|| {
                AppError::configuration("template strategy requires enabled vision")
            })?)
        } else {
            None
        };
        let dimensions = prepared
            .frames
            .iter()
            .map(|frame| (frame.width, frame.height))
            .collect::<Vec<_>>();
        let mut session = ReflexSession::new(prepared.controller);
        let interval_ms = session.options().interval_ms;
        let max_frames = session.options().max_frames;
        let max_actions = session.options().max_actions;
        let control = session.control();
        let now_ms;
        let mut policy = ChatPausePolicy::new(control.clone(), ChatPauseOptions::default())?;
        {
            let mut state = self.lock()?;
            now_ms = self.clock.now_ms()?;
            if !is_current(&state, session_id, operation_id)
                || state.closed
                || state
                    .snapshot
                    .session
                    .as_ref()
                    .is_none_or(|current| current.phase != AutomationSessionPhase::Loading)
            {
                return Ok(());
            }
            ensure_revision(&mut state)?;
            if state.last_chat_ms.is_some_and(|previous| now_ms < previous) {
                return Err(AppError::invalid_transition(
                    "chat clock regressed during template loading",
                ));
            }
            let eligibility = self.eligibility(state.pending_chat);
            let quiet_pending = state
                .last_chat_deadline_ms
                .is_some_and(|deadline| now_ms < deadline);
            let explicit_resume = state
                .current
                .as_ref()
                .is_some_and(|current| current.manual_resume);
            let last_chat_ms = state.last_chat_ms.unwrap_or(now_ms);
            let desired_mode = state
                .snapshot
                .session
                .as_ref()
                .map(|current| current.control_mode);
            let chat_report = match desired_mode {
                Some(AutomationControlMode::PausedForChat) => {
                    control.pause_for_chat()?;
                    None
                }
                Some(AutomationControlMode::Running)
                    if eligibility != ResumeEligibility::Idle
                        || (quiet_pending && !explicit_resume) =>
                {
                    Some(policy.on_chat(last_chat_ms)?)
                }
                Some(AutomationControlMode::Stopped) | None => return Ok(()),
                _ => None,
            };
            let observed = control.snapshot()?;
            let mode = match prepared.mode.as_str() {
                "click_once" => AutomationTemplateMode::ClickOnce,
                "click_repeat" => AutomationTemplateMode::ClickRepeat,
                "hover" => AutomationTemplateMode::Hover,
                _ => return Err(AppError::configuration("prepared template mode is invalid")),
            };
            if let Some(snapshot) = state.snapshot.session.as_mut() {
                snapshot.phase = AutomationSessionPhase::Active;
                snapshot.control_mode = wire_control(observed.mode);
                snapshot.control_generation = observed.generation;
                snapshot.profile_name = Some(bounded_text(&prepared.profile, 512));
                snapshot.target = Some(bounded_text(&prepared.target, 512));
                snapshot.mode = Some(mode);
                snapshot.input_frames = prepared.input_frames as u64;
                snapshot.scheduled_frames = prepared.geometries.len() as u64;
                snapshot.skipped_frames = snapshot
                    .input_frames
                    .saturating_sub(snapshot.scheduled_frames);
                snapshot.max_frames = max_frames;
                snapshot.max_actions = max_actions;
                snapshot.resume_eligibility = wire_eligibility(eligibility);
            }
            if let Some(current) = state.current.as_mut() {
                current.control = Some(control.clone());
                current.policy = Some(policy);
            }
            if let Some(report) = chat_report {
                apply_policy(&mut state, &report);
            }
            bump_revision(&mut state);
        }
        let audit_capacity = prepared
            .action_capacity
            .checked_mul(3)
            .ok_or_else(|| AppError::configuration("synthetic audit capacity overflow"))?;
        let records = Arc::new(Mutex::new(Vec::new()));
        let audit = HostAudit {
            records: Arc::clone(&records),
            capacity: audit_capacity,
        };
        // This gate only authorizes dry-run input to the exact loaded target.
        // The configured gate is untouched and its emergency latch is checked
        // again by the only port this host can construct.
        let dry_safety = Arc::new(SafetyGate::new(SafetyPolicy {
            automation_enabled: true,
            allowed_capabilities: BTreeSet::from([Capability::ScreenRead, Capability::MouseInput]),
            allowed_targets: vec![prepared.target],
        }));
        let port = HostPort {
            inner: DryRunAutomationPort::with_frames(prepared.action_capacity, prepared.frames)?,
            global_safety: Arc::clone(&self.global_safety),
        };
        let strategy = match (strategy_factory, prepared.strategy) {
            (Some(factory), Some(options)) => Some(StrategyRuntime::start(
                factory,
                options,
                Arc::clone(&self.strategy_encoding),
            )?),
            _ => None,
        };
        let strategy_handle = strategy.as_ref().map(StrategyRuntime::handle);
        let port = StrategyPort {
            inner: port,
            strategy: strategy_handle.clone(),
        };
        let mut coordinator = AutomationCoordinator::new(dry_safety, port, audit);
        let mut reports = Vec::new();
        let mut failure = None;
        let mut frames = 0u64;
        let mut next_due = now_ms;
        let mut installed_hint: Option<StrategyTicket> = None;
        let mut last_proposal = 0u64;
        let run_result: Result<(), AppError> = async {
        if let Some(handle) = strategy_handle.as_ref() {
            let mut state = self.lock()?;
            if !is_current(&state, session_id, operation_id) || state.closed
                || state.snapshot.session.as_ref().is_none_or(|current| current.phase != AutomationSessionPhase::Active)
            { handle.cancel(); return Ok(()); }
            state.strategy = Some(handle.clone());
        }
        loop {
            let notified = self.wake.notified();
            let decision = self.poll_owner(session_id, operation_id)?;
            let Some((now_ms, mode)) = decision else { break; };
            if mode == ReflexControlMode::Stopped {
                self.finish_current(session_id, operation_id, AutomationStepOutcome::Stopped)?;
                break;
            }
            if mode != ReflexControlMode::Running || now_ms < next_due {
                let wait_ms = if mode == ReflexControlMode::Running {
                    next_due.saturating_sub(now_ms).clamp(1, OWNER_POLL_MS)
                } else { OWNER_POLL_MS };
                tokio::select! { () = notified => {}, () = tokio::time::sleep(Duration::from_millis(wait_ms)) => {} }
                continue;
            }
            let Some(geometry) = usize::try_from(frames).ok().and_then(|index| prepared.geometries.get(index)) else {
                self.finish_current(session_id, operation_id, AutomationStepOutcome::InputExhausted)?;
                break;
            };
            if reports.len() >= audit_capacity {
                return Err(AppError::unavailable("synthetic partial report budget exhausted"));
            }
            let capture_generation = if let Some(handle) = strategy_handle.as_ref() {
                self.prepare_strategy_step(session_id, operation_id, &mut session, handle,
                    geometry, dimensions.get(frames as usize).copied(), frames, &mut installed_hint, &mut last_proposal)?
            } else { None };
            // Never select/cancel a started SDK step: finish its receipts and audit.
            let step = session.step(&mut coordinator, geometry, now_ms).await;
            let report = match step {
                Ok(report) => report,
                Err(error) => {
                    let report = ReflexSessionReport { outcome: ReflexSessionOutcome::Advanced, step: error.report.clone(), interruption: None };
                    failure = Some(error);
                    report
                }
            };
            // Paused or pre-dispatch rejected captures do not consume a source
            // frame; cumulative SDK captures therefore identify its next geometry.
            let capture_consumed = report.step.frames > frames;
            frames = report.step.frames;
            reports.push(report);
            let latest = reports.last().ok_or_else(|| AppError::unavailable("synthetic report is missing"))?;
            if let Some(handle) = strategy_handle.as_ref() {
                self.commit_strategy_capture(session_id, operation_id, handle, capture_generation,
                    latest, failure.is_none() && !matches!(session.state(), ReflexState::Completed | ReflexState::BudgetExhausted | ReflexState::Failed))?;
            }
            self.publish_step(session_id, operation_id, latest, failure.as_ref())?;
            if failure.is_some() || matches!(session.state(), ReflexState::Completed | ReflexState::BudgetExhausted | ReflexState::Failed) {
                break;
            }
            if capture_consumed {
                next_due = now_ms.checked_add(interval_ms)
                    .ok_or_else(|| AppError::unavailable("synthetic cadence clock exhausted"))?;
            }
        }
        Ok(())
        }.await;
        let stop_result = control.stop();
        let strategy_result = match strategy {
            Some(strategy) => strategy.shutdown().await,
            None => Ok(()),
        };
        let mut state = self.lock()?;
        if state.archives.len() == ARCHIVE_CAPACITY {
            state.archives.pop_front();
        }
        state.archives.push_back(SessionArchive {
            _session_id: session_id,
            _reports: reports,
            _failure: failure,
            _audit: records,
        });
        stop_result?;
        strategy_result?;
        run_result
    }

    /// The host lock is the single commit point for strategy and chat/control.
    #[allow(clippy::too_many_arguments)]
    fn prepare_strategy_step(
        &self,
        session_id: Uuid,
        operation_id: Uuid,
        session: &mut ReflexSession,
        handle: &StrategyHandle,
        geometry: &ai_ex_automation::FrameGeometry,
        dimensions: Option<(u32, u32)>,
        frames: u64,
        installed: &mut Option<StrategyTicket>,
        last_proposal: &mut u64,
    ) -> Result<Option<ai_ex_automation::ReflexControlSnapshot>, AppError> {
        let mut state = self.lock()?;
        let now_ms = self.clock.now_ms()?;
        if !is_current(&state, session_id, operation_id)
            || state.closed
            || !state.snapshot.available
            || self.global_safety.emergency_stop_active()
            || self.eligibility(state.pending_chat) != ResumeEligibility::Idle
        {
            handle.discard_capture();
            return Ok(None);
        }
        let mut observed = session.control().snapshot()?;
        if observed.mode != ReflexControlMode::Running {
            handle.discard_capture();
            return Ok(None);
        }
        // A hint is only an optimization for its own source geometry/epoch.
        // Chat resume, expiry or a changed source restores the immutable manifest ROI.
        let clear = installed.as_ref().is_some_and(|hint| {
            hint.context.control != observed
                || dimensions != Some((hint.width, hint.height))
                || hint.context.geometry != *geometry
                || now_ms < hint.context.captured_ms
                || now_ms - hint.context.captured_ms > hint.max_age_ms
        });
        if clear {
            ensure_revision(&mut state)?;
            let Some(updated) = session.refocus_if_current(observed, None)? else {
                return Ok(None);
            };
            observed = updated;
            *installed = None;
            update_control_generation(&mut state)?;
            bump_revision(&mut state);
        }
        if let Some(proposal) = handle.take_proposal() {
            let ticket = proposal.ticket;
            let context = ticket.context;
            let valid = context.instance_id == state.snapshot.instance_id
                && context.session_id == session_id
                && context.operation_id == operation_id
                && context.control == observed
                && ticket.request_id > *last_proposal
                && context.frame_sequence > 0
                && context.frame_sequence <= frames
                && dimensions == Some((ticket.width, ticket.height))
                && context.geometry == *geometry
                && now_ms >= context.captured_ms
                && now_ms - context.captured_ms <= ticket.max_age_ms;
            *last_proposal = (*last_proposal).max(ticket.request_id);
            if !valid {
                handle.rejected();
            } else {
                match proposal.decision {
                    StrategyDecision::Keep => handle.applied(),
                    StrategyDecision::FocusRegion(region) => {
                        ensure_revision(&mut state)?;
                        // The SDK intersects with the original fixed ROI and keeps
                        // every consumed frame/action budget and cadence timestamp.
                        match session.refocus_if_current(observed, Some(region)) {
                            Ok(Some(updated)) => {
                                observed = updated;
                                let mut hint = ticket;
                                hint.context.control = updated;
                                *installed = Some(hint);
                                handle.invalidate_pending();
                                handle.applied();
                                update_control_generation(&mut state)?;
                                bump_revision(&mut state);
                            }
                            Ok(None) | Err(_) => handle.rejected(),
                        }
                    }
                }
            }
        }
        handle.arm(StrategyFrameContext {
            instance_id: state.snapshot.instance_id,
            session_id,
            operation_id,
            control: observed,
            frame_sequence: frames.saturating_add(1),
            captured_ms: now_ms,
            geometry: *geometry,
        });
        Ok(Some(observed))
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_strategy_capture(
        &self,
        session_id: Uuid,
        operation_id: Uuid,
        handle: &StrategyHandle,
        issued: Option<ai_ex_automation::ReflexControlSnapshot>,
        report: &ReflexSessionReport,
        nonterminal_success: bool,
    ) -> Result<(), AppError> {
        let state = self.lock()?;
        let observed = state
            .current
            .as_ref()
            .and_then(|current| current.control.as_ref())
            .map(ReflexControl::snapshot)
            .transpose()?;
        let audited = report.step.receipts.iter().any(|receipt| {
            matches!(&receipt.action, AutomationAction::CaptureScreen)
                && receipt.completion_audit_persisted
        });
        if nonterminal_success
            && audited
            && report.outcome == ReflexSessionOutcome::Advanced
            && is_current(&state, session_id, operation_id)
            && !state.closed
            && !self.global_safety.emergency_stop_active()
            && self.eligibility(state.pending_chat) == ResumeEligibility::Idle
            && issued.is_some_and(|issued| {
                issued.mode == ReflexControlMode::Running && observed == Some(issued)
            })
        {
            handle.commit_capture();
        } else {
            handle.discard_capture();
        }
        Ok(())
    }

    fn poll_owner(
        &self,
        session_id: Uuid,
        operation_id: Uuid,
    ) -> Result<Option<(u64, ReflexControlMode)>, AppError> {
        let mut state = self.lock()?;
        let now_ms = self.clock.now_ms()?;
        if !is_current(&state, session_id, operation_id) {
            return Ok(None);
        }
        if self.global_safety.emergency_stop_active() {
            stop_current(&mut state)?;
        }
        let eligibility = self.eligibility(state.pending_chat);
        update_eligibility(&mut state, eligibility);
        ensure_revision(&mut state)?;
        let report = state
            .current
            .as_mut()
            .and_then(|current| current.policy.as_mut())
            .map(|policy| policy.poll(now_ms, eligibility))
            .transpose()?;
        if let Some(report) = report {
            let previous = state.snapshot.session.as_ref().map(|session| {
                (
                    session.control_generation,
                    session.control_mode,
                    session.chat_deadline_ms,
                )
            });
            apply_policy(&mut state, &report);
            let updated = state.snapshot.session.as_ref().map(|session| {
                (
                    session.control_generation,
                    session.control_mode,
                    session.chat_deadline_ms,
                )
            });
            if previous != updated {
                ensure_revision(&mut state)?;
                bump_revision(&mut state);
            }
        }
        let mode = state
            .current
            .as_ref()
            .and_then(|current| current.control.as_ref())
            .ok_or_else(|| AppError::unavailable("synthetic session control is missing"))?
            .snapshot()?
            .mode;
        Ok(Some((now_ms, mode)))
    }

    fn publish_step(
        &self,
        session_id: Uuid,
        operation_id: Uuid,
        report: &ReflexSessionReport,
        failure: Option<&ReflexFailure>,
    ) -> Result<(), AppError> {
        let mut state = self.lock()?;
        if !is_current(&state, session_id, operation_id) {
            return Ok(());
        }
        let terminal = matches!(
            report.step.state,
            ReflexState::Completed | ReflexState::BudgetExhausted | ReflexState::Failed
        );
        if terminal {
            ensure_revision(&mut state)?;
        }
        if let Some(session) = state.snapshot.session.as_mut() {
            session.frames = report.step.frames;
            session.actions = report.step.actions;
            session.search =
                report
                    .step
                    .search
                    .zip(report.step.search_incomplete)
                    .map(|(progress, reason)| {
                        AutomationSearchSnapshot {
                    region: AutomationSearchRegion {
                        x: progress.region.x,
                        y: progress.region.y,
                        width: progress.region.width,
                        height: progress.region.height,
                    },
                    candidates: progress.candidates,
                    verified: progress.verified,
                    bound_queries: progress.bound_queries,
                    exact_evaluations: progress.exact_evaluations,
                    exact_pixel_pairs: progress.exact_pixel_pairs,
                    bound_work: progress.bound_work,
                    work: progress.work,
                    max_work: progress.max_work,
                    reason: match reason {
                        ai_ex_automation::TemplateSearchIncompleteReason::WorkBudgetExhausted => {
                            AutomationSearchIncompleteReason::WorkBudgetExhausted
                        }
                    },
                }
                    });
            session.latest_outcome = Some(if session.search.is_some() {
                AutomationStepOutcome::SearchIncomplete
            } else if failure.is_some() {
                AutomationStepOutcome::Failed
            } else if report.outcome == ReflexSessionOutcome::Interrupted {
                AutomationStepOutcome::Interrupted
            } else {
                wire_outcome(report.step.outcome)
            });
            session.last_error = failure.map(|error| bounded_text(&error.message, 1024));
            if terminal {
                session.phase = AutomationSessionPhase::Finished;
                session.control_mode = AutomationControlMode::Stopped;
                session.pause_reason = None;
                session.chat_deadline_ms = None;
            }
        }
        if terminal {
            if let Some(current) = state.current.as_mut() {
                current.policy = None;
                if let Some(control) = current.control.as_ref() {
                    control.stop()?;
                }
            }
            update_control_generation(&mut state)?;
            bump_revision(&mut state);
        } else {
            bump_projection(&mut state);
        }
        Ok(())
    }

    fn finish_current(
        &self,
        session_id: Uuid,
        operation_id: Uuid,
        outcome: AutomationStepOutcome,
    ) -> Result<(), AppError> {
        let mut state = self.lock()?;
        if !is_current(&state, session_id, operation_id) {
            return Ok(());
        }
        ensure_revision(&mut state)?;
        if let Some(current) = state.current.as_mut() {
            current.policy = None;
            if let Some(control) = current.control.as_ref() {
                control.stop()?;
            }
        }
        if let Some(session) = state.snapshot.session.as_mut() {
            session.phase = AutomationSessionPhase::Finished;
            session.control_mode = AutomationControlMode::Stopped;
            session.latest_outcome = Some(outcome);
            session.pause_reason = None;
            session.chat_deadline_ms = None;
        }
        update_control_generation(&mut state)?;
        bump_revision(&mut state);
        Ok(())
    }

    fn fail_reservation(&self, session_id: Uuid, operation_id: Uuid, error: &AppError) {
        if let Ok(mut state) = self.lock()
            && is_current(&state, session_id, operation_id)
            && state
                .snapshot
                .session
                .as_ref()
                .is_some_and(|session| session.phase != AutomationSessionPhase::Finished)
        {
            if let Some(current) = state.current.as_mut() {
                current.policy = None;
                if let Some(control) = current.control.as_ref() {
                    let _stopped = control.stop();
                }
            }
            if let Some(session) = state.snapshot.session.as_mut() {
                session.phase = AutomationSessionPhase::Finished;
                session.control_mode = AutomationControlMode::Stopped;
                session.latest_outcome = Some(AutomationStepOutcome::Failed);
                session.search = None;
                session.last_error = Some(bounded_text(&error.message, 1024));
                session.pause_reason = None;
                session.chat_deadline_ms = None;
            }
            let _generation = update_control_generation(&mut state);
            bump_revision(&mut state);
        }
    }

    fn eligibility(&self, pending: usize) -> ResumeEligibility {
        if pending > 0 {
            return ResumeEligibility::Busy;
        }
        match self.speech.snapshot() {
            SpeechActivitySnapshot::Idle => ResumeEligibility::Idle,
            SpeechActivitySnapshot::Busy { .. } => ResumeEligibility::Busy,
            SpeechActivitySnapshot::Unavailable => ResumeEligibility::Unknown,
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, HostState>, AppError> {
        self.inner
            .lock()
            .map_err(|_| AppError::unavailable("automation arbitration lock failed"))
    }
}

#[derive(Clone, Copy)]
enum Mutation {
    Pause,
    Resume,
    Stop,
}

fn check_instance(state: &HostState, instance_id: Uuid) -> Result<(), AppError> {
    if state.snapshot.instance_id != instance_id {
        Err(AppError::invalid_transition(
            "automation host instance is stale",
        ))
    } else {
        Ok(())
    }
}

fn check_revision(state: &HostState, revision: u64) -> Result<(), AppError> {
    if state.snapshot.revision != revision {
        Err(AppError::invalid_transition(
            "automation revision is stale; refresh status",
        ))
    } else {
        Ok(())
    }
}

fn ensure_revision(state: &mut HostState) -> Result<(), AppError> {
    if state.snapshot.revision >= u64::MAX - 1 || state.snapshot.projection_sequence >= u64::MAX - 1
    {
        fail_closed(state);
        Err(AppError::unavailable("automation revision exhausted"))
    } else {
        Ok(())
    }
}

fn bump_revision(state: &mut HostState) {
    match state.snapshot.revision.checked_add(1) {
        Some(revision) => state.snapshot.revision = revision,
        None => fail_closed(state),
    }
    bump_projection(state);
}

fn bump_projection(state: &mut HostState) {
    match state.snapshot.projection_sequence.checked_add(1) {
        Some(sequence) => state.snapshot.projection_sequence = sequence,
        None => fail_closed(state),
    }
}

fn fail_closed(state: &mut HostState) {
    if let Some(strategy) = state.strategy.as_ref() {
        strategy.cancel();
    }
    state.snapshot.available = false;
    state.closed = true;
    let observed = state.current.as_mut().and_then(|current| {
        current.policy = None;
        current.control.as_ref().and_then(|control| {
            let _stopped = control.stop();
            control.snapshot().ok()
        })
    });
    if let Some(session) = state.snapshot.session.as_mut() {
        session.control_mode = AutomationControlMode::Stopped;
        session.control_generation =
            observed.map_or(session.control_generation, |snapshot| snapshot.generation);
        session.phase = if observed.is_some() {
            AutomationSessionPhase::Stopping
        } else {
            AutomationSessionPhase::Finished
        };
        session.pause_reason = None;
        session.chat_deadline_ms = None;
    }
}

fn is_current(state: &HostState, session_id: Uuid, operation_id: Uuid) -> bool {
    state.snapshot.session.as_ref().is_some_and(|session| {
        session.session_id == session_id && session.operation_id == operation_id
    })
}

fn stop_current(state: &mut HostState) -> Result<(), AppError> {
    if state
        .snapshot
        .session
        .as_ref()
        .is_none_or(|session| session.phase == AutomationSessionPhase::Finished)
    {
        return Ok(());
    }
    ensure_revision(state)?;
    if let Some(strategy) = state.strategy.as_ref() {
        strategy.cancel();
    }
    let control = state.current.as_mut().and_then(|current| {
        current.policy = None;
        current.control.as_ref()
    });
    let observed = control.map(ReflexControl::stop).transpose()?;
    if let Some(session) = state.snapshot.session.as_mut() {
        session.phase = if observed.is_some() {
            AutomationSessionPhase::Stopping
        } else {
            AutomationSessionPhase::Finished
        };
        session.control_mode = AutomationControlMode::Stopped;
        session.control_generation =
            observed.map_or(session.control_generation, |snapshot| snapshot.generation);
        session.pause_reason = None;
        session.chat_deadline_ms = None;
        if observed.is_none() {
            session.latest_outcome = Some(AutomationStepOutcome::Stopped);
        }
    }
    bump_revision(state);
    Ok(())
}

fn update_control_generation(state: &mut HostState) -> Result<(), AppError> {
    if let Some(control) = state
        .current
        .as_ref()
        .and_then(|current| current.control.as_ref())
    {
        let observed = control.snapshot()?;
        if let Some(session) = state.snapshot.session.as_mut() {
            session.control_generation = observed.generation;
        }
    }
    Ok(())
}

fn update_eligibility(state: &mut HostState, eligibility: ResumeEligibility) {
    if let Some(session) = state.snapshot.session.as_mut()
        && session.resume_eligibility != wire_eligibility(eligibility)
    {
        session.resume_eligibility = wire_eligibility(eligibility);
        bump_projection(state);
    }
}

fn apply_policy(state: &mut HostState, report: &ChatPauseReport) {
    if let Some(session) = state.snapshot.session.as_mut() {
        session.chat_deadline_ms = report.deadline_ms;
        if let Some(control) = report.control {
            session.control_generation = control.generation;
            session.control_mode = wire_control(control.mode);
        }
        match session.control_mode {
            AutomationControlMode::Running | AutomationControlMode::Stopped => {
                session.pause_reason = None
            }
            AutomationControlMode::PausedForChat if report.deadline_ms.is_some() => {
                session.pause_reason = Some(AutomationPauseReason::Chat)
            }
            AutomationControlMode::PausedForChat => {
                if report.outcome == ChatPauseOutcome::Superseded
                    && session.pause_reason != Some(AutomationPauseReason::Manual)
                {
                    session.pause_reason = Some(AutomationPauseReason::Unknown);
                }
            }
        }
    }
}

fn wire_control(mode: ReflexControlMode) -> AutomationControlMode {
    match mode {
        ReflexControlMode::Running => AutomationControlMode::Running,
        ReflexControlMode::PausedForChat => AutomationControlMode::PausedForChat,
        ReflexControlMode::Stopped => AutomationControlMode::Stopped,
    }
}

fn wire_eligibility(eligibility: ResumeEligibility) -> AutomationResumeEligibility {
    match eligibility {
        ResumeEligibility::Idle => AutomationResumeEligibility::Idle,
        ResumeEligibility::Busy => AutomationResumeEligibility::Busy,
        ResumeEligibility::Unknown => AutomationResumeEligibility::Unknown,
    }
}

fn wire_outcome(outcome: ReflexOutcome) -> AutomationStepOutcome {
    match outcome {
        ReflexOutcome::Waiting => AutomationStepOutcome::Waiting,
        ReflexOutcome::Matched => AutomationStepOutcome::Matched,
        ReflexOutcome::Missed => AutomationStepOutcome::Missed,
        ReflexOutcome::Completed => AutomationStepOutcome::Completed,
        ReflexOutcome::BudgetExhausted => AutomationStepOutcome::BudgetExhausted,
        ReflexOutcome::Inactive => AutomationStepOutcome::Inactive,
    }
}

fn bounded_text(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

struct HostPort {
    inner: DryRunAutomationPort,
    global_safety: Arc<SafetyGate>,
}

#[async_trait]
impl AutomationPort for HostPort {
    async fn execute(
        &mut self,
        permit: &Permit,
        action: &AutomationAction,
    ) -> Result<ActionResult, AppError> {
        if self.global_safety.emergency_stop_active() {
            return Err(AppError::safety("global emergency stop is active"));
        }
        self.inner.execute(permit, action).await
    }
}

struct StrategyPort {
    inner: HostPort,
    strategy: Option<StrategyHandle>,
}

#[async_trait]
impl AutomationPort for StrategyPort {
    async fn execute(
        &mut self,
        permit: &Permit,
        action: &AutomationAction,
    ) -> Result<ActionResult, AppError> {
        let result = self.inner.execute(permit, action).await?;
        if let ActionResult::ScreenCaptured(frame) = &result
            && let Some(strategy) = self.strategy.as_ref()
        {
            // No encoding/provider work happens here. The owner signs this staged
            // copy only after the complete step proves the capture audit persisted.
            strategy.stage_capture(frame);
        }
        Ok(result)
    }
}

struct HostAudit {
    records: Arc<Mutex<Vec<AuditRecord>>>,
    capacity: usize,
}

#[async_trait]
impl AuditSink for HostAudit {
    async fn record(&mut self, record: AuditRecord) -> Result<(), AppError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| AppError::unavailable("synthetic audit lock failed"))?;
        if records.len() >= self.capacity {
            return Err(AppError::unavailable("synthetic audit budget exhausted"));
        }
        records.push(record);
        Ok(())
    }
}

#[cfg(test)]
#[path = "template_host_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "strategy_host_tests.rs"]
mod strategy_tests;
