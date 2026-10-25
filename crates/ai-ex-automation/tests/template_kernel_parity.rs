//! Small deterministic differential fixtures for the exact integer dot kernel.
//! The reference scans each candidate directly with scalar u64 arithmetic; it
//! shares neither the optimized kernel nor the production integral image.

use ai_ex_automation::{MatchOptions, PixelRect, ScreenFrame, TemplateMatch, TemplateMatcher};
use ai_ex_domain::AppError;

fn texture(width: u32, height: u32, seed: u32) -> ScreenFrame {
    let mut state = seed;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        let mut pixel = [0; 4];
        for channel in &mut pixel {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *channel = state as u8;
        }
        rgba.extend_from_slice(&pixel);
    }
    ScreenFrame::new(width, height, rgba).unwrap()
}

fn solid(width: u32, height: u32, value: u8) -> ScreenFrame {
    ScreenFrame::new(
        width,
        height,
        [value, value, value, 255].repeat((width * height) as usize),
    )
    .unwrap()
}

fn gray_at(frame: &ScreenFrame, x: u32, y: u32) -> u8 {
    let index = ((y * frame.width + x) * 4) as usize;
    let red = u32::from(frame.rgba[index]);
    let green = u32::from(frame.rgba[index + 1]);
    let blue = u32::from(frame.rgba[index + 2]);
    ((4_899 * red + 9_617 * green + 1_868 * blue + 8_192) >> 14) as u8
}

fn gray(frame: &ScreenFrame) -> Vec<u8> {
    (0..frame.height)
        .flat_map(|y| (0..frame.width).map(move |x| gray_at(frame, x, y)))
        .collect()
}

fn near_copy(template: &ScreenFrame) -> ScreenFrame {
    let mut changed = template.clone();
    let value = gray_at(template, 0, 0).wrapping_add(47);
    changed.rgba[..3].fill(value);
    changed
}

fn paste(frame: &mut ScreenFrame, patch: &ScreenFrame, x: u32, y: u32) {
    assert!(x + patch.width <= frame.width && y + patch.height <= frame.height);
    for row in 0..patch.height {
        let from = (row * patch.width * 4) as usize;
        let to = (((y + row) * frame.width + x) * 4) as usize;
        frame.rgba[to..to + patch.width as usize * 4]
            .copy_from_slice(&patch.rgba[from..from + patch.width as usize * 4]);
    }
}

fn scalar_u64_reference(
    template: &ScreenFrame,
    frame: &ScreenFrame,
    region: Option<PixelRect>,
    options: MatchOptions,
) -> Result<TemplateMatch, AppError> {
    // Fixtures are valid frames/regions. Clipping and small-region behavior are
    // retained so offset/tail comparisons exercise the public search boundary.
    let mut region = region.unwrap_or(PixelRect {
        x: 0,
        y: 0,
        width: frame.width,
        height: frame.height,
    });
    assert!(
        region.width > 0 && region.height > 0 && region.x < frame.width && region.y < frame.height
    );
    region.width = region.width.min(frame.width - region.x);
    region.height = region.height.min(frame.height - region.y);
    if region.width < template.width || region.height < template.height {
        return Ok(TemplateMatch {
            score: 0.0,
            bounds: None,
        });
    }
    let template_gray = gray(template);
    let frame_gray = gray(frame);
    let count = u64::from(template.width) * u64::from(template.height);
    let template_sum = template_gray
        .iter()
        .map(|value| u64::from(*value))
        .sum::<u64>();
    let template_squared = template_gray
        .iter()
        .map(|value| u64::from(*value) * u64::from(*value))
        .sum::<u64>();
    let template_variance = count * template_squared - template_sum * template_sum;
    assert!(template_variance > 0);
    let mut work = 0_u64;
    let mut best = TemplateMatch {
        score: -1.0,
        bounds: None,
    };
    for y in region.y..=region.y + region.height - template.height {
        for x in region.x..=region.x + region.width - template.width {
            let mut sum = 0_u64;
            let mut squared = 0_u64;
            for row in 0..template.height {
                for column in 0..template.width {
                    let value =
                        u64::from(frame_gray[((y + row) * frame.width + x + column) as usize]);
                    sum += value;
                    squared += value * value;
                }
            }
            let variance = count * squared - sum * sum;
            if variance == 0 {
                continue;
            }
            work = work
                .checked_add(count)
                .filter(|next| *next <= options.max_work)
                .ok_or_else(|| AppError::unavailable("template matching work limit exhausted"))?;
            let mut dot = 0_u64;
            for row in 0..template.height {
                for column in 0..template.width {
                    let template_value =
                        u64::from(template_gray[(row * template.width + column) as usize]);
                    let frame_value =
                        u64::from(frame_gray[((y + row) * frame.width + x + column) as usize]);
                    dot += template_value * frame_value;
                }
            }
            let numerator = i128::from(count * dot) - i128::from(template_sum * sum);
            let variance_product = u128::from(template_variance) * u128::from(variance);
            let absolute = numerator.unsigned_abs();
            let perfect = numerator > 0 && absolute * absolute == variance_product;
            let score = if perfect {
                1.0
            } else {
                (numerator as f64 / (variance_product as f64).sqrt()).clamp(-1.0, 1.0)
            };
            let bounds = PixelRect {
                x,
                y,
                width: template.width,
                height: template.height,
            };
            if best.bounds.is_none() || score > best.score {
                best = TemplateMatch {
                    score,
                    bounds: Some(bounds),
                };
            }
            if perfect {
                return Ok(TemplateMatch {
                    score: 1.0,
                    bounds: Some(bounds),
                });
            }
        }
    }
    if best.bounds.is_none() {
        best.score = 0.0;
    }
    if best.score < options.threshold {
        best.bounds = None;
    }
    Ok(best)
}

fn parity(
    label: &str,
    template: &ScreenFrame,
    frame: &ScreenFrame,
    region: Option<PixelRect>,
    threshold: f64,
    max_work: u64,
) -> Result<TemplateMatch, AppError> {
    let options = MatchOptions {
        threshold,
        max_work,
    };
    let expected = scalar_u64_reference(template, frame, region, options);
    let actual = TemplateMatcher::new(template, options)
        .unwrap()
        .find_match(frame, region);
    match (&actual, &expected) {
        (Ok(actual), Ok(expected)) => {
            assert_eq!(
                actual.score.to_bits(),
                expected.score.to_bits(),
                "{label}: exact f64 score bits"
            );
            assert_eq!(actual.bounds, expected.bounds, "{label}: row-major bounds");
        }
        (Err(actual), Err(expected)) => {
            assert_eq!(actual, expected, "{label}: typed budget failure")
        }
        _ => panic!("{label}: optimized={actual:?}, scalar={expected:?}"),
    }
    actual
}

#[test]
fn colored_nonperfect_searches_match_scalar_for_kernel_widths_heights_and_tails() {
    for (width, height) in [
        (20, 20),
        (21, 23),
        (31, 20),
        (99, 21),
        (100, 100),
        (199, 23),
        (200, 200),
        (20, 200),
        (200, 20),
    ] {
        let template = texture(width, height, 0x1a2b_3c4d ^ width ^ (height << 8));
        let mut frame = texture(width + 2, height + 2, 0x89ab_cdef ^ width);
        paste(&mut frame, &near_copy(&template), 1, 1);
        let found = parity(
            &format!("{width}x{height}"),
            &template,
            &frame,
            None,
            0.7,
            u64::from(width) * u64::from(height) * 9,
        )
        .unwrap();
        assert!(found.score < 1.0 && found.score > 0.9);
        assert_eq!(
            found.bounds.unwrap(),
            PixelRect {
                x: 1,
                y: 1,
                width,
                height
            }
        );
    }
}

#[test]
fn maximum_size_dot_above_i32_range_remains_exact_for_perfect_and_near_matches() {
    let mut template = solid(200, 200, 255);
    let last = template.rgba.len() - 4;
    template.rgba[last..last + 3].fill(254);
    let maximum_dot = 40_000_u64 * 65_025 - (65_025 - 64_516);
    assert!(maximum_dot > i32::MAX as u64 && maximum_dot <= u64::from(u32::MAX));
    let perfect = parity("maximum perfect", &template, &template, None, 1.0, 40_000).unwrap();
    assert_eq!(perfect.score, 1.0);
    let mut near = template.clone();
    near.rgba[..3].fill(253);
    let found = parity("maximum near", &template, &near, None, 0.0, 40_000).unwrap();
    assert!(found.score < 1.0);
    assert!(parity("maximum one less", &template, &template, None, 1.0, 39_999).is_err());
}

#[test]
fn negative_score_and_positive_affine_perfect_equality_are_unchanged() {
    let mut template = texture(21, 23, 0x1020_3040);
    for y in 0..template.height {
        for x in 0..template.width {
            let value = gray_at(&template, x, y) % 100;
            let index = ((y * template.width + x) * 4) as usize;
            template.rgba[index..index + 3].fill(value);
        }
    }
    let mut inverse = template.clone();
    let mut affine = template.clone();
    for (source, (inverse, affine)) in template.rgba.chunks_exact(4).zip(
        inverse
            .rgba
            .chunks_exact_mut(4)
            .zip(affine.rgba.chunks_exact_mut(4)),
    ) {
        inverse[..3].fill(255 - source[0]);
        affine[..3].fill(source[0] * 2 + 10);
    }
    let count = u64::from(template.width) * u64::from(template.height);
    let negative = parity("inverse", &template, &inverse, None, 0.0, count).unwrap();
    assert_eq!(negative.score, -1.0);
    assert!(negative.bounds.is_none());
    let perfect = parity("affine", &template, &affine, None, 1.0, count).unwrap();
    assert_eq!(perfect.score, 1.0);
    assert!(perfect.bounds.is_some());
}

#[test]
fn nonperfect_equal_scores_keep_first_row_major_tie_and_later_stronger_candidate_wins() {
    let template = texture(20, 20, 0x8765_4321);
    let strong = near_copy(&template);
    let mut frame = solid(41, 20, 7);
    paste(&mut frame, &strong, 0, 0);
    paste(&mut frame, &strong, 21, 0);
    let tied = parity("nonperfect tie", &template, &frame, None, 0.7, 22 * 400).unwrap();
    assert!(tied.score < 1.0 && tied.score > 0.9);
    assert_eq!(tied.bounds.unwrap().x, 0);
    let mut weak = template.clone();
    for pixel in weak.rgba.chunks_exact_mut(4).step_by(5) {
        pixel[..3].fill(127);
    }
    paste(&mut frame, &weak, 0, 0);
    let strongest = parity("later stronger", &template, &frame, None, 0.7, 22 * 400).unwrap();
    assert_eq!(strongest.bounds.unwrap().x, 21);
}

#[test]
fn threshold_equality_and_adjacent_f64_thresholds_preserve_score_bits_and_bounds() {
    let template = texture(31, 20, 0x1122_3344);
    let frame = near_copy(&template);
    let expected = scalar_u64_reference(
        &template,
        &frame,
        None,
        MatchOptions {
            threshold: 0.0,
            max_work: 620,
        },
    )
    .unwrap();
    assert!(expected.score > 0.0 && expected.score < 1.0);
    let bits = expected.score.to_bits();
    for threshold in [
        0.0,
        f64::from_bits(bits - 1),
        expected.score,
        f64::from_bits(bits + 1),
        1.0,
    ] {
        let found = parity(
            "threshold boundary",
            &template,
            &frame,
            None,
            threshold,
            620,
        )
        .unwrap();
        assert_eq!(found.bounds.is_some(), threshold <= expected.score);
    }
}

#[test]
fn clipped_roi_offsets_and_region_smaller_than_template_match_scalar() {
    let template = texture(31, 23, 0x5566_7788);
    let mut frame = texture(40, 34, 0x1234_5678);
    paste(&mut frame, &near_copy(&template), 7, 10);
    let found = parity(
        "clipped offset",
        &template,
        &frame,
        Some(PixelRect {
            x: 6,
            y: 9,
            width: u32::MAX,
            height: u32::MAX,
        }),
        0.7,
        12 * 31 * 23,
    )
    .unwrap();
    assert_eq!(
        found.bounds.unwrap(),
        PixelRect {
            x: 7,
            y: 10,
            width: 31,
            height: 23
        }
    );
    let miss = parity(
        "small region",
        &template,
        &frame,
        Some(PixelRect {
            x: 6,
            y: 9,
            width: 30,
            height: 23,
        }),
        0.7,
        1,
    )
    .unwrap();
    assert_eq!(
        miss,
        TemplateMatch {
            score: 0.0,
            bounds: None
        }
    );
}

#[test]
fn exact_kernel_budget_boundaries_never_return_partial_best() {
    let template = texture(20, 20, 0x1234_1234);
    let frame = texture(21, 20, 0x5678_5678);
    for budget in [399, 400, 799, 800, 801] {
        let result = parity(
            "two nonperfect windows",
            &template,
            &frame,
            None,
            0.7,
            budget,
        );
        assert_eq!(result.is_ok(), budget >= 800);
    }
}

#[test]
fn constant_windows_remain_free_and_first_perfect_kernel_stops_exactly() {
    let mut template = texture(20, 20, 0x2468_1357);
    template.rgba[..3].fill(200);
    let constant = solid(60, 60, 7);
    assert_eq!(
        parity("constant no work", &template, &constant, None, 0.0, 1).unwrap(),
        TemplateMatch {
            score: 0.0,
            bounds: None
        }
    );
    let mut prefix = solid(42, 20, 7);
    paste(&mut prefix, &template, 22, 0);
    assert!(
        parity(
            "constant prefix one less",
            &template,
            &prefix,
            None,
            1.0,
            7_999
        )
        .is_err()
    );
    let found = parity(
        "constant prefix exact",
        &template,
        &prefix,
        None,
        1.0,
        8_000,
    )
    .unwrap();
    assert_eq!(found.bounds.unwrap().x, 22);
    paste(&mut prefix, &template, 0, 0);
    let found = parity("first perfect exact", &template, &prefix, None, 1.0, 400).unwrap();
    assert_eq!(found.bounds.unwrap().x, 0);
}
