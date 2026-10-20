//! Deterministic synthetic matcher measurements; no screen capture or input is used.
//!
//! Run with `cargo run -p ai-ex-automation --example template_bench --release`.
//! The small frames also keep a debug run practical; they do not represent a desktop workload.

use std::error::Error;
use std::hint::black_box;
use std::time::{Duration, Instant};

use ai_ex_automation::{MatchOptions, PixelRect, ScreenFrame, TemplateMatch, TemplateMatcher};
use ai_ex_domain::AppError;

const SAMPLES: usize = 7;
const TRACKING_MARGIN: u32 = 4;
const SEED: u32 = 0x41_49_45_58;

fn main() -> Result<(), Box<dyn Error>> {
    println!(
        "Synthetic template matcher benchmark: seed=0x{SEED:08x}, debug_assertions={}",
        cfg!(debug_assertions)
    );
    println!("Each case: 1 untimed warmup, {SAMPLES} timed searches, median reported.");
    println!("An additional untimed exact-work/one-less-budget check verifies the work count.");

    let mut measured_pixel_pairs = 0_u64;
    for (size, width, height, target_x, target_y) in [(20, 96, 72, 68, 44), (100, 128, 116, 20, 12)]
    {
        let fixture = Fixture::new(size, width, height, target_x, target_y, SEED ^ size)?;
        println!(
            "\nTemplate {size}x{size}; frame {width}x{height}; target ({target_x},{target_y}); ROI ({},{}) {}x{}",
            fixture.tracking.x, fixture.tracking.y, fixture.tracking.width, fixture.tracking.height
        );
        println!(
            "{:<12} {:>12} {:>13} {:>12} {:>15} {:>12}",
            "search", "region px", "grid positions", "visited", "pixel-pairs", "median ms"
        );
        let full = measure(&fixture, None)?;
        let tracking = measure(&fixture, Some(fixture.tracking))?;
        assert_eq!(full.found, tracking.found, "full/ROI result parity");
        print_measurement("full frame", &full);
        print_measurement("tracking ROI", &tracking);
        measured_pixel_pairs +=
            (full.work.pixel_pairs + tracking.work.pixel_pairs) * SAMPLES as u64;
        if !tracking.median.is_zero() {
            println!(
                "Full/ROI median ratio: {:.3}x on this synthetic fixture.",
                full.median.as_secs_f64() / tracking.median.as_secs_f64()
            );
        }
    }
    println!(
        "\nTimed searches completed {measured_pixel_pairs} grayscale pixel-pair multiplications in total."
    );
    println!(
        "Region pixels include grayscale/integral preprocessing; visited positions stop at the first perfect match."
    );
    println!("Timings exclude fixture setup, template construction, warmup and budget checks.");
    println!(
        "These synthetic measurements do not establish a screen capture or control-loop frequency."
    );
    Ok(())
}

struct Fixture {
    frame: ScreenFrame,
    template: ScreenFrame,
    expected: PixelRect,
    tracking: PixelRect,
}

impl Fixture {
    fn new(
        size: u32,
        width: u32,
        height: u32,
        target_x: u32,
        target_y: u32,
        seed: u32,
    ) -> Result<Self, AppError> {
        assert!(target_x >= TRACKING_MARGIN && target_y >= TRACKING_MARGIN);
        assert!(target_x + size + TRACKING_MARGIN <= width);
        assert!(target_y + size + TRACKING_MARGIN <= height);
        let mut state = seed;
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                // Equal RGB channels preserve this value under matcher grayscale conversion.
                // Adjacent horizontal pixels lie in disjoint halves of [0,255], so every
                // candidate window is nonconstant and therefore consumes a complete kernel.
                let gray = (state as u8 & 0x7f) | (((x + y) & 1) as u8 * 128);
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
        }
        let frame = ScreenFrame::new(width, height, rgba)?;
        let mut patch = Vec::with_capacity(size as usize * size as usize * 4);
        for row in target_y..target_y + size {
            let start = (row as usize * width as usize + target_x as usize) * 4;
            patch.extend_from_slice(&frame.rgba[start..start + size as usize * 4]);
        }
        Ok(Self {
            frame,
            template: ScreenFrame::new(size, size, patch)?,
            expected: PixelRect {
                x: target_x,
                y: target_y,
                width: size,
                height: size,
            },
            tracking: PixelRect {
                x: target_x - TRACKING_MARGIN,
                y: target_y - TRACKING_MARGIN,
                width: size + TRACKING_MARGIN * 2,
                height: size + TRACKING_MARGIN * 2,
            },
        })
    }
}

#[derive(Clone, Copy)]
struct Work {
    region_pixels: u64,
    grid_positions: u64,
    visited_positions: u64,
    pixel_pairs: u64,
}

impl Work {
    fn for_fixture(fixture: &Fixture, region: Option<PixelRect>) -> Self {
        let region = region.unwrap_or(PixelRect {
            x: 0,
            y: 0,
            width: fixture.frame.width,
            height: fixture.frame.height,
        });
        let columns = u64::from(region.width - fixture.template.width + 1);
        let rows = u64::from(region.height - fixture.template.height + 1);
        let target_column = u64::from(fixture.expected.x - region.x);
        let target_row = u64::from(fixture.expected.y - region.y);
        assert!(target_column < columns && target_row < rows);
        let visited_positions = target_row * columns + target_column + 1;
        let template_pixels =
            u64::from(fixture.template.width) * u64::from(fixture.template.height);
        Self {
            region_pixels: u64::from(region.width) * u64::from(region.height),
            grid_positions: columns * rows,
            visited_positions,
            pixel_pairs: visited_positions * template_pixels,
        }
    }
}

struct Measurement {
    found: TemplateMatch,
    work: Work,
    median: Duration,
}

fn measure(fixture: &Fixture, region: Option<PixelRect>) -> Result<Measurement, AppError> {
    let work = Work::for_fixture(fixture, region);
    let matcher = TemplateMatcher::new(
        &fixture.template,
        MatchOptions {
            threshold: 0.999,
            max_work: work.pixel_pairs,
        },
    )?;
    let insufficient = TemplateMatcher::new(
        &fixture.template,
        MatchOptions {
            threshold: 0.999,
            max_work: work.pixel_pairs - 1,
        },
    )?;
    let exhausted = insufficient
        .find_match(&fixture.frame, region)
        .expect_err("one fewer pixel-pair must exhaust the budget before the known perfect match");
    assert!(
        exhausted.to_string().contains("work limit exhausted"),
        "the lower-budget check must fail specifically on kernel work: {exhausted}"
    );

    let expected = TemplateMatch {
        score: 1.0,
        bounds: Some(fixture.expected),
    };
    // Exact budget success plus one-less failure proves the counted kernel work through
    // the public API. Known bounds also detect an accidental earlier perfect/affine match.
    let warmup = matcher.find_match(black_box(&fixture.frame), black_box(region))?;
    assert_eq!(warmup, expected, "warmup must find the exact copied patch");
    black_box(warmup);

    let mut times = [Duration::ZERO; SAMPLES];
    for elapsed in &mut times {
        let started = Instant::now();
        let found = matcher.find_match(black_box(&fixture.frame), black_box(region))?;
        *elapsed = started.elapsed();
        assert_eq!(
            found, expected,
            "every timed search must preserve score/bounds parity"
        );
        black_box(found);
    }
    times.sort_unstable();
    Ok(Measurement {
        found: warmup,
        work,
        median: times[SAMPLES / 2],
    })
}

fn print_measurement(label: &str, measurement: &Measurement) {
    println!(
        "{label:<12} {:>12} {:>13} {:>12} {:>15} {:>12.3}",
        measurement.work.region_pixels,
        measurement.work.grid_positions,
        measurement.work.visited_positions,
        measurement.work.pixel_pairs,
        measurement.median.as_secs_f64() * 1_000.0
    );
}
