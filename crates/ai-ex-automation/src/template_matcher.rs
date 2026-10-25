#![forbid(unsafe_code)]

use ai_ex_domain::AppError;
use serde::{Deserialize, Serialize};

use crate::ScreenFrame;

// Bounds frame validation and integral-image allocation independently of the
// convolution budget; full-frame captures up to 4096 x 4096 pixels fit.
pub(crate) const MAX_FRAME_PIXELS: u64 = 16_777_216;
pub(crate) const MAX_SEARCH_CANDIDATES: u64 = 16_777_216;

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

    pub(crate) fn clipped_to(self, width: u32, height: u32) -> Result<Self, AppError> {
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
    /// Maximum cumulative exact pixel-pair work plus conservative bound-query work.
    /// Each hierarchical 16-block query costs 48 units; exact kernels charge
    /// only completed pixel-pair multiplications. Constant windows remain free.
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
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) gray: Vec<u8>,
    pub(crate) sum: u64,
    pub(crate) variance: u64,
    pub(crate) options: MatchOptions,
    pub(crate) projection: Option<crate::template_search::BlockProjection>,
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
        let projection = crate::template_search::BlockProjection::new(
            &gray,
            template.width as usize,
            template.height as usize,
            squared_sum,
        );
        Ok(Self {
            width: template.width as usize,
            height: template.height as usize,
            gray,
            sum,
            variance,
            options,
            projection,
        })
    }

    /// Start an incremental exact search over this immutable frame and region.
    pub fn begin_search<'a>(
        &'a self,
        frame: &'a ScreenFrame,
        region: Option<PixelRect>,
    ) -> Result<crate::template_search::TemplateMatchSearch<'a>, AppError> {
        crate::template_search::TemplateMatchSearch::new(self, frame, region)
    }

    /// Compatibility wrapper: only a certified result can escape; an incomplete
    /// search retains the original work-limit error and exposes no guessed hit.
    pub fn find_match(
        &self,
        frame: &ScreenFrame,
        region: Option<PixelRect>,
    ) -> Result<TemplateMatch, AppError> {
        let mut search = self.begin_search(frame, region)?;
        loop {
            match search.advance(65_536)? {
                crate::template_search::TemplateSearchStep::Pending => {}
                crate::template_search::TemplateSearchStep::Complete(complete) => {
                    return Ok(complete.matched);
                }
                crate::template_search::TemplateSearchStep::Incomplete(_) => {
                    return Err(AppError::unavailable(
                        "template matching work limit exhausted",
                    ));
                }
            }
        }
    }
}
pub(crate) fn validate_frame(frame: &ScreenFrame) -> Result<(), AppError> {
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
