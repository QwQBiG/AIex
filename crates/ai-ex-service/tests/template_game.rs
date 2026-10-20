use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use image::{GenericImage, ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};

const OFFLINE_CONFIG: &str = r#"
[model]
backend = "deepseek"

[memory]
enabled = true
path = "must-not-create-memory.jsonl"

[memory.embedding]
base_url = "http://127.0.0.1:1"
model = "offline-test"
space = "offline-test-v1"
timeout_seconds = 1

[control]
enabled = true
token_path = "must-not-read-token"

[safety]
automation_enabled = true
allowed_capabilities = ["screen_read", "mouse_input"]
allowed_targets = ["primary_target"]
audit_path = "must-not-create-audit.jsonl"
"#;

static CASE_NUMBER: AtomicU64 = AtomicU64::new(0);

struct Case {
    directory: PathBuf,
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}

impl Case {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "aiex-template-cli-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            CASE_NUMBER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&directory).unwrap();
        let mut case = Self {
            directories: vec![directory.clone()],
            directory,
            files: Vec::new(),
        };
        case.write("config.toml", OFFLINE_CONFIG.as_bytes());
        for name in [
            "must-not-create-memory.jsonl",
            "must-not-create-audit.jsonl",
            "must-not-read-token",
        ] {
            case.expect_file(name);
        }
        case
    }

    fn path(&self, name: &str) -> PathBuf {
        let mut components = Path::new(name).components();
        assert!(matches!(components.next(), Some(Component::Normal(_))));
        assert!(components.next().is_none());
        self.directory.join(name)
    }

    fn expect_file(&mut self, name: &str) -> PathBuf {
        let path = self.path(name);
        if !self.files.contains(&path) {
            self.files.push(path.clone());
        }
        path
    }

    fn write(&mut self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.expect_file(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        path
    }

    fn png(&mut self, name: &str, canvas: &RgbaImage) {
        let path = self.expect_file(name);
        assert!(!path.exists());
        canvas.save_with_format(path, ImageFormat::Png).unwrap();
    }

    fn manifest(&self, frames: Vec<Value>) -> Value {
        json!({
            "schema_version": 1,
            "profile": legacy_profile(),
            "desktop": {"left": 0, "top": 0, "width": 1920, "height": 1080},
            "frames": frames,
        })
    }

    fn run(&mut self, name: &str, manifest: &Value) -> Output {
        self.write(name, &serde_json::to_vec_pretty(manifest).unwrap());
        self.run_path(name, &[])
    }

    fn run_path(&self, manifest: &str, extra: &[&str]) -> Output {
        let mut arguments = vec!["--config", "config.toml", "--template-game", manifest];
        arguments.extend_from_slice(extra);
        self.run_arguments(&arguments)
    }

    fn run_arguments(&self, arguments: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-ex-service"));
        command
            .current_dir(&self.directory)
            .args(arguments)
            .env_remove("DEEPSEEK_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = bounded_output(&mut command);
        self.assert_no_services();
        output
    }

    fn assert_no_services(&self) {
        for name in [
            "must-not-create-memory.jsonl",
            "must-not-create-audit.jsonl",
            "must-not-read-token",
        ] {
            assert!(!self.path(name).exists(), "offline mode accessed {name}");
        }
        for entry in fs::read_dir(&self.directory).unwrap() {
            let path = entry.unwrap().path();
            assert!(
                self.files.contains(&path) || self.directories.contains(&path),
                "offline mode created an unexpected entry: {}",
                path.display()
            );
        }
    }

    fn assert_input_error(&self, output: &Output, report: &str) {
        assert!(!output.status.success());
        assert!(
            output.stdout.iter().all(u8::is_ascii_whitespace),
            "input errors must not emit an execution report: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(!output.stderr.is_empty());
        assert!(!self.path(report).exists());
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        // Remove only exact, owned test paths, including known forbidden outputs
        // should a regression have created one. Never recursively remove a tree.
        for path in self.files.iter().rev() {
            let _ = fs::remove_file(path);
        }
        for path in self.directories.iter().rev() {
            let _ = fs::remove_dir(path);
        }
    }
}

fn legacy_profile() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("assets/games/cookie-clicker/profile.json")
}

fn canvas_with_cookie(x: u32, y: u32) -> RgbaImage {
    let template_path = legacy_profile()
        .parent()
        .unwrap()
        .join("templates/big-cookie.png");
    let template = image::open(template_path).unwrap().into_rgba8();
    assert_eq!(template.dimensions(), (100, 100));
    let mut canvas = RgbaImage::from_pixel(140, 150, Rgba([0, 0, 0, 255]));
    // Preserve all RGBA bytes; alpha blending would alter the old template input.
    canvas.copy_from(&template, x, y).unwrap();
    canvas
}

fn frame(image: &str, timestamp_ms: u64) -> Value {
    json!({"image": image, "timestamp_ms": timestamp_ms})
}

fn bounded_output(command: &mut Command) -> Output {
    let mut child = command.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    // Drain both pipes during try_wait, including on Windows with small buffers.
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(30) {
            timed_out = true;
            let _ = child.kill();
            break child.wait().unwrap();
        }
        thread::sleep(Duration::from_millis(10));
    };
    let output = Output {
        status,
        stdout: stdout_reader.join().unwrap(),
        stderr: stderr_reader.join().unwrap(),
    };
    assert!(
        !timed_out,
        "offline template CLI exceeded 30s; stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON report: {error}; status={}; stdout={}; stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn successful_report(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "status={}; stderr={}; stdout={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let report = report(output);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["execution_mode"], "dry_run");
    assert_eq!(report["profile"], "Cookie Clicker");
    assert_eq!(report["target"], "primary_target");
    assert!(report["failure"].is_null());
    report
}

fn receipt_types(step: &Value) -> Vec<&str> {
    step["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|receipt| {
            assert_eq!(receipt["completion_audit_persisted"], true);
            receipt["action"]["type"].as_str().unwrap()
        })
        .collect()
}

fn assert_point(actual: &Value, expected: (f64, f64)) {
    let coordinates = actual.as_array().unwrap();
    assert_eq!(coordinates.len(), 2);
    assert!((coordinates[0].as_f64().unwrap() - expected.0).abs() < 1e-12);
    assert!((coordinates[1].as_f64().unwrap() - expected.1).abs() < 1e-12);
}

#[test]
fn legacy_profile_runs_one_complete_click_and_writes_the_same_new_report() {
    let mut case = Case::new();
    case.png("frame.png", &canvas_with_cookie(20, 30));
    let report_path = case.expect_file("report.json");
    let mut first = frame("frame.png", 0);
    first["origin_x"] = json!(100);
    first["origin_y"] = json!(50);
    first["scale_x"] = json!(2.0);
    first["scale_y"] = json!(2.0);
    let mut manifest = case.manifest(vec![first, frame("frame.png", 60)]);
    manifest["report"] = json!("report.json");
    let output = case.run("manifest.json", &manifest);
    let report = successful_report(&output);
    assert_eq!(report["outcome"], "completed");
    assert_eq!(report["mode"], "click_once");
    assert_eq!(report["input_frames"], 2);
    assert_eq!(report["scheduled_frames"], 2);
    assert_eq!(report["skipped_frames"], 0);
    let steps = report["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1, "later supplied frames must not execute");
    let step = &steps[0];
    assert_eq!(step["state"], "completed");
    assert_eq!(step["outcome"], "completed");
    assert_eq!(
        step["matched_bounds"],
        json!({"x":20,"y":30,"width":100,"height":100})
    );
    assert_eq!(step["score"], 1.0);
    assert_eq!(step["frames"], 1);
    assert_eq!(step["actions"], 2);
    assert_eq!(step["misses"], 0);
    assert_point(&step["absolute"], (240.0, 210.0));
    assert_point(&step["normalized"], (240.0 / 1919.0, 210.0 / 1079.0));
    assert_eq!(
        receipt_types(step),
        vec!["capture_screen", "move_pointer", "click"]
    );
    assert_eq!(step["receipts"][2]["action"]["button"], "left");
    let audit = report["audit"].as_array().unwrap();
    assert_eq!(audit.len(), 9);
    for (receipt, records) in step["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .zip(audit.chunks(3))
    {
        for (record, stage) in records.iter().zip(["requested", "authorized", "completed"]) {
            assert_eq!(record["stage"], stage);
            assert_eq!(record["action_id"], receipt["action_id"]);
            assert_eq!(record["target"], "primary_target");
        }
    }
    assert_eq!(audit[0]["capability"], "screen_read");
    assert_eq!(audit[3]["capability"], "mouse_input");
    let saved: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(saved, report);

    // The manifest is self-contained: neither the default nor an explicit
    // configuration path may be opened for this dedicated dry-run mode.
    fs::remove_file(case.path("config.toml")).unwrap();
    manifest.as_object_mut().unwrap().remove("report");
    case.write(
        "self-contained.json",
        &serde_json::to_vec_pretty(&manifest).unwrap(),
    );
    for arguments in [
        vec!["--template-game", "self-contained.json"],
        vec![
            "--config",
            "missing-explicit.toml",
            "--template-game",
            "self-contained.json",
        ],
    ] {
        let output = case.run_arguments(&arguments);
        let standalone = successful_report(&output);
        assert_eq!(standalone["outcome"], "completed");
        assert_eq!(standalone["audit"].as_array().unwrap().len(), 9);
        assert!(!case.path("missing-explicit.toml").exists());
        assert!(!case.path("config.toml").exists());
    }
}

#[test]
fn hover_and_repeat_keep_due_frames_and_geometry_aligned_after_cadence_filtering() {
    for mode in ["hover", "click_repeat"] {
        let mut case = Case::new();
        case.png("first.png", &canvas_with_cookie(20, 30));
        case.png("second.png", &canvas_with_cookie(40, 10));
        let skipped = json!({
            "image":"must-not-decode.png", "timestamp_ms":10,
            "origin_x":100_000, "scale_x":0.0,
        });
        let mut second = frame("second.png", 60);
        second["origin_x"] = json!(200);
        second["origin_y"] = json!(100);
        second["scale_x"] = json!(1.5);
        second["scale_y"] = json!(2.0);
        let mut manifest = case.manifest(vec![frame("first.png", 0), skipped, second]);
        manifest["mode"] = json!(mode);
        let output = case.run("manifest.json", &manifest);
        let report = successful_report(&output);
        assert_eq!(report["mode"], mode);
        assert_eq!(report["input_frames"], 3);
        assert_eq!(report["scheduled_frames"], 2);
        assert_eq!(report["skipped_frames"], 1);
        assert_eq!(report["outcome"], "budget_exhausted");
        let steps = report["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["timestamp_ms"], 0);
        assert_eq!(steps[1]["timestamp_ms"], 60);
        assert_eq!(
            steps[1]["matched_bounds"],
            json!({"x":40,"y":10,"width":100,"height":100})
        );
        assert_point(&steps[0]["absolute"], (70.0, 80.0));
        assert_point(&steps[1]["absolute"], (335.0, 220.0));
        assert_point(&steps[1]["normalized"], (335.0 / 1919.0, 220.0 / 1079.0));
        assert_eq!(steps[1]["frames"], 2);
        let expected = if mode == "hover" {
            vec!["capture_screen", "move_pointer"]
        } else {
            vec!["capture_screen", "move_pointer", "click"]
        };
        for step in steps {
            assert_eq!(receipt_types(step), expected);
        }
        assert_eq!(
            report["audit"].as_array().unwrap().len(),
            expected.len() * 6
        );
        assert_eq!(steps[1]["actions"], if mode == "hover" { 2 } else { 4 });
        assert!(!case.path("must-not-decode.png").exists());
    }
}

#[test]
fn a_black_frame_only_captures_and_never_moves_or_clicks() {
    let mut case = Case::new();
    case.png(
        "black.png",
        &RgbaImage::from_pixel(140, 150, Rgba([0, 0, 0, 255])),
    );
    let manifest = case.manifest(vec![frame("black.png", 0)]);
    let output = case.run("manifest.json", &manifest);
    let report = successful_report(&output);
    assert_eq!(report["outcome"], "budget_exhausted");
    let steps = report["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1);
    let step = &steps[0];
    assert!(step["matched_bounds"].is_null());
    assert!(step["absolute"].is_null());
    assert!(step["normalized"].is_null());
    assert_eq!(step["score"], 0.0);
    assert_eq!(step["misses"], 1);
    assert_eq!(step["frames"], 1);
    assert_eq!(step["actions"], 0);
    assert_eq!(receipt_types(step), vec!["capture_screen"]);
    assert_eq!(report["audit"].as_array().unwrap().len(), 3);
}

#[test]
fn exact_target_keys_and_new_report_paths_protect_existing_files() {
    let mut case = Case::new();
    case.png("frame.png", &canvas_with_cookie(20, 30));
    case.expect_file("rejected-report.json");
    let mut unknown = case.manifest(vec![frame("frame.png", 0)]);
    unknown["target"] = json!("big-cookie");
    unknown["report"] = json!("rejected-report.json");
    let output = case.run("unknown.json", &unknown);
    case.assert_input_error(&output, "rejected-report.json");

    case.write("sentinel.json", b"existing report sentinel");
    let mut existing = case.manifest(vec![frame("frame.png", 0)]);
    existing["report"] = json!("sentinel.json");
    let output = case.run("existing.json", &existing);
    assert!(!output.status.success());
    assert!(output.stdout.iter().all(u8::is_ascii_whitespace));
    assert!(String::from_utf8_lossy(&output.stderr).contains("overwrite"));
    assert_eq!(
        fs::read(case.path("sentinel.json")).unwrap().as_slice(),
        b"existing report sentinel"
    );

    let missing_parent = case.directory.join("uncreated-report-parent");
    assert!(missing_parent.starts_with(&case.directory));
    assert!(!missing_parent.exists());
    case.files.push(missing_parent.join("report.json"));
    case.directories.push(missing_parent.clone());
    let mut nested = case.manifest(vec![frame("frame.png", 0)]);
    nested["report"] = json!("uncreated-report-parent/report.json");
    let output = case.run("nested.json", &nested);
    assert!(!output.status.success());
    let executed = report(&output);
    assert_eq!(executed["execution_mode"], "dry_run");
    assert_eq!(executed["outcome"], "completed");
    assert_eq!(executed["steps"].as_array().unwrap().len(), 1);
    assert_eq!(
        receipt_types(&executed["steps"][0]),
        vec!["capture_screen", "move_pointer", "click"]
    );
    assert_eq!(executed["audit"].as_array().unwrap().len(), 9);
    assert!(
        !missing_parent.exists(),
        "report writing must not create parent directories"
    );
}

#[test]
fn invalid_manifests_and_input_budgets_fail_before_any_report_or_services() {
    let mut cases: Vec<(&str, Value)> = Vec::new();
    let prototype = Case::new().manifest(vec![frame("frame.png", 0)]);
    for (field, value) in [
        ("schema_version", json!(2)),
        ("mode", json!("unsupported")),
        ("interval_ms", json!(0)),
        ("interval_ms", json!(60_001)),
        ("max_frames", json!(0)),
        ("max_frames", json!(257)),
        ("max_actions", json!(0)),
        ("max_actions", json!(4097)),
        ("max_work", json!(0)),
        ("threshold", json!(1.1)),
        ("unexpected", json!(true)),
    ] {
        let mut manifest = prototype.clone();
        manifest[field] = value;
        cases.push((field, manifest));
    }
    let mut empty = prototype.clone();
    empty["frames"] = json!([]);
    cases.push(("empty frames", empty));
    let mut too_many = prototype.clone();
    too_many["frames"] = Value::Array((0..257).map(|i| frame("frame.png", i * 60)).collect());
    cases.push(("too many frames", too_many));
    let mut repeated_time = prototype.clone();
    repeated_time["frames"] = json!([frame("frame.png", 0), frame("frame.png", 0)]);
    cases.push(("repeated timestamp", repeated_time));
    let mut descending_time = prototype.clone();
    descending_time["frames"] = json!([frame("frame.png", 60), frame("frame.png", 0)]);
    cases.push(("descending timestamp", descending_time));
    let mut unknown_frame = prototype.clone();
    unknown_frame["frames"][0]["unexpected"] = json!(true);
    cases.push(("unknown frame field", unknown_frame));

    for (label, mut manifest) in cases {
        let mut case = Case::new();
        case.png("frame.png", &canvas_with_cookie(20, 30));
        case.expect_file("rejected-report.json");
        manifest["report"] = json!("rejected-report.json");
        let output = case.run("manifest.json", &manifest);
        assert!(!output.status.success(), "{label} was accepted");
        case.assert_input_error(&output, "rejected-report.json");
    }

    let mut malformed = Case::new();
    malformed.write("manifest.json", b"{not valid JSON");
    let output = malformed.run_path("manifest.json", &[]);
    malformed.assert_input_error(&output, "rejected-report.json");
}

#[test]
fn invalid_images_are_input_errors_and_execution_failures_retain_capture_evidence() {
    let mut broken = Case::new();
    broken.write("broken.png", b"not an image");
    broken.expect_file("rejected-report.json");
    let mut manifest = broken.manifest(vec![frame("broken.png", 0)]);
    manifest["report"] = json!("rejected-report.json");
    let output = broken.run("manifest.json", &manifest);
    broken.assert_input_error(&output, "rejected-report.json");

    for invalid_scale in [0.0, -1.0] {
        let mut case = Case::new();
        case.png("frame.png", &canvas_with_cookie(20, 30));
        case.expect_file("rejected-report.json");
        let mut invalid = frame("frame.png", 0);
        invalid["scale_x"] = json!(invalid_scale);
        let mut manifest = case.manifest(vec![invalid]);
        manifest["report"] = json!("rejected-report.json");
        let output = case.run("manifest.json", &manifest);
        case.assert_input_error(&output, "rejected-report.json");
    }

    for stage in ["geometry", "match"] {
        let mut case = Case::new();
        case.png("frame.png", &canvas_with_cookie(20, 30));
        let report_path = case.expect_file("failure.json");
        let mut input = frame("frame.png", 0);
        if stage == "geometry" {
            input["origin_x"] = json!(2000);
        }
        let mut manifest = case.manifest(vec![input]);
        manifest["report"] = json!("failure.json");
        if stage == "match" {
            manifest["max_work"] = json!(1);
        }
        let output = case.run("manifest.json", &manifest);
        assert!(!output.status.success());
        let report = report(&output);
        assert_eq!(report["execution_mode"], "dry_run");
        assert_eq!(report["outcome"], "failed");
        assert_eq!(report["failure"]["stage"], stage);
        assert!(report["steps"].as_array().unwrap().is_empty());
        let partial = &report["failure"]["report"];
        assert_eq!(partial["state"], "failed");
        assert_eq!(partial["frames"], 1);
        assert_eq!(partial["actions"], 0);
        assert_eq!(receipt_types(partial), vec!["capture_screen"]);
        assert_eq!(report["audit"].as_array().unwrap().len(), 3);
        let saved: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
        assert_eq!(saved, report);
    }
}

#[test]
fn pointer_and_frame_budgets_end_execution_without_extra_actions() {
    for (max_frames, max_actions, expected_receipts, expected_actions) in [
        (2, 1, vec!["capture_screen", "move_pointer"], 1),
        (1, 4, vec!["capture_screen", "move_pointer", "click"], 2),
    ] {
        let mut case = Case::new();
        case.png("frame.png", &canvas_with_cookie(20, 30));
        let mut manifest = case.manifest(vec![frame("frame.png", 0), frame("frame.png", 60)]);
        manifest["mode"] = json!("click_repeat");
        manifest["max_frames"] = json!(max_frames);
        manifest["max_actions"] = json!(max_actions);
        let output = case.run("manifest.json", &manifest);
        let report = successful_report(&output);
        assert_eq!(report["outcome"], "budget_exhausted");
        let steps = report["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0]["frames"], 1);
        assert_eq!(steps[0]["actions"], expected_actions);
        assert_eq!(receipt_types(&steps[0]), expected_receipts);
        assert_eq!(
            report["audit"].as_array().unwrap().len(),
            expected_receipts.len() * 3
        );
    }
}

#[test]
fn jpeg_frames_decode_offline_and_template_mode_is_mutually_exclusive() {
    let mut case = Case::new();
    let jpeg_path = case.expect_file("frame.jpg");
    assert!(!jpeg_path.exists());
    image::DynamicImage::ImageRgba8(canvas_with_cookie(20, 30))
        .to_rgb8()
        .save_with_format(jpeg_path, ImageFormat::Jpeg)
        .unwrap();
    let manifest = case.manifest(vec![frame("frame.jpg", 0)]);
    let output = case.run("manifest.json", &manifest);
    let report = successful_report(&output);
    assert_eq!(report["outcome"], "completed");
    let step = &report["steps"][0];
    assert!(step["matched_bounds"].is_object());
    assert!(step["score"].as_f64().unwrap() >= 0.7);
    assert_eq!(
        receipt_types(step),
        vec!["capture_screen", "move_pointer", "click"]
    );

    let conflicting = case.run_path("manifest.json", &["--check"]);
    assert!(!conflicting.status.success());
    assert!(String::from_utf8_lossy(&conflicting.stderr).contains("mutually exclusive"));
}
