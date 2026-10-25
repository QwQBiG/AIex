//! Device-free exactness and accounting tests for bounded proposal/certification.
use ai_ex_automation::{
    MatchOptions, PixelRect, ScreenFrame, TemplateMatch, TemplateMatcher,
    TemplateSearchIncompleteReason, TemplateSearchProof, TemplateSearchStep,
};

fn texture(width: u32, height: u32, mut seed: u32) -> ScreenFrame {
    let mut bytes = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let value = (32 + seed % 192) as u8;
        bytes.extend_from_slice(&[value, value, value, seed as u8]);
    }
    ScreenFrame::new(width, height, bytes).unwrap()
}
fn paste(frame: &mut ScreenFrame, patch: &ScreenFrame, x: u32, y: u32) {
    for row in 0..patch.height {
        let to = (((row + y) * frame.width + x) * 4) as usize;
        let from = (row * patch.width * 4) as usize;
        frame.rgba[to..to + patch.width as usize * 4]
            .copy_from_slice(&patch.rgba[from..from + patch.width as usize * 4]);
    }
}
fn near(template: &ScreenFrame) -> ScreenFrame {
    let mut frame = template.clone();
    let last = frame.rgba.len() - 4;
    for channel in &mut frame.rgba[last..last + 3] {
        *channel += 3;
    }
    frame
}
fn reference(template: &ScreenFrame, frame: &ScreenFrame, threshold: f64) -> TemplateMatch {
    let count = u64::from(template.width) * u64::from(template.height);
    let a = template
        .rgba
        .chunks_exact(4)
        .map(|pixel| u64::from(pixel[0]))
        .collect::<Vec<_>>();
    let b = frame
        .rgba
        .chunks_exact(4)
        .map(|pixel| u64::from(pixel[0]))
        .collect::<Vec<_>>();
    let sum_a = a.iter().sum::<u64>();
    let squared_a = a.iter().map(|value| value * value).sum::<u64>();
    let variance_a = count * squared_a - sum_a * sum_a;
    let mut best = TemplateMatch {
        score: -1.0,
        bounds: None,
    };
    for y in 0..=frame.height - template.height {
        for x in 0..=frame.width - template.width {
            let (mut sum, mut squared, mut dot) = (0_u64, 0_u64, 0_u64);
            for row in 0..template.height {
                for column in 0..template.width {
                    let value = b[((y + row) * frame.width + x + column) as usize];
                    sum += value;
                    squared += value * value;
                    dot += a[(row * template.width + column) as usize] * value;
                }
            }
            let variance = count * squared - sum * sum;
            if variance == 0 {
                continue;
            }
            let numerator = i128::from(count * dot) - i128::from(sum_a * sum);
            let product = u128::from(variance_a) * u128::from(variance);
            let perfect = numerator > 0 && numerator.unsigned_abs().pow(2) == product;
            let score = if perfect {
                1.0
            } else {
                (numerator as f64 / (product as f64).sqrt()).clamp(-1.0, 1.0)
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
                return best;
            }
        }
    }
    if best.bounds.is_none() {
        best.score = 0.0;
    }
    if best.score < threshold {
        best.bounds = None;
    }
    best
}
fn finish(
    matcher: &TemplateMatcher,
    frame: &ScreenFrame,
    region: Option<PixelRect>,
    quantum: u32,
) -> TemplateSearchStep {
    let mut search = matcher.begin_search(frame, region).unwrap();
    let mut previous = search.progress();
    for _ in 0..5_000_000 {
        let step = search.advance(quantum).unwrap();
        let next = search.progress();
        assert_eq!(next.region, previous.region);
        assert_eq!(next.candidates, previous.candidates);
        assert_eq!(next.max_work, previous.max_work);
        assert!(next.verified >= previous.verified && next.verified <= next.candidates);
        assert!(
            next.exact_evaluations >= previous.exact_evaluations
                && next.exact_evaluations <= next.verified
        );
        assert!(next.exact_pixel_pairs >= previous.exact_pixel_pairs);
        assert!(
            next.bound_queries >= previous.bound_queries && next.bound_work >= previous.bound_work
        );
        assert_eq!(next.bound_work, next.bound_queries * 48);
        assert_eq!(next.work, next.exact_pixel_pairs + next.bound_work);
        assert!(next.work >= previous.work && next.work <= next.max_work);
        if step != TemplateSearchStep::Pending {
            assert_eq!(
                step,
                search.advance(quantum).unwrap(),
                "terminal advance is idempotent"
            );
            return step;
        }
        previous = next;
    }
    panic!("bounded fixture did not terminate");
}
fn complete(step: TemplateSearchStep) -> ai_ex_automation::TemplateSearchComplete {
    match step {
        TemplateSearchStep::Complete(done) => done,
        other => panic!("expected complete, got {other:?}"),
    }
}
fn same(actual: TemplateMatch, expected: TemplateMatch) {
    assert_eq!(actual.score.to_bits(), expected.score.to_bits());
    assert_eq!(actual.bounds, expected.bounds);
}

#[test]
fn hierarchical_nonperfect_hit_and_miss_certify_the_same_global_score_as_scalar() {
    let template = texture(20, 20, 0x1020_3040);
    for hit in [false, true] {
        let mut frame = texture(85, 85, 0x5060_7080);
        // Threshold=1 makes this a certified miss with a high nonperfect
        // global best, rather than pretending an absent random target is easy.
        paste(&mut frame, &near(&template), 65, 65);
        let threshold = if hit { 0.999 } else { 1.0 };
        let expected = reference(&template, &frame, threshold);
        let matcher = TemplateMatcher::new(
            &template,
            MatchOptions {
                threshold,
                max_work: 1_000_000,
            },
        )
        .unwrap();
        let done = complete(finish(&matcher, &frame, None, 1_021));
        same(done.matched, expected);
        assert_eq!(done.proof, TemplateSearchProof::CertifiedBounds);
        assert_eq!(done.progress.verified, done.progress.candidates);
        assert!(done.progress.bound_queries > 0);
        if hit {
            assert!(done.matched.score < 1.0 && done.matched.bounds.is_some());
        } else {
            assert!(done.matched.bounds.is_none());
        }
        assert!(done.progress.max_work < done.progress.candidates * 400);
    }
    let absent = texture(85, 85, 0x5060_7080);
    let matcher = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: 0.999,
            max_work: 1_000_000,
        },
    )
    .unwrap();
    let TemplateSearchStep::Incomplete(incomplete) = finish(&matcher, &absent, None, 1_021) else {
        panic!("unresolved random miss must not become a guessed Complete");
    };
    assert!(incomplete.progress.bound_queries > 0);
    assert!(incomplete.progress.verified < incomplete.progress.candidates);
}

#[test]
fn seed_perfect_and_nonperfect_ties_still_choose_first_row_major_candidate() {
    let template = texture(20, 20, 0x1357_2468);
    for perfect in [false, true] {
        let patch = if perfect {
            template.clone()
        } else {
            near(&template)
        };
        let mut frame = texture(85, 85, 0xabcd_1234);
        paste(&mut frame, &patch, 3, 4);
        paste(&mut frame, &patch, 60, 61);
        let matcher = TemplateMatcher::new(
            &template,
            MatchOptions {
                threshold: 0.7,
                max_work: 1_000_000,
            },
        )
        .unwrap();
        let done = complete(finish(&matcher, &frame, None, 997));
        same(done.matched, reference(&template, &frame, 0.7));
        assert_eq!(
            done.matched.bounds.unwrap(),
            PixelRect {
                x: 3,
                y: 4,
                width: 20,
                height: 20
            }
        );
        assert_eq!(done.progress.verified, done.progress.candidates);
    }
}

#[test]
fn hierarchical_threshold_equality_and_adjacent_f64_values_are_exact() {
    let template = texture(20, 20, 0x5555_aaaa);
    let mut frame = texture(85, 85, 0x3333_7777);
    paste(&mut frame, &near(&template), 65, 65);
    let expected = reference(&template, &frame, 0.0);
    let bits = expected.score.to_bits();
    for threshold in [
        f64::from_bits(bits - 1),
        expected.score,
        f64::from_bits(bits + 1),
    ] {
        let matcher = TemplateMatcher::new(
            &template,
            MatchOptions {
                threshold,
                max_work: 1_000_000,
            },
        )
        .unwrap();
        let done = complete(finish(&matcher, &frame, None, 4_096));
        assert_eq!(done.matched.score.to_bits(), bits);
        assert_eq!(done.matched.bounds.is_some(), threshold <= expected.score);
    }
}

#[test]
fn insufficient_projection_budget_is_typed_without_leaking_a_provisional_hit() {
    let template = texture(20, 20, 0x1111_2222);
    let mut frame = texture(85, 85, 0x3333_4444);
    paste(&mut frame, &near(&template), 0, 0);
    let matcher = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: 0.7,
            max_work: 400 + 4 * 48,
        },
    )
    .unwrap();
    for quantum in [1, 257, 16_384] {
        let TemplateSearchStep::Incomplete(incomplete) = finish(&matcher, &frame, None, quantum)
        else {
            panic!("uncertified hit must not escape as Complete")
        };
        assert_eq!(
            incomplete.reason,
            TemplateSearchIncompleteReason::WorkBudgetExhausted
        );
        assert_eq!(incomplete.progress.exact_evaluations, 1);
        assert_eq!(incomplete.progress.exact_pixel_pairs, 400);
        assert_eq!(incomplete.progress.bound_queries, 4);
        assert_eq!(incomplete.progress.work, 592);
    }
    assert!(
        matcher
            .find_match(&frame, None)
            .unwrap_err()
            .message
            .contains("work limit exhausted")
    );
}

#[test]
fn first_perfect_large_grid_retains_single_kernel_and_one_less_budget_contract() {
    let template = texture(20, 20, 0x2468_ace0);
    let mut frame = texture(85, 85, 0x9753_8642);
    paste(&mut frame, &template, 0, 0);
    for budget in [399, 400] {
        let matcher = TemplateMatcher::new(
            &template,
            MatchOptions {
                threshold: 1.0,
                max_work: budget,
            },
        )
        .unwrap();
        match finish(&matcher, &frame, None, 31) {
            TemplateSearchStep::Complete(done) => {
                assert_eq!(budget, 400);
                assert_eq!(done.proof, TemplateSearchProof::PerfectCorrelation);
                assert_eq!(done.progress.exact_pixel_pairs, 400);
                assert_eq!(done.progress.bound_queries, 0);
            }
            TemplateSearchStep::Incomplete(done) => {
                assert_eq!(budget, 399);
                assert_eq!(done.progress.exact_pixel_pairs, 0);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn constant_scans_and_preprocessing_yield_without_spending_kernel_or_projection_work() {
    let template = texture(20, 20, 0x9876_5432);
    let frame = ScreenFrame::new(85, 85, [7, 7, 7, 255].repeat(85 * 85)).unwrap();
    let matcher = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: 0.0,
            max_work: 1,
        },
    )
    .unwrap();
    let mut job = matcher.begin_search(&frame, None).unwrap();
    let before = job.progress();
    assert!(job.advance(0).is_err());
    assert_eq!(before, job.progress());
    assert_eq!(job.advance(1).unwrap(), TemplateSearchStep::Pending);
    assert_eq!(job.progress().verified, 0);
    let mut observed_scan_yield = false;
    loop {
        match job.advance(17).unwrap() {
            TemplateSearchStep::Pending => {
                observed_scan_yield |= job.progress().verified > 0;
            }
            TemplateSearchStep::Complete(done) => {
                assert!(observed_scan_yield);
                assert_eq!(done.progress.verified, 4_356);
                assert_eq!(done.progress.work, 0);
                assert_eq!(
                    done.matched,
                    TemplateMatch {
                        score: 0.0,
                        bounds: None
                    }
                );
                break;
            }
            other => panic!("constant frame cannot consume budget: {other:?}"),
        }
    }
}

#[test]
fn small_or_unequal_block_grids_keep_direct_exact_kernel_accounting() {
    for (width, height, frame_width, frame_height) in [(20, 20, 21, 20), (21, 23, 87, 89)] {
        let template = texture(width, height, 0x1010_2020);
        let frame = texture(frame_width, frame_height, 0x3030_4040);
        let candidates = u64::from(frame_width - width + 1) * u64::from(frame_height - height + 1);
        let work = candidates * u64::from(width) * u64::from(height);
        let matcher = TemplateMatcher::new(
            &template,
            MatchOptions {
                threshold: 0.7,
                max_work: work,
            },
        )
        .unwrap();
        let done = complete(finish(&matcher, &frame, None, 2_003));
        same(done.matched, reference(&template, &frame, 0.7));
        assert_eq!(done.proof, TemplateSearchProof::Exhaustive);
        assert_eq!(done.progress.bound_queries, 0);
        assert_eq!(done.progress.work, work);
        let insufficient = TemplateMatcher::new(
            &template,
            MatchOptions {
                threshold: 0.7,
                max_work: work - 1,
            },
        )
        .unwrap();
        assert!(matches!(
            finish(&insufficient, &frame, None, 2_003),
            TemplateSearchStep::Incomplete(_)
        ));
    }
}

#[test]
fn clipping_never_searches_beyond_the_requested_region_and_quantums_do_not_reset_work() {
    let template = texture(20, 20, 0x9876_1234);
    let mut frame = texture(90, 90, 0x1234_abcd);
    paste(&mut frame, &template, 0, 0);
    paste(&mut frame, &near(&template), 70, 70);
    let region = PixelRect {
        x: 5,
        y: 5,
        width: u32::MAX,
        height: u32::MAX,
    };
    let matcher = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: 0.999,
            max_work: 1_000_000,
        },
    )
    .unwrap();
    let left = complete(finish(&matcher, &frame, Some(region), 257));
    let right = complete(finish(&matcher, &frame, Some(region), 16_384));
    assert_eq!(left, right);
    assert_eq!(
        left.progress.region,
        PixelRect {
            x: 5,
            y: 5,
            width: 85,
            height: 85
        }
    );
    assert_eq!(
        left.matched.bounds.unwrap(),
        PixelRect {
            x: 70,
            y: 70,
            width: 20,
            height: 20
        }
    );
    assert!(
        left.matched.score < 1.0,
        "outside perfect candidate is not authorized"
    );
}
