use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use ai_ex_control::{ControlClient, ControlCommand, ControlPayload};
use ai_ex_domain::{
    AppError, AutomationRequest, AutomationSnapshot, ComponentHealth, PersonaSnapshot,
    StageSnapshot,
};
use ai_ex_observability::{RuntimeSnapshot, SequencedEvent};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

const HEALTH_REFRESH_TICKS: u8 = 40;

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;

pub struct WorkerSettings {
    pub address: String,
    pub token: String,
    pub max_message_bytes: usize,
}

pub enum WorkerCommand {
    Submit(String),
    Interrupt,
    EmergencyStop,
    SetPersona(PersonaSnapshot),
    Automation {
        request_id: u64,
        request: AutomationRequest,
    },
    Memory {
        request_id: u64,
        request: ai_ex_domain::MemoryRequest,
    },
}

pub enum WorkerEvent {
    Automation {
        request_id: u64,
        result: Result<AutomationSnapshot, AppError>,
    },
    AutomationStatus(Result<AutomationSnapshot, AppError>),
    Connection(bool),
    Snapshot(RuntimeSnapshot),
    HistoryGap(RuntimeSnapshot),
    Memory {
        request_id: u64,
        result: Result<Box<ai_ex_control::MemoryReply>, AppError>,
    },
    SubmitRejected {
        text: String,
        error: String,
    },
    SubmitUncertain {
        text: String,
        error: String,
    },
    Health(Vec<ComponentHealth>),
    Stage(StageSnapshot),
    Persona(PersonaSnapshot),
    PersonaApplied(PersonaSnapshot),
    PersonaApplyFailed(String),
    Events(Vec<SequencedEvent>),
    Log(String),
    Failure(String),
}

pub struct WorkerHandle {
    pub commands: UnboundedSender<WorkerCommand>,
    pub events: Receiver<WorkerEvent>,
}

pub fn spawn_worker(settings: WorkerSettings) -> Result<WorkerHandle, AppError> {
    let client = ControlClient::new(
        &settings.address,
        settings.token,
        settings.max_message_bytes,
    )?;
    let (commands, command_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (event_sender, events) = mpsc::channel();
    std::thread::Builder::new()
        .name("ai-ex-desktop-network".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => runtime.block_on(run_worker(client, command_receiver, event_sender)),
                Err(error) => {
                    let _ignored = event_sender.send(WorkerEvent::Failure(format!(
                        "cannot start desktop network runtime: {error}",
                    )));
                }
            }
        })
        .map_err(|error| AppError::unavailable(format!("cannot start desktop worker: {error}")))?;
    Ok(WorkerHandle { commands, events })
}

async fn run_worker(
    client: ControlClient,
    commands: UnboundedReceiver<WorkerCommand>,
    events: Sender<WorkerEvent>,
) {
    // Read-only polling must not hold up user commands. Dropping either future
    // also cancels pending network I/O when the desktop channels close.
    let persona_epoch = AtomicU64::new(0);
    tokio::select! {
        _ = run_commands(&client, commands, &events, &persona_epoch) => {}
        _ = run_polling(client.clone(), events.clone(), &persona_epoch) => {}
    }
}

async fn run_ordinary_commands(
    client: &ControlClient,
    commands: UnboundedReceiver<WorkerCommand>,
    events: &Sender<WorkerEvent>,
    persona_epoch: &AtomicU64,
) {
    let (regular, receiver) = tokio::sync::mpsc::channel(32);
    let (urgent, priority) = tokio::sync::watch::channel(UrgentCommands::default());
    let (completed, completion) = tokio::sync::watch::channel(0_u64);
    let command_epoch = AtomicU64::new(0);
    tokio::select! {
        _ = route_commands(commands, regular, urgent, events, &command_epoch) => {}
        _ = async {
            tokio::join!(
                execute_commands(client, receiver, events, persona_epoch, &command_epoch, completion),
                execute_urgent(client, priority, events, completed),
            );
        } => {}
    }
}

#[derive(Clone, Copy, Default)]
struct UrgentCommands {
    interrupt: u64,
    stop: u64,
    epoch: u64,
}

struct QueuedCommand {
    epoch: u64,
    command: WorkerCommand,
}

fn reject_command(events: &Sender<WorkerEvent>, command: WorkerCommand, message: &str) -> bool {
    let event = match command {
        WorkerCommand::Submit(text) => WorkerEvent::SubmitRejected {
            text,
            error: message.to_owned(),
        },
        WorkerCommand::SetPersona(_) => WorkerEvent::PersonaApplyFailed(message.to_owned()),
        WorkerCommand::Automation { request_id, .. } => WorkerEvent::Automation {
            request_id,
            result: Err(AppError::unavailable(message)),
        },
        WorkerCommand::Memory { request_id, .. } => WorkerEvent::Memory {
            request_id,
            result: Err(AppError::unavailable(message)),
        },
        _ => WorkerEvent::Failure(message.to_owned()),
    };
    emit(events, event)
}

async fn route_commands(
    mut commands: UnboundedReceiver<WorkerCommand>,
    regular: tokio::sync::mpsc::Sender<QueuedCommand>,
    urgent: tokio::sync::watch::Sender<UrgentCommands>,
    events: &Sender<WorkerEvent>,
    command_epoch: &AtomicU64,
) {
    while let Some(command) = commands.recv().await {
        match command {
            WorkerCommand::Interrupt => urgent.send_modify(|pending| {
                pending.epoch = command_epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
                pending.interrupt = pending.interrupt.wrapping_add(1);
            }),
            WorkerCommand::EmergencyStop => urgent.send_modify(|pending| {
                pending.epoch = command_epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
                pending.stop = pending.stop.wrapping_add(1);
            }),
            command => {
                let queued = QueuedCommand {
                    epoch: command_epoch.load(Ordering::Acquire),
                    command,
                };
                if let Err(error) = regular.try_send(queued)
                    && !reject_command(
                        events,
                        error.into_inner().command,
                        "桌面操作队列已满或已停止，请稍后重试。",
                    )
                {
                    return;
                }
            }
        }
    }
}

async fn execute_urgent(
    client: &ControlClient,
    mut priority: tokio::sync::watch::Receiver<UrgentCommands>,
    events: &Sender<WorkerEvent>,
    completed: tokio::sync::watch::Sender<u64>,
) {
    let mut handled = UrgentCommands::default();
    while priority.changed().await.is_ok() {
        let pending = *priority.borrow_and_update();
        // Both intents are retained, while repeated clicks occupy no queue.
        let commands = [
            (pending.stop != handled.stop, WorkerCommand::EmergencyStop),
            (
                pending.interrupt != handled.interrupt,
                WorkerCommand::Interrupt,
            ),
        ];
        for (requested, command) in commands {
            if requested {
                let event = match send_command(client, command).await {
                    Ok(_) => WorkerEvent::Log("control command sent".to_owned()),
                    Err(error) => WorkerEvent::Failure(error.to_string()),
                };
                if !emit(events, event) {
                    return;
                }
            }
        }
        handled = pending;
        if completed.send(pending.epoch).is_err() {
            return;
        }
    }
}

async fn execute_commands(
    client: &ControlClient,
    mut commands: tokio::sync::mpsc::Receiver<QueuedCommand>,
    events: &Sender<WorkerEvent>,
    persona_epoch: &AtomicU64,
    command_epoch: &AtomicU64,
    mut completion: tokio::sync::watch::Receiver<u64>,
) {
    while let Some(queued) = commands.recv().await {
        // Preserve user ordering across the priority lane: new messages follow
        // its acknowledgement, while obsolete waiting messages are returned.
        while queued.epoch > *completion.borrow() {
            if completion.changed().await.is_err() {
                return;
            }
        }
        if queued.epoch != command_epoch.load(Ordering::Acquire)
            && matches!(&queued.command, WorkerCommand::Submit(_))
        {
            if !reject_command(
                events,
                queued.command,
                "已取消打断前尚未发出的消息，文字已退回。",
            ) {
                return;
            }
            continue;
        }
        let command = queued.command;
        if let WorkerCommand::Memory {
            request_id,
            request,
        } = command
        {
            let result = match client.send(ControlCommand::Memory { request }).await {
                Ok(ControlPayload::Memory(response)) => Ok(response),
                Ok(_) => Err(AppError::protocol("memory returned an unexpected payload")),
                Err(error) => Err(error),
            };
            if !emit(events, WorkerEvent::Memory { request_id, result }) {
                return;
            }
            continue;
        }
        let submitted_text = match &command {
            WorkerCommand::Submit(text) => Some(text.clone()),
            _ => None,
        };
        let persona_command = matches!(&command, WorkerCommand::SetPersona(_));
        if persona_command {
            persona_epoch.fetch_add(1, Ordering::AcqRel);
        }
        let result = send_command(client, command).await;
        if persona_command {
            persona_epoch.fetch_add(1, Ordering::AcqRel);
        }
        match result {
            Ok(Some(profile)) => {
                if !emit(events, WorkerEvent::PersonaApplied(profile))
                    || !emit(
                        events,
                        WorkerEvent::Log("persona apply accepted".to_owned()),
                    )
                {
                    return;
                }
            }
            Ok(None) => {
                if !emit(events, WorkerEvent::Log("control command sent".to_owned())) {
                    return;
                }
            }
            Err(error) => {
                if let Some(text) = submitted_text {
                    let definite = matches!(
                        error.kind,
                        ai_ex_domain::ErrorKind::Configuration
                            | ai_ex_domain::ErrorKind::InvalidTransition
                            | ai_ex_domain::ErrorKind::Safety
                            | ai_ex_domain::ErrorKind::Unavailable
                    );
                    let event = if definite {
                        WorkerEvent::SubmitRejected {
                            text,
                            error: error.to_string(),
                        }
                    } else {
                        WorkerEvent::SubmitUncertain {
                            text,
                            error: error.to_string(),
                        }
                    };
                    if !emit(events, event) {
                        return;
                    }
                    continue;
                }
                if persona_command
                    && !emit(events, WorkerEvent::PersonaApplyFailed(error.to_string()))
                {
                    return;
                }
                if !emit(events, WorkerEvent::Failure(error.to_string())) {
                    return;
                }
            }
        }
    }
}

async fn run_polling(
    client: ControlClient,
    events: Sender<WorkerEvent>,
    persona_epoch: &AtomicU64,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut connected = false;
    let mut initialized = false;
    let mut cursor = 0;
    let mut instance_id = None;
    let mut ticks = 0_u8;
    let mut failure_reported = false;
    loop {
        tokio::select! {
            _ = interval.tick() =>
            {
                if !connected
                {
                    match fetch_snapshot(&client).await
                    {
                        Ok(snapshot) =>
                        {
                            let health = match fetch_health(&client).await
                            {
                                Ok(health) => health,
                                Err(error) =>
                                {
                                    if !failure_reported
                                    {
                                        failure_reported = true;
                                        if !emit(&events, WorkerEvent::Connection(false))
                                            || !emit(&events, WorkerEvent::Failure(error.to_string()))
                                        {
                                            return;
                                        }
                                    }
                                    continue;
                                }
                            };
                            if !emit(&events, WorkerEvent::Connection(true))
                                || !emit(&events, WorkerEvent::Health(health))
                            {
                                return;
                            }
                            if !initialized {
                                cursor = snapshot.last_sequence;
                                instance_id = snapshot.instance_id;
                                if !emit(&events, WorkerEvent::Snapshot(snapshot)) {
                                    return;
                                }
                                initialized = true;
                            } else {
                                match synchronize_snapshot(&client, &events, &mut cursor, snapshot, &mut instance_id).await {
                                    Ok(true) => {}
                                    Ok(false) => return,
                                    Err(error) => {
                                        if !emit(&events, WorkerEvent::Connection(false))
                                            || !emit(&events, WorkerEvent::Failure(error.to_string())) {
                                            return;
                                        }
                                        continue;
                                    }
                                }
                            }
                            connected = true;
                            failure_reported = false;
                            match fetch_persona_current(&client, persona_epoch).await
                            {
                                Ok(Some(profile)) =>
                                {
                                    if !emit(&events, WorkerEvent::Persona(profile))
                                    {
                                        return;
                                    }
                                }
                                Ok(None) => {}
                                Err(error) =>
                                {
                                    if !emit(&events, WorkerEvent::Log(format!("persona refresh failed: {error}")))
                                    {
                                        return;
                                    }
                                }
                            }
                            match fetch_stage(&client).await
                            {
                                Ok(snapshot) =>
                                {
                                    if !emit(&events, WorkerEvent::Stage(snapshot))
                                    {
                                        return;
                                    }
                                }
                                Err(error) =>
                                {
                                    if !emit(&events, WorkerEvent::Log(format!("stage refresh failed: {error}")))
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                        Err(error) =>
                        {
                            if !failure_reported
                            {
                                failure_reported = true;
                                if !emit(&events, WorkerEvent::Connection(false))
                                    || !emit(&events, WorkerEvent::Failure(error.to_string()))
                                {
                                    return;
                                }
                            }
                        }
                    }
                    continue;
                }
                let polled = match poll(&client, cursor).await {
                    Ok(items) => forward_events(&client, &events, &mut cursor, items, &mut instance_id).await,
                    Err(error) => Err(error),
                };
                match polled
                {
                    Ok(true) => {}
                    Ok(false) => return,
                    Err(error) =>
                    {
                        connected = false;
                        failure_reported = true;
                        if !emit(&events, WorkerEvent::Connection(false))
                            || !emit(&events, WorkerEvent::Failure(error.to_string()))
                        {
                            return;
                        }
                    }
                }
                ticks = (ticks + 1) % HEALTH_REFRESH_TICKS;
                if connected && ticks == 0
                {
                    if let Ok(snapshot) = fetch_snapshot(&client).await {
                        match synchronize_snapshot(&client, &events, &mut cursor, snapshot, &mut instance_id).await {
                            Ok(true) => {}
                            Ok(false) => return,
                            Err(error) => {
                                connected = false;
                                failure_reported = true;
                                if !emit(&events, WorkerEvent::Connection(false))
                                    || !emit(&events, WorkerEvent::Failure(error.to_string())) {
                                    return;
                                }
                                continue;
                            }
                        }
                    }
                    match fetch_health(&client).await
                    {
                        Ok(health) =>
                        {
                            if !emit(&events, WorkerEvent::Health(health))
                            {
                                return;
                            }
                        }
                        Err(error) =>
                        {
                            if !emit(
                                &events,
                                WorkerEvent::Log(format!("health refresh failed: {error}")),
                            )
                            {
                                return;
                            }
                        }
                    }
                    match fetch_persona_current(&client, persona_epoch).await
                    {
                        Ok(Some(profile)) =>
                        {
                            if !emit(&events, WorkerEvent::Persona(profile))
                            {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(error) =>
                        {
                            if !emit(&events, WorkerEvent::Log(format!("persona refresh failed: {error}")))
                            {
                                return;
                            }
                        }
                    }
                    match fetch_stage(&client).await
                    {
                        Ok(snapshot) =>
                        {
                            if !emit(&events, WorkerEvent::Stage(snapshot))
                            {
                                return;
                            }
                        }
                        Err(error) =>
                        {
                            if !emit(&events, WorkerEvent::Log(format!("stage refresh failed: {error}")))
                            {
                                return;
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn send_command(
    client: &ControlClient,
    command: WorkerCommand,
) -> Result<Option<PersonaSnapshot>, AppError> {
    let (command, persona) = match command {
        WorkerCommand::Submit(text) => (ControlCommand::Submit { text }, None),
        WorkerCommand::Interrupt => (
            ControlCommand::Interrupt {
                reason: "desktop user interrupt".to_owned(),
            },
            None,
        ),
        WorkerCommand::EmergencyStop => (ControlCommand::EmergencyStop, None),
        WorkerCommand::Automation { .. } => {
            return Err(AppError::protocol(
                "automation uses a dedicated reply channel",
            ));
        }
        WorkerCommand::Memory { .. } => {
            return Err(AppError::protocol("memory uses a dedicated reply channel"));
        }
        WorkerCommand::SetPersona(profile) => (
            ControlCommand::SetPersona {
                profile: profile.clone(),
            },
            Some(profile),
        ),
    };
    match client.send(command).await? {
        ControlPayload::Accepted => Ok(persona),
        _ => Err(AppError::protocol(
            "control command returned an unexpected payload",
        )),
    }
}

async fn synchronize_snapshot(
    client: &ControlClient,
    events: &Sender<WorkerEvent>,
    cursor: &mut u64,
    snapshot: RuntimeSnapshot,
    instance_id: &mut Option<uuid::Uuid>,
) -> Result<bool, AppError> {
    if snapshot.instance_id != *instance_id || snapshot.last_sequence < *cursor {
        // Identity also detects a restarted service whose sequence caught up.
        // Sequence rollback remains a fallback for older services without IDs.
        *cursor = snapshot.last_sequence;
        *instance_id = snapshot.instance_id;
        return Ok(emit(events, WorkerEvent::HistoryGap(snapshot)));
    }
    if snapshot.last_sequence > *cursor {
        let items = poll(client, *cursor).await?;
        if !forward_events(client, events, cursor, items, instance_id).await? {
            return Ok(false);
        }
    }
    // A runtime snapshot contains no conversation text. Publish it only after
    // every event it covers, and never replace newer event state with it.
    // Catch-up is bounded to one page; further pages arrive on the next tick.
    Ok(snapshot.instance_id != *instance_id
        || snapshot.last_sequence != *cursor
        || emit(events, WorkerEvent::Snapshot(snapshot)))
}

async fn forward_events(
    client: &ControlClient,
    events: &Sender<WorkerEvent>,
    cursor: &mut u64,
    items: Vec<SequencedEvent>,
    instance_id: &mut Option<uuid::Uuid>,
) -> Result<bool, AppError> {
    let Some(first) = items.first() else {
        return Ok(true);
    };
    if first.sequence != cursor.saturating_add(1) {
        // Retention may have expired during disconnection. A fresh snapshot
        // re-establishes a known boundary without pretending to recover text.
        let snapshot = fetch_snapshot(client).await?;
        *cursor = snapshot.last_sequence;
        *instance_id = snapshot.instance_id;
        return Ok(emit(events, WorkerEvent::HistoryGap(snapshot)));
    }
    if items
        .windows(2)
        .any(|pair| pair[1].sequence != pair[0].sequence.saturating_add(1))
    {
        return Err(AppError::protocol("control event batch is not contiguous"));
    }
    let sequence = items.last().expect("nonempty events").sequence;
    let count = items.len();
    if !emit(events, WorkerEvent::Events(items)) {
        return Ok(false);
    }
    *cursor = sequence;
    Ok(emit(
        events,
        WorkerEvent::Log(format!("received {count} event(s)")),
    ))
}

async fn fetch_persona(client: &ControlClient) -> Result<PersonaSnapshot, AppError> {
    match client.send(ControlCommand::Persona).await? {
        ControlPayload::Persona(profile) => Ok(profile),
        _ => Err(AppError::protocol("persona returned an unexpected payload")),
    }
}

async fn fetch_persona_current(
    client: &ControlClient,
    epoch: &AtomicU64,
) -> Result<Option<PersonaSnapshot>, AppError> {
    let before = epoch.load(Ordering::Acquire);
    if !before.is_multiple_of(2) {
        return Ok(None);
    }
    let profile = fetch_persona(client).await?;
    Ok((epoch.load(Ordering::Acquire) == before).then_some(profile))
}

async fn fetch_stage(client: &ControlClient) -> Result<StageSnapshot, AppError> {
    match client.send(ControlCommand::Stage).await? {
        ControlPayload::Stage(snapshot) => Ok(snapshot),
        _ => Err(AppError::protocol("stage returned an unexpected payload")),
    }
}

async fn fetch_snapshot(client: &ControlClient) -> Result<RuntimeSnapshot, AppError> {
    match client.send(ControlCommand::Status).await? {
        ControlPayload::Snapshot(snapshot) => Ok(snapshot),
        _ => Err(AppError::protocol("status returned an unexpected payload")),
    }
}

async fn fetch_health(client: &ControlClient) -> Result<Vec<ComponentHealth>, AppError> {
    match client.send(ControlCommand::Health).await? {
        ControlPayload::Health(health) => Ok(health),
        _ => Err(AppError::protocol("health returned an unexpected payload")),
    }
}

async fn poll(client: &ControlClient, after: u64) -> Result<Vec<SequencedEvent>, AppError> {
    match client
        .send(ControlCommand::Events { after, limit: 256 })
        .await?
    {
        ControlPayload::Events(events) => Ok(events),
        _ => Err(AppError::protocol("events returned an unexpected payload")),
    }
}

fn emit(sender: &Sender<WorkerEvent>, event: WorkerEvent) -> bool {
    sender.send(event).is_ok()
}

struct QueuedAutomation {
    request_id: u64,
    request: AutomationRequest,
    epoch: u64,
}

// Separate bounded lanes keep both Pause and Stop independent of a slow
// conversation, Start, Resume, Status, or even the other priority request.
pub(crate) async fn run_commands(
    client: &ControlClient,
    mut commands: UnboundedReceiver<WorkerCommand>,
    events: &Sender<WorkerEvent>,
    persona_epoch: &AtomicU64,
) {
    let (ordinary, ordinary_rx) = tokio::sync::mpsc::unbounded_channel();
    let (regular, regular_rx) = tokio::sync::mpsc::channel(32);
    let (status, status_rx) = tokio::sync::mpsc::channel(1);
    let (pause, pause_rx) = tokio::sync::mpsc::channel(1);
    let (stop, stop_rx) = tokio::sync::mpsc::channel(1);
    let epoch = AtomicU64::new(0);
    let router = async {
        while let Some(command) = commands.recv().await {
            if matches!(&command, WorkerCommand::EmergencyStop) {
                let _previous = epoch.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    value.checked_add(1)
                });
            }
            let WorkerCommand::Automation {
                request_id,
                request,
            } = command
            else {
                if ordinary.send(command).is_err() {
                    return;
                }
                continue;
            };
            if let Err(error) = request.validate() {
                if !emit(
                    events,
                    WorkerEvent::Automation {
                        request_id,
                        result: Err(error),
                    },
                ) {
                    return;
                }
                continue;
            }
            let priority = matches!(
                &request,
                AutomationRequest::Pause { .. } | AutomationRequest::Stop { .. }
            );
            if priority {
                // MAX is a permanent fail-closed epoch; never wrap old work live.
                let _previous = epoch.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    value.checked_add(1)
                });
            }
            let lane = match &request {
                AutomationRequest::Status => &status,
                AutomationRequest::Pause { .. } => &pause,
                AutomationRequest::Stop { .. } => &stop,
                _ => &regular,
            };
            let queued = QueuedAutomation {
                request_id,
                request,
                epoch: epoch.load(Ordering::Acquire),
            };
            if let Err(error) = lane.try_send(queued) {
                let queued = error.into_inner();
                if !emit(
                    events,
                    WorkerEvent::Automation {
                        request_id: queued.request_id,
                        result: Err(AppError::unavailable(
                            "automation command queue is full or stopped",
                        )),
                    },
                ) {
                    return;
                }
            }
        }
    };
    tokio::select! {
        _ = router => {},
        _ = async {
            tokio::join!(
                run_ordinary_commands(client, ordinary_rx, events, persona_epoch),
                execute_automation(client, regular_rx, events, Some(&epoch)),
                execute_automation(client, pause_rx, events, None),
                execute_automation(client, stop_rx, events, None),
                poll_automation(client, status_rx, events),
            );
        } => {},
    }
}

async fn automation_request(
    client: &ControlClient,
    request: AutomationRequest,
) -> Result<AutomationSnapshot, AppError> {
    request.validate()?;
    match client.send(ControlCommand::Automation { request }).await? {
        ControlPayload::Automation(snapshot) => {
            snapshot.validate()?;
            Ok(*snapshot)
        }
        _ => Err(AppError::protocol(
            "automation returned an unexpected payload",
        )),
    }
}

async fn execute_automation(
    client: &ControlClient,
    mut commands: tokio::sync::mpsc::Receiver<QueuedAutomation>,
    events: &Sender<WorkerEvent>,
    epoch: Option<&AtomicU64>,
) {
    while let Some(queued) = commands.recv().await {
        let revoked = epoch.is_some_and(|current| {
            let current = current.load(Ordering::Acquire);
            current == u64::MAX || current != queued.epoch
        });
        let result = if revoked {
            Err(AppError::unavailable(
                "queued automation command revoked by Pause or Stop",
            ))
        } else {
            // A dispatched request may already have reached the service. Never
            // retry it after a timeout; only the Status lane reconciles state.
            automation_request(client, queued.request).await
        };
        if !emit(
            events,
            WorkerEvent::Automation {
                request_id: queued.request_id,
                result,
            },
        ) {
            return;
        }
    }
}

async fn poll_automation(
    client: &ControlClient,
    mut commands: tokio::sync::mpsc::Receiver<QueuedAutomation>,
    events: &Sender<WorkerEvent>,
) {
    // Do not send unsolicited automation requests to older services. The panel
    // activates this read-only stream with its first explicit Status request.
    let mut active = false;
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            queued = commands.recv() => {
                let Some(queued) = queued else { return; };
                let result = automation_request(client, AutomationRequest::Status).await;
                if !emit(events, WorkerEvent::Automation { request_id: queued.request_id, result }) { return; }
                active = true;
                interval.reset();
            }
            _ = interval.tick(), if active => {
                let result = automation_request(client, AutomationRequest::Status).await;
                if !emit(events, WorkerEvent::AutomationStatus(result)) { return; }
            }
        }
    }
}
