//! Shared document and view types.
//!
//! Strokes are stored as the raw input samples that were accepted. Smoothing is
//! a pure function of those samples (see [`crate::input::smooth`]), so files
//! never contain derived geometry and any renderer can reproduce the same curve.

use serde::{Deserialize, Serialize};

/// Fraction of the nib width used at zero pressure.
pub const MIN_PRESSURE_FACTOR: f32 = 0.2;

/// One accepted input sample in page units, with normalized pressure.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    /// Normalized pressure in `0.0..=1.0`.
    pub pressure: f32,
}

/// Axis-aligned rectangle in page units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

impl Rect {
    pub fn new(min: [f64; 2], max: [f64; 2]) -> Self {
        Self { min, max }
    }

    /// Inclusive overlap test; touching edges count as intersecting.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.min[0] <= other.max[0]
            && other.min[0] <= self.max[0]
            && self.min[1] <= other.max[1]
            && other.min[1] <= self.max[1]
    }
}

/// Nib width for a pressure value, scaled between [`MIN_PRESSURE_FACTOR`] and 1.
pub fn nib_width(base: f32, pressure: f32) -> f32 {
    base * (MIN_PRESSURE_FACTOR + (1.0 - MIN_PRESSURE_FACTOR) * pressure.clamp(0.0, 1.0))
}

/// A completed stroke.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    /// Straight (non-premultiplied) RGBA.
    pub color: [u8; 4],
    /// Nib diameter in page units at full pressure.
    pub width: f32,
    /// Accepted raw samples in page coordinates. Must not be empty.
    pub points: Vec<StrokePoint>,
}

impl Stroke {
    /// Bounding box padded by the nib width, so ink is never clipped by culling.
    /// Panics if the stroke has no points.
    pub fn bounds(&self) -> Rect {
        let mut min = [f64::MAX; 2];
        let mut max = [f64::MIN; 2];
        for p in &self.points {
            min[0] = min[0].min(p.x as f64);
            min[1] = min[1].min(p.y as f64);
            max[0] = max[0].max(p.x as f64);
            max[1] = max[1].max(p.y as f64);
        }
        let pad = self.width as f64;
        Rect::new([min[0] - pad, min[1] - pad], [max[0] + pad, max[1] + pad])
    }
}

/// Background pattern drawn behind the strokes across the whole canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pattern {
    #[default]
    Blank,
    Ruled,
    Grid,
    Dot,
}

impl Pattern {
    /// Distance between lines (or dots), in page units. `None` for blank.
    pub fn spacing(self) -> Option<f64> {
        match self {
            Pattern::Blank => None,
            Pattern::Ruled => Some(26.0),
            Pattern::Grid => Some(20.0),
            Pattern::Dot => Some(20.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Pattern::Blank => "Blank",
            Pattern::Ruled => "Ruled",
            Pattern::Grid => "Grid",
            Pattern::Dot => "Dots",
        }
    }

    /// Parses a command-line name such as `ruled`.
    pub fn from_name(name: &str) -> Option<Pattern> {
        match name.to_ascii_lowercase().as_str() {
            "blank" => Some(Pattern::Blank),
            "ruled" => Some(Pattern::Ruled),
            "grid" => Some(Pattern::Grid),
            "dot" | "dots" => Some(Pattern::Dot),
            _ => None,
        }
    }
}

/// An imported page, placed on the canvas in page units. It is a PDF page
/// rendered at 2x, or a PNG or JPEG file used as a template. `data` is the
/// encoded PNG or JPEG bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageImage {
    pub rect: Rect,
    pub data: Vec<u8>,
}

/// A notebook document. Currently a single infinite canvas.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Notebook {
    pub title: String,
    pub strokes: Vec<Stroke>,
    /// Imported pages and image templates, drawn beneath the strokes.
    pub images: Vec<PageImage>,
    /// Background pattern for the whole canvas.
    pub pattern: Pattern,
    /// Guide lines the user placed from the rulers, in points.
    pub guides: Vec<Guide>,
    /// The artboard. `None` is an unbounded canvas with no sheet.
    pub page: Option<Page>,
}

/// A standard US paper size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Paper {
    Letter,
    Legal,
    Tabloid,
}

impl Paper {
    /// Portrait size in points (72 per inch).
    pub fn portrait_size(self) -> (f64, f64) {
        match self {
            Paper::Letter => (612.0, 792.0),
            Paper::Legal => (612.0, 1008.0),
            Paper::Tabloid => (792.0, 1224.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Paper::Letter => "Letter",
            Paper::Legal => "Legal",
            Paper::Tabloid => "Tabloid",
        }
    }

    /// Parses a command-line name such as `letter`.
    pub fn from_name(name: &str) -> Option<Paper> {
        match name.to_ascii_lowercase().as_str() {
            "letter" => Some(Paper::Letter),
            "legal" => Some(Paper::Legal),
            "tabloid" => Some(Paper::Tabloid),
            _ => None,
        }
    }
}

/// The artboard: a sheet of paper in points, starting at the page origin, so the
/// rulers measure from its upper-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub width: f64,
    pub height: f64,
}

impl Page {
    pub fn new(paper: Paper, landscape: bool) -> Page {
        let (w, h) = paper.portrait_size();
        if landscape { Page { width: h, height: w } } else { Page { width: w, height: h } }
    }

    pub fn is_landscape(self) -> bool {
        self.width > self.height
    }

    pub fn rect(self) -> Rect {
        Rect::new([0.0, 0.0], [self.width, self.height])
    }
}

/// A guide line: vertical at `x = pos`, or horizontal at `y = pos`, in page units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub vertical: bool,
    pub pos: f64,
}

/// Pan and zoom state. `pan` is the page coordinate shown at the top-left of
/// the viewport; `zoom` is screen pixels per page unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub pan: [f64; 2],
    pub zoom: f64,
}

impl Camera {
    pub const MIN_ZOOM: f64 = 0.1;
    pub const MAX_ZOOM: f64 = 8.0;

    pub fn screen_to_page(&self, screen: [f64; 2]) -> [f64; 2] {
        [
            screen[0] / self.zoom + self.pan[0],
            screen[1] / self.zoom + self.pan[1],
        ]
    }

    pub fn page_to_screen(&self, page: [f64; 2]) -> [f64; 2] {
        [
            (page[0] - self.pan[0]) * self.zoom,
            (page[1] - self.pan[1]) * self.zoom,
        ]
    }

    /// Moves the view by a screen-space drag delta.
    pub fn pan_by_screen(&mut self, dx: f64, dy: f64) {
        self.pan[0] -= dx / self.zoom;
        self.pan[1] -= dy / self.zoom;
    }

    /// Multiplies zoom by `factor`, keeping the page point under `anchor` fixed.
    pub fn zoom_at(&mut self, factor: f64, anchor: [f64; 2]) {
        let focus = self.screen_to_page(anchor);
        self.zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.pan = [
            focus[0] - anchor[0] / self.zoom,
            focus[1] - anchor[1] / self.zoom,
        ];
    }

    /// Page-space rectangle currently visible in a viewport of `viewport` pixels.
    pub fn visible_rect(&self, viewport: [f64; 2]) -> Rect {
        Rect::new(
            self.pan,
            [
                self.pan[0] + viewport[0] / self.zoom,
                self.pan[1] + viewport[1] / self.zoom,
            ],
        )
    }
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            pan: [0.0, 0.0],
            zoom: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_anchor_point_fixed() {
        let mut cam = Camera {
            pan: [100.0, 50.0],
            ..Camera::default()
        };
        let anchor = [300.0, 200.0];
        let before = cam.screen_to_page(anchor);
        cam.zoom_at(2.0, anchor);
        let after = cam.screen_to_page(anchor);
        assert!((before[0] - after[0]).abs() < 1e-9);
        assert!((before[1] - after[1]).abs() < 1e-9);
        assert_eq!(cam.zoom, 2.0);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut cam = Camera::default();
        cam.zoom_at(1000.0, [0.0, 0.0]);
        assert_eq!(cam.zoom, Camera::MAX_ZOOM);
        cam.zoom_at(1e-9, [0.0, 0.0]);
        assert_eq!(cam.zoom, Camera::MIN_ZOOM);
    }

    #[test]
    fn stroke_bounds_include_nib_padding() {
        let stroke = Stroke {
            color: [0, 0, 0, 255],
            width: 4.0,
            points: vec![
                StrokePoint { x: 10.0, y: 20.0, pressure: 1.0 },
                StrokePoint { x: 30.0, y: 5.0, pressure: 1.0 },
            ],
        };
        let b = stroke.bounds();
        assert_eq!(b.min, [6.0, 1.0]);
        assert_eq!(b.max, [34.0, 24.0]);
    }

    #[test]
    fn nib_width_scales_with_pressure() {
        assert!((nib_width(10.0, 0.0) - 2.0).abs() < 1e-6);
        assert!((nib_width(10.0, 1.0) - 10.0).abs() < 1e-6);
        assert!((nib_width(10.0, 9.0) - 10.0).abs() < 1e-6);
    }
}
