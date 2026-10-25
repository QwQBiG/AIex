//! Offline, resumable default-budget matcher measurements on the frozen full-HD fixtures.
//!
//! cargo run -p ai-ex-automation --release --example template_budget_bench -- --samples 1
//! Use --samples 3 (the default) for a median of three searches after one warmup.
//! No full-frame exhaustive baseline, capture, models, files or live input are used.

use std::error::Error;
use std::hint::black_box;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use ai_ex_automation::{
    MatchOptions, PixelRect, ScreenFrame, TemplateMatch, TemplateMatcher, TemplateSearchProgress,
    TemplateSearchProof, TemplateSearchStep,
};
use ai_ex_domain::AppError;

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const TEMPLATE_SIZE: u32 = 100;
const SAFE_ROI_SIZE: u32 = 200;
const SEED: u32 = 0x41_49_45_58;
const THRESHOLD: f64 = 0.999;
const SCORE_TOLERANCE: f64 = 1.0e-10;
const QUANTUM: u32 = 16_384;
const MAX_ADVANCES: u64 = 1_000_000;

fn samples() -> Result<Option<usize>, AppError> {
    let mut samples = 3;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--samples" => {
                samples = arguments
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|value| matches!(value, 1 | 3))
                    .ok_or_else(|| AppError::configuration("--samples requires 1 or 3"))?;
            }
            "--help" | "-h" => {
                println!("template_budget_bench [--samples 1|3]");
                return Ok(None);
            }
            _ => {
                return Err(AppError::configuration(format!(
                    "unknown benchmark argument: {argument}"
                )));
            }
        }
    }
    Ok(Some(samples))
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(samples) = samples()? else {
        return Ok(());
    };
    let max_work = MatchOptions::default().max_work;
    assert_eq!(
        max_work, 250_000_000,
        "this regression measures the default 250m budget"
    );
    println!(
        "Budgeted synthetic matcher: frame={WIDTH}x{HEIGHT}, template={TEMPLATE_SIZE}x{TEMPLATE_SIZE}, seed=0x{SEED:08x}, threshold={THRESHOLD}, max_work={max_work}, quantum={QUANTUM}, debug_assertions={}",
        cfg!(debug_assertions),
    );
    println!("Each case/scope: 1 warmup + {samples} measured searches; sequential execution only.");
    println!(
        "Fixtures are copied unchanged from template_full_hd_bench; fingerprints identify the same input bytes."
    );
    println!(
        "Full near hits must be certified; full misses may be explicitly Incomplete with no returned bounds or score."
    );
    println!(
        "The 200x200 ROI must Complete, match an independent scalar ROI oracle and never expand its region."
    );
    println!(
        "total_ms sums begin_search + advance calls, excluding setup, progress assertions, printing and terminal replay checks."
    );

    for background in [Background::Random, Background::SmoothRepeat] {
        for near_hit in [true, false] {
            let fixture = Fixture::new(background, near_hit, TEMPLATE_SIZE)?;
            let safe_region = PixelRect {
                x: WIDTH - SAFE_ROI_SIZE,
                y: HEIGHT - SAFE_ROI_SIZE,
                width: SAFE_ROI_SIZE,
                height: SAFE_ROI_SIZE,
            };
            println!(
                "\nCASE {}: fixture_fingerprint={:016x}, template_fingerprint={:016x}, expected_bounds={:?}, planted_window={:?}",
                fixture.label,
                fingerprint(&fixture.frame.rgba),
                fingerprint(&fixture.template.rgba),
                fixture.expected,
                fixture.roi,
            );
            println!(
                "SETUP {}: independent scalar oracle for 200x200 ROI only",
                fixture.label
            );
            io::stdout().flush()?;
            let oracle = scalar_roi_oracle(&fixture, safe_region);
            fixture.verify(oracle);
            println!(
                "ORACLE {}: region={safe_region:?}, bounds={:?}, best_score={:.12}",
                fixture.label, oracle.bounds, oracle.score,
            );
            let full = measure(&fixture, None, "full", samples, None)?;
            let roi = measure(
                &fixture,
                Some(safe_region),
                "safe_roi_200",
                samples,
                Some(oracle),
            )?;
            let certified_roi = roi.outcome.matched().expect("safe ROI always completes");
            if near_hit {
                let certified_full = full
                    .outcome
                    .matched()
                    .expect("full near hit always completes");
                compare_match(
                    certified_full,
                    certified_roi,
                    "certified full/ROI near-hit parity",
                );
            }
            println!(
                "SUMMARY {}: full_terminal={}, full_median_ms={:.3}, roi_terminal={}, roi_median_ms={:.3}",
                fixture.label,
                full.outcome.label(),
                full.median.as_secs_f64() * 1000.0,
                roi.outcome.label(),
                roi.median.as_secs_f64() * 1000.0,
            );
        }
    }
    println!("\nAll budget continuity, deterministic repetition and certified ROI checks passed.");
    println!(
        "Incomplete means unverified search, never a miss or a usable provisional coordinate."
    );
    println!("These offline search measurements do not establish capture or control-loop FPS.");
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

// Independent scalar reference: enumerate only the requested 200x200 safety ROI.
// It deliberately does not call the matcher or copy its bound/seed/integral algorithm.
fn scalar_roi_oracle(fixture: &Fixture, region: PixelRect) -> TemplateMatch {
    let size = fixture.template.width as usize;
    let template: Vec<u64> = fixture
        .template
        .rgba
        .chunks_exact(4)
        .map(|pixel| u64::from(pixel[0]))
        .collect();
    let mut gray = Vec::with_capacity(region.width as usize * region.height as usize);
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            gray.push(u64::from(
                fixture.frame.rgba[(y as usize * WIDTH as usize + x as usize) * 4],
            ));
        }
    }
    let sum_template: u64 = template.iter().sum();
    let squared_template: u64 = template.iter().map(|value| value * value).sum();
    let count = template.len() as u64;
    let variance_template = count * squared_template - sum_template * sum_template;
    let stride = region.width as usize;
    let mut best: Option<(f64, PixelRect)> = None;
    for y in 0..=region.height as usize - size {
        for x in 0..=region.width as usize - size {
            let (mut sum, mut squared, mut dot) = (0_u64, 0_u64, 0_u64);
            for row in 0..size {
                for column in 0..size {
                    let pixel = gray[(y + row) * stride + x + column];
                    sum += pixel;
                    squared += pixel * pixel;
                    dot += pixel * template[row * size + column];
                }
            }
            let variance = count * squared - sum * sum;
            if variance == 0 {
                continue;
            }
            let numerator = i128::from(count * dot) - i128::from(sum_template * sum);
            let denominator = (u128::from(variance_template) * u128::from(variance)) as f64;
            let score = (numerator as f64 / denominator.sqrt()).clamp(-1.0, 1.0);
            if best.is_none_or(|(previous, _)| score > previous) {
                best = Some((
                    score,
                    PixelRect {
                        x: region.x + x as u32,
                        y: region.y + y as u32,
                        width: size as u32,
                        height: size as u32,
                    },
                ));
            }
        }
    }
    let Some((score, bounds)) = best else {
        return TemplateMatch {
            score: 0.0,
            bounds: None,
        };
    };
    TemplateMatch {
        score,
        bounds: (score >= THRESHOLD).then_some(bounds),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    Complete {
        matched: TemplateMatch,
        proof: String,
    },
    Incomplete {
        reason: String,
    },
}

impl Outcome {
    fn label(&self) -> &'static str {
        match self {
            Self::Complete { .. } => "Complete",
            Self::Incomplete { .. } => "Incomplete",
        }
    }

    fn matched(&self) -> Option<TemplateMatch> {
        match self {
            Self::Complete { matched, .. } => Some(*matched),
            Self::Incomplete { .. } => None,
        }
    }
}

fn terminal(step: TemplateSearchStep, progress: TemplateSearchProgress) -> Option<Outcome> {
    match step {
        TemplateSearchStep::Pending => None,
        TemplateSearchStep::Complete(complete) => {
            if complete.proof != TemplateSearchProof::PerfectCorrelation {
                assert_eq!(
                    progress.verified, progress.candidates,
                    "non-perfect completion requires certification of every candidate"
                );
            }
            assert_eq!(
                complete.progress, progress,
                "terminal and job progress agree"
            );
            Some(Outcome::Complete {
                matched: complete.matched,
                proof: format!("{:?}", complete.proof),
            })
        }
        TemplateSearchStep::Incomplete(incomplete) => {
            assert_eq!(
                incomplete.progress, progress,
                "terminal and job progress agree"
            );
            Some(Outcome::Incomplete {
                reason: format!("{:?}", incomplete.reason),
            })
        }
    }
}

fn check_progress(previous: TemplateSearchProgress, current: TemplateSearchProgress) {
    assert_eq!(
        current.region, previous.region,
        "search never expands its authorized region"
    );
    assert_eq!(
        current.candidates, previous.candidates,
        "candidate coverage does not reset"
    );
    assert_eq!(
        current.max_work, previous.max_work,
        "budget is not replenished at a quantum boundary"
    );
    assert_eq!(current.work, current.exact_pixel_pairs + current.bound_work);
    assert!(
        current.work <= current.max_work,
        "search exceeds its total work budget"
    );
    assert!(
        current.verified <= current.candidates,
        "cannot certify nonexistent candidates"
    );
    for (name, before, after) in [
        ("verified", previous.verified, current.verified),
        (
            "bound_queries",
            previous.bound_queries,
            current.bound_queries,
        ),
        (
            "exact_evaluations",
            previous.exact_evaluations,
            current.exact_evaluations,
        ),
        (
            "exact_pixel_pairs",
            previous.exact_pixel_pairs,
            current.exact_pixel_pairs,
        ),
        ("bound_work", previous.bound_work, current.bound_work),
        ("work", previous.work, current.work),
    ] {
        assert!(
            after >= before,
            "{name} reset across a search quantum: {before} -> {after}"
        );
    }
}

#[derive(Debug, Clone, PartialEq)]
struct SearchReport {
    outcome: Outcome,
    progress: TemplateSearchProgress,
    calls: u64,
}

struct Sample {
    report: SearchReport,
    total: Duration,
    initialization: Duration,
    slowest_advance: Duration,
}

fn run(fixture: &Fixture, region: Option<PixelRect>) -> Result<Sample, AppError> {
    let options = MatchOptions {
        threshold: THRESHOLD,
        ..MatchOptions::default()
    };
    let matcher = TemplateMatcher::new(&fixture.template, options)?;
    let expected_region = region.unwrap_or(PixelRect {
        x: 0,
        y: 0,
        width: WIDTH,
        height: HEIGHT,
    });
    let expected_candidates = u64::from(expected_region.width - TEMPLATE_SIZE + 1)
        * u64::from(expected_region.height - TEMPLATE_SIZE + 1);
    let started = Instant::now();
    let mut job = matcher.begin_search(black_box(&fixture.frame), black_box(region))?;
    let initialization = started.elapsed();
    let mut total = initialization;
    let mut previous = job.progress();
    assert_eq!(previous.region, expected_region);
    assert_eq!(previous.candidates, expected_candidates);
    assert_eq!(previous.max_work, options.max_work);
    check_progress(previous, previous);
    let mut calls = 0;
    let mut slowest_advance = Duration::ZERO;
    loop {
        assert_eq!(
            job.progress(),
            previous,
            "progress persists between consecutive advances"
        );
        assert!(
            calls < MAX_ADVANCES,
            "bounded search failed to reach a terminal state"
        );
        let started = Instant::now();
        let step = job.advance(black_box(QUANTUM))?;
        let duration = started.elapsed();
        total += duration;
        slowest_advance = slowest_advance.max(duration);
        calls += 1;
        let progress = job.progress();
        check_progress(previous, progress);
        previous = progress;
        if let Some(outcome) = terminal(step, progress) {
            // A completed or budget-exhausted job cannot secretly start a new search.
            let replay = terminal(job.advance(QUANTUM)?, job.progress())
                .expect("terminal search stays terminal");
            assert_eq!(replay, outcome, "terminal result is stable");
            assert_eq!(
                job.progress(),
                progress,
                "terminal replay does not replenish or consume work"
            );
            return Ok(Sample {
                report: SearchReport {
                    outcome,
                    progress,
                    calls,
                },
                total,
                initialization,
                slowest_advance,
            });
        }
    }
}

fn compare_match(actual: TemplateMatch, expected: TemplateMatch, context: &str) {
    assert_eq!(actual.bounds, expected.bounds, "{context}: bounds");
    assert!(actual.score.is_finite(), "{context}: finite score");
    assert!(
        (actual.score - expected.score).abs() <= SCORE_TOLERANCE,
        "{context}: score {} != {}",
        actual.score,
        expected.score
    );
}

fn verify(fixture: &Fixture, sample: &Sample, oracle: Option<TemplateMatch>) {
    if let Some(matched) = sample.report.outcome.matched() {
        fixture.verify(matched);
        if let Some(oracle) = oracle {
            compare_match(matched, oracle, "independent complete-ROI oracle");
        }
    } else {
        assert!(
            fixture.expected.is_none(),
            "a near hit must complete within the default budget"
        );
        assert!(
            oracle.is_none(),
            "the bounded 200x200 safety ROI must always complete"
        );
    }
}

fn print_sample(label: &str, scope: &str, repetition: &str, sample: &Sample) {
    let p = sample.report.progress;
    let result = match &sample.report.outcome {
        Outcome::Complete { matched, proof } => format!(
            "Complete proof={proof} bounds={:?} score={:.12}",
            matched.bounds, matched.score
        ),
        Outcome::Incomplete { reason } => {
            format!("Incomplete reason={reason} bounds=UNAVAILABLE score=UNAVAILABLE")
        }
    };
    println!(
        "RESULT {label} {scope} {repetition}: terminal={result}; region={:?}, verified={}/{}, bound_queries={}, exact_evaluations={}, raw_exact_pixel_pairs={}, bound_work={}, work={}/{}, calls={}, begin_ms={:.3}, slowest_advance_ms={:.3}, total_ms={:.3}",
        p.region,
        p.verified,
        p.candidates,
        p.bound_queries,
        p.exact_evaluations,
        p.exact_pixel_pairs,
        p.bound_work,
        p.work,
        p.max_work,
        sample.report.calls,
        sample.initialization.as_secs_f64() * 1000.0,
        sample.slowest_advance.as_secs_f64() * 1000.0,
        sample.total.as_secs_f64() * 1000.0,
    );
}

struct Measurement {
    outcome: Outcome,
    median: Duration,
}

fn measure(
    fixture: &Fixture,
    region: Option<PixelRect>,
    scope: &str,
    samples: usize,
    oracle: Option<TemplateMatch>,
) -> Result<Measurement, AppError> {
    println!("PROGRESS {} {scope}: warmup 1/1", fixture.label);
    io::stdout()
        .flush()
        .map_err(|error| AppError::unavailable(error.to_string()))?;
    let warmup = run(fixture, region)?;
    print_sample(&fixture.label, scope, "warmup", &warmup);
    verify(fixture, &warmup, oracle);
    let mut elapsed = Vec::with_capacity(samples);
    for index in 1..=samples {
        println!(
            "PROGRESS {} {scope}: sample {index}/{samples}",
            fixture.label
        );
        io::stdout()
            .flush()
            .map_err(|error| AppError::unavailable(error.to_string()))?;
        let sample = run(fixture, region)?;
        verify(fixture, &sample, oracle);
        assert_eq!(
            sample.report, warmup.report,
            "same input preserves terminal result, work counters and quantum count"
        );
        print_sample(
            &fixture.label,
            scope,
            &format!("sample {index}/{samples}"),
            &sample,
        );
        elapsed.push(sample.total);
    }
    elapsed.sort_unstable();
    Ok(Measurement {
        outcome: warmup.report.outcome,
        median: elapsed[samples / 2],
    })
}
