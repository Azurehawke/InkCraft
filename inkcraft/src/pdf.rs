//! PDF import and export.
//!
//! * Import renders each PDF page to a PNG at [`RENDER_SCALE`] and places the
//!   pages in a column on the canvas, one page unit per PDF point. Rendering is
//!   done by `hayro`, a pure-Rust PDF rasterizer. Text is rendered as shapes.
//! * Export writes one PDF page sized to the content. Page images are embedded
//!   as images, and strokes are written as vector paths by Skia's PDF backend.

use crate::model::{PageImage, Pattern, Rect, Stroke};
use crate::renderer::{paint_pattern, paint_strokes, DrawStroke};
use hayro::hayro_interpret::hayro_syntax::Pdf;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use skia_safe::{
    pdf, Data, EncodedImageFormat, Image, ImageInfo, AlphaType, ColorType, Paint, Rect as SkRect,
    Size,
};
use std::{fmt, io, path::Path};

/// Pixels per PDF point for imported pages. 2x keeps text sharp on screen.
const RENDER_SCALE: f32 = 2.0;
/// Space between imported pages, in page units.
pub const PAGE_GAP: f64 = 24.0;
/// White margin around exported content, in points.
const EXPORT_MARGIN: f64 = 24.0;

#[derive(Debug)]
pub enum PdfError {
    /// The file could not be parsed as a PDF.
    Load(String),
    /// A page could not be encoded to PNG.
    Encode,
    Io(io::Error),
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfError::Load(msg) => write!(f, "not a readable PDF: {msg}"),
            PdfError::Encode => write!(f, "failed to encode a page as PNG"),
            PdfError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PdfError {}

impl From<io::Error> for PdfError {
    fn from(e: io::Error) -> Self {
        PdfError::Io(e)
    }
}

/// Bounding box of all content, or `None` for an empty notebook.
pub fn content_bounds(strokes: &[Stroke], images: &[PageImage]) -> Option<Rect> {
    let mut min = [f64::MAX; 2];
    let mut max = [f64::MIN; 2];
    let mut any = false;
    for r in strokes.iter().map(Stroke::bounds).chain(images.iter().map(|i| i.rect)) {
        any = true;
        min[0] = min[0].min(r.min[0]);
        min[1] = min[1].min(r.min[1]);
        max[0] = max[0].max(r.max[0]);
        max[1] = max[1].max(r.max[1]);
    }
    any.then(|| Rect::new(min, max))
}

/// Renders every page of a PDF into a column starting at `top_y` on the canvas, left-aligned at x = 0.
/// Returns one [`PageImage`] per page.
pub fn import(data: Vec<u8>, top_y: f64) -> Result<Vec<PageImage>, PdfError> {
    let pdf = Pdf::new(data).map_err(|e| PdfError::Load(format!("{e:?}")))?;
    let cache = RenderCache::new();
    let interpreter = InterpreterSettings::default();
    let render = RenderSettings::default();
    let mut pages = Vec::new();
    let mut y = top_y;
    for page in pdf.pages().iter() {
        let (w, h) = page.render_dimensions();
        let pixmap = hayro::render(
            page,
            &cache,
            &interpreter,
            &render,
            &PixmapSettings {
                x_scale: RENDER_SCALE,
                y_scale: RENDER_SCALE,
                bg_color: WHITE,
            },
        );
        let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
        let info = ImageInfo::new((pw, ph), ColorType::RGBA8888, AlphaType::Premul, None);
        let image = skia_safe::images::raster_from_data(
            &info,
            Data::new_copy(pixmap.data_as_u8_slice()),
            (pw * 4) as usize,
        )
        .ok_or(PdfError::Encode)?;
        let png = image.encode(None, EncodedImageFormat::PNG, 100).ok_or(PdfError::Encode)?;
        let w = w as f64;
        let h = h as f64;
        pages.push(PageImage {
            rect: Rect::new([0.0, y], [w, y + h]),
            data: png.as_bytes().to_vec(),
        });
        y += h + PAGE_GAP;
    }
    Ok(pages)
}

/// Width of an image template on the canvas. Matches A4 in points, so a
/// scanned page or photo lands at the same scale as an imported PDF page.
pub const IMAGE_TEMPLATE_WIDTH: f64 = 595.0;

/// Places a PNG or JPEG file as one template page with its top edge at `top_y`.
/// The file's bytes are stored as they are; the pixels are decoded on demand.
pub fn import_image(data: Vec<u8>, top_y: f64) -> Result<PageImage, PdfError> {
    let decoded = Image::from_encoded(Data::new_copy(&data))
        .ok_or_else(|| PdfError::Load("not a PNG or JPEG image".into()))?;
    let (w, h) = (decoded.width() as f64, decoded.height() as f64);
    if w <= 0.0 || h <= 0.0 {
        return Err(PdfError::Load("the image is empty".into()));
    }
    let height = IMAGE_TEMPLATE_WIDTH * h / w;
    Ok(PageImage {
        rect: Rect::new([0.0, top_y], [IMAGE_TEMPLATE_WIDTH, top_y + height]),
        data,
    })
}

/// Imports a file by its extension: PDF pages, or a PNG or JPEG template.
pub fn import_file(path: &Path, top_y: f64) -> Result<Vec<PageImage>, PdfError> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let bytes = std::fs::read(path)?;
    match ext.as_str() {
        "pdf" => import(bytes, top_y),
        "png" | "jpg" | "jpeg" => Ok(vec![import_image(bytes, top_y)?]),
        _ => Err(PdfError::Load(format!("unsupported file type .{ext}"))),
    }
}

/// Writes the notebook to a PDF at `path`, one PDF page per imported page.
///
/// Strokes are drawn on every page they overlap. Strokes that lie outside all
/// imported pages go on one extra page. A notebook with no imports is one page.
pub fn export(
    path: &Path,
    strokes: &[Stroke],
    images: &[PageImage],
    pattern: Pattern,
) -> Result<(), PdfError> {
    // (region in page units, margin in points)
    let mut regions: Vec<(Rect, f64)> = images.iter().map(|i| (i.rect, 0.0)).collect();
    let loose: Vec<Rect> = strokes
        .iter()
        .map(Stroke::bounds)
        .filter(|b| !images.iter().any(|i| i.rect.intersects(b)))
        .collect();
    if let Some(bounds) = union(&loose) {
        regions.push((bounds, EXPORT_MARGIN));
    }
    if regions.is_empty() {
        regions.push((Rect::new([0.0, 0.0], [612.0, 792.0]), EXPORT_MARGIN));
    }

    let mut buffer: Vec<u8> = Vec::new();
    {
        let mut document = pdf::new_document(&mut buffer, None);
        for (region, margin) in regions {
            let width = (region.max[0] - region.min[0] + 2.0 * margin) as f32;
            let height = (region.max[1] - region.min[1] + 2.0 * margin) as f32;
            let mut page = document.begin_page(Size::new(width, height), None);
            {
                let canvas = page.canvas();
                canvas.translate((
                    (margin - region.min[0]) as f32,
                    (margin - region.min[1]) as f32,
                ));
                paint_pattern(canvas, region, pattern);
                for image in images.iter().filter(|i| i.rect.intersects(&region)) {
                    let Some(decoded) = Image::from_encoded(Data::new_copy(&image.data)) else {
                        continue;
                    };
                    let r = image.rect;
                    let dst = SkRect::from_xywh(
                        r.min[0] as f32,
                        r.min[1] as f32,
                        (r.max[0] - r.min[0]) as f32,
                        (r.max[1] - r.min[1]) as f32,
                    );
                    canvas.draw_image_rect(&decoded, None, dst, &Paint::default());
                }
                let draw: Vec<DrawStroke> = strokes
                    .iter()
                    .filter(|s| s.bounds().intersects(&region))
                    .map(DrawStroke::from_stroke)
                    .collect();
                paint_strokes(canvas, &draw);
            }
            document = page.end_page();
        }
        document.close();
    }
    std::fs::write(path, buffer)?;
    Ok(())
}

/// Smallest rectangle containing all `rects`.
fn union(rects: &[Rect]) -> Option<Rect> {
    let mut min = [f64::MAX; 2];
    let mut max = [f64::MIN; 2];
    for r in rects {
        min[0] = min[0].min(r.min[0]);
        min[1] = min[1].min(r.min[1]);
        max[0] = max[0].max(r.max[0]);
        max[1] = max[1].max(r.max[1]);
    }
    (!rects.is_empty()).then(|| Rect::new(min, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StrokePoint;

    fn line(a: (f32, f32), b: (f32, f32)) -> Stroke {
        Stroke {
            color: [0, 0, 0, 255],
            width: 3.0,
            points: vec![
                StrokePoint { x: a.0, y: a.1, pressure: 1.0 },
                StrokePoint { x: b.0, y: b.1, pressure: 1.0 },
            ],
        }
    }

    #[test]
    fn content_bounds_cover_strokes_and_images() {
        assert!(content_bounds(&[], &[]).is_none());
        let images = vec![PageImage {
            rect: Rect::new([0.0, 100.0], [50.0, 200.0]),
            data: Vec::new(),
        }];
        let b = content_bounds(&[line((10.0, 10.0), (20.0, 20.0))], &images).unwrap();
        assert_eq!(b.min, [0.0, 10.0 - 3.0]);
        assert_eq!(b.max, [50.0, 200.0]);
    }

    #[test]
    fn exported_pdf_imports_back_as_one_page() {
        let dir = std::env::temp_dir().join(format!("inkcraft-pdf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.pdf");
        export(&path, &[line((10.0, 10.0), (200.0, 120.0))], &[], Pattern::Blank).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        let pages = import(bytes, 0.0).unwrap();
        assert_eq!(pages.len(), 1);
        let r = pages[0].rect;
        assert!(r.max[0] - r.min[0] > 190.0, "width was {}", r.max[0] - r.min[0]);
        assert!(r.max[1] - r.min[1] > 100.0, "height was {}", r.max[1] - r.min[1]);
        assert_eq!(&pages[0].data[..4], &[0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn rejects_bytes_that_are_not_a_pdf() {
        assert!(matches!(import(b"hello".to_vec(), 0.0), Err(PdfError::Load(_))));
    }
}
