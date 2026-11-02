//! Applied behavior settings and owned global shortcuts, independent of devices.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ai_ex_domain::{
    AppError, DesktopDiagnosticReport, DesktopRuntimeReply, DesktopRuntimeRequest,
    DesktopRuntimeSettings, DesktopRuntimeSnapshot, DiscoveredVtsHotkey,
};
use async_trait::async_trait;
use tokio::sync::Mutex;
use uuid::Uuid;

mod hotkeys;
mod persistence;
#[cfg(test)]
mod tests;

#[async_trait]
pub(crate) trait DesktopRuntimeActions: Send + Sync {
    async fn interrupt(&self) -> Result<(), AppError>;
    async fn emergency_stop(&self) -> Result<(), AppError>;
    async fn pause_native(&self) -> Result<(), AppError>;
    fn resume_native(&self) -> Result<tokio::task::JoinHandle<Result<(), AppError>>, AppError>;
    fn native_step(&self) -> Result<tokio::task::JoinHandle<Result<(), AppError>>, AppError>;
    fn revoke_native(&self) -> Result<(), AppError>;
    /// Must reserve idle admission atomically, not queue behind an active user turn.
    async fn proactive(&self) -> Result<(), AppError>;
    async fn cancel_proactive(&self) -> Result<(), AppError>;
    fn local_minute(&self) -> Option<u16>;
}

#[async_trait]
pub(crate) trait VtsDiscoveryPort: Send + Sync {
    async fn discover(&self) -> Result<Vec<DiscoveredVtsHotkey>, AppError>;
}

#[async_trait]
impl VtsDiscoveryPort for ai_ex_vts::VtsClient {
    async fn discover(&self) -> Result<Vec<DiscoveredVtsHotkey>, AppError> {
        Ok(self
            .discover_hotkeys()
            .await?
            .into_iter()
            .map(|key| DiscoveredVtsHotkey {
                id: key.id,
                name: key.name,
            })
            .collect())
    }
}

struct State {
    snapshot: DesktopRuntimeSnapshot,
    hotkeys: Option<Box<dyn hotkeys::OwnedHotkeys>>,
    last_proactive: Instant,
    generation: u64,
    stopped: bool,
    proactive_task: Option<tokio::task::JoinHandle<()>>,
    hotkey_tasks: [Option<tokio::task::JoinHandle<()>>; 6],
    previously_allowed: bool,
    native_tasks: [Option<tokio::task::JoinHandle<Result<(), AppError>>>; 2],
}

#[derive(Clone)]
pub(crate) struct DesktopRuntime {
    state: Arc<Mutex<State>>,
    path: Arc<PathBuf>,
    actions: Arc<dyn DesktopRuntimeActions>,
    vts: Option<Arc<dyn VtsDiscoveryPort>>,
    started: Instant,
    hotkey_factory: hotkeys::Factory,
}

impl DesktopRuntime {
    pub(crate) fn spawn(&self) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(50));
            loop {
                interval.tick().await;
                if !this.tick().await {
                    break;
                }
            }
        })
    }

    pub(crate) async fn stop(&self) -> Result<(), AppError> {
        let mut state = self.state.lock().await;
        state.stopped = true;
        let revoked = self.actions.revoke_native();
        for task in state.native_tasks.iter_mut().filter_map(Option::take) {
            task.abort();
        }
        for task in state.hotkey_tasks.iter_mut().filter_map(Option::take) {
            task.abort();
        }
        if let Some(task) = state.proactive_task.take() {
            task.abort();
        }
        state.snapshot.hotkeys_registered = false;
        let cleanup = if let Some(mut owner) = state.hotkeys.take() {
            owner.shutdown()
        } else {
            Ok(())
        };
        drop(state);
        self.actions.cancel_proactive().await?;
        cleanup.and(revoked)
    }

    pub(crate) async fn suspend_proactive(&self) -> Result<(), AppError> {
        let mut state = self.state.lock().await;
        state.previously_allowed = false;
        state.snapshot.settings.proactive_enabled = false;
        state.snapshot.revision = state.snapshot.revision.saturating_add(1);
        if let Some(task) = state.proactive_task.take() {
            task.abort();
        }
        drop(state);
        self.actions.cancel_proactive().await
    }

    async fn tick(&self) -> bool {
        let (events, cancel_proactive) = {
            let mut state = self.state.lock().await;
            if state.stopped {
                return false;
            }
            let events = match state.hotkeys.as_mut().map(|owner| owner.poll()).transpose() {
                Ok(events) => events.unwrap_or_default(),
                Err(_) => {
                    state.snapshot.hotkeys_registered = false;
                    state.snapshot.hotkeys_error = true;
                    state.snapshot.settings.proactive_enabled = false;
                    state.snapshot.revision = state.snapshot.revision.saturating_add(1);
                    if let Some(mut owner) = state.hotkeys.take() {
                        let _cleanup = owner.shutdown();
                    }
                    drop(state);
                    let _stopped = self.actions.emergency_stop().await;
                    return true;
                }
            };
            let mut events = events
                .into_iter()
                .filter(|(_, generation)| *generation == state.generation)
                .collect::<Vec<_>>();
            // An emergency dominates all other intents drained in the same batch.
            if let Some(event) = events.iter().find(|(id, _)| *id == 2).copied() {
                events = vec![event];
            }
            let local_minute = self.actions.local_minute();
            let proactive = events.is_empty()
                && state
                    .proactive_task
                    .as_ref()
                    .is_none_or(|task| task.is_finished())
                && state.snapshot.settings.allows_proactive(local_minute)
                && state.last_proactive.elapsed().as_secs()
                    >= u64::from(state.snapshot.settings.cooldown_seconds);
            // Audio may still be playing after generation completed.
            let allowed = state.snapshot.settings.allows_proactive(local_minute);
            let cancel_proactive = !allowed && state.previously_allowed;
            state.previously_allowed = allowed;
            if cancel_proactive && let Some(task) = state.proactive_task.take() {
                task.abort();
            }
            if proactive {
                state.last_proactive = Instant::now();
                let actions = self.actions.clone();
                state.proactive_task = Some(tokio::spawn(async move {
                    let _outcome = actions.proactive().await;
                }));
            }
            (events, cancel_proactive)
        };
        if cancel_proactive {
            let _cancelled = self.actions.cancel_proactive().await;
        }
        for (id, generation) in events {
            let mut state = self.state.lock().await;
            if state.stopped
                || state.generation != generation
                || !state.snapshot.hotkeys_registered
                || !(1..=6).contains(&id)
            {
                continue;
            }
            let slot = (id - 1) as usize;
            if id == 4 || id == 6 {
                let native_slot = usize::from(id == 6);
                if state.native_tasks[native_slot]
                    .as_ref()
                    .is_some_and(|task| !task.is_finished())
                {
                    continue;
                }
                state.native_tasks[native_slot] = if id == 6 {
                    self.actions.native_step()
                } else {
                    self.actions.resume_native()
                }
                .ok();
                continue;
            }
            if state.hotkey_tasks[slot]
                .as_ref()
                .is_some_and(|task| !task.is_finished())
            {
                continue;
            }
            if id == 2 {
                let _revoked = self.actions.revoke_native();
                for task in state.native_tasks.iter_mut().filter_map(Option::take) {
                    task.abort();
                }
                for task in state.hotkey_tasks.iter_mut().filter_map(Option::take) {
                    task.abort();
                }
            }
            let this = self.clone();
            state.hotkey_tasks[slot] = Some(tokio::spawn(async move {
                {
                    let state = this.state.lock().await;
                    if state.stopped
                        || state.generation != generation
                        || !state.snapshot.hotkeys_registered
                    {
                        return;
                    }
                }
                let _outcome = this.execute_hotkey(id, generation).await;
            }));
        }
        true
    }

    async fn execute_hotkey(&self, id: i32, generation: u64) -> Result<(), AppError> {
        match id {
            1 => self.actions.interrupt().await,
            2 => {
                let _cancelled = self.suspend_proactive().await;
                self.actions.emergency_stop().await
            }
            3 => self.actions.pause_native().await,
            5 => {
                let mut state = self.state.lock().await;
                if state.stopped
                    || state.generation != generation
                    || !state.snapshot.hotkeys_registered
                {
                    return Err(AppError::invalid_transition("shortcut generation retired"));
                }
                let mut settings = state.snapshot.settings.clone();
                settings.captions_visible = !settings.captions_visible;
                if let Some(next) = state.snapshot.revision.checked_add(1) {
                    persistence::save(&self.path, &settings).map(|()| {
                        state.snapshot.settings = settings;
                        state.snapshot.revision = next;
                    })
                } else {
                    Err(AppError::unavailable("desktop revision exhausted"))
                }
            }
            _ => Ok(()),
        }
    }

    /// Loads preferences only; construction never registers a shortcut or starts work.
    pub(crate) fn new(
        instance_id: Uuid,
        path: PathBuf,
        actions: Arc<dyn DesktopRuntimeActions>,
        vts: Option<Arc<dyn VtsDiscoveryPort>>,
    ) -> Result<Self, AppError> {
        let settings = persistence::load(&path)?;
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                snapshot: DesktopRuntimeSnapshot {
                    instance_id,
                    revision: 0,
                    settings,
                    hotkeys_registered: false,
                    hotkeys_error: false,
                    proactive_available: true,
                    vts_discovery_available: vts.is_some(),
                },
                hotkeys: None,
                last_proactive: Instant::now(),
                generation: 0,
                stopped: false,
                proactive_task: None,
                hotkey_tasks: std::array::from_fn(|_| None),
                previously_allowed: false,
                native_tasks: std::array::from_fn(|_| None),
            })),
            path: Arc::new(path),
            actions,
            vts,
            started: Instant::now(),
            hotkey_factory: hotkeys::factory(),
        })
    }

    pub(crate) async fn handle(
        &self,
        request: DesktopRuntimeRequest,
    ) -> Result<DesktopRuntimeReply, AppError> {
        match request {
            DesktopRuntimeRequest::DiscoverVts => {
                let port = self
                    .vts
                    .as_ref()
                    .ok_or_else(|| AppError::unavailable("VTS discovery is disabled"))?;
                Ok(DesktopRuntimeReply::VtsDiscovery(port.discover().await?))
            }
            DesktopRuntimeRequest::Diagnostics => {
                Ok(DesktopRuntimeReply::Diagnostics(DesktopDiagnosticReport {
                    schema_version: 1,
                    application_version: env!("CARGO_PKG_VERSION").into(),
                    desktop: self.snapshot().await,
                    uptime_seconds: self.started.elapsed().as_secs(),
                }))
            }
            DesktopRuntimeRequest::State => Ok(DesktopRuntimeReply::State(self.snapshot().await)),
            DesktopRuntimeRequest::Apply {
                instance_id,
                expected_revision,
                settings,
            } => self.apply(instance_id, expected_revision, settings).await,
            DesktopRuntimeRequest::Reload {
                instance_id,
                expected_revision,
            } => {
                self.apply(
                    instance_id,
                    expected_revision,
                    persistence::load(&self.path)?,
                )
                .await
            }
        }
    }

    pub(crate) async fn snapshot(&self) -> DesktopRuntimeSnapshot {
        self.state.lock().await.snapshot.clone()
    }

    async fn apply(
        &self,
        instance: Uuid,
        revision: u64,
        settings: DesktopRuntimeSettings,
    ) -> Result<DesktopRuntimeReply, AppError> {
        settings.validate()?;
        let mut state = self.state.lock().await;
        if state.stopped
            || instance != state.snapshot.instance_id
            || revision != state.snapshot.revision
        {
            return Err(AppError::configuration(
                "desktop settings changed; refresh before applying",
            ));
        }
        let next = revision
            .checked_add(1)
            .ok_or_else(|| AppError::unavailable("desktop revision exhausted"))?;
        let reconfigure = settings.hotkeys_enabled != state.snapshot.hotkeys_registered
            || state.snapshot.hotkeys_error;
        let generation = if reconfigure {
            state
                .generation
                .checked_add(1)
                .ok_or_else(|| AppError::unavailable("shortcut generation exhausted"))?
        } else {
            state.generation
        };
        // A failed durable write leaves both the old preferences and registrations intact.
        persistence::save(&self.path, &settings)?;
        if reconfigure {
            let revoke_failed = self.actions.revoke_native().is_err();
            for task in state.native_tasks.iter_mut().filter_map(Option::take) {
                task.abort();
            }
            for task in state.hotkey_tasks.iter_mut().filter_map(Option::take) {
                task.abort();
            }
            let shutdown_failed = state
                .hotkeys
                .as_mut()
                .is_some_and(|owner| owner.shutdown().is_err());
            if !shutdown_failed {
                state.hotkeys = None;
            }
            state.generation = generation;
            let registration = if settings.hotkeys_enabled && !shutdown_failed && !revoke_failed {
                (self.hotkey_factory)(generation).map(Some)
            } else {
                Ok(None)
            };
            state.snapshot.hotkeys_error =
                revoke_failed || shutdown_failed || registration.is_err();
            let owner = registration.ok().flatten();
            state.snapshot.hotkeys_registered = owner.is_some();
            if !shutdown_failed {
                state.hotkeys = owner;
            }
        }
        state.snapshot.settings = settings;
        let cancel_proactive = !state
            .snapshot
            .settings
            .allows_proactive(self.actions.local_minute());
        if cancel_proactive && let Some(task) = state.proactive_task.take() {
            task.abort();
        }
        state.previously_allowed = !cancel_proactive;
        state.snapshot.revision = next;
        state.last_proactive = Instant::now();
        let snapshot = state.snapshot.clone();
        drop(state);
        if cancel_proactive {
            self.actions.cancel_proactive().await?;
        }
        Ok(DesktopRuntimeReply::State(snapshot))
    }
}
