//! Incremental exact NCC with a bounded projection proposal pass and certification.
//! Images and templates are borrowed for one search; candidates never escape an
//! incomplete result. Existing frame/candidate caps also bound preprocessing.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use ai_ex_domain::AppError;
use serde::Serialize;

use crate::template_matcher::{MAX_FRAME_PIXELS, MAX_SEARCH_CANDIDATES, validate_frame};
use crate::{PixelRect, ScreenFrame, TemplateMatch, TemplateMatcher};

const BLOCKS: usize = 16;
const GRID: usize = 4;
const SEEDS: usize = 32;
const SMALL_SEARCH: u64 = 4_096;
/// Each started 16-block query is conservatively charged 2*K+16 units, in
/// addition to actual exact-kernel pixel pairs. This is not a pixel-pair count.
const BOUND_WORK: u64 = 2 * BLOCKS as u64 + 16;
const MAX_INTEGRAL_CELLS: usize = MAX_FRAME_PIXELS as usize * 2 + 2;
const MAX_SCRATCH_BYTES: usize = 320 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TemplateSearchProgress {
    pub region: PixelRect,
    pub candidates: u64,
    /// Distinct exact, constant, or strictly dominated candidates; cached seeds
    /// are counted once, even when visited again by the certification pass.
    pub verified: u64,
    /// Started projection queries, including any partly processed Pending query.
    pub bound_queries: u64,
    pub exact_evaluations: u64,
    /// Actual completed grayscale template/frame pixel-pair multiplications.
    pub exact_pixel_pairs: u64,
    /// Conservative projection charges; always bound_queries * 48.
    pub bound_work: u64,
    /// exact_pixel_pairs + bound_work, cumulative across every advance call.
    pub work: u64,
    pub max_work: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateSearchProof {
    Exhaustive,
    PerfectCorrelation,
    CertifiedBounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateSearchIncompleteReason {
    WorkBudgetExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TemplateSearchComplete {
    pub matched: TemplateMatch,
    pub progress: TemplateSearchProgress,
    pub proof: TemplateSearchProof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TemplateSearchIncomplete {
    pub reason: TemplateSearchIncompleteReason,
    pub progress: TemplateSearchProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum TemplateSearchStep {
    Pending,
    Complete(TemplateSearchComplete),
    Incomplete(TemplateSearchIncomplete),
}

#[derive(Debug)]
pub(crate) struct BlockProjection {
    width: usize,
    height: usize,
    sums: [u64; BLOCKS],
    residual: u64,
}

impl BlockProjection {
    pub(crate) fn new(gray: &[u8], width: usize, height: usize, squared: u64) -> Option<Self> {
        if !width.is_multiple_of(GRID) || !height.is_multiple_of(GRID) {
            return None;
        }
        let mut sums = [0_u64; BLOCKS];
        let block_width = width / GRID;
        let block_height = height / GRID;
        for y in 0..height {
            for x in 0..width {
                sums[(y / block_height) * GRID + x / block_width] += u64::from(gray[y * width + x]);
            }
        }
        let residual = gray.len() as u64 * squared
            - BLOCKS as u64 * sums.iter().map(|sum| sum * sum).sum::<u64>();
        Some(Self {
            width: block_width,
            height: block_height,
            sums,
            residual,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Moments {
    sum: u64,
    squared: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Gray,
    Integral,
    Probe,
    Direct,
    SeedScan,
    Seeds,
    Certify,
    Dot,
    Bound,
    Done,
}

#[derive(Debug, Clone, Copy)]
struct Best {
    score: f64,
    index: u64,
}

#[derive(Debug, Clone, Copy)]
struct Seed {
    index: u64,
    upper: f64,
    score: Option<f64>,
}
impl PartialEq for Seed {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.upper.to_bits() == other.upper.to_bits()
    }
}
impl Eq for Seed {}
impl PartialOrd for Seed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Seed {
    fn cmp(&self, other: &Self) -> Ordering {
        // The heap root is the worst retained seed: lowest upper, latest tie.
        other
            .upper
            .total_cmp(&self.upper)
            .then_with(|| self.index.cmp(&other.index))
    }
}

#[derive(Debug, Clone, Copy)]
struct DotJob {
    index: u64,
    moments: Moments,
    variance: u64,
    offset: usize,
    sum: u32,
    next: Phase,
}
#[derive(Debug, Clone, Copy)]
struct BoundJob {
    index: u64,
    moments: Moments,
    variance: u64,
    block: usize,
    projected: u64,
    squared: u64,
    next: Phase,
}

/// A single immutable frame/region search. Each operation processes at most one
/// grayscale pixel, integral cell, candidate classification, projection block,
/// or exact pixel pair (fixed-size scalar finalization/heap work is bounded).
/// No call resets the cumulative hard budget. Terminal calls are idempotent.
pub struct TemplateMatchSearch<'a> {
    matcher: &'a TemplateMatcher,
    frame: &'a ScreenFrame,
    progress: TemplateSearchProgress,
    width: usize,
    columns: usize,
    gray: Vec<u8>,
    integral: Vec<Moments>,
    integral_cells: usize,
    row: Moments,
    phase: Phase,
    cursor: u64,
    hierarchical: bool,
    probe_end: u64,
    best: Option<Best>,
    heap: BinaryHeap<Seed>,
    seeds: Vec<Seed>,
    seed_cursor: usize,
    dot: Option<DotJob>,
    bound: Option<BoundJob>,
    terminal: Option<TemplateSearchStep>,
}

impl<'a> TemplateMatchSearch<'a> {
    pub(crate) fn new(
        matcher: &'a TemplateMatcher,
        frame: &'a ScreenFrame,
        region: Option<PixelRect>,
    ) -> Result<Self, AppError> {
        validate_frame(frame)?;
        let region = region
            .unwrap_or(PixelRect {
                x: 0,
                y: 0,
                width: frame.width,
                height: frame.height,
            })
            .clipped_to(frame.width, frame.height)?;
        let width = region.width as usize;
        let height = region.height as usize;
        let fits = matcher.width <= width && matcher.height <= height;
        let columns = if fits { width - matcher.width + 1 } else { 0 };
        let candidates = if fits {
            (columns as u64)
                .checked_mul((height - matcher.height + 1) as u64)
                .ok_or_else(|| AppError::configuration("template candidate count overflow"))?
        } else {
            0
        };
        if candidates > MAX_SEARCH_CANDIDATES {
            return Err(AppError::configuration(
                "template search contains too many candidate positions",
            ));
        }
        let integral_cells = if fits {
            (width + 1)
                .checked_mul(height + 1)
                .filter(|count| *count <= MAX_INTEGRAL_CELLS)
                .ok_or_else(|| AppError::protocol("template integral image size overflow"))?
        } else {
            0
        };
        // With validated >=20-pixel template sides the actual integral+gray
        // workspace is below this cap, even for the longest legal thin frame.
        // Add a constant allowance for both bounded seed containers.
        let scratch = integral_cells
            .checked_mul(std::mem::size_of::<Moments>())
            .and_then(|bytes| bytes.checked_add(if fits { width * height } else { 0 }))
            .and_then(|bytes| bytes.checked_add(SEEDS * std::mem::size_of::<Seed>() * 2))
            .filter(|bytes| *bytes <= MAX_SCRATCH_BYTES)
            .ok_or_else(|| AppError::unavailable("template search scratch exceeds 320 MiB"))?;
        let _bounded_scratch = scratch;
        // If the caller can already pay the entire direct search, avoid an
        // unnecessary screening bill and retain the previous exact-budget API.
        let exhaustive_work = candidates
            .checked_mul(matcher.gray.len() as u64)
            .ok_or_else(|| AppError::configuration("template exhaustive work overflow"))?;
        let hierarchical = candidates > SMALL_SEARCH
            && matcher.projection.is_some()
            && matcher.options.max_work < exhaustive_work;
        let mut gray = Vec::new();
        let mut integral = Vec::new();
        let mut heap = BinaryHeap::new();
        let mut seeds = Vec::new();
        if fits {
            gray.try_reserve_exact(width * height)
                .map_err(|_| AppError::unavailable("cannot allocate template grayscale image"))?;
            integral
                .try_reserve_exact(integral_cells)
                .map_err(|_| AppError::unavailable("cannot allocate template integral image"))?;
        }
        if hierarchical {
            heap.try_reserve_exact(SEEDS)
                .map_err(|_| AppError::unavailable("cannot allocate template candidate heap"))?;
            seeds
                .try_reserve_exact(SEEDS)
                .map_err(|_| AppError::unavailable("cannot allocate template seed cache"))?;
        }
        let progress = TemplateSearchProgress {
            region,
            candidates,
            verified: 0,
            bound_queries: 0,
            exact_evaluations: 0,
            exact_pixel_pairs: 0,
            bound_work: 0,
            work: 0,
            max_work: matcher.options.max_work,
        };
        let mut search = Self {
            matcher,
            frame,
            progress,
            width,
            columns,
            gray,
            integral,
            integral_cells,
            row: Moments::default(),
            phase: Phase::Gray,
            cursor: 0,
            hierarchical,
            probe_end: 0,
            best: None,
            heap,
            seeds,
            seed_cursor: 0,
            dot: None,
            bound: None,
            terminal: None,
        };
        if !fits {
            search.complete(TemplateSearchProof::Exhaustive);
        }
        Ok(search)
    }

    pub fn progress(&self) -> TemplateSearchProgress {
        self.progress
    }

    pub fn advance(&mut self, max_operations: u32) -> Result<TemplateSearchStep, AppError> {
        if let Some(terminal) = self.terminal {
            return Ok(terminal);
        }
        if max_operations == 0 {
            return Err(AppError::configuration(
                "template search advance limit must be positive",
            ));
        }
        let mut left = max_operations as usize;
        while left > 0 && self.terminal.is_none() {
            match self.phase {
                Phase::Gray => self.advance_gray(&mut left),
                Phase::Integral => self.advance_integral(&mut left),
                Phase::Dot => self.advance_dot(&mut left),
                Phase::Bound => {
                    self.advance_bound();
                    left -= 1;
                }
                Phase::Probe | Phase::Direct | Phase::SeedScan | Phase::Certify => {
                    self.advance_candidate();
                    left -= 1;
                }
                Phase::Seeds => {
                    self.advance_seed();
                    left -= 1;
                }
                Phase::Done => break,
            }
        }
        Ok(self.terminal.unwrap_or(TemplateSearchStep::Pending))
    }

    fn advance_gray(&mut self, left: &mut usize) {
        let pixels = self.progress.region.width as usize * self.progress.region.height as usize;
        let count = (*left).min(pixels - self.gray.len());
        for index in self.gray.len()..self.gray.len() + count {
            let x = self.progress.region.x as usize + index % self.width;
            let y = self.progress.region.y as usize + index / self.width;
            let offset = (y * self.frame.width as usize + x) * 4;
            let red = u32::from(self.frame.rgba[offset]);
            let green = u32::from(self.frame.rgba[offset + 1]);
            let blue = u32::from(self.frame.rgba[offset + 2]);
            self.gray
                .push(((4_899 * red + 9_617 * green + 1_868 * blue + 8_192) >> 14) as u8);
        }
        *left -= count;
        if self.gray.len() == pixels {
            self.phase = Phase::Integral;
        }
    }

    fn advance_integral(&mut self, left: &mut usize) {
        let stride = self.width + 1;
        let count = (*left).min(self.integral_cells - self.integral.len());
        for index in self.integral.len()..self.integral.len() + count {
            let x = index % stride;
            let y = index / stride;
            let value = if x == 0 || y == 0 {
                self.row = Moments::default();
                Moments::default()
            } else {
                let pixel = u64::from(self.gray[(y - 1) * self.width + x - 1]);
                self.row.sum += pixel;
                self.row.squared += pixel * pixel;
                let above = self.integral[index - stride];
                Moments {
                    sum: above.sum + self.row.sum,
                    squared: above.squared + self.row.squared,
                }
            };
            self.integral.push(value);
        }
        *left -= count;
        if self.integral.len() == self.integral_cells {
            self.phase = Phase::Probe;
        }
    }

    fn window(&self, x: usize, y: usize, width: usize, height: usize) -> Moments {
        let stride = self.width + 1;
        let a = self.integral[y * stride + x];
        let b = self.integral[y * stride + x + width];
        let c = self.integral[(y + height) * stride + x];
        let d = self.integral[(y + height) * stride + x + width];
        Moments {
            sum: d.sum + a.sum - b.sum - c.sum,
            squared: d.squared + a.squared - b.squared - c.squared,
        }
    }

    fn advance_candidate(&mut self) {
        if self.cursor == self.progress.candidates {
            if self.phase == Phase::SeedScan {
                while let Some(seed) = self.heap.pop() {
                    self.seeds.push(seed);
                }
                self.seeds.sort_unstable_by(|a, b| {
                    b.upper
                        .total_cmp(&a.upper)
                        .then_with(|| a.index.cmp(&b.index))
                });
                self.seed_cursor = 0;
                self.phase = Phase::Seeds;
            } else {
                self.complete(if self.phase == Phase::Certify {
                    TemplateSearchProof::CertifiedBounds
                } else {
                    TemplateSearchProof::Exhaustive
                });
            }
            return;
        }
        if self.phase == Phase::Certify {
            while self.seed_cursor < self.seeds.len()
                && self.seeds[self.seed_cursor].index < self.cursor
            {
                self.seed_cursor += 1;
            }
            if self.seed_cursor < self.seeds.len()
                && self.seeds[self.seed_cursor].index == self.cursor
                && self.seeds[self.seed_cursor].score.is_some()
            {
                self.cursor += 1;
                return;
            }
        }
        let x = self.cursor as usize % self.columns;
        let y = self.cursor as usize / self.columns;
        let moments = self.window(x, y, self.matcher.width, self.matcher.height);
        let variance = self.matcher.gray.len() as u64 * moments.squared - moments.sum * moments.sum;
        if variance == 0 {
            if self.phase != Phase::SeedScan {
                self.progress.verified += 1;
            }
            self.cursor += 1;
            return;
        }
        if matches!(self.phase, Phase::Probe | Phase::Direct) {
            self.start_dot(self.cursor, moments, variance, self.phase);
        } else {
            self.start_bound(self.cursor, moments, variance, self.phase);
        }
    }

    fn affordable(&mut self, cost: u64) -> bool {
        if cost <= self.progress.max_work - self.progress.work {
            true
        } else {
            self.phase = Phase::Done;
            self.terminal = Some(TemplateSearchStep::Incomplete(TemplateSearchIncomplete {
                reason: TemplateSearchIncompleteReason::WorkBudgetExhausted,
                progress: self.progress,
            }));
            false
        }
    }

    fn start_dot(&mut self, index: u64, moments: Moments, variance: u64, next: Phase) {
        // Reserve affordability of the complete kernel without claiming its
        // multiplication work before those chunks have actually executed.
        if !self.affordable(self.matcher.gray.len() as u64) {
            return;
        }
        self.dot = Some(DotJob {
            index,
            moments,
            variance,
            offset: 0,
            sum: 0,
            next,
        });
        self.phase = Phase::Dot;
    }

    fn advance_dot(&mut self, left: &mut usize) {
        let mut dot = self.dot.take().expect("dot phase owns one kernel");
        let target_x = dot.index as usize % self.columns;
        let target_y = dot.index as usize / self.columns;
        while *left > 0 && dot.offset < self.matcher.gray.len() {
            let row = dot.offset / self.matcher.width;
            let column = dot.offset % self.matcher.width;
            let count = (*left).min(self.matcher.width - column);
            let frame_offset = (target_y + row) * self.width + target_x + column;
            let template = &self.matcher.gray[dot.offset..dot.offset + count];
            let frame = &self.gray[frame_offset..frame_offset + count];
            for (&a, &b) in template.iter().zip(frame) {
                dot.sum += u32::from(a) * u32::from(b);
            }
            dot.offset += count;
            *left -= count;
            self.progress.exact_pixel_pairs += count as u64;
            self.progress.work += count as u64;
        }
        if dot.offset != self.matcher.gray.len() {
            self.dot = Some(dot);
            return;
        }
        self.progress.exact_evaluations += 1;
        self.progress.verified += 1;
        let count = self.matcher.gray.len() as u64;
        let numerator =
            i128::from(count * u64::from(dot.sum)) - i128::from(self.matcher.sum * dot.moments.sum);
        let product = u128::from(self.matcher.variance) * u128::from(dot.variance);
        let absolute = numerator.unsigned_abs();
        let perfect = numerator > 0 && absolute * absolute == product;
        let score = if perfect {
            1.0
        } else {
            (numerator as f64 / (product as f64).sqrt()).clamp(-1.0, 1.0)
        };
        if self.best.is_none_or(|best| {
            score > best.score || (score == best.score && dot.index < best.index)
        }) {
            self.best = Some(Best {
                score,
                index: dot.index,
            });
        }
        match dot.next {
            Phase::Probe | Phase::Direct if perfect => {
                self.best = Some(Best {
                    score: 1.0,
                    index: dot.index,
                });
                self.complete(TemplateSearchProof::PerfectCorrelation);
            }
            Phase::Probe => {
                self.cursor = dot.index + 1;
                self.probe_end = self.cursor;
                self.phase = if self.hierarchical {
                    Phase::SeedScan
                } else {
                    Phase::Direct
                };
            }
            Phase::Direct | Phase::Certify => {
                self.cursor = dot.index + 1;
                self.phase = dot.next;
            }
            Phase::Seeds => {
                self.seeds[self.seed_cursor].score = Some(score);
                self.seed_cursor += 1;
                self.phase = Phase::Seeds;
            }
            _ => unreachable!("only exact phases launch kernels"),
        }
    }

    fn start_bound(&mut self, index: u64, moments: Moments, variance: u64, next: Phase) {
        if !self.affordable(BOUND_WORK) {
            return;
        }
        self.progress.bound_queries += 1;
        self.progress.bound_work += BOUND_WORK;
        self.progress.work += BOUND_WORK;
        self.bound = Some(BoundJob {
            index,
            moments,
            variance,
            block: 0,
            projected: 0,
            squared: 0,
            next,
        });
        self.phase = Phase::Bound;
    }

    fn advance_bound(&mut self) {
        let mut bound = self.bound.take().expect("bound phase owns one projection");
        let projection = self
            .matcher
            .projection
            .as_ref()
            .expect("hierarchical dimensions have equal blocks");
        let x = bound.index as usize % self.columns + bound.block % GRID * projection.width;
        let y = bound.index as usize / self.columns + bound.block / GRID * projection.height;
        let sum = self.window(x, y, projection.width, projection.height).sum;
        bound.projected += projection.sums[bound.block] * sum;
        bound.squared += sum * sum;
        bound.block += 1;
        if bound.block < BLOCKS {
            self.bound = Some(bound);
            return;
        }
        // Equal-area orthogonal block projection gives A <= p + sqrt(rT*rF),
        // where A is the exact NCC numerator. Ceil integer sqrt is conservative.
        let count = self.matcher.gray.len() as u64;
        let p = i128::from(BLOCKS as u64 * bound.projected)
            - i128::from(self.matcher.sum * bound.moments.sum);
        let residual = count * bound.moments.squared - BLOCKS as u64 * bound.squared;
        let remainder_product = u128::from(projection.residual) * u128::from(residual);
        let floor = remainder_product.isqrt();
        let ceil = floor + u128::from(floor * floor != remainder_product);
        let upper_numerator = p + ceil as i128;
        let product = u128::from(self.matcher.variance) * u128::from(bound.variance);
        // Use exactly the kernel denominator. Integer->float conversion and
        // division are monotone. Preserve the integer-perfect override as well.
        let upper = if upper_numerator > 0 && upper_numerator.unsigned_abs().pow(2) >= product {
            1.0
        } else {
            (upper_numerator as f64 / (product as f64).sqrt()).clamp(-1.0, 1.0)
        };
        match bound.next {
            Phase::SeedScan => {
                let seed = Seed {
                    index: bound.index,
                    upper,
                    score: None,
                };
                if self.heap.len() < SEEDS {
                    self.heap.push(seed);
                } else if self.heap.peek().is_some_and(|worst| seed < *worst) {
                    *self.heap.peek_mut().expect("full heap has a root") = seed;
                }
                self.cursor = bound.index + 1;
                self.phase = Phase::SeedScan;
            }
            Phase::Certify if self.best.is_some_and(|best| upper < best.score) => {
                self.progress.verified += 1;
                self.cursor = bound.index + 1;
                self.phase = Phase::Certify;
            }
            Phase::Certify => {
                self.start_dot(bound.index, bound.moments, bound.variance, Phase::Certify)
            }
            _ => unreachable!("only projection phases launch queries"),
        }
    }

    fn advance_seed(&mut self) {
        if self.seed_cursor == self.seeds.len() || self.best.is_some_and(|best| best.score == 1.0) {
            self.seeds.sort_unstable_by_key(|seed| seed.index);
            self.seed_cursor = 0;
            self.cursor = self.probe_end;
            self.phase = Phase::Certify;
            return;
        }
        let seed = self.seeds[self.seed_cursor];
        if self.best.is_some_and(|best| seed.upper < best.score) {
            self.seed_cursor += 1;
            return;
        }
        let moments = self.window(
            seed.index as usize % self.columns,
            seed.index as usize / self.columns,
            self.matcher.width,
            self.matcher.height,
        );
        let variance = self.matcher.gray.len() as u64 * moments.squared - moments.sum * moments.sum;
        self.start_dot(seed.index, moments, variance, Phase::Seeds);
    }

    fn complete(&mut self, proof: TemplateSearchProof) {
        let matched = self.best.map_or(
            TemplateMatch {
                score: 0.0,
                bounds: None,
            },
            |best| TemplateMatch {
                score: best.score,
                bounds: (best.score >= self.matcher.options.threshold).then_some(PixelRect {
                    x: self.progress.region.x + (best.index as usize % self.columns) as u32,
                    y: self.progress.region.y + (best.index as usize / self.columns) as u32,
                    width: self.matcher.width as u32,
                    height: self.matcher.height as u32,
                }),
            },
        );
        self.phase = Phase::Done;
        self.terminal = Some(TemplateSearchStep::Complete(TemplateSearchComplete {
            matched,
            progress: self.progress,
            proof,
        }));
    }
}
