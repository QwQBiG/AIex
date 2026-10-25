//! Bounded offline template/reflex replay. Only supplied image files and the dry-run port are used.
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ai_ex_automation::{
    AuditRecord, AuditSink, AutomationCoordinator, DesktopBounds, DryRunAutomationPort,
    FrameGeometry, GameProfile, MatchOptions, PixelRect, ReflexController, ReflexFailure,
    ReflexMode, ReflexOptions, ReflexState, ReflexStepReport, ScreenFrame, TemplateMatcher,
    run_reflex_step,
};
use ai_ex_domain::AppError;
use ai_ex_safety::{Capability, SafetyGate, SafetyPolicy};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PIXELS: u64 = 16_777_216;
const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;
const MAX_FRAMES: u64 = 256;
const MAX_ACTIONS: u64 = 4096;
const MAX_REPORT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    profile: PathBuf,
    #[serde(default = "default_target")]
    target: String,
    #[serde(default = "default_mode")]
    mode: String,
    desktop: DesktopBounds,
    frames: Vec<FrameInput>,
    #[serde(default = "default_threshold")]
    threshold: f64,
    #[serde(default = "default_work")]
    max_work: u64,
    #[serde(default = "default_interval")]
    interval_ms: u64,
    max_frames: Option<u64>,
    max_actions: Option<u64>,
    search_region: Option<PixelRect>,
    report: Option<PathBuf>,
    strategy: Option<crate::strategy_loop::StrategyOptions>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameInput {
    image: PathBuf,
    timestamp_ms: u64,
    #[serde(default)]
    origin_x: i32,
    #[serde(default)]
    origin_y: i32,
    #[serde(default = "default_scale")]
    scale_x: f64,
    #[serde(default = "default_scale")]
    scale_y: f64,
}
fn default_target() -> String {
    "primary_target".into()
}
fn default_mode() -> String {
    "click_once".into()
}
fn default_threshold() -> f64 {
    0.7
}
fn default_work() -> u64 {
    250_000_000
}
fn default_interval() -> u64 {
    50
}
fn default_scale() -> f64 {
    1.0
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    execution_mode: &'static str,
    profile: String,
    target: String,
    mode: String,
    input_frames: usize,
    scheduled_frames: usize,
    skipped_frames: usize,
    outcome: String,
    steps: Vec<ReflexStepReport>,
    failure: Option<ReflexFailure>,
    audit: Vec<AuditRecord>,
}
struct ReplayAudit {
    records: Arc<Mutex<Vec<AuditRecord>>>,
    capacity: usize,
}
#[async_trait]
impl AuditSink for ReplayAudit {
    async fn record(&mut self, record: AuditRecord) -> Result<(), AppError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| AppError::unavailable("template replay audit lock failed"))?;
        if records.len() >= self.capacity {
            return Err(AppError::unavailable(
                "template replay audit budget exhausted",
            ));
        }
        records.push(record);
        Ok(())
    }
}

/// Validated, cadence-filtered replay input shared by CLI and service sessions.
/// Frame pixels, geometry and timestamps have matching positions in their vectors.
pub(crate) struct PreparedTemplate {
    pub(crate) controller: ReflexController,
    pub(crate) frames: Vec<ScreenFrame>,
    pub(crate) geometries: Vec<FrameGeometry>,
    pub(crate) timestamps_ms: Vec<u64>,
    pub(crate) profile: String,
    pub(crate) target: String,
    pub(crate) mode: String,
    pub(crate) input_frames: usize,
    pub(crate) report_path: Option<PathBuf>,
    pub(crate) action_capacity: usize,
    pub(crate) strategy: Option<crate::strategy_loop::StrategyOptions>,
}

/// Load bounded session input without accessing the CLI-only report destination.
pub(crate) fn load_session(path: &Path) -> Result<PreparedTemplate, AppError> {
    prepare(path, false)
}

fn prepare(path: &Path, check_report_path: bool) -> Result<PreparedTemplate, AppError> {
    let bytes = read_limited(path, MAX_MANIFEST_BYTES)?;
    let mut manifest: Manifest = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::configuration(format!(
            "invalid template replay JSON at line {} column {}",
            error.line(),
            error.column()
        ))
    })?;
    if manifest.schema_version != 1
        || manifest.frames.is_empty()
        || manifest.frames.len() > MAX_FRAMES as usize
        || manifest.interval_ms == 0
        || manifest.interval_ms > 60_000
    {
        return Err(AppError::configuration(
            "template replay requires schema 1, 1..=256 frames and interval_ms in 1..=60000",
        ));
    }
    if let Some(strategy) = &manifest.strategy {
        strategy.validate()?;
        if check_report_path {
            return Err(AppError::configuration(
                "strategy options require a service-hosted template session",
            ));
        }
    }
    let mode = match manifest.mode.as_str() {
        "click_once" => ReflexMode::ClickOnce,
        "click_repeat" => ReflexMode::ClickRepeat,
        "hover" => ReflexMode::Hover,
        _ => {
            return Err(AppError::configuration(
                "template replay mode must be click_once, click_repeat or hover",
            ));
        }
    };
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let report_path = manifest.report.as_ref().map(|p| resolve(base, p));
    if check_report_path && report_path.as_ref().is_some_and(|p| p.exists()) {
        return Err(AppError::configuration(
            "refusing to overwrite template replay report",
        ));
    }
    let profile_path = resolve(base, &manifest.profile);
    let profile_bytes = read_limited(&profile_path, MAX_MANIFEST_BYTES)?;
    let profile = GameProfile::parse(
        std::str::from_utf8(&profile_bytes)
            .map_err(|_| AppError::configuration("game profile must be UTF-8"))?,
    )?;
    let template_name = profile.template_file(&manifest.target)?;
    let templates = fs::canonicalize(
        profile_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("templates"),
    )
    .map_err(|_| AppError::configuration("game templates directory is unavailable"))?;
    let template_path = fs::canonicalize(templates.join(template_name))
        .map_err(|_| AppError::configuration("selected game template is unavailable"))?;
    if !template_path.starts_with(&templates) {
        return Err(AppError::configuration(
            "game template must stay inside its templates directory",
        ));
    }
    let template = load_image(&template_path)?;
    let matcher = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: manifest.threshold,
            max_work: manifest.max_work,
        },
    )?;
    let input_frames = manifest.frames.len();
    let mut scheduled = Vec::new();
    let mut last_input = None;
    let mut last_due = None;
    for frame in std::mem::take(&mut manifest.frames) {
        if last_input.is_some_and(|last| frame.timestamp_ms <= last) {
            return Err(AppError::configuration(
                "template frame timestamps must strictly increase",
            ));
        }
        last_input = Some(frame.timestamp_ms);
        if last_due.is_none_or(|last| frame.timestamp_ms - last >= manifest.interval_ms) {
            last_due = Some(frame.timestamp_ms);
            scheduled.push(frame);
        }
    }
    let max_frames = manifest.max_frames.unwrap_or(scheduled.len() as u64);
    let max_actions = manifest.max_actions.unwrap_or((scheduled.len() as u64) * 2);
    if !(1..=MAX_FRAMES).contains(&max_frames) || !(1..=MAX_ACTIONS).contains(&max_actions) {
        return Err(AppError::configuration(
            "template replay budgets require max_frames 1..=256 and max_actions 1..=4096",
        ));
    }
    let mut frames = Vec::with_capacity(scheduled.len());
    let mut geometries = Vec::with_capacity(scheduled.len());
    let mut timestamps_ms = Vec::with_capacity(scheduled.len());
    let mut frame_bytes = 0usize;
    for input in &scheduled {
        let frame = load_image(&resolve(base, &input.image))?;
        frame_bytes = frame_bytes
            .checked_add(frame.rgba.len())
            .ok_or_else(|| AppError::configuration("template replay frame budget overflow"))?;
        if frame_bytes > MAX_FRAME_BYTES {
            return Err(AppError::configuration(
                "decoded template frames exceed 128 MiB",
            ));
        }
        let geometry = FrameGeometry::new(
            input.origin_x,
            input.origin_y,
            input.scale_x,
            input.scale_y,
            manifest.desktop,
        )?;
        frames.push(frame);
        geometries.push(geometry);
        timestamps_ms.push(input.timestamp_ms);
    }
    let options = ReflexOptions {
        mode,
        interval_ms: manifest.interval_ms,
        max_frames,
        max_actions,
        search_region: manifest.search_region,
    };
    let controller = ReflexController::new(manifest.target.clone(), matcher, options)?;
    let action_capacity = usize::try_from(max_frames + max_actions)
        .map_err(|_| AppError::configuration("template replay action budget overflow"))?;
    Ok(PreparedTemplate {
        controller,
        frames,
        geometries,
        timestamps_ms,
        profile: profile.display_name,
        target: manifest.target,
        mode: manifest.mode,
        input_frames,
        report_path,
        action_capacity,
        strategy: manifest.strategy,
    })
}

pub async fn replay(path: &Path) -> Result<(), AppError> {
    let PreparedTemplate {
        mut controller,
        frames,
        geometries,
        timestamps_ms,
        profile,
        target,
        mode,
        input_frames,
        report_path,
        action_capacity,
        strategy: _,
    } = prepare(path, true)?;
    let scheduled_frames = timestamps_ms.len();
    let safety = Arc::new(SafetyGate::new(SafetyPolicy {
        automation_enabled: true,
        allowed_capabilities: BTreeSet::from([Capability::ScreenRead, Capability::MouseInput]),
        allowed_targets: vec![target.clone()],
    }));
    let records = Arc::new(Mutex::new(Vec::new()));
    let audit = ReplayAudit {
        records: Arc::clone(&records),
        capacity: action_capacity * 3,
    };
    let port = DryRunAutomationPort::with_frames(action_capacity, frames)?;
    let mut coordinator = AutomationCoordinator::new(safety, port, audit);
    let mut steps = Vec::new();
    let mut failure = None;
    for (timestamp_ms, geometry) in timestamps_ms.iter().zip(&geometries) {
        match run_reflex_step(&mut controller, &mut coordinator, geometry, *timestamp_ms).await {
            Ok(step) => steps.push(step),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
        if matches!(
            controller.state(),
            ReflexState::Completed | ReflexState::BudgetExhausted | ReflexState::Failed
        ) {
            break;
        }
    }
    let outcome = if failure
        .as_ref()
        .is_some_and(|error| error.report.search_incomplete.is_some())
    {
        "search_incomplete"
    } else if failure.is_some() {
        "failed"
    } else {
        match controller.state() {
            ReflexState::Completed => "completed",
            ReflexState::BudgetExhausted => "budget_exhausted",
            _ => "input_exhausted",
        }
    };
    let report = Report {
        schema_version: 1,
        execution_mode: "dry_run",
        profile,
        target,
        mode,
        input_frames,
        scheduled_frames,
        skipped_frames: input_frames - scheduled_frames,
        outcome: outcome.into(),
        steps,
        failure,
        audit: records
            .lock()
            .map_err(|_| AppError::unavailable("template replay audit lock failed"))?
            .clone(),
    };
    let bytes = serde_json::to_vec_pretty(&report)
        .map_err(|_| AppError::protocol("cannot encode template replay report"))?;
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(AppError::unavailable(
            "template replay report exceeds 8 MiB",
        ));
    }
    println!("{}", std::str::from_utf8(&bytes).expect("JSON is UTF-8"));
    if let Some(path) = report_path {
        write_new_report(&path, &bytes)?;
    }
    if report.failure.is_some() {
        return Err(AppError::unavailable(
            "template dry-run failed; see replay report",
        ));
    }
    Ok(())
}

fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}
fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| AppError::configuration("template replay input is unavailable"))?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::configuration("cannot read template replay input"))?;
    if bytes.len() > limit {
        return Err(AppError::configuration(
            "template replay input exceeds its size limit",
        ));
    }
    Ok(bytes)
}
fn load_image(path: &Path) -> Result<ScreenFrame, AppError> {
    let bytes = read_limited(path, MAX_IMAGE_BYTES)?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes.as_slice()))
        .with_guessed_format()
        .map_err(|_| AppError::configuration("cannot identify template replay image"))?;
    let format = reader
        .format()
        .filter(|format| matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg))
        .ok_or_else(|| AppError::configuration("template replay images must be PNG or JPEG"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(MAX_PIXELS * 4);
    reader.limits(limits.clone());
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| AppError::configuration("invalid template replay image dimensions"))?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(AppError::configuration(
            "template replay image exceeds 16777216 pixels",
        ));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| AppError::configuration("cannot decode bounded template replay image"))?
        .into_rgba8();
    ScreenFrame::new(width, height, image.into_raw())
}
fn write_new_report(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| AppError::unavailable("cannot create new template replay report"))?;
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(AppError::unavailable(
            "cannot persist template replay report",
        ));
    }
    Ok(())
}
