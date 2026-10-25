#![forbid(unsafe_code)]

mod args;
mod automation_replay;
mod control_snapshot;
mod conversation_activity;
mod events;
mod lifecycle;
mod playback;
mod strategy_loop;
mod template_game;
mod template_host;

#[cfg(test)]
mod duplex_activity_tests;
#[cfg(test)]
mod interruption_tests;
#[cfg(test)]
mod speech_tests;

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ai_ex_asr::WhisperHttpTranscriber;
use ai_ex_audio::{AudioPlayer, SpeechQueue, SpeechReceiver};
use ai_ex_audit::JsonlAuditLog;
use ai_ex_automation::{
    AutomationPluginRequest, AutomationPluginResponse, AutomationPluginTransport,
};
use ai_ex_bilibili::{BilibiliConnector, BilibiliSettings};
use ai_ex_config::{AppConfig, BilibiliConfig, ModelBackend, PluginConfig};
use ai_ex_control::{ControlBackend, ControlCommand, ControlPayload, ControlServer, MemoryReply};
use ai_ex_core::{
    ConversationPolicy, EventSink, LanguageModelPort, ModelRequest, Runtime, StageJournal,
    StageOutput, spawn_runtime,
};
use ai_ex_deepseek::{DeepSeekClient, DeepSeekSettings};
use ai_ex_domain::{
    AppError, ComponentHealth, ErrorKind, LiveResponseMode, PersonaSnapshot, SystemEvent, TurnId,
};
use ai_ex_event_bus::{EventBus, EventPolicy, PublishOutcome, load_jsonl, project_memory};
use ai_ex_koboldcpp::{KoboldCppClient, KoboldCppSettings};
use ai_ex_memory::MemoryStore;
use ai_ex_observability::{EventHub, TeeEventSink};
use ai_ex_ollama::{OllamaClient, OllamaEmbeddingClient};
use ai_ex_plugin::{PluginHealth, PluginRegistry, StdioPlugin};
use ai_ex_safety::{Capability, SafetyGate, SafetyPolicy};
use ai_ex_stage::{StageExecutor, StageRouter};
use ai_ex_stage_obs::{
    ObsDryRunStage, ObsSettings, ObsWebSocketStage, parse_records_jsonl, replay_records,
};
use ai_ex_tts::{GptSovitsClient, GptSovitsSettings};
use ai_ex_vision::{
    OllamaVisionClient, OllamaVisionSettings, VisionAnalyzerPort, VisionRequest, VisualFrame,
};
use ai_ex_vts::{VtsClient, VtsSettings};
use args::Args;
use async_trait::async_trait;
use automation_replay::replay as replay_automation;
use conversation_activity::ConversationActivity;
use events::ConsoleEvents;
use template_host::TemplateHost;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

#[cfg(feature = "native-capture")]
use ai_ex_capture::{CaptureSettings, NativeAudioSource};
#[cfg(any(feature = "native-capture", test))]
use ai_ex_duplex::{
    AudioSourcePort, DuplexController, DuplexDirective, EnergyVad, TranscriberPort, VadConfig,
};

enum ConfiguredModel {
    Ollama(OllamaClient),
    DeepSeek(DeepSeekClient),
    KoboldCpp(KoboldCppClient),
}

impl ConfiguredModel {
    async fn health(&self) -> ComponentHealth {
        match self {
            Self::Ollama(client) => client.health().await,
            Self::DeepSeek(client) => client.health().await,
            Self::KoboldCpp(client) => client.health().await,
        }
    }
}

#[async_trait]
impl LanguageModelPort for ConfiguredModel {
    async fn stream(
        &mut self,
        request: ModelRequest,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<String, AppError>>, AppError> {
        match self {
            Self::Ollama(client) => client.stream(request).await,
            Self::DeepSeek(client) => client.stream(request).await,
            Self::KoboldCpp(client) => client.stream(request).await,
        }
    }

    async fn cancel(&mut self, turn_id: TurnId) -> Result<(), AppError> {
        match self {
            Self::Ollama(client) => client.cancel(turn_id).await,
            Self::DeepSeek(client) => client.cancel(turn_id).await,
            Self::KoboldCpp(client) => client.cancel(turn_id).await,
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if let Err(error) = run().await {
        eprintln!("AIex failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), AppError> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments == ["--version"] {
        println!("ai-ex-service {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let args = Args::parse(arguments)?;
    if let Some(path) = args.template_game.as_ref() {
        return template_game::replay(path).await;
    }
    let config = AppConfig::load(&args.config).await?;
    if args.validate_config {
        println!("Configuration is valid (offline; no services or credentials accessed).");
        return Ok(());
    }
    if args.maintain_media {
        if !config.memory.enabled {
            return Err(AppError::configuration(
                "--maintain-media requires enabled memory",
            ));
        }
        let mut memory = MemoryStore::open(&config.memory.path).await?;
        memory.select_profile(&config.persona.profile_id).await?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| AppError::unavailable("system clock is before the Unix epoch"))?
            .as_millis();
        let report = memory
            .maintain_media(
                now_ms,
                args.media_capacity,
                args.apply_media_maintenance,
                args.archive_memory.as_deref(),
            )
            .await?;
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| AppError::protocol(error.to_string()))?
        );
        return Ok(());
    }
    if args.reindex_memory {
        if !config.memory.enabled || config.memory.embedding.is_none() {
            return Err(AppError::configuration(
                "--reindex-memory requires enabled memory and an explicit [memory.embedding] model",
            ));
        }
        let mut memory = build_memory(&config).await?;
        memory.select_profile(&config.persona.profile_id).await?;
        let count = memory.reindex_embeddings().await?;
        println!("Reindexed {count} memory record(s) for the configured persona.");
        return Ok(());
    }
    if (args.serve || args.managed) && !config.control.enabled {
        return Err(AppError::configuration(
            "--serve and --managed require control.enabled = true",
        ));
    }
    if let Some(path) = args.replay_events.as_ref() {
        let mut memory = if config.memory.enabled {
            MemoryStore::open(&config.memory.path).await?
        } else {
            MemoryStore::disabled()
        };
        memory.select_profile(&config.persona.profile_id).await?;
        return replay_events_to_memory(path, &mut memory, args.replay_report.as_deref()).await;
    }
    if let Some(path) = args.replay_stage.as_ref() {
        return replay_stage_records(path).await;
    }
    if let Some(path) = args.replay_automation.as_ref() {
        return replay_automation(path, &config).await;
    }
    let vision = build_vision(&config)?;
    if let (Some(image_path), Some(prompt)) = (args.vision_image, args.vision_prompt) {
        let mut vision = vision.ok_or_else(|| {
            AppError::configuration("vision analysis requires vision.enabled = true")
        })?;
        let bytes = tokio::fs::read(&image_path).await.map_err(|error| {
            AppError::unavailable(format!(
                "failed to read vision image {}: {error}",
                image_path.display(),
            ))
        })?;
        let request = VisionRequest::new(prompt, VisualFrame::detect(bytes)?)?;
        let observation = vision.analyze(request).await?;
        println!("{}", observation.text);
        return Ok(());
    }
    let model = build_model(&config)?;
    let vts = connect_vts(&config).await;
    let obs_runtime = connect_obs(&config).await;
    if obs_runtime.connected {
        tracing::info!("OBS WebSocket stage connected");
    }
    let (speech, receiver) = SpeechQueue::new(config.audio.queue_capacity)?;
    let player = receiver.player();
    let speech_activity = speech.activity();
    let tts = build_tts(&config).await?;
    let safety = Arc::new(build_safety(&config)?);
    let audit = build_audit(&config).await?;
    let plugin_runtime = connect_plugins(&config.plugins).await;

    let memory = build_memory(&config).await?;
    memory.select_profile(&config.persona.profile_id).await?;

    if args.check {
        return run_check(HealthContext {
            model: &model,
            vts: &vts,
            obs: &obs_runtime.health,
            speech: &speech,
            memory: &memory,
            player: &player,
            tts: tts.as_ref(),
            safety: safety.as_ref(),
            audit: audit.as_ref(),
            plugins: &plugin_runtime.registry,
            vision: vision.as_ref(),
            config: &config,
        })
        .await;
    }

    let startup_health = collect_health(HealthContext {
        model: &model,
        vts: &vts,
        obs: &obs_runtime.health,
        speech: &speech,
        memory: &memory,
        player: &player,
        tts: tts.as_ref(),
        safety: safety.as_ref(),
        audit: audit.as_ref(),
        plugins: &plugin_runtime.registry,
        vision: vision.as_ref(),
        config: &config,
    })
    .await;
    let event_hub = EventHub::new(256)?;
    let mut automation = TemplateHost::new(
        event_hub
            .current()
            .instance_id
            .ok_or_else(|| AppError::unavailable("service instance identity is unavailable"))?,
        speech_activity,
        Arc::clone(&safety),
    )?;
    if config.vision.enabled {
        let settings = OllamaVisionSettings {
            base_url: config.vision.base_url.clone(),
            model: config.vision.model.clone(),
            timeout: Duration::from_secs(config.vision.timeout_seconds),
        };
        automation = automation.with_strategy_factory(Arc::new(move || {
            Ok(Box::new(OllamaVisionClient::new(settings.clone())?) as Box<dyn VisionAnalyzerPort>)
        }));
    }
    let health_snapshot = Arc::new(RwLock::new(startup_health));
    publish_health_snapshot(&health_snapshot, &event_hub).await;
    let _plugin_refresh = plugin_runtime.spawn_health_refresh(Arc::clone(&health_snapshot));
    let _obs_health_refresh = spawn_obs_health_refresh(
        Arc::clone(&health_snapshot),
        obs_runtime.health_handle.clone(),
        event_hub.clone(),
    );

    let mut stage_router = StageRouter::new();
    stage_router.push(speech);
    stage_router.push(vts);
    stage_router.push_box(obs_runtime.stage);
    let stage_output = StageOutput::new(stage_router);
    let speech_port = stage_output.speech();
    let avatar_port = stage_output.avatar();
    let speech_task = tokio::spawn(run_speech_worker(receiver, tts, player, event_hub.clone()));
    let events = TeeEventSink::new(ConsoleEvents, event_hub.clone());
    let live_memory = memory.clone();
    let mut runtime = Runtime::with_policy(
        model,
        speech_port,
        avatar_port,
        memory,
        events,
        ConversationPolicy {
            system_prompt: config.effective_system_prompt(),
            history_turn_limit: config.conversation.history_turn_limit,
            memory_recall_limit: config.conversation.memory_recall_limit,
        },
    )?;
    runtime
        .set_persona(
            config.persona.profile_id.clone(),
            config.effective_system_prompt(),
        )
        .await?;
    let runtime = ConversationActivity::new(spawn_runtime(runtime, 32)?, automation.clone());
    if let Some(prompt) = args.prompt {
        runtime.submit(prompt).await?;
        automation.shutdown().await?;
        runtime.shutdown().await?;
        drop(stage_output);
        join_speech_worker(speech_task).await?;
        return Ok(());
    }

    let bilibili_runtime = spawn_bilibili(
        &config.bilibili,
        live_memory,
        runtime.clone(),
        event_hub.clone(),
        Arc::clone(&safety),
    )?;
    if let Some(handle) = bilibili_runtime.health_handle.clone() {
        let health = handle
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut snapshot = health_snapshot.write().await;
        update_component_health(&mut snapshot, health, &event_hub);
    }
    let _bilibili_health_refresh = spawn_bilibili_health_refresh(
        Arc::clone(&health_snapshot),
        bilibili_runtime.health_handle.clone(),
        event_hub.clone(),
    );
    let control_task = spawn_control(
        &config,
        runtime.clone(),
        event_hub.clone(),
        Arc::clone(&safety),
        Arc::clone(&health_snapshot),
        stage_output.journal(),
        automation.clone(),
    )
    .await?;
    let duplex_task = spawn_duplex(&config, runtime.clone(), automation.clone())?;
    let session_result = if args.serve || args.managed {
        lifecycle::wait_for_shutdown(args.managed).await
    } else {
        run_interactive(&runtime, &event_hub, &safety, &automation).await
    };
    if let Some(task) = bilibili_runtime.task {
        task.abort();
    }
    if let Some(task) = duplex_task {
        task.abort();
    }
    if let Some(task) = control_task {
        task.abort();
    }
    let automation_result = automation.shutdown().await;
    let shutdown_result = runtime.shutdown().await;
    drop(stage_output);
    let speech_result = join_speech_worker(speech_task).await;
    session_result?;
    automation_result?;
    shutdown_result?;
    speech_result
}

async fn run_interactive(
    runtime: &ConversationActivity,
    event_hub: &EventHub,
    safety: &SafetyGate,
    automation: &TemplateHost,
) -> Result<(), AppError> {
    println!("AIex Rust interactive mode. Commands: /status, /interrupt, /emergency-stop, /quit.",);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| AppError::unavailable(error.to_string()))?
    {
        let line = line.trim();
        if line == "/quit" {
            break;
        }
        if line.is_empty() {
            continue;
        }
        if line == "/interrupt" {
            if let Err(error) = runtime.interrupt("interactive barge-in").await {
                eprintln!("interrupt failed: {error}");
            }
            continue;
        }
        if line == "/status" {
            println!(
                "{}",
                serde_json::to_string_pretty(&event_hub.current()).unwrap_or_default(),
            );
            continue;
        }
        if line == "/emergency-stop" {
            safety.trigger_emergency_stop();
            automation.emergency_stop()?;
            if let Err(error) = runtime.interrupt("emergency stop").await
                && error.kind != ErrorKind::InvalidTransition
            {
                eprintln!("emergency interrupt failed: {error}");
            }
            println!("Emergency stop is active until the service restarts.");
            continue;
        }
        let pending = match runtime.start_submit(line.to_owned()) {
            Ok(pending) => pending,
            Err(error) => {
                eprintln!("turn failed: {error}");
                continue;
            }
        };
        tokio::spawn(async move {
            if let Err(error) = pending.wait().await {
                eprintln!("turn failed: {error}");
            }
        });
    }
    Ok(())
}

struct BilibiliRuntime {
    task: Option<tokio::task::JoinHandle<()>>,
    health_handle: Option<std::sync::Arc<std::sync::RwLock<ComponentHealth>>>,
}

fn spawn_bilibili(
    config: &BilibiliConfig,
    memory: MemoryStore,
    runtime: ConversationActivity,
    event_hub: EventHub,
    safety: Arc<SafetyGate>,
) -> Result<BilibiliRuntime, AppError> {
    if !config.enabled {
        return Ok(BilibiliRuntime {
            task: None,
            health_handle: None,
        });
    }
    let mut settings = BilibiliSettings::new(config.room_id)?;
    settings.endpoint = config.endpoint.clone();
    settings.cookie_env = config
        .cookie_env
        .clone()
        .filter(|value| !value.trim().is_empty());
    settings.reconnect_delay_ms = config.reconnect_delay_ms;
    let response_mode = config.response_mode();
    let reaction_cooldown_ms = config.reaction_cooldown_ms;
    let reconnect_delay_ms = config.reconnect_delay_ms;
    let connector = BilibiliConnector::new(settings);
    let health_handle = Some(connector.health_handle());
    let task = tokio::spawn(async move {
        let mut memory = memory;
        let mut event_hub = event_hub;
        let mut connector = connector;
        let (mut bus, mut receiver) = EventBus::new(EventPolicy::default());
        let mut reactions = tokio::task::JoinSet::new();
        let mut last_reaction_ms = None;
        loop {
            match connector.next_events().await {
                Ok(events) => {
                    for event in events {
                        if bus.publish(event) != PublishOutcome::Accepted {
                            continue;
                        }
                        while let Ok(delivered) = receiver.try_recv() {
                            let event_id = delivered.event_id;
                            let event_type = delivered.payload.event_type().to_owned();
                            let summary = delivered.payload.summary();
                            tracing::info!(%event_id, %event_type, %summary, "live event accepted");
                            event_hub
                                .publish(SystemEvent::LiveEventReceived {
                                    event_id,
                                    source: delivered.source.clone(),
                                    event_type,
                                    summary,
                                })
                                .await;
                            for projection in project_memory(&delivered) {
                                if let Err(error) = memory.remember_projection(&projection).await {
                                    tracing::error!(%error, "bilibili memory projection failed");
                                }
                            }
                            let Some(prompt) = delivered.payload.reaction_prompt() else {
                                continue;
                            };
                            let automatic = reaction_allowed(
                                response_mode,
                                safety.emergency_stop_active(),
                                last_reaction_ms,
                                delivered.timestamp_ms,
                                reaction_cooldown_ms,
                            );
                            tracing::info!(%event_id, automatic, "live reaction suggestion emitted");
                            event_hub
                                .publish(SystemEvent::LiveResponseSuggested {
                                    event_id,
                                    text: prompt.clone(),
                                    automatic,
                                })
                                .await;
                            if !automatic {
                                continue;
                            }
                            last_reaction_ms = Some(delivered.timestamp_ms);
                            let pending = match runtime.start_submit(prompt) {
                                Ok(pending) => pending,
                                Err(error) => {
                                    tracing::warn!(%error, %event_id, "live reaction failed");
                                    continue;
                                }
                            };
                            reactions.spawn(async move {
                                if let Err(error) = pending.wait().await {
                                    tracing::warn!(%error, %event_id, "live reaction failed");
                                }
                            });
                        }
                    }
                    while reactions.try_join_next().is_some() {}
                }
                Err(error) => {
                    tracing::warn!(%error, "bilibili event input stopped; retrying");
                    tokio::time::sleep(Duration::from_millis(reconnect_delay_ms)).await;
                }
            }
        }
    });
    Ok(BilibiliRuntime {
        task: Some(task),
        health_handle,
    })
}
fn reaction_allowed(
    response_mode: LiveResponseMode,
    emergency_stop: bool,
    last_reaction_ms: Option<u64>,
    now_ms: u64,
    cooldown_ms: u64,
) -> bool {
    response_mode.allows_automatic()
        && !emergency_stop
        && cooldown_ms > 0
        && last_reaction_ms.is_none_or(|previous| now_ms.saturating_sub(previous) >= cooldown_ms)
}
async fn replay_events_to_memory(
    path: &Path,
    memory: &mut MemoryStore,
    report_path: Option<&Path>,
) -> Result<(), AppError> {
    let events = load_jsonl(path)?;
    let input_count = events.len();
    let mut report = report_path
        .map(|path| {
            File::create(path).map(BufWriter::new).map_err(|error| {
                AppError::unavailable(format!("cannot create replay report: {error}"))
            })
        })
        .transpose()?;
    let (mut bus, mut receiver) = EventBus::new(EventPolicy::default());
    let mut accepted = 0_usize;
    let mut filtered = 0_usize;
    let mut projected = 0_usize;
    let mut reaction_suggestions = 0_usize;
    let before = memory.len().await;
    for event in events {
        let event_id = event.event_id.to_string();
        let timestamp_ms = event.timestamp_ms;
        let event_type = event.payload.event_type().to_owned();
        let priority = format!("{:?}", event.payload.priority());
        let outcome = bus.publish(event);
        let (reaction_prompt, projection_count) = if outcome == PublishOutcome::Accepted {
            accepted += 1;
            match receiver.try_recv() {
                Ok(delivered) => {
                    let reaction_prompt = delivered.payload.reaction_prompt();
                    if let Some(prompt) = reaction_prompt.as_ref() {
                        reaction_suggestions += 1;
                        println!(
                            "live reaction suggestion: event={} type={} summary={} prompt={}",
                            delivered.event_id,
                            delivered.payload.event_type(),
                            delivered.payload.summary(),
                            prompt,
                        );
                    }
                    let projections = project_memory(&delivered);
                    let projection_count = projections.len();
                    for projection in projections {
                        memory.remember_projection(&projection).await?;
                        projected += 1;
                    }
                    (reaction_prompt, projection_count)
                }
                Err(_) => (None, 0),
            }
        } else {
            filtered += 1;
            println!("live event filtered: type={event_type} outcome={outcome:?}");
            (None, 0)
        };
        if let Some(writer) = report.as_mut() {
            let record = serde_json::json!({
                "event_id": event_id,
                "timestamp_ms": timestamp_ms,
                "event_type": event_type,
                "priority": priority,
                "outcome": format!("{outcome:?}"),
                "reaction_prompt": reaction_prompt,
                "memory_projections": projection_count,
            });
            serde_json::to_writer(&mut *writer, &record).map_err(|error| {
                AppError::protocol(format!("cannot encode replay report: {error}"))
            })?;
            writer.write_all(b"\n").map_err(|error| {
                AppError::unavailable(format!("cannot write replay report: {error}"))
            })?;
        }
    }
    if let Some(writer) = report.as_mut() {
        writer.flush().map_err(|error| {
            AppError::unavailable(format!("cannot flush replay report: {error}"))
        })?;
    }
    let persisted = memory.len().await.saturating_sub(before);
    println!(
        "event replay complete: input={input_count} accepted={accepted} filtered={filtered} reaction_suggestions={reaction_suggestions} projected_memory={projected} persisted_memory={persisted} report={}",
        report_path.map_or_else(|| "none".to_owned(), |path| path.display().to_string()),
    );
    Ok(())
}
async fn replay_stage_records(path: &Path) -> Result<(), AppError> {
    let input = tokio::fs::read_to_string(path).await.map_err(|error| {
        AppError::unavailable(format!(
            "failed to read OBS stage JSONL {}: {error}",
            path.display()
        ))
    })?;
    let records = parse_records_jsonl(&input)?;
    let mut stage = ObsDryRunStage::new(records.len().max(1))?;
    let count = replay_records(&mut stage, &records).await?;
    println!("OBS dry-run replay complete: records={count}");
    for record in stage.records() {
        println!(
            "stage action #{}: {}",
            record.sequence,
            record.action.kind()
        );
    }
    Ok(())
}

async fn join_speech_worker(task: tokio::task::JoinHandle<()>) -> Result<(), AppError> {
    task.await
        .map_err(|error| AppError::unavailable(format!("speech worker failed: {error}")))
}

async fn connect_vts(config: &AppConfig) -> VtsClient {
    if !config.vts.enabled {
        return VtsClient::disabled();
    }
    let settings = VtsSettings {
        host: config.vts.host.clone(),
        port: config.vts.port,
        token_path: PathBuf::from(&config.vts.token_path),
        plugin_name: config.vts.plugin_name.clone(),
        developer: config.vts.developer.clone(),
        expression_hotkeys: config.vts.expression_hotkeys.clone(),
    };
    match VtsClient::connect(settings).await {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(%error, "VTS unavailable; avatar output disabled");
            VtsClient::unavailable(error.to_string())
        }
    }
}

struct ObsRuntime {
    stage: Box<dyn StageExecutor>,
    health: ComponentHealth,
    health_handle: Option<std::sync::Arc<std::sync::RwLock<ComponentHealth>>>,
    connected: bool,
}
fn fallback_obs_stage() -> Box<dyn StageExecutor> {
    Box::new(ObsDryRunStage::new(256).expect("valid OBS dry-run capacity"))
}
async fn connect_obs(config: &AppConfig) -> ObsRuntime {
    if !config.obs.enabled {
        return ObsRuntime {
            stage: fallback_obs_stage(),
            health: obs_stage_health(),
            health_handle: None,
            connected: false,
        };
    }
    let mut settings = match ObsSettings::new(config.obs.host.clone(), config.obs.port) {
        Ok(settings) => settings,
        Err(error) => {
            return ObsRuntime {
                stage: fallback_obs_stage(),
                health: ComponentHealth::unavailable("obs-websocket", error.to_string()),
                health_handle: None,
                connected: false,
            };
        }
    };
    settings.password = std::env::var(&config.obs.password_env).ok();
    settings.subtitle_input = Some(config.obs.subtitle_input.clone());
    settings.timeout = Duration::from_secs(config.obs.timeout_seconds);
    match ObsWebSocketStage::connect(settings).await {
        Ok(stage) => {
            let health = stage.health();
            let health_handle = Some(stage.health_handle());
            ObsRuntime {
                stage: Box::new(stage),
                health,
                health_handle,
                connected: true,
            }
        }
        Err(error) => {
            tracing::warn!(%error, "OBS WebSocket unavailable; stage output disabled");
            ObsRuntime {
                stage: fallback_obs_stage(),
                health: ComponentHealth::unavailable("obs-websocket", error.to_string()),
                health_handle: None,
                connected: false,
            }
        }
    }
}

fn replace_component_health(snapshot: &mut Vec<ComponentHealth>, health: ComponentHealth) {
    snapshot.retain(|item| item.component != health.component);
    snapshot.push(health);
}

fn update_component_health(
    snapshot: &mut Vec<ComponentHealth>,
    health: ComponentHealth,
    events: &EventHub,
) {
    let changed = snapshot
        .iter()
        .find(|item| item.component == health.component)
        != Some(&health);
    let component = health.component.clone();
    let ready = health.ready;
    let detail = health.detail.clone();
    replace_component_health(snapshot, health);
    if changed {
        events.publish_now(SystemEvent::ComponentHealthChanged {
            component,
            ready,
            detail,
        });
    }
}

async fn publish_health_snapshot(
    health_snapshot: &Arc<RwLock<Vec<ComponentHealth>>>,
    events: &EventHub,
) {
    let snapshot = health_snapshot.read().await.clone();
    for health in snapshot {
        events.publish_now(SystemEvent::ComponentHealthChanged {
            component: health.component,
            ready: health.ready,
            detail: health.detail,
        });
    }
}

fn spawn_obs_health_refresh(
    health_snapshot: Arc<RwLock<Vec<ComponentHealth>>>,
    obs_health: Option<std::sync::Arc<std::sync::RwLock<ComponentHealth>>>,
    events: EventHub,
) -> Option<tokio::task::JoinHandle<()>> {
    let obs_health = obs_health?;
    Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let health = obs_health
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let mut snapshot = health_snapshot.write().await;
            update_component_health(&mut snapshot, health, &events);
        }
    }))
}

fn spawn_bilibili_health_refresh(
    health_snapshot: Arc<RwLock<Vec<ComponentHealth>>>,
    bilibili_health: Option<std::sync::Arc<std::sync::RwLock<ComponentHealth>>>,
    events: EventHub,
) -> Option<tokio::task::JoinHandle<()>> {
    let bilibili_health = bilibili_health?;
    Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let health = bilibili_health
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let mut snapshot = health_snapshot.write().await;
            update_component_health(&mut snapshot, health, &events);
        }
    }))
}

pub struct StdioAutomationTransport {
    plugin: StdioPlugin,
}

impl StdioAutomationTransport {
    pub fn new(plugin: StdioPlugin) -> Self {
        Self { plugin }
    }
}

#[async_trait]
impl AutomationPluginTransport for StdioAutomationTransport {
    async fn call(
        &mut self,
        request: AutomationPluginRequest,
    ) -> Result<AutomationPluginResponse, AppError> {
        request.validate()?;
        let params = serde_json::to_value(&request).map_err(|error| {
            AppError::protocol(format!("automation plugin request encode failed: {error}"))
        })?;
        let value = self.plugin.request("automation", params).await?;
        let response: AutomationPluginResponse =
            serde_json::from_value(value).map_err(|error| {
                AppError::protocol(format!("invalid automation plugin response: {error}"))
            })?;
        response.validate()?;
        Ok(response)
    }
}
struct PluginRuntime {
    registry: PluginRegistry,
    processes: Vec<ManagedPlugin>,
}

struct ManagedPlugin {
    id: String,
    process: StdioPlugin,
}

impl PluginRuntime {
    fn spawn_health_refresh(
        self,
        health_snapshot: Arc<RwLock<Vec<ComponentHealth>>>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if self.processes.is_empty() {
            return None;
        }
        Some(tokio::spawn(async move {
            let mut runtime = self;
            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;
                for plugin in &mut runtime.processes {
                    let health = match plugin.process.try_wait() {
                        Ok(Some(status)) => PluginHealth {
                            ready: false,
                            detail: format!("process exited: {status}"),
                        },
                        Err(error) => PluginHealth {
                            ready: false,
                            detail: error.to_string(),
                        },
                        Ok(None) => match tokio::time::timeout(
                            Duration::from_secs(5),
                            plugin.process.health(),
                        )
                        .await
                        {
                            Ok(Ok(health)) => health,
                            Ok(Err(error)) => PluginHealth {
                                ready: false,
                                detail: error.to_string(),
                            },
                            Err(_) => PluginHealth {
                                ready: false,
                                detail: "plugin health request timed out".to_owned(),
                            },
                        },
                    };
                    if let Err(error) = runtime.registry.update_health(&plugin.id, health) {
                        tracing::warn!(plugin = %plugin.id, %error, "plugin health refresh rejected");
                    }
                }
                let registry_health = runtime.registry.health();
                let plugin_health = runtime.registry.component_health();
                let mut snapshot = health_snapshot.write().await;
                snapshot.retain(|item| {
                    item.component != "plugin-registry" && !item.component.starts_with("plugin:")
                });
                snapshot.push(registry_health);
                snapshot.extend(plugin_health);
            }
        }))
    }
}
async fn connect_plugins(config: &PluginConfig) -> PluginRuntime {
    let mut registry = PluginRegistry::new();
    let mut processes = Vec::new();
    if !config.enabled {
        return PluginRuntime {
            registry,
            processes,
        };
    }
    for command in &config.commands {
        let mut plugin = match StdioPlugin::spawn(&command.program, &command.args) {
            Ok(plugin) => plugin,
            Err(error) => {
                tracing::warn!(plugin = %command.id, %error, "plugin process unavailable");
                let _ignored = registry.register_unavailable(&command.id, error.to_string());
                continue;
            }
        };
        let manifest = match tokio::time::timeout(Duration::from_secs(5), plugin.manifest()).await {
            Ok(result) => result,
            Err(_) => Err(AppError::connectivity("plugin manifest request timed out")),
        };
        let manifest = match manifest {
            Ok(manifest) if manifest.id == command.id => manifest,
            Ok(manifest) => {
                let detail = format!(
                    "manifest id mismatch: configured={}, reported={}",
                    command.id, manifest.id,
                );
                tracing::warn!(plugin = %command.id, "plugin manifest rejected");
                let _ignored = registry.register_unavailable(&command.id, detail);
                continue;
            }
            Err(error) => {
                tracing::warn!(plugin = %command.id, %error, "plugin manifest unavailable");
                let _ignored = registry.register_unavailable(&command.id, error.to_string());
                continue;
            }
        };
        if let Err(error) = registry.register(manifest) {
            tracing::warn!(plugin = %command.id, %error, "plugin registration rejected");
            let _ignored = registry.register_unavailable(&command.id, error.to_string());
            continue;
        }
        let health = match tokio::time::timeout(Duration::from_secs(5), plugin.health()).await {
            Ok(Ok(health)) => health,
            Ok(Err(error)) => PluginHealth {
                ready: false,
                detail: error.to_string(),
            },
            Err(_) => PluginHealth {
                ready: false,
                detail: "plugin health request timed out".to_owned(),
            },
        };
        if let Err(error) = registry.update_health(&command.id, health) {
            tracing::warn!(plugin = %command.id, %error, "plugin health update rejected");
        }
        processes.push(ManagedPlugin {
            id: command.id.clone(),
            process: plugin,
        });
    }
    PluginRuntime {
        registry,
        processes,
    }
}
struct HealthContext<'a> {
    model: &'a ConfiguredModel,
    vts: &'a VtsClient,
    obs: &'a ComponentHealth,
    speech: &'a SpeechQueue,
    memory: &'a MemoryStore,
    player: &'a AudioPlayer,
    tts: Option<&'a GptSovitsClient>,
    safety: &'a SafetyGate,
    audit: Option<&'a JsonlAuditLog>,
    plugins: &'a PluginRegistry,
    vision: Option<&'a OllamaVisionClient>,
    config: &'a AppConfig,
}

async fn run_check(context: HealthContext<'_>) -> Result<(), AppError> {
    let health = collect_health(context).await;
    println!(
        "{}",
        serde_json::to_string_pretty(&health).unwrap_or_default()
    );
    let failed: Vec<&ComponentHealth> = health.iter().filter(|item| !item.ready).collect();
    if !failed.is_empty() {
        return Err(AppError::unavailable(format!(
            "{} component(s) unavailable",
            failed.len()
        )));
    }
    Ok(())
}

async fn collect_health(context: HealthContext<'_>) -> Vec<ComponentHealth> {
    let mut health = vec![
        context.model.health().await,
        context.vts.health().clone(),
        context.obs.clone(),
        context.speech.health(),
        context.memory.health().await,
        context.safety.health(),
    ];
    if let Some(tts) = context.tts {
        health.push(tts.health().await);
        health.push(context.player.health().await);
    }
    if context.config.duplex.enabled {
        match build_asr(context.config) {
            Ok(asr) => health.push(asr.health().await),
            Err(error) => health.push(ComponentHealth::unavailable("asr", error.to_string())),
        }
        health.push(capture_health(context.config));
    }
    if context.config.control.enabled {
        health.push(control_health(context.config).await);
    }
    if let Some(audit) = context.audit {
        health.push(audit.health());
    }
    health.push(context.plugins.health());
    health.extend(context.plugins.component_health());
    if let Some(vision) = context.vision {
        health.push(vision.health().await);
    }
    health
}

fn obs_stage_health() -> ComponentHealth {
    ComponentHealth {
        component: "obs-dry-run".to_owned(),
        ready: true,
        detail: "no external side effects; capabilities=subtitle,scene,hotkey,interrupt".to_owned(),
    }
}

async fn run_speech_worker(
    mut receiver: SpeechReceiver,
    tts: Option<GptSovitsClient>,
    player: AudioPlayer,
    events: EventHub,
) {
    while let Some(job) = receiver.receive().await {
        let Some(tts) = &tts else {
            tracing::debug!(turn_id = ?job.turn_id, "TTS disabled; speech job skipped");
            continue;
        };
        let synthesis = tokio::select! {
            biased;
            _ = player.wait_for_cancellation(&job) => continue,
            result = tts.synthesize(&job.text) => result,
        };
        match synthesis {
            Ok(audio) => {
                let playback_events = playback::PlaybackPublisher::new(events.clone());
                if let Err(error) = player
                    .play_wav_observed(&job, audio.bytes, move |playback| {
                        playback_events.observe(playback);
                    })
                    .await
                {
                    tracing::error!(%error, "audio playback failed");
                }
            }
            Err(error) => tracing::error!(%error, "speech synthesis failed"),
        }
    }
}

async fn build_memory(config: &AppConfig) -> Result<MemoryStore, AppError> {
    if !config.memory.enabled {
        return Ok(MemoryStore::disabled());
    }
    let memory = MemoryStore::open(&config.memory.path)
        .await?
        .with_recall_policy(
            config.memory.recall_policy,
            config.memory.exclude_recent_turns,
        )?;
    match &config.memory.embedding {
        Some(settings) => Ok(memory.with_embedder(Arc::new(OllamaEmbeddingClient::new(
            &settings.base_url,
            &settings.model,
            &settings.space,
            Duration::from_secs(settings.timeout_seconds),
        )?))),
        None => Ok(memory),
    }
}
async fn build_tts(config: &AppConfig) -> Result<Option<GptSovitsClient>, AppError> {
    if !config.tts.enabled {
        return Ok(None);
    }
    let reference = tokio::fs::canonicalize(&config.tts.ref_audio_path)
        .await
        .map_err(|error| {
            AppError::configuration(format!(
                "cannot resolve TTS reference audio {}: {error}",
                config.tts.ref_audio_path
            ))
        })?;
    let settings = GptSovitsSettings {
        base_url: config.tts.base_url.clone(),
        timeout: Duration::from_secs(config.tts.timeout_seconds),
        text_lang: config.tts.text_lang.clone(),
        ref_audio_path: reference.to_string_lossy().into_owned(),
        prompt_text: config.tts.prompt_text.clone(),
        prompt_lang: config.tts.prompt_lang.clone(),
    };
    Ok(Some(GptSovitsClient::new(settings)?))
}

fn build_safety(config: &AppConfig) -> Result<SafetyGate, AppError> {
    let capabilities = config
        .safety
        .allowed_capabilities
        .iter()
        .map(|value| value.parse::<Capability>())
        .collect::<Result<BTreeSet<_>, _>>()?;
    if config.safety.automation_enabled
        && (capabilities.is_empty() || config.safety.allowed_targets.is_empty())
    {
        return Err(AppError::configuration(
            "enabled automation requires allowed capabilities and targets",
        ));
    }
    Ok(SafetyGate::new(SafetyPolicy {
        automation_enabled: config.safety.automation_enabled,
        allowed_capabilities: capabilities,
        allowed_targets: config.safety.allowed_targets.clone(),
    }))
}

async fn build_audit(config: &AppConfig) -> Result<Option<JsonlAuditLog>, AppError> {
    if !config.safety.automation_enabled {
        return Ok(None);
    }
    Ok(Some(JsonlAuditLog::open(&config.safety.audit_path).await?))
}

fn build_vision(config: &AppConfig) -> Result<Option<OllamaVisionClient>, AppError> {
    if !config.vision.enabled {
        return Ok(None);
    }
    Ok(Some(OllamaVisionClient::new(OllamaVisionSettings {
        base_url: config.vision.base_url.clone(),
        model: config.vision.model.clone(),
        timeout: Duration::from_secs(config.vision.timeout_seconds),
    })?))
}

fn build_model(config: &AppConfig) -> Result<ConfiguredModel, AppError> {
    match config.model.backend {
        ModelBackend::Ollama => Ok(ConfiguredModel::Ollama(OllamaClient::new(
            &config.ollama.base_url,
            &config.ollama.model,
            Duration::from_secs(config.ollama.timeout_seconds),
        )?)),
        ModelBackend::DeepSeek => {
            let api_key = std::env::var(&config.deepseek.api_key_env).map_err(|_| {
                AppError::configuration(format!(
                    "DeepSeek API key environment variable {} is not set",
                    config.deepseek.api_key_env,
                ))
            })?;
            Ok(ConfiguredModel::DeepSeek(DeepSeekClient::new(
                DeepSeekSettings {
                    base_url: config.deepseek.base_url.clone(),
                    model: config.deepseek.model.clone(),
                    api_key,
                    timeout: Duration::from_secs(config.deepseek.timeout_seconds),
                    thinking: config.deepseek.thinking,
                    reasoning_effort: config.deepseek.reasoning_effort.clone(),
                },
            )?))
        }
        ModelBackend::KoboldCpp => Ok(ConfiguredModel::KoboldCpp(KoboldCppClient::new(
            KoboldCppSettings {
                base_url: config.koboldcpp.base_url.clone(),
                timeout: Duration::from_secs(config.koboldcpp.timeout_seconds),
                max_context_length: config.koboldcpp.max_context_length,
                max_length: config.koboldcpp.max_length,
                temperature: config.koboldcpp.temperature,
            },
        )?)),
    }
}

fn build_asr(config: &AppConfig) -> Result<WhisperHttpTranscriber, AppError> {
    let language = match config.duplex.asr.language.trim() {
        "" => None,
        language => Some(language.to_owned()),
    };
    WhisperHttpTranscriber::new(
        &config.duplex.asr.endpoint,
        &config.duplex.asr.model,
        language,
        Duration::from_secs(config.duplex.asr.timeout_seconds),
    )
}

#[derive(Clone)]
struct ServiceControl {
    runtime: ConversationActivity,
    events: EventHub,
    safety: Arc<SafetyGate>,
    health: Arc<RwLock<Vec<ComponentHealth>>>,
    stage: StageJournal,
    automation: TemplateHost,
    persona: Arc<RwLock<PersonaSnapshot>>,
}

#[async_trait::async_trait]
impl ControlBackend for ServiceControl {
    async fn execute(&self, command: ControlCommand) -> Result<ControlPayload, AppError> {
        match command {
            ControlCommand::Automation { request } => Ok(ControlPayload::Automation(Box::new(
                self.automation.execute(request).await?,
            ))),
            ControlCommand::Submit { text } => {
                let text = text.trim();
                if text.is_empty() {
                    return Err(AppError::configuration(
                        "control submit text must not be empty",
                    ));
                }
                let pending = self.runtime.enqueue(text.to_owned()).await?;
                tokio::spawn(async move {
                    if let Err(error) = pending.wait().await {
                        tracing::error!(%error, "control turn failed");
                    }
                });
                Ok(ControlPayload::Accepted)
            }
            ControlCommand::Interrupt { reason } => {
                let reason = if reason.trim().is_empty() {
                    "control interrupt"
                } else {
                    reason.trim()
                };
                self.runtime.interrupt(reason).await?;
                Ok(ControlPayload::Accepted)
            }
            ControlCommand::Status => Ok(ControlPayload::Snapshot(control_snapshot::bounded(
                self.events.current(),
            ))),
            ControlCommand::Memory { request } => {
                let response = self.runtime.memory(request).await?;
                Ok(ControlPayload::Memory(Box::new(MemoryReply {
                    response,
                    snapshot: control_snapshot::bounded(self.events.current()),
                })))
            }
            ControlCommand::Persona => {
                Ok(ControlPayload::Persona(self.persona.read().await.clone()))
            }
            ControlCommand::SetPersona { profile } => {
                profile.validate()?;
                // Serialize identity updates and snapshots across the actor acknowledgement.
                let mut current = self.persona.write().await;
                self.runtime
                    .set_persona(profile.profile_id.clone(), profile.compiled_system_prompt())
                    .await?;
                let profile_id = profile.profile_id.clone();
                let revision = profile.revision;
                *current = profile;
                tracing::info!(%profile_id, revision, "persona changed");
                self.events.publish_now(SystemEvent::PersonaChanged {
                    profile_id,
                    revision,
                });
                Ok(ControlPayload::Accepted)
            }
            ControlCommand::Stage => Ok(ControlPayload::Stage(self.stage.snapshot())),
            ControlCommand::Health => Ok(ControlPayload::Health(self.health.read().await.clone())),
            ControlCommand::Events { after, limit } => {
                if limit == 0 || limit > 1_000 {
                    return Err(AppError::configuration(
                        "control event limit must be between 1 and 1000",
                    ));
                }
                Ok(ControlPayload::Events(
                    self.events.events_since(after, limit),
                ))
            }
            ControlCommand::EmergencyStop => {
                self.safety.trigger_emergency_stop();
                let automation_result = self.automation.emergency_stop();
                if let Err(error) = self.runtime.interrupt("emergency stop").await
                    && error.kind != ErrorKind::InvalidTransition
                {
                    return Err(error);
                }
                automation_result?;
                Ok(ControlPayload::Accepted)
            }
        }
    }
}

async fn spawn_control(
    config: &AppConfig,
    runtime: ConversationActivity,
    events: EventHub,
    safety: Arc<SafetyGate>,
    health: Arc<RwLock<Vec<ComponentHealth>>>,
    stage: StageJournal,
    automation: TemplateHost,
) -> Result<Option<tokio::task::JoinHandle<()>>, AppError> {
    if !config.control.enabled {
        return Ok(None);
    }
    let token = tokio::fs::read_to_string(&config.control.token_path)
        .await
        .map_err(|error| {
            AppError::configuration(format!(
                "cannot read control token {}: {error}",
                config.control.token_path,
            ))
        })?;
    let server = ControlServer::bind(
        &config.control.bind,
        token.trim(),
        config.control.max_message_bytes,
    )
    .await?;
    let address = server.local_addr()?;
    let persona = PersonaSnapshot {
        profile_id: config.persona.profile_id.clone(),
        revision: config.persona.revision,
        name: config.persona.name.clone(),
        system_prompt: config.persona.system_prompt.clone(),
        tone: config.persona.tone.clone(),
        taboos: config.persona.taboos.clone(),
        live_mode: config.persona.live_mode.clone(),
    };
    persona.validate()?;
    let backend = Arc::new(ServiceControl {
        runtime,
        events,
        safety,
        health,
        persona: Arc::new(RwLock::new(persona)),
        stage,
        automation,
    });
    tracing::info!(%address, "local control server ready");
    Ok(Some(tokio::spawn(async move {
        if let Err(error) = server.serve(backend).await {
            tracing::error!(%error, "control server stopped");
        }
    })))
}

async fn control_health(config: &AppConfig) -> ComponentHealth {
    let token = match tokio::fs::read_to_string(&config.control.token_path).await {
        Ok(token) => token,
        Err(error) => {
            return ComponentHealth::unavailable(
                "control",
                format!("cannot read token file: {error}"),
            );
        }
    };
    match ControlServer::bind(
        &config.control.bind,
        token.trim(),
        config.control.max_message_bytes,
    )
    .await
    {
        Ok(server) => match server.local_addr() {
            Ok(address) => ComponentHealth {
                component: "control".to_owned(),
                ready: true,
                detail: format!("loopback endpoint available at {address}"),
            },
            Err(error) => ComponentHealth::unavailable("control", error.to_string()),
        },
        Err(error) => ComponentHealth::unavailable("control", error.to_string()),
    }
}

#[cfg(not(feature = "native-capture"))]
fn capture_health(_config: &AppConfig) -> ComponentHealth {
    ComponentHealth::unavailable(
        "audio-input",
        "binary was built without the native-capture feature",
    )
}

#[cfg(feature = "native-capture")]
fn capture_health(config: &AppConfig) -> ComponentHealth {
    let device_name = match config.duplex.input_device.trim() {
        "" => None,
        name => Some(name.to_owned()),
    };
    let settings = CaptureSettings {
        device_name,
        queue_capacity: config.duplex.capture_queue_capacity,
    };
    match NativeAudioSource::open(settings) {
        Ok(_source) => ComponentHealth::ready("audio-input"),
        Err(error) => ComponentHealth::unavailable("audio-input", error.to_string()),
    }
}

#[cfg(not(feature = "native-capture"))]
fn spawn_duplex(
    config: &AppConfig,
    _runtime: ConversationActivity,
    _automation: TemplateHost,
) -> Result<Option<tokio::task::JoinHandle<()>>, AppError> {
    if config.duplex.enabled {
        return Err(AppError::configuration(
            "duplex is enabled but this binary lacks the native-capture feature",
        ));
    }
    Ok(None)
}

#[cfg(feature = "native-capture")]
fn spawn_duplex(
    config: &AppConfig,
    runtime: ConversationActivity,
    automation: TemplateHost,
) -> Result<Option<tokio::task::JoinHandle<()>>, AppError> {
    if !config.duplex.enabled {
        return Ok(None);
    }
    let device_name = match config.duplex.input_device.trim() {
        "" => None,
        name => Some(name.to_owned()),
    };
    let source = NativeAudioSource::open(CaptureSettings {
        device_name,
        queue_capacity: config.duplex.capture_queue_capacity,
    })?;
    let transcriber = build_asr(config)?;
    let vad = EnergyVad::new(VadConfig {
        start_threshold: config.duplex.start_threshold,
        continue_threshold: config.duplex.continue_threshold,
        start_frames: config.duplex.start_frames,
        end_silence_frames: config.duplex.end_silence_frames,
        max_utterance_frames: config.duplex.max_utterance_frames,
    })?;
    let controller = DuplexController::new(vad, transcriber);
    Ok(Some(tokio::spawn(async move {
        if let Err(error) = run_duplex(source, controller, runtime, automation).await {
            tracing::error!(%error, "full-duplex input stopped");
        }
    })))
}

#[cfg(any(feature = "native-capture", test))]
async fn run_duplex<S, T>(
    mut source: S,
    mut controller: DuplexController<T>,
    runtime: ConversationActivity,
    automation: TemplateHost,
) -> Result<(), AppError>
where
    S: AudioSourcePort,
    T: TranscriberPort,
{
    let mut voice_activity = None;
    while let Some(frame) = source.next_frame().await? {
        let Some(directive) = controller.process_frame(frame).await? else {
            if !controller.speech_in_progress() {
                drop(voice_activity.take());
            }
            continue;
        };
        match directive {
            DuplexDirective::InterruptCurrentTurn => {
                let activity = automation.begin_chat()?;
                voice_activity = Some(activity);
                if let Err(error) = runtime.interrupt("voice barge-in").await
                    && error.kind != ErrorKind::InvalidTransition
                {
                    return Err(error);
                }
            }
            DuplexDirective::SubmitTranscript(transcript) => {
                let pending = runtime.start_submit(transcript)?;
                drop(voice_activity.take());
                tokio::spawn(async move {
                    if let Err(error) = pending.wait().await {
                        tracing::error!(%error, "transcribed turn failed");
                    }
                });
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::{LiveResponseMode, reaction_allowed, replace_component_health};
    use ai_ex_domain::ComponentHealth;

    #[test]
    fn replaces_obs_health_without_duplicate_entries() {
        let mut snapshot = vec![
            ComponentHealth::ready("model"),
            ComponentHealth::ready("obs-websocket"),
        ];
        replace_component_health(
            &mut snapshot,
            ComponentHealth::unavailable("obs-websocket", "socket closed"),
        );
        assert_eq!(
            snapshot
                .iter()
                .filter(|item| item.component == "obs-websocket")
                .count(),
            1
        );
        let obs = snapshot
            .iter()
            .find(|item| item.component == "obs-websocket")
            .expect("OBS health remains present");
        assert!(!obs.ready);
        assert_eq!(obs.detail, "socket closed");
    }
    #[test]
    fn reaction_policy_requires_opt_in_and_cooldown() {
        assert!(!reaction_allowed(
            LiveResponseMode::Suggest,
            false,
            None,
            10,
            5
        ));
        assert!(!reaction_allowed(
            LiveResponseMode::Automatic,
            true,
            None,
            10,
            5
        ));
        assert!(reaction_allowed(
            LiveResponseMode::Automatic,
            false,
            None,
            10,
            5
        ));
        assert!(!reaction_allowed(
            LiveResponseMode::Automatic,
            false,
            Some(8),
            10,
            5
        ));
        assert!(reaction_allowed(
            LiveResponseMode::Automatic,
            false,
            Some(4),
            10,
            5
        ));
    }
}
