//! Rendering pipeline.
//!
//! * **Overlay:** strokes that are not yet in the tile cache (in progress, or
//!   committed within the last few frames) are painted with Skia into a
//!   viewport-sized raster and composited on top.
//! * **Tile cache:** settled ink is rasterized in page-space tiles on a
//!   background worker and uploaded to `wgpu` textures. Frames draw only the
//!   tiles that intersect the viewport.
//!
//! Skia rasterizes on the CPU here. Sharing a `wgpu` device with `skia-safe`'s
//! GPU backends requires reaching into raw Vulkan/Metal/GL handles, which is
//! not done in this skeleton.

use crate::input::smooth;
use crate::model::{nib_width, Camera, PageImage, Pattern, Rect, Stroke, StrokePoint};
use crate::spatial::StrokeIndex;
use crossbeam_channel::{unbounded, Receiver, Sender};
use skia_safe::{
    surfaces, AlphaType, Canvas, Color, ColorType, ImageInfo, Paint, PaintCap, PaintJoin,
    PaintStyle, Surface,
};
use std::{collections::HashMap, error::Error, sync::Arc, thread::{self, JoinHandle}};
use wgpu::util::DeviceExt;
use winit::{event_loop::OwnedDisplayHandle, window::Window};

/// Tile edge length in page units (and in texture pixels at zoom 1).
pub const TILE_SIZE: u32 = 512;
const TILE_BYTES: usize = (TILE_SIZE * TILE_SIZE * 4) as usize;
const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Workspace surround, `#e9eaec` (Print Me Checks' home background).
const SURROUND: wgpu::Color = wgpu::Color {
    r: 233.0 / 255.0,
    g: 234.0 / 255.0,
    b: 236.0 / 255.0,
    a: 1.0,
};

const BLIT_WGSL: &str = r#"
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@group(0) @binding(0) var layer_texture: texture_2d<f32>;
@group(0) @binding(1) var layer_sampler: sampler;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(layer_texture, layer_sampler, in.uv);
}
"#;

/// Integer coordinates of a tile in the page grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub x: i32,
    pub y: i32,
}

impl TileKey {
    pub fn origin(self) -> [f64; 2] {
        let size = TILE_SIZE as f64;
        [self.x as f64 * size, self.y as f64 * size]
    }

    pub fn rect(self) -> Rect {
        let o = self.origin();
        let size = TILE_SIZE as f64;
        Rect::new(o, [o[0] + size, o[1] + size])
    }

    /// Every tile key whose area overlaps `rect`.
    pub fn covering(rect: Rect) -> impl Iterator<Item = TileKey> {
        let size = TILE_SIZE as f64;
        let x0 = (rect.min[0] / size).floor() as i32;
        let y0 = (rect.min[1] / size).floor() as i32;
        let x1 = (rect.max[0] / size).floor() as i32;
        let y1 = (rect.max[1] / size).floor() as i32;
        (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| TileKey { x, y }))
    }
}

/// A stroke ready for Skia: smoothed polyline plus base nib width.
pub struct DrawStroke {
    pub color: [u8; 4],
    pub width: f32,
    pub points: Vec<StrokePoint>,
}

impl DrawStroke {
    pub fn from_stroke(stroke: &Stroke) -> Self {
        Self {
            color: stroke.color,
            width: stroke.width,
            points: smooth(&stroke.points),
        }
    }
}

/// Paints strokes in page coordinates. Shared by tiles, overlay, and PDF export.
pub fn paint_strokes(canvas: &Canvas, strokes: &[DrawStroke]) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_stroke_cap(PaintCap::Round);
    paint.set_stroke_join(PaintJoin::Round);
    for stroke in strokes {
        let [r, g, b, a] = stroke.color;
        // Segments are drawn one by one, so overlapping round caps would stack
        // their alpha. A translucent stroke is drawn opaque into its own layer
        // and composited once at its alpha instead.
        let layered = a < 255;
        if layered {
            canvas.save_layer_alpha(None, a as u32);
        }
        paint.set_color(Color::from_argb(255, r, g, b));
        match stroke.points.as_slice() {
            [] => {}
            [only] => {
                paint.set_style(PaintStyle::Fill);
                let radius = nib_width(stroke.width, only.pressure) / 2.0;
                canvas.draw_circle((only.x, only.y), radius, &paint);
            }
            points => {
                paint.set_style(PaintStyle::Stroke);
                for pair in points.windows(2) {
                    let (a_pt, b_pt) = (pair[0], pair[1]);
                    let pressure = (a_pt.pressure + b_pt.pressure) * 0.5;
                    paint.set_stroke_width(nib_width(stroke.width, pressure));
                    canvas.draw_line((a_pt.x, a_pt.y), (b_pt.x, b_pt.y), &paint);
                }
            }
        }
        if layered {
            canvas.restore();
        }
    }
}

fn tile_image_info() -> ImageInfo {
    ImageInfo::new(
        (TILE_SIZE as i32, TILE_SIZE as i32),
        ColorType::RGBA8888,
        AlphaType::Premul,
        None,
    )
}

/// Rasterizes the strokes that fall within the tile at `origin`.
/// Returns premultiplied RGBA8 pixels, row-major, top row first.
/// Paints a background pattern over `region` in page coordinates. Lines and
/// dots are light, so strokes stay readable on top.
pub fn paint_pattern(canvas: &Canvas, region: Rect, pattern: Pattern) {
    let Some(spacing) = pattern.spacing() else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    let (x0, x1) = (region.min[0], region.max[0]);
    let (y0, y1) = (region.min[1], region.max[1]);
    let first_col = (x0 / spacing).floor() * spacing;
    let first_row = (y0 / spacing).floor() * spacing;
    match pattern {
        Pattern::Blank => {}
        Pattern::Ruled | Pattern::Grid => {
            paint.set_color(Color::from_argb(255, 0xa7, 0xc7, 0xe7));
            if pattern == Pattern::Grid {
                paint.set_color(Color::from_argb(255, 0xd4, 0xd4, 0xd4));
            }
            paint.set_style(PaintStyle::Stroke);
            paint.set_stroke_width(1.0);
            let mut y = first_row;
            while y <= y1 {
                canvas.draw_line((x0 as f32, y as f32), (x1 as f32, y as f32), &paint);
                y += spacing;
            }
            if pattern == Pattern::Grid {
                let mut x = first_col;
                while x <= x1 {
                    canvas.draw_line((x as f32, y0 as f32), (x as f32, y1 as f32), &paint);
                    x += spacing;
                }
            }
        }
        Pattern::Dot => {
            paint.set_color(Color::from_argb(255, 0x9a, 0x9a, 0x9a));
            paint.set_style(PaintStyle::Fill);
            let mut y = first_row;
            while y <= y1 {
                let mut x = first_col;
                while x <= x1 {
                    canvas.draw_circle((x as f32, y as f32), 1.2, &paint);
                    x += spacing;
                }
                y += spacing;
            }
        }
    }
}

pub fn rasterize_tile(
    origin: [f64; 2],
    strokes: &[DrawStroke],
    images: &[Arc<PageImage>],
    pattern: Pattern,
) -> Option<Vec<u8>> {
    let info = tile_image_info();
    let mut surface = surfaces::raster(&info, None, None)?;
    {
        let canvas = surface.canvas();
        canvas.clear(Color::TRANSPARENT);
        canvas.translate((-origin[0] as f32, -origin[1] as f32));
        let region = Rect::new(origin, [origin[0] + TILE_SIZE as f64, origin[1] + TILE_SIZE as f64]);
        paint_pattern(canvas, region, pattern);
        for image in images {
            if let Some(decoded) = skia_safe::Image::from_encoded(skia_safe::Data::new_copy(&image.data)) {
                let r = image.rect;
                let dst = skia_safe::Rect::from_xywh(
                    r.min[0] as f32,
                    r.min[1] as f32,
                    (r.max[0] - r.min[0]) as f32,
                    (r.max[1] - r.min[1]) as f32,
                );
                canvas.draw_image_rect(&decoded, None, dst, &Paint::default());
            }
        }
        paint_strokes(canvas, strokes);
    }
    let mut pixels = vec![0u8; TILE_BYTES];
    let row_bytes = (TILE_SIZE * 4) as usize;
    surface
        .read_pixels(&info, &mut pixels, row_bytes, (0, 0))
        .then_some(pixels)
}

/// Reusable viewport-sized raster for in-progress strokes.
#[derive(Default)]
pub struct OverlayPainter {
    size: (u32, u32),
    surface: Option<Surface>,
    pixels: Vec<u8>,
}

impl OverlayPainter {
    /// Paints `strokes` through `camera` into a `size` raster and returns its pixels.
    pub fn paint(&mut self, camera: Camera, size: (u32, u32), strokes: &[DrawStroke]) -> &[u8] {
        let (w, h) = (size.0.max(1), size.1.max(1));
        if self.size != (w, h) {
            let info = ImageInfo::new(
                (w as i32, h as i32),
                ColorType::RGBA8888,
                AlphaType::Premul,
                None,
            );
            self.surface = surfaces::raster(&info, None, None);
            self.pixels = vec![0u8; (w * h * 4) as usize];
            self.size = (w, h);
        }
        if let Some(surface) = self.surface.as_mut() {
            let info = ImageInfo::new(
                (w as i32, h as i32),
                ColorType::RGBA8888,
                AlphaType::Premul,
                None,
            );
            {
                let canvas = surface.canvas();
                canvas.clear(Color::TRANSPARENT);
                canvas.scale((camera.zoom as f32, camera.zoom as f32));
                canvas.translate((-camera.pan[0] as f32, -camera.pan[1] as f32));
                paint_strokes(canvas, strokes);
            }
            surface.read_pixels(&info, &mut self.pixels, (w * 4) as usize, (0, 0));
        }
        &self.pixels
    }
}

struct RasterJob {
    key: TileKey,
    seq: u64,
    strokes: Vec<Stroke>,
    images: Vec<Arc<PageImage>>,
    pattern: Pattern,
}

struct RasterResult {
    key: TileKey,
    seq: u64,
    pixels: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
struct TileState {
    /// Newest commit sequence that touches this tile.
    requested_seq: u64,
    /// Commit sequence the uploaded texture reflects.
    cached_seq: u64,
    dirty: bool,
    in_flight: bool,
}

/// Decides which tiles need re-rasterizing and hands jobs to the worker thread.
pub struct TileScheduler {
    states: HashMap<TileKey, TileState>,
    jobs: Sender<RasterJob>,
    results: Receiver<RasterResult>,
    _worker: JoinHandle<()>,
}

impl Default for TileScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl TileScheduler {
    pub fn new() -> Self {
        let (job_tx, job_rx) = unbounded::<RasterJob>();
        let (result_tx, result_rx) = unbounded::<RasterResult>();
        let worker = thread::Builder::new()
            .name("inkcraft-raster".into())
            .spawn(move || {
                for job in job_rx {
                    let draw: Vec<DrawStroke> = job.strokes.iter().map(DrawStroke::from_stroke).collect();
                    let pixels = rasterize_tile(job.key.origin(), &draw, &job.images, job.pattern).unwrap_or_else(|| {
                        eprintln!("skia failed to rasterize tile {:?}", job.key);
                        vec![0u8; TILE_BYTES]
                    });
                    if result_tx
                        .send(RasterResult {
                            key: job.key,
                            seq: job.seq,
                            pixels,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("failed to spawn raster thread");
        Self {
            states: HashMap::new(),
            jobs: job_tx,
            results: result_rx,
            _worker: worker,
        }
    }

    /// Marks every known tile for re-rasterizing, e.g. after a pattern change.
    pub fn mark_all_dirty(&mut self, seq: u64) {
        for state in self.states.values_mut() {
            state.requested_seq = state.requested_seq.max(seq);
            state.dirty = true;
        }
    }

    /// Requests a tile that has never been rasterized, so empty regions show a pattern.
    pub fn ensure(&mut self, key: TileKey, seq: u64) {
        let state = self.states.entry(key).or_default();
        if state.cached_seq == 0 && !state.dirty && !state.in_flight {
            state.requested_seq = state.requested_seq.max(seq);
            state.dirty = true;
        }
    }

    /// Records that the tile now needs re-rasterizing for commit `seq`.
    pub fn mark_dirty(&mut self, key: TileKey, seq: u64) {
        let state = self.states.entry(key).or_default();
        state.requested_seq = state.requested_seq.max(seq);
        state.dirty = true;
    }

    /// Commit sequence the tile's texture reflects (0 if never rasterized).
    pub fn cached_seq(&self, key: TileKey) -> u64 {
        self.states.get(&key).map_or(0, |s| s.cached_seq)
    }

    /// True while any tile is waiting for a job or has one running.
    pub fn is_busy(&self) -> bool {
        self.states.values().any(|s| s.dirty || s.in_flight)
    }

    pub fn needs_dispatch(&self) -> bool {
        self.states.values().any(|s| s.dirty && !s.in_flight)
    }

    /// Sends a job for every dirty tile that has no job in flight.
    pub fn dispatch(
        &mut self,
        index: &StrokeIndex,
        strokes: &[Stroke],
        images: &[Arc<PageImage>],
        pattern: Pattern,
    ) {
        for (&key, state) in self.states.iter_mut() {
            if !state.dirty || state.in_flight {
                continue;
            }
            let batch: Vec<Stroke> = index
                .query(key.rect())
                .map(|id| strokes[id].clone())
                .collect();
            let page_images: Vec<Arc<PageImage>> = images
                .iter()
                .filter(|image| image.rect.intersects(&key.rect()))
                .cloned()
                .collect();
            let job = RasterJob {
                key,
                seq: state.requested_seq,
                strokes: batch,
                images: page_images,
                pattern,
            };
            if self.jobs.send(job).is_ok() {
                state.dirty = false;
                state.in_flight = true;
            }
        }
    }

    /// Collects finished tiles, returning `(key, pixels)` pairs ready for upload.
    pub fn poll(&mut self) -> Vec<(TileKey, Vec<u8>)> {
        let mut done = Vec::new();
        while let Ok(result) = self.results.try_recv() {
            if let Some(state) = self.states.get_mut(&result.key) {
                state.in_flight = false;
                state.cached_seq = state.cached_seq.max(result.seq);
            }
            done.push((result.key, result.pixels));
        }
        done
    }
}

struct TileTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

#[derive(Clone, Copy)]
enum Layer {
    Paper,
    Tile(TileKey),
    Overlay,
    Chrome,
}

struct Draw {
    layer: Layer,
    first: u32,
    scissor: [u32; 4],
}

/// Inputs for one frame. See [`Renderer::render`].
pub struct FrameInput<'a> {
    pub camera: Camera,
    /// Canvas rectangle in window pixels: `[x, y, width, height]`.
    pub canvas: [u32; 4],
    /// Premultiplied RGBA8, canvas-sized. `None` hides the overlay this frame.
    pub overlay: Option<&'a [u8]>,
    /// Premultiplied RGBA8, window-sized. `None` keeps the last uploaded chrome.
    pub chrome: Option<&'a [u8]>,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    tiles: HashMap<TileKey, TileTexture>,
    overlay: Option<TileTexture>,
    overlay_size: (u32, u32),
    chrome_layer: Option<TileTexture>,
    chrome_size: (u32, u32),
    paper: Option<TileTexture>,
}

impl Renderer {
    pub fn new(display: OwnedDisplayHandle, window: Arc<Window>) -> Result<Self, Box<dyn Error>> {
        // `_from_env` lets WGPU_BACKEND pick a backend (dx12, vulkan, gl) for testing.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
            Box::new(display),
        ));
        crate::storage::trace("renderer: instance created");
        let surface = instance.create_surface(window.clone())?;
        crate::storage::trace("renderer: surface created");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))?;
        crate::storage::trace("renderer: adapter selected");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        crate::storage::trace("renderer: device created");

        let caps = surface.get_capabilities(&adapter);
        // A non-sRGB format keeps sampled colors unchanged on output.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("inkcraft-blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("inkcraft-layer-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("inkcraft-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("inkcraft-blit-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Tiles and overlay are premultiplied, so blend them that way.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("inkcraft-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let mut renderer = Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_layout,
            sampler,
            tiles: HashMap::new(),
            overlay: None,
            overlay_size: (0, 0),
            chrome_layer: None,
            chrome_size: (0, 0),
            paper: None,
        };
        // A 1x1 white texture stands in for the page background.
        let paper = renderer.create_layer(1, 1, "inkcraft-paper");
        renderer.write_layer(&paper, 1, 1, &[255, 255, 255, 255]);
        renderer.paper = Some(paper);
        Ok(renderer)
    }

    pub fn viewport(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn create_layer(&self, width: u32, height: u32, label: &str) -> TileTexture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        TileTexture {
            texture,
            bind_group,
        }
    }

    fn write_layer(&self, layer: &TileTexture, width: u32, height: u32, pixels: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &layer.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Creates or replaces the GPU texture for a tile.
    pub fn upload_tile(&mut self, key: TileKey, pixels: &[u8]) {
        if !self.tiles.contains_key(&key) {
            let tile = self.create_layer(TILE_SIZE, TILE_SIZE, "inkcraft-tile");
            self.tiles.insert(key, tile);
        }
        if let Some(tile) = self.tiles.get(&key) {
            self.write_layer(tile, TILE_SIZE, TILE_SIZE, pixels);
        }
    }

    /// Draws the canvas (paper, cached tiles, and the stroke overlay), clipped to
    /// `frame.canvas`, then the chrome layer over the whole window.
    pub fn render(&mut self, frame: FrameInput<'_>) -> Result<(), Box<dyn Error>> {
        let (vw, vh) = self.viewport();
        let [cx, cy, cw, ch] = frame.canvas;

        if let (Some(pixels), true) = (frame.overlay, cw > 0 && ch > 0) {
            if self.overlay.is_none() || self.overlay_size != (cw, ch) {
                self.overlay = Some(self.create_layer(cw, ch, "inkcraft-overlay"));
                self.overlay_size = (cw, ch);
            }
            if let Some(layer) = &self.overlay {
                self.write_layer(layer, cw, ch, pixels);
            }
        }
        if let Some(pixels) = frame.chrome {
            if self.chrome_layer.is_none() || self.chrome_size != (vw, vh) {
                self.chrome_layer = Some(self.create_layer(vw, vh, "inkcraft-chrome"));
                self.chrome_size = (vw, vh);
            }
            if let Some(layer) = &self.chrome_layer {
                self.write_layer(layer, vw, vh, pixels);
            }
        }

        let (w, h) = (vw as f64, vh as f64);
        let to_ndc = |p: [f64; 2]| [(p[0] / w * 2.0 - 1.0) as f32, (1.0 - p[1] / h * 2.0) as f32];
        let origin = [cx as f64, cy as f64];
        let canvas_scissor = [cx, cy, cw, ch];
        let mut vertices: Vec<f32> = Vec::new();
        let mut draws: Vec<Draw> = Vec::new();

        if cw > 0 && ch > 0 {
            let far = [origin[0] + cw as f64, origin[1] + ch as f64];
            draws.push(Draw {
                layer: Layer::Paper,
                first: quad_index(&vertices),
                scissor: canvas_scissor,
            });
            push_quad(&mut vertices, origin, far, &to_ndc);

            let camera = frame.camera;
            let visible = camera.visible_rect([cw as f64, ch as f64]);
            for key in TileKey::covering(visible) {
                if !self.tiles.contains_key(&key) {
                    continue;
                }
                let top_left = offset(origin, camera.page_to_screen(key.origin()));
                let bottom_right = offset(origin, camera.page_to_screen(key.rect().max));
                draws.push(Draw {
                    layer: Layer::Tile(key),
                    first: quad_index(&vertices),
                    scissor: canvas_scissor,
                });
                push_quad(&mut vertices, top_left, bottom_right, &to_ndc);
            }
            if frame.overlay.is_some() && self.overlay.is_some() {
                draws.push(Draw {
                    layer: Layer::Overlay,
                    first: quad_index(&vertices),
                    scissor: canvas_scissor,
                });
                push_quad(&mut vertices, origin, far, &to_ndc);
            }
        }
        if self.chrome_layer.is_some() {
            draws.push(Draw {
                layer: Layer::Chrome,
                first: quad_index(&vertices),
                scissor: [0, 0, vw, vh],
            });
            push_quad(&mut vertices, [0.0, 0.0], [w, h], &to_ndc);
        }

        let frame_tex = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(()),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            other => return Err(format!("surface unavailable: {other:?}").into()),
        };

        let vertex_bytes: Vec<u8> = vertices.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let vertex_buffer = (!vertex_bytes.is_empty()).then(|| {
            self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("inkcraft-quads"),
                contents: &vertex_bytes,
                usage: wgpu::BufferUsages::VERTEX,
            })
        });

        let view = frame_tex.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("inkcraft-frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("inkcraft-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(SURROUND),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(buffer) = &vertex_buffer {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                for draw in &draws {
                    let bind_group = match draw.layer {
                        Layer::Paper => self.paper.as_ref().map(|t| &t.bind_group),
                        Layer::Tile(key) => self.tiles.get(&key).map(|t| &t.bind_group),
                        Layer::Overlay => self.overlay.as_ref().map(|t| &t.bind_group),
                        Layer::Chrome => self.chrome_layer.as_ref().map(|t| &t.bind_group),
                    };
                    if let Some(bind_group) = bind_group {
                        let [sx, sy, sw, sh] = draw.scissor;
                        pass.set_scissor_rect(sx, sy, sw, sh);
                        pass.set_bind_group(0, bind_group, &[]);
                        pass.draw(draw.first..draw.first + 6, 0..1);
                    }
                }
            }
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame_tex);
        Ok(())
    }
}

fn quad_index(vertices: &[f32]) -> u32 {
    (vertices.len() / 4) as u32
}

fn offset(origin: [f64; 2], p: [f64; 2]) -> [f64; 2] {
    [origin[0] + p[0], origin[1] + p[1]]
}

/// Appends two triangles covering the screen-space rectangle `top_left..bottom_right`.
/// Each vertex is `[x, y, u, v]` (NDC position, texture coordinate).
fn push_quad(out: &mut Vec<f32>, top_left: [f64; 2], bottom_right: [f64; 2], to_ndc: &impl Fn([f64; 2]) -> [f32; 2]) {
    let tl = to_ndc(top_left);
    let tr = to_ndc([bottom_right[0], top_left[1]]);
    let bl = to_ndc([top_left[0], bottom_right[1]]);
    let br = to_ndc(bottom_right);
    let corners = [
        (tl, [0.0, 0.0]),
        (tr, [1.0, 0.0]),
        (bl, [0.0, 1.0]),
        (tr, [1.0, 0.0]),
        (br, [1.0, 1.0]),
        (bl, [0.0, 1.0]),
    ];
    for (pos, uv) in corners {
        out.extend_from_slice(&[pos[0], pos[1], uv[0], uv[1]]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StrokePoint;

    fn red_line(from: (f32, f32), to: (f32, f32)) -> DrawStroke {
        DrawStroke {
            color: [255, 0, 0, 255],
            width: 8.0,
            points: vec![
                StrokePoint { x: from.0, y: from.1, pressure: 1.0 },
                StrokePoint { x: to.0, y: to.1, pressure: 1.0 },
            ],
        }
    }

    fn alpha_at(pixels: &[u8], x: u32, y: u32) -> u8 {
        pixels[((y * TILE_SIZE + x) * 4 + 3) as usize]
    }

    #[test]
    fn tile_rasterization_paints_inside_tile_only() {
        // A line at page (600, 100)..(700, 100) lies in tile (1, 0), which starts at x=512.
        let origin = TileKey { x: 1, y: 0 }.origin();
        let pixels = rasterize_tile(origin, &[red_line((600.0, 100.0), (700.0, 100.0))], &[], Pattern::Blank).unwrap();
        assert_eq!(pixels.len(), TILE_BYTES);
        assert_eq!(alpha_at(&pixels, 100, 100), 255, "ink on the line");
        assert_eq!(alpha_at(&pixels, 10, 10), 0, "far from the line");
    }

    #[test]
    fn covering_yields_all_overlapping_tiles() {
        let keys: Vec<_> = TileKey::covering(Rect::new([500.0, 0.0], [1100.0, 10.0])).collect();
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&TileKey { x: 0, y: 0 }));
        assert!(keys.contains(&TileKey { x: 2, y: 0 }));
    }

    #[test]
    fn overlay_matches_viewport_size() {
        let mut painter = OverlayPainter::default();
        let pixels = painter.paint(Camera::default(), (64, 32), &[red_line((10.0, 10.0), (50.0, 10.0))]);
        assert_eq!(pixels.len(), 64 * 32 * 4);
    }

    #[test]
    fn scheduler_tracks_dirty_and_cached_sequences() {
        let mut sched = TileScheduler::new();
        let key = TileKey { x: 0, y: 0 };
        let strokes = vec![Stroke {
            color: [0, 0, 0, 255],
            width: 4.0,
            points: vec![
                StrokePoint { x: 5.0, y: 5.0, pressure: 1.0 },
                StrokePoint { x: 50.0, y: 5.0, pressure: 1.0 },
            ],
        }];
        let index = StrokeIndex::from_strokes(&strokes);

        sched.mark_dirty(key, 3);
        assert!(sched.needs_dispatch());
        sched.dispatch(&index, &strokes, &[], Pattern::Blank);
        assert!(!sched.needs_dispatch());
        assert!(sched.is_busy());

        let results = loop {
            let r = sched.poll();
            if !r.is_empty() {
                break r;
            }
            thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(results[0].0, key);
        assert_eq!(sched.cached_seq(key), 3);
        assert!(!sched.is_busy());
    }
}

#[cfg(test)]
mod pattern_tests {
    use super::*;

    #[test]
    fn ruled_tile_has_lines_and_blank_does_not() {
        let blank = rasterize_tile([0.0, 0.0], &[], &[], Pattern::Blank).unwrap();
        let ruled = rasterize_tile([0.0, 0.0], &[], &[], Pattern::Ruled).unwrap();
        let alpha = |px: &[u8], x: usize, y: usize| px[(y * TILE_SIZE as usize + x) * 4 + 3];
        assert_eq!(alpha(&blank, 100, 26), 0);
        assert!(alpha(&ruled, 100, 26) > 0, "line at y = 26 should be drawn");
        assert_eq!(alpha(&ruled, 100, 10), 0, "no line between rules");
    }
}
