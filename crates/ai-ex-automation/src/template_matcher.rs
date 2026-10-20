#![forbid(unsafe_code)]

use ai_ex_domain::AppError;
use serde::{Deserialize, Serialize};

use crate::ScreenFrame;

// Bounds frame validation and integral-image allocation independently of the
// convolution budget; full-frame captures up to 4096 x 4096 pixels fit.
const MAX_FRAME_PIXELS: u64 = 16_777_216;
const MAX_SEARCH_CANDIDATES: u64 = 16_777_216;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelPoint {
    pub x: u32,
    pub y: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn center(self) -> PixelPoint {
        PixelPoint {
            x: self.x.saturating_add(self.width / 2),
            y: self.y.saturating_add(self.height / 2),
        }
    }

    fn clipped_to(self, width: u32, height: u32) -> Result<Self, AppError> {
        if self.width == 0 || self.height == 0 || self.x >= width || self.y >= height {
            return Err(AppError::configuration(
                "template search region is empty or outside the frame",
            ));
        }
        Ok(Self {
            x: self.x,
            y: self.y,
            width: self.width.min(width - self.x),
            height: self.height.min(height - self.y),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchOptions {
    pub threshold: f64,
    /// Maximum completed grayscale pixel-pair multiplications per search.
    /// Constant windows require no kernel work and do not consume this budget.
    pub max_work: u64,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self {
            threshold: 0.7,
            max_work: 250_000_000,
        }
    }
}

impl MatchOptions {
    pub fn validate(&self) -> Result<(), AppError> {
        if !self.threshold.is_finite() || !(0.0..=1.0).contains(&self.threshold) {
            return Err(AppError::configuration(
                "template threshold must be finite and between zero and one",
            ));
        }
        if self.max_work == 0 {
            return Err(AppError::configuration(
                "template matching work limit must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TemplateMatch {
    /// The best normalized correlation in [-1, 1]; zero when no nonconstant
    /// candidate exists. A score alone is not evidence of a found target.
    pub score: f64,
    /// Image-local bounds, including the search-region offset, on a threshold hit.
    pub bounds: Option<PixelRect>,
}

#[derive(Debug)]
pub struct TemplateMatcher {
    width: usize,
    height: usize,
    gray: Vec<u8>,
    sum: u64,
    variance: u64,
    options: MatchOptions,
}

impl TemplateMatcher {
    pub fn new(template: &ScreenFrame, options: MatchOptions) -> Result<Self, AppError> {
        options.validate()?;
        validate_frame(template)?;
        if !(20..=200).contains(&template.width) || !(20..=200).contains(&template.height) {
            return Err(AppError::configuration(
                "template dimensions must each be between 20 and 200 pixels",
            ));
        }
        let region = PixelRect {
            x: 0,
            y: 0,
            width: template.width,
            height: template.height,
        };
        let gray = grayscale(template, region)?;
        let sum = gray.iter().map(|pixel| u64::from(*pixel)).sum::<u64>();
        let squared_sum = gray
            .iter()
            .map(|pixel| u64::from(*pixel).pow(2))
            .sum::<u64>();
        let variance = gray.len() as u64 * squared_sum - sum * sum;
        if variance == 0 {
            return Err(AppError::configuration(
                "template must contain nonconstant grayscale detail",
            ));
        }
        Ok(Self {
            width: template.width as usize,
            height: template.height as usize,
            gray,
            sum,
            variance,
            options,
        })
    }

    pub fn find_match(
        &self,
        frame: &ScreenFrame,
        region: Option<PixelRect>,
    ) -> Result<TemplateMatch, AppError> {
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
        if self.width > width || self.height > height {
            return Ok(TemplateMatch {
                score: 0.0,
                bounds: None,
            });
        }
        let columns = width - self.width + 1;
        let rows = height - self.height + 1;
        let candidates = (columns as u64)
            .checked_mul(rows as u64)
            .ok_or_else(|| AppError::configuration("template candidate count overflow"))?;
        if candidates > MAX_SEARCH_CANDIDATES {
            return Err(AppError::configuration(
                "template search contains too many candidate positions",
            ));
        }

        let gray = grayscale(frame, region)?;
        let integral = IntegralImage::new(&gray, width, height)?;
        let pixel_count = self.gray.len() as u64;
        let mut work = 0_u64;
        let mut best_score = -1.0_f64;
        let mut best_bounds = None;

        for y in 0..rows {
            for x in 0..columns {
                let moments = integral.window(x, y, self.width, self.height);
                let variance = pixel_count * moments.squared_sum - moments.sum * moments.sum;
                if variance == 0 {
                    continue;
                }

                // Charge only this actual kernel, after the constant-window
                // shortcut. A partial search must never return a guessed best hit.
                work = work
                    .checked_add(pixel_count)
                    .filter(|next| *next <= self.options.max_work)
                    .ok_or_else(|| {
                        AppError::unavailable("template matching work limit exhausted")
                    })?;
                let dot = self.dot(&gray, width, x, y);
                let numerator = i128::from(pixel_count * dot) - i128::from(self.sum * moments.sum);
                let variance_product = u128::from(self.variance) * u128::from(variance);
                let absolute_numerator = numerator.unsigned_abs();
                let perfect =
                    numerator > 0 && absolute_numerator * absolute_numerator == variance_product;
                let score = if perfect {
                    1.0
                } else {
                    (numerator as f64 / (variance_product as f64).sqrt()).clamp(-1.0, 1.0)
                };
                let bounds = PixelRect {
                    x: region.x + x as u32,
                    y: region.y + y as u32,
                    width: self.width as u32,
                    height: self.height as u32,
                };
                if best_bounds.is_none() || score > best_score {
                    best_score = score;
                    best_bounds = Some(bounds);
                }
                // Integer Cauchy equality proves that later candidates cannot
                // improve this result. Row-major traversal preserves the first tie.
                if perfect {
                    return Ok(TemplateMatch {
                        score: 1.0,
                        bounds: Some(bounds),
                    });
                }
            }
        }

        Ok(TemplateMatch {
            score: if best_bounds.is_some() {
                best_score
            } else {
                0.0
            },
            bounds: if best_score >= self.options.threshold {
                best_bounds
            } else {
                None
            },
        })
    }

    fn dot(&self, gray: &[u8], stride: usize, x: usize, y: usize) -> u64 {
        let mut dot = 0_u64;
        for row in 0..self.height {
            let template_offset = row * self.width;
            let frame_offset = (y + row) * stride + x;
            let template_row = &self.gray[template_offset..template_offset + self.width];
            let frame_row = &gray[frame_offset..frame_offset + self.width];
            for (&template_pixel, &frame_pixel) in template_row.iter().zip(frame_row) {
                dot += u64::from(template_pixel) * u64::from(frame_pixel);
            }
        }
        dot
    }
}

fn validate_frame(frame: &ScreenFrame) -> Result<(), AppError> {
    let pixels = u64::from(frame.width) * u64::from(frame.height);
    if frame.width == 0 || frame.height == 0 || pixels > MAX_FRAME_PIXELS {
        return Err(AppError::protocol(
            "template frame must contain 1 to 16777216 pixels",
        ));
    }
    let bytes = usize::try_from(pixels)
        .ok()
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| AppError::protocol("template frame byte size overflow"))?;
    if frame.rgba.len() != bytes {
        return Err(AppError::protocol("template frame RGBA size is invalid"));
    }
    Ok(())
}

fn grayscale(frame: &ScreenFrame, region: PixelRect) -> Result<Vec<u8>, AppError> {
    let capacity = region.width as usize * region.height as usize;
    let mut gray = Vec::new();
    gray.try_reserve_exact(capacity)
        .map_err(|_| AppError::unavailable("cannot allocate template grayscale image"))?;
    let stride = frame.width as usize;
    for y in region.y as usize..(region.y + region.height) as usize {
        for x in region.x as usize..(region.x + region.width) as usize {
            let offset = (y * stride + x) * 4;
            // OpenCV's 14-bit integer RGB luminance coefficients. Alpha is
            // ignored, matching the old default three-channel template load.
            let red = u32::from(frame.rgba[offset]);
            let green = u32::from(frame.rgba[offset + 1]);
            let blue = u32::from(frame.rgba[offset + 2]);
            gray.push(((4_899 * red + 9_617 * green + 1_868 * blue + 8_192) >> 14) as u8);
        }
    }
    Ok(gray)
}

#[derive(Debug, Clone, Copy, Default)]
struct Moments {
    sum: u64,
    squared_sum: u64,
}

struct IntegralImage {
    stride: usize,
    moments: Vec<Moments>,
}

impl IntegralImage {
    fn new(gray: &[u8], width: usize, height: usize) -> Result<Self, AppError> {
        let stride = width
            .checked_add(1)
            .ok_or_else(|| AppError::protocol("template integral width overflow"))?;
        let count = height
            .checked_add(1)
            .and_then(|height| stride.checked_mul(height))
            .ok_or_else(|| AppError::protocol("template integral image size overflow"))?;
        let mut moments = Vec::new();
        moments
            .try_reserve_exact(count)
            .map_err(|_| AppError::unavailable("cannot allocate template integral image"))?;
        moments.resize(count, Moments::default());
        for y in 0..height {
            let mut row = Moments::default();
            for x in 0..width {
                let pixel = u64::from(gray[y * width + x]);
                row.sum += pixel;
                row.squared_sum += pixel * pixel;
                let above = moments[y * stride + x + 1];
                moments[(y + 1) * stride + x + 1] = Moments {
                    sum: above.sum + row.sum,
                    squared_sum: above.squared_sum + row.squared_sum,
                };
            }
        }
        Ok(Self { stride, moments })
    }

    fn window(&self, x: usize, y: usize, width: usize, height: usize) -> Moments {
        let top_left = self.moments[y * self.stride + x];
        let top_right = self.moments[y * self.stride + x + width];
        let bottom_left = self.moments[(y + height) * self.stride + x];
        let bottom_right = self.moments[(y + height) * self.stride + x + width];
        Moments {
            sum: bottom_right.sum + top_left.sum - top_right.sum - bottom_left.sum,
            squared_sum: bottom_right.squared_sum + top_left.squared_sum
                - top_right.squared_sum
                - bottom_left.squared_sum,
        }
    }
}
