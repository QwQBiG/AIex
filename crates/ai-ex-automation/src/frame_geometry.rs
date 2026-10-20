#![forbid(unsafe_code)]

use ai_ex_domain::AppError;
use serde::{Deserialize, Serialize};

use crate::template_matcher::PixelPoint;

/// Physical desktop pixels; the origin can be negative on a multi-monitor desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopBounds {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl DesktopBounds {
    pub fn new(left: i32, top: i32, width: u32, height: u32) -> Result<Self, AppError> {
        let bounds = Self {
            left,
            top,
            width,
            height,
        };
        bounds.validate()?;
        Ok(bounds)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.width == 0 || self.height == 0 {
            return Err(AppError::configuration(
                "desktop bounds require positive dimensions",
            ));
        }
        Ok(())
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        let left = f64::from(self.left);
        let top = f64::from(self.top);
        x >= left
            && y >= top
            && x <= left + f64::from(self.width - 1)
            && y <= top + f64::from(self.height - 1)
    }
}

/// The capture origin is in physical desktop pixels. Scales express physical
/// desktop pixels per image pixel, so a downsampled capture uses a scale above 1.
/// Matching ROIs remain image-local and must not be added to this origin twice.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrameGeometry {
    pub origin_x: i32,
    pub origin_y: i32,
    pub scale_x: f64,
    pub scale_y: f64,
    pub desktop: DesktopBounds,
}

impl FrameGeometry {
    pub fn new(
        origin_x: i32,
        origin_y: i32,
        scale_x: f64,
        scale_y: f64,
        desktop: DesktopBounds,
    ) -> Result<Self, AppError> {
        let geometry = Self {
            origin_x,
            origin_y,
            scale_x,
            scale_y,
            desktop,
        };
        geometry.validate()?;
        Ok(geometry)
    }

    pub fn validate(&self) -> Result<(), AppError> {
        self.desktop.validate()?;
        if !self.scale_x.is_finite()
            || !self.scale_y.is_finite()
            || self.scale_x <= 0.0
            || self.scale_y <= 0.0
        {
            return Err(AppError::configuration(
                "frame coordinate scales must be positive and finite",
            ));
        }
        Ok(())
    }

    /// Map an image-local point exactly once and reject desktop overflow.
    /// Validation also runs for values constructed directly or deserialized.
    pub fn absolute(&self, point: PixelPoint) -> Result<(f64, f64), AppError> {
        self.validate()?;
        let x = f64::from(self.origin_x) + f64::from(point.x) * self.scale_x;
        let y = f64::from(self.origin_y) + f64::from(point.y) * self.scale_y;
        if !x.is_finite() || !y.is_finite() || !self.desktop.contains(x, y) {
            return Err(AppError::configuration(
                "matched coordinates are outside desktop bounds",
            ));
        }
        Ok((x, y))
    }

    /// Normalize to the existing AutomationAction::MovePointer domain [0, 1].
    /// A single-pixel axis has one valid coordinate, represented by zero.
    pub fn normalize(&self, point: PixelPoint) -> Result<(f64, f64), AppError> {
        let (x, y) = self.absolute(point)?;
        let normalized_x = if self.desktop.width == 1 {
            0.0
        } else {
            (x - f64::from(self.desktop.left)) / f64::from(self.desktop.width - 1)
        };
        let normalized_y = if self.desktop.height == 1 {
            0.0
        } else {
            (y - f64::from(self.desktop.top)) / f64::from(self.desktop.height - 1)
        };
        Ok((normalized_x, normalized_y))
    }
}
