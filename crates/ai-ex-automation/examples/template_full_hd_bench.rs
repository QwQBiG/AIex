//! Reproducible full-HD synthetic searches; no capture, input, models or files are used.
//!
//! Baseline: cargo run -p ai-ex-automation --release --example template_full_hd_bench -- --samples 1
//! Median:   cargo run -p ai-ex-automation --release --example template_full_hd_bench -- --samples 3
//! Optional smaller template: append --template-size 20 (the default is 100).
//! Setup, template construction and the default-budget probe are outside timed searches.

use std::error::Error;
use std::hint::black_box;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use ai_ex_automation::{MatchOptions, PixelRect, ScreenFrame, TemplateMatch, TemplateMatcher};
use ai_ex_domain::{AppError, ErrorKind};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const SEED: u32 = 0x41_49_45_58;
const THRESHOLD: f64 = 0.999;
const SCORE_TOLERANCE: f64 = 1.0e-10;

struct Settings {
    samples: usize,
    template_size: u32,
}

impl Settings {
    fn parse() -> Result<Option<Self>, AppError> {
        let mut settings = Self {
            samples: 3,
            template_size: 100,
        };
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--samples" => {
                    settings.samples = arguments
                        .next()
                        .and_then(|value| value.parse().ok())
                        .filter(|value| matches!(value, 1 | 3))
                        .ok_or_else(|| AppError::configuration("--samples requires 1 or 3"))?;
                }
                "--template-size" => {
                    settings.template_size = arguments
                        .next()
                        .and_then(|value| value.parse().ok())
                        .filter(|value| matches!(value, 20 | 100))
                        .ok_or_else(|| {
                            AppError::configuration("--template-size requires 20 or 100")
                        })?;
                }
                "--help" | "-h" => {
                    println!("template_full_hd_bench [--samples 1|3] [--template-size 20|100]");
                    return Ok(None);
                }
                _ => {
                    return Err(AppError::configuration(format!(
                        "unknown benchmark argument: {argument}"
                    )));
                }
            }
        }
        Ok(Some(settings))
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(settings) = Settings::parse()? else {
        return Ok(());
    };
    println!(
        "Full-HD synthetic matcher: frame={WIDTH}x{HEIGHT}, template={}x{}, seed=0x{SEED:08x}, threshold={THRESHOLD}, debug_assertions={}",
        settings.template_size,
        settings.template_size,
        cfg!(debug_assertions),
    );
    println!(
        "Each full-frame/ROI case: 1 untimed warmup + {} timed searches; median reported. Runs are sequential.",
        settings.samples
    );
    println!(
        "Near hits occupy the last legal candidate; only their final pixel is perturbed by +3 RGB."
    );
    println!("The independent template is textured; smooth_repeat describes the frame background.");
    println!(
        "logical_worst_pixel_pairs = candidate_positions * template_pixels; it is NOT measured executed work."
    );
    println!(
        "default_budget probes use the default max_work with the benchmark threshold; no budget-exhaustion outcome is assumed."
    );
    if cfg!(debug_assertions) {
        println!("This is a debug build; use --release for the intended full-HD comparison.");
    }

    for background in [Background::Random, Background::SmoothRepeat] {
        for near_hit in [true, false] {
            let fixture = Fixture::new(background, near_hit, settings.template_size)?;
            println!(
                "\nCASE {}: expected_bounds={:?}, expected_score={}, fixture_fingerprint={:016x}",
                fixture.label,
                fixture.expected,
                fixture.expected_score.map_or_else(
                    || format!("below {THRESHOLD} (target absent)"),
                    |score| format!("{score:.12} (strictly below 1)"),
                ),
                fingerprint(&fixture.frame.rgba),
            );
            io::stdout().flush()?;
            let full = measure(&fixture, None, "full", settings.samples)?;
            let roi = measure(&fixture, Some(fixture.roi), "exact_roi", settings.samples)?;
            assert_eq!(
                full.found.bounds, roi.found.bounds,
                "full/ROI bounds parity"
            );
            if near_hit {
                assert!(
                    (full.found.score - roi.found.score).abs() <= SCORE_TOLERANCE,
                    "full/ROI hit score parity: full={:?}, roi={:?}",
                    full.found,
                    roi.found,
                );
                println!(
                    "PARITY {}: full/ROI hit bounds and scores agree.",
                    fixture.label
                );
            } else {
                println!(
                    "PARITY {}: full/ROI both miss; scores may differ because the full search ranks additional absent-target candidates.",
                    fixture.label,
                );
            }
            println!(
                "SUMMARY {}: full_median_ms={:.3}, exact_roi_median_ms={:.3}, default_budget_exhausted(full/roi)={}/{}",
                fixture.label,
                full.median.as_secs_f64() * 1000.0,
                roi.median.as_secs_f64() * 1000.0,
                full.default_exhausted,
                roi.default_exhausted,
            );
        }
    }
    println!("\nAll repeated results and hit ROI/full-frame parity checks passed.");
    println!(
        "Timings measure matcher calls including their preprocessing, excluding fixture/template setup and checks."
    );
    println!(
        "Synthetic search latency does not establish screen capture, live automation, or control-loop FPS."
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum Background {
    Random,
    SmoothRepeat,
}

impl Background {
    fn label(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::SmoothRepeat => "smooth_repeat",
        }
    }
}

struct Fixture {
    label: String,
    frame: ScreenFrame,
    template: ScreenFrame,
    roi: PixelRect,
    expected: Option<PixelRect>,
    expected_score: Option<f64>,
}

impl Fixture {
    fn new(background: Background, near_hit: bool, size: u32) -> Result<Self, AppError> {
        let mut frame_random = SEED ^ 0x66_72_61_6d;
        let mut rgba = Vec::with_capacity(WIDTH as usize * HEIGHT as usize * 4);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let random = next_random(&mut frame_random);
                let gray = match background {
                    // The full-range texture has no intentionally constant areas.
                    Background::Random => random as u8,
                    // Repeated low-frequency ramps plus <=3 gray levels of seeded grain.
                    // Even a 20x20 window spans slope/grain, avoiding the constant shortcut.
                    Background::SmoothRepeat => {
                        let horizontal = (x % 96).min(95 - x % 96) / 2;
                        let vertical = (y % 80).min(79 - y % 80) / 2;
                        (80 + horizontal + vertical + (random & 3)) as u8
                    }
                };
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
        }
        let mut template_random = SEED ^ 0x74_65_6d_70 ^ size;
        let mut patch = Vec::with_capacity(size as usize * size as usize * 4);
        for _ in 0..size * size {
            // Leave room for the +3 perturbation without clipping, and use an
            // independent seed so absent targets are not copied background windows.
            let gray = (32 + next_random(&mut template_random) % 192) as u8;
            patch.extend_from_slice(&[gray, gray, gray, 255]);
        }
        let roi = PixelRect {
            x: WIDTH - size,
            y: HEIGHT - size,
            width: size,
            height: size,
        };
        if near_hit {
            for row in 0..size as usize {
                let destination = ((roi.y as usize + row) * WIDTH as usize + roi.x as usize) * 4;
                let source = row * size as usize * 4;
                rgba[destination..destination + size as usize * 4]
                    .copy_from_slice(&patch[source..source + size as usize * 4]);
            }
            // Change only the last pixel of the copied target, in all RGB channels.
            // It is no longer an exact affine copy: perfect-match early return is invalid.
            let last = ((HEIGHT - 1) as usize * WIDTH as usize + (WIDTH - 1) as usize) * 4;
            for channel in 0..3 {
                rgba[last + channel] += 3;
            }
        }
        let frame = ScreenFrame::new(WIDTH, HEIGHT, rgba)?;
        let template = ScreenFrame::new(size, size, patch)?;
        let expected_score = near_hit.then(|| reference_score(&frame, &template, roi));
        if let Some(score) = expected_score {
            assert!(
                (THRESHOLD..1.0).contains(&score),
                "near hit must not be perfect: {score}"
            );
        }
        Ok(Self {
            label: format!(
                "{}_{}",
                background.label(),
                if near_hit { "near_hit" } else { "miss" }
            ),
            frame,
            template,
            roi,
            expected: near_hit.then_some(roi),
            expected_score,
        })
    }

    fn verify(&self, result: TemplateMatch) {
        assert_eq!(
            result.bounds, self.expected,
            "{} expected target bounds",
            self.label
        );
        assert!(
            result.score.is_finite(),
            "{} score must be finite",
            self.label
        );
        if let Some(expected) = self.expected_score {
            assert!(
                result.score < 1.0,
                "perturbed target must not trigger perfect-match shortcut"
            );
            assert!(
                (result.score - expected).abs() <= SCORE_TOLERANCE,
                "{} reference score {expected:.12}, actual {:.12}",
                self.label,
                result.score,
            );
        } else {
            assert!(
                result.score < THRESHOLD,
                "{} absent target must be below threshold",
                self.label
            );
        }
    }
}

fn next_random(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

// Setup-only reference for the planted window. Equal RGB makes grayscale exact.
fn reference_score(frame: &ScreenFrame, template: &ScreenFrame, roi: PixelRect) -> f64 {
    let (mut a, mut b, mut aa, mut bb, mut ab) = (0_u64, 0_u64, 0_u64, 0_u64, 0_u64);
    for y in 0..roi.height as usize {
        for x in 0..roi.width as usize {
            let left = u64::from(template.rgba[(y * roi.width as usize + x) * 4]);
            let right = u64::from(
                frame.rgba[((y + roi.y as usize) * WIDTH as usize + x + roi.x as usize) * 4],
            );
            a += left;
            b += right;
            aa += left * left;
            bb += right * right;
            ab += left * right;
        }
    }
    let count = u64::from(roi.width) * u64::from(roi.height);
    let numerator = i128::from(count * ab) - i128::from(a * b);
    let variance = u128::from(count * aa - a * a) * u128::from(count * bb - b * b);
    numerator as f64 / (variance as f64).sqrt()
}

fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

struct Measurement {
    found: TemplateMatch,
    median: Duration,
    default_exhausted: bool,
}

fn measure(
    fixture: &Fixture,
    region: Option<PixelRect>,
    scope: &str,
    samples: usize,
) -> Result<Measurement, Box<dyn Error>> {
    let search = region.unwrap_or(PixelRect {
        x: 0,
        y: 0,
        width: WIDTH,
        height: HEIGHT,
    });
    let positions = u64::from(search.width - fixture.template.width + 1)
        * u64::from(search.height - fixture.template.height + 1);
    let template_pixels = u64::from(fixture.template.width) * u64::from(fixture.template.height);
    let logical_worst = positions * template_pixels;
    let default_budget = MatchOptions::default().max_work;
    let matcher = TemplateMatcher::new(
        &fixture.template,
        MatchOptions {
            threshold: THRESHOLD,
            max_work: logical_worst,
        },
    )?;
    let default_matcher = TemplateMatcher::new(
        &fixture.template,
        MatchOptions {
            threshold: THRESHOLD,
            max_work: default_budget,
        },
    )?;
    println!(
        "BEGIN {} {scope}: region={search:?}, candidate_positions={positions}, logical_worst_pixel_pairs={logical_worst}, max_work={logical_worst}, default_max_work={default_budget}",
        fixture.label,
    );
    println!(
        "PROGRESS {} {scope}: default-budget probe (untimed)",
        fixture.label
    );
    io::stdout().flush()?;
    let default_result = match default_matcher.find_match(&fixture.frame, region) {
        Ok(found) => {
            fixture.verify(found);
            println!(
                "DEFAULT {} {scope}: exhausted=false, actual_bounds={:?}, score={:.12}",
                fixture.label, found.bounds, found.score
            );
            Some(found)
        }
        Err(error)
            if error.kind == ErrorKind::Unavailable
                && error.message.contains("work limit exhausted") =>
        {
            println!(
                "DEFAULT {} {scope}: exhausted=true, error={error}",
                fixture.label
            );
            None
        }
        Err(error) => return Err(error.into()),
    };
    println!("PROGRESS {} {scope}: warmup 1/1", fixture.label);
    io::stdout().flush()?;
    let warmup = matcher.find_match(black_box(&fixture.frame), black_box(region))?;
    fixture.verify(warmup);
    if let Some(found) = default_result {
        assert_eq!(
            found, warmup,
            "successful default-budget search must preserve its result"
        );
    }
    let mut elapsed = Vec::with_capacity(samples);
    for sample in 1..=samples {
        println!(
            "PROGRESS {} {scope}: sample {sample}/{samples}",
            fixture.label
        );
        io::stdout().flush()?;
        let started = Instant::now();
        let found = matcher.find_match(black_box(&fixture.frame), black_box(region))?;
        let duration = started.elapsed();
        fixture.verify(found);
        assert_eq!(
            found, warmup,
            "every repetition must preserve score and bounds"
        );
        black_box(found);
        elapsed.push(duration);
        println!(
            "SAMPLE {} {scope} {sample}/{samples}: ms={:.3}, expected_bounds={:?}, actual_bounds={:?}, score={:.12}",
            fixture.label,
            duration.as_secs_f64() * 1000.0,
            fixture.expected,
            found.bounds,
            found.score
        );
    }
    elapsed.sort_unstable();
    let median = elapsed[samples / 2];
    println!(
        "RESULT {} {scope}: median_ms={:.3}, expected_bounds={:?}, actual_bounds={:?}, score={:.12}, logical_worst_pixel_pairs={logical_worst}, default_budget_exhausted={}",
        fixture.label,
        median.as_secs_f64() * 1000.0,
        fixture.expected,
        warmup.bounds,
        warmup.score,
        default_result.is_none()
    );
    Ok(Measurement {
        found: warmup,
        median,
        default_exhausted: default_result.is_none(),
    })
}
