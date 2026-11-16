//! InkCraft entry point: winit event loop, input routing, and the frame loop.
//!
//! Usage: `inkcraft [NOTEBOOK.ink]`. Without an argument the notebook is
//! `Untitled.ink` in the data directory (`$INKCRAFT_HOME`, or `~/.inkcraft`).
//!
//! Tools (Samsung Notes-style, see the README for the full list):
//! * Pens: Pen, Pencil, Highlighter, Brush. Pressure applies to all of them.
//! * Line: drag for a straight line.
//! * Eraser: drag over strokes to remove them whole.
//! * Select: drag a rectangle (or click) to select strokes; Delete removes them.
//! * Hand: drag to move the view. Middle-drag also pans.
//! * Mouse wheel or Ctrl+/- zooms; Ctrl+0 resets to 100%.
//! * Ctrl+Z undo, Ctrl+Y or Ctrl+Shift+Z redo, Ctrl+S save.
//! * Shortcuts: P pen, C pencil, M highlighter, B brush, L line, E eraser, S select, H hand.

use inkcraft::guides;
use inkcraft::{
    document::Document,
    input::{
        input_channel, pressure_from_force, InputConsumer, InputEvent, InputProducer, Phase,
        PointerSample, StrokeSmoother,
    },
    model::{Camera, Guide, Notebook, PageImage, Pattern, Rect, Stroke, StrokePoint},
    renderer::{DrawStroke, FrameInput, OverlayPainter, Renderer, TileKey, TileScheduler},
    storage::{default_data_dir, load_notebook, save_notebook, unix_now, Autosaver, Metadata},
    pdf,
    ui::{self, Action, Chrome, Fonts, Info, PxRect, Tool, UiState, PEN_COLORS, PEN_WIDTHS},
};
use std::{
    collections::BTreeSet,
    error::Error,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, Touch, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowAttributes, WindowId},
};

/// Tiles are rasterized once the page has been idle this long.
const IDLE_FLUSH: Duration = Duration::from_millis(300);
const TOAST_DURATION: Duration = Duration::from_millis(2500);
/// A mouse has no pressure sensor, so it draws at full pressure.
const MOUSE_PRESSURE: f32 = 1.0;
/// Eraser and click-select radii, in logical pixels. Scaled to page units per frame.
const ERASER_RADIUS: f64 = 10.0;
const CLICK_SELECT_RADIUS: f64 = 5.0;
/// Rectangles smaller than this (logical px) count as a click.
const CLICK_DRAG_LIMIT: f64 = 4.0;
/// Erase preview color, drawn over strokes that will be removed on release.
const ERASE_PREVIEW: [u8; 4] = [220, 40, 40, 170];

/// A committed stroke whose tiles have not yet been rasterized at its sequence.
/// It is drawn through the overlay until then.
struct LiveStroke {
    id: usize,
    seq: u64,
}

/// What the current canvas gesture is doing. Only one runs at a time.
enum Gesture {
    None,
    Ink { smoother: StrokeSmoother, tool: Tool },
    Line { start: StrokePoint, end: StrokePoint },
    /// Strokes hit so far. Removal happens on release, as one undoable edit.
    Erase { hits: BTreeSet<usize> },
    Select { start: [f64; 2], end: [f64; 2] },
}

/// A guide being placed from a ruler, or an existing guide being moved.
#[derive(Clone, Copy, Debug, PartialEq)]
enum GuideDrag {
    Create { vertical: bool },
    Move(usize),
}

/// Screen distance within which a pointer grabs a guide.
const GUIDE_GRAB_PX: f64 = 4.0;
/// Screen distance within which a drawn point snaps to a guide.
const SNAP_PX: f64 = 6.0;

fn axis_of(page: [f64; 2], vertical: bool) -> f64 {
    if vertical { page[0] } else { page[1] }
}

/// What the current left-button press is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drag {
    /// Started on a control; the release does nothing further.
    Chrome,
    /// Started on the canvas with a tool other than Hand.
    Canvas,
    Pan,
    /// Placing a guide from a ruler, or moving one.
    Guide,
}

/// Everything that exists once the window and GPU are up.
struct Session {
    window: Arc<Window>,
    renderer: Renderer,
    painter: OverlayPainter,
    fonts: Fonts,
    chrome: Chrome,
    ui_state: UiState,
    /// Inputs the last chrome paint used. A change repaints the chrome layer.
    painted: Option<(Info, UiState, (u32, u32), f32)>,
    toast: Option<(String, Instant)>,
    title: String,
    doc: Document,
    images: Vec<Arc<PageImage>>,
    pattern: Pattern,
    selection: Vec<usize>,
    commit_seq: u64,
    live: Vec<LiveStroke>,
    gesture: Gesture,
    camera: Camera,
    tiles: TileScheduler,
    path: PathBuf,
    autosaver: Autosaver,
    metadata: Metadata,
    last_edit: Instant,
    last_edit_wall: Option<SystemTime>,
    flush_requested: bool,
    guides: Vec<Guide>,
    guide_drag: Option<GuideDrag>,
    guide_preview: Option<Guide>,
}

impl Session {
    fn open(event_loop: &ActiveEventLoop, path: PathBuf) -> Result<Self, Box<dyn Error>> {
        let notebook = if path.exists() {
            load_notebook(&path)?
        } else {
            Notebook {
                title: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".into()),
                strokes: Vec::new(),
                images: Vec::new(),
                pattern: Pattern::Blank,
                guides: Vec::new(),
            }
        };

        let window = Arc::new(event_loop.create_window(
            WindowAttributes::default()
                .with_title(format!("InkCraft - {}", notebook.title))
                .with_inner_size(LogicalSize::new(1280.0, 800.0)),
        )?);
        let renderer = Renderer::new(event_loop.owned_display_handle(), window.clone())?;
        let fonts = Fonts::load()?;

        let metadata = Metadata::open(&default_data_dir().join("inkcraft.db"))?;
        metadata.register_notebook(&notebook.title, &path, None, unix_now())?;

        let placeholder = Info {
            title: String::new(),
            file_name: String::new(),
            stroke_count: 0,
            zoom_percent: 100,
            save_status: String::new(),
            can_undo: false,
            can_redo: false,
            toast: None,
            has_selection: false,
            marquee: None,
            selection: Vec::new(),
            pattern: Pattern::Blank,
            guides: Vec::new(),
            guide_preview: None,
            zoom: 1.0,
            pan: [0.0, 0.0],
        };
        let chrome = ui::layout((1, 1), 1.0, &UiState::default(), &placeholder, &fonts);
        let loaded = !notebook.strokes.is_empty();
        let mut session = Session {
            window,
            renderer,
            painter: OverlayPainter::default(),
            fonts,
            chrome,
            ui_state: UiState::default(),
            painted: None,
            toast: None,
            title: notebook.title.clone(),
            images: notebook.images.iter().cloned().map(Arc::new).collect(),
            pattern: notebook.pattern,
            guides: notebook.guides,
            guide_drag: None,
            guide_preview: None,
            doc: Document::new(notebook.strokes),
            selection: Vec::new(),
            commit_seq: if loaded { 1 } else { 0 },
            live: Vec::new(),
            gesture: Gesture::None,
            camera: Camera::default(),
            tiles: TileScheduler::new(),
            autosaver: Autosaver::spawn(),
            metadata,
            last_edit: Instant::now(),
            last_edit_wall: None,
            flush_requested: false,
            path,
        };

        // Loaded strokes start in the overlay and are rasterized into tiles in the background.
        let seq = session.commit_seq;
        for id in 0..session.doc.strokes().len() {
            let bounds = session.doc.strokes()[id].bounds();
            session.live.push(LiveStroke { id, seq });
            for key in TileKey::covering(bounds) {
                session.tiles.mark_dirty(key, seq);
            }
        }
        for image in &session.images {
            for key in TileKey::covering(image.rect) {
                session.tiles.mark_dirty(key, seq);
            }
        }
        session.relayout();
        Ok(session)
    }

    fn info(&self) -> Info {
        let canvas = self.chrome.canvas;
        let to_window = |page: Rect| {
            let a = self.camera.page_to_screen(page.min);
            let b = self.camera.page_to_screen(page.max);
            PxRect {
                x: canvas.x + a[0] as f32,
                y: canvas.y + a[1] as f32,
                w: (b[0] - a[0]) as f32,
                h: (b[1] - a[1]) as f32,
            }
        };
        let marquee = match self.gesture {
            Gesture::Select { start, end } => Some(to_window(normalize(start, end))),
            _ => None,
        };
        let selection = self
            .selection
            .iter()
            .map(|&id| to_window(self.doc.strokes()[id].bounds()))
            .collect();
        let idle = matches!(self.gesture, Gesture::None);

        Info {
            title: self.title.clone(),
            file_name: self
                .path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
            stroke_count: self.doc.strokes().len(),
            zoom_percent: (self.camera.zoom * 100.0).round() as u32,
            save_status: self.save_status().into(),
            can_undo: idle && self.doc.can_undo(),
            can_redo: idle && self.doc.can_redo(),
            toast: self.toast.as_ref().map(|(message, _)| message.clone()),
            has_selection: !self.selection.is_empty(),
            marquee,
            selection,
            pattern: self.pattern,
            guides: self.guides.clone(),
            guide_preview: self.guide_preview,
            zoom: self.camera.zoom,
            pan: self.camera.pan,
        }
    }

    fn save_status(&self) -> &'static str {
        match (self.last_edit_wall, self.autosaver.last_saved()) {
            (None, _) => "All changes saved",
            (Some(edit), Some(saved)) if saved >= edit => "All changes saved",
            _ => "Saving...",
        }
    }

    fn relayout(&mut self) {
        let info = self.info();
        let (w, h) = self.renderer.viewport();
        let scale = self.window.scale_factor() as f32;
        self.chrome = ui::layout((w, h), scale, &self.ui_state, &info, &self.fonts);
    }

    fn is_busy(&self) -> bool {
        !matches!(self.gesture, Gesture::None)
            || !self.live.is_empty()
            || self.tiles.is_busy()
            || self.toast.is_some()
            || matches!(
                (self.last_edit_wall, self.autosaver.last_saved()),
                (Some(edit), saved) if saved.is_none_or(|s| s < edit)
            )
    }

    /// Radius in page units for a logical-pixel distance at the current zoom.
    fn page_radius(&self, logical: f64) -> f64 {
        logical * self.chrome.scale as f64 / self.camera.zoom
    }

    /// Advances one frame: applies queued input, schedules rasterization,
    /// uploads finished tiles, repaints the chrome if it changed, and draws.
    fn step(&mut self, events: Vec<InputEvent>) -> Result<(), Box<dyn Error>> {
        let now = Instant::now();
        if let Some((_, since)) = &self.toast {
            if now.duration_since(*since) > TOAST_DURATION {
                self.toast = None;
            }
        }

        let mut gesture_ended = false;
        for event in events {
            match event {
                InputEvent::Pointer(sample) => {
                    gesture_ended |= self.pointer(sample, now);
                }
                InputEvent::Pan { dx, dy } => {
                    if matches!(self.gesture, Gesture::None) {
                        self.camera.pan_by_screen(dx, dy);
                    }
                }
                InputEvent::Zoom { factor, anchor } => {
                    if matches!(self.gesture, Gesture::None) {
                        self.camera.zoom_at(factor, anchor);
                    }
                }
            }
        }

        if self.pattern != Pattern::Blank {
            // Empty regions show the pattern too, so request tiles for everything on screen.
            let c = self.chrome.canvas;
            let visible = self.camera.visible_rect([c.w as f64, c.h as f64]);
            for key in TileKey::covering(visible) {
                self.tiles.ensure(key, self.commit_seq);
            }
        }

        let idle = now.duration_since(self.last_edit) >= IDLE_FLUSH;
        if self.tiles.needs_dispatch() && (gesture_ended || self.flush_requested || idle) {
            self.tiles
                .dispatch(self.doc.index(), self.doc.strokes(), &self.images, self.pattern);
            self.flush_requested = false;
        }

        let results = self.tiles.poll();
        if !results.is_empty() {
            for (key, pixels) in &results {
                self.renderer.upload_tile(*key, pixels);
            }
            let tiles = &self.tiles;
            let strokes = self.doc.strokes();
            // A stroke leaves the overlay once every tile it touches shows it.
            self.live.retain(|live| {
                !TileKey::covering(strokes[live.id].bounds()).all(|key| tiles.cached_seq(key) >= live.seq)
            });
        }

        self.relayout();
        let info = self.info();
        let size = self.chrome.size;
        let key = (info.clone(), self.ui_state.clone(), (size.0 as u32, size.1 as u32), self.chrome.scale);
        let chrome_pixels = if self.painted.as_ref() != Some(&key) {
            self.painted = Some(key);
            ui::paint(&self.chrome, &self.ui_state, &info, &self.fonts)
        } else {
            None
        };

        let canvas = self.chrome.canvas;
        let (cw, ch) = (canvas.w as u32, canvas.h as u32);
        let visible = self.camera.visible_rect([cw as f64, ch as f64]);
        let strokes = self.doc.strokes();
        let mut overlay_strokes: Vec<DrawStroke> = self
            .live
            .iter()
            .filter(|l| strokes[l.id].bounds().intersects(&visible))
            .map(|l| DrawStroke::from_stroke(&strokes[l.id]))
            .collect();
        match &self.gesture {
            Gesture::Ink { smoother, tool } => {
                let (color, width) = self.look(*tool);
                overlay_strokes.push(DrawStroke {
                    color,
                    width,
                    points: smoother.smoothed().to_vec(),
                });
            }
            Gesture::Line { start, end } => {
                let (color, width) = self.look(Tool::Line);
                overlay_strokes.push(DrawStroke {
                    color,
                    width,
                    points: line_points(*start, *end),
                });
            }
            Gesture::Erase { hits } => {
                for &id in hits {
                    let mut preview = DrawStroke::from_stroke(&strokes[id]);
                    preview.color = ERASE_PREVIEW;
                    overlay_strokes.push(preview);
                }
            }
            Gesture::Select { .. } | Gesture::None => {}
        }
        let overlay = if overlay_strokes.is_empty() || cw == 0 || ch == 0 {
            None
        } else {
            Some(self.painter.paint(self.camera, (cw, ch), &overlay_strokes))
        };

        let frame = FrameInput {
            camera: self.camera,
            canvas: canvas.as_u32(),
            overlay,
            chrome: chrome_pixels.as_deref(),
        };
        if let Err(e) = self.renderer.render(frame) {
            eprintln!("render failed: {e}");
        }
        Ok(())
    }

    /// Color and width for `tool` with the current choices applied.
    fn look(&self, tool: Tool) -> ([u8; 4], f32) {
        ui::ink_look(tool, PEN_COLORS[self.ui_state.color], PEN_WIDTHS[self.ui_state.width])
    }

    /// Handles one pointer sample in canvas-local coordinates.
    /// Returns `true` if a gesture finished and produced a change.
    fn pointer(&mut self, sample: PointerSample, now: Instant) -> bool {
        let page = self.camera.screen_to_page(sample.screen);
        let point = StrokePoint {
            x: page[0] as f32,
            y: page[1] as f32,
            pressure: sample.pressure.clamp(0.0, 1.0),
        };
        let tool = self.ui_state.tool;
        let drawing = match sample.phase {
            Phase::Down => tool.is_ink() || tool == Tool::Line,
            _ => matches!(self.gesture, Gesture::Ink { .. } | Gesture::Line { .. }),
        };
        let point = if drawing { self.snap(point) } else { point };
        match sample.phase {
            Phase::Down => {
                let changed = self.finish_gesture(now);
                let tool = self.ui_state.tool;
                self.gesture = if tool.is_ink() {
                    let mut smoother = StrokeSmoother::new();
                    smoother.push(point);
                    Gesture::Ink { smoother, tool }
                } else {
                    match tool {
                        Tool::Line => Gesture::Line { start: point, end: point },
                        Tool::Eraser => {
                            let mut hits = BTreeSet::new();
                            let r = self.page_radius(ERASER_RADIUS);
                            hits.extend(self.doc.hit(page, r));
                            Gesture::Erase { hits }
                        }
                        Tool::Select => Gesture::Select { start: page, end: page },
                        _ => Gesture::None,
                    }
                };
                changed
            }
            Phase::Move => {
                self.update_gesture(point, page, now);
                false
            }
            Phase::Up => {
                self.update_gesture(point, page, now);
                self.finish_gesture(now)
            }
        }
    }

    fn update_gesture(&mut self, point: StrokePoint, page: [f64; 2], now: Instant) {
        let radius = self.page_radius(ERASER_RADIUS);
        match &mut self.gesture {
            Gesture::Ink { smoother, .. } => {
                if smoother.push(point) {
                    self.last_edit = now;
                }
            }
            Gesture::Line { end, .. } => *end = point,
            Gesture::Erase { hits } => hits.extend(self.doc.hit(page, radius)),
            Gesture::Select { end, .. } => *end = page,
            Gesture::None => {}
        }
    }

    /// Ends the current gesture and applies its result. Returns `true` if the document changed.
    fn finish_gesture(&mut self, now: Instant) -> bool {
        match std::mem::replace(&mut self.gesture, Gesture::None) {
            Gesture::None => false,
            Gesture::Ink { smoother, tool } => {
                let raw = smoother.finish().raw;
                if raw.is_empty() {
                    return false;
                }
                let (color, width) = self.look(tool);
                self.commit(Stroke { color, width, points: raw }, now);
                true
            }
            Gesture::Line { start, end } => {
                let points = line_points(start, end);
                if points.len() < 2 || (end.x - start.x).hypot(end.y - start.y) < 0.5 {
                    return false;
                }
                let (color, width) = self.look(Tool::Line);
                self.commit(Stroke { color, width, points }, now);
                true
            }
            Gesture::Erase { hits } => {
                if hits.is_empty() {
                    return false;
                }
                let ids: Vec<usize> = hits.into_iter().collect();
                self.doc.erase(&ids);
                self.after_history(now);
                true
            }
            Gesture::Select { start, end } => {
                self.select(start, end);
                false
            }
        }
    }

    fn select(&mut self, start: [f64; 2], end: [f64; 2]) {
        let area = normalize(start, end);
        let small = (area.max[0] - area.min[0]) < CLICK_DRAG_LIMIT
            && (area.max[1] - area.min[1]) < CLICK_DRAG_LIMIT;
        self.selection = if small {
            self.doc.hit(start, self.page_radius(CLICK_SELECT_RADIUS))
        } else {
            self.doc.within(area)
        };
    }

    fn commit(&mut self, stroke: Stroke, now: Instant) {
        let id = self.doc.commit(stroke);
        self.selection.clear();
        let seq = self.sync_tiles();
        self.live.push(LiveStroke { id, seq });
        self.edited(now);
    }

    /// Queues tile rasterization for everything the document changed. Returns the new sequence.
    fn sync_tiles(&mut self) -> u64 {
        self.commit_seq += 1;
        let seq = self.commit_seq;
        for bounds in self.doc.take_dirty() {
            for key in TileKey::covering(bounds) {
                self.tiles.mark_dirty(key, seq);
            }
        }
        self.flush_requested = true;
        seq
    }

    /// Ids shift after undo, redo, or erase, so the overlay list is reset and
    /// every touched tile is rasterized again.
    fn after_history(&mut self, now: Instant) {
        self.live.clear();
        self.selection.clear();
        self.sync_tiles();
        self.edited(now);
    }

    fn edited(&mut self, now: Instant) {
        self.last_edit = now;
        self.last_edit_wall = Some(SystemTime::now());
        // Cloning the whole notebook per edit is O(n). Fine for now; incremental
        // saves are the next step if large notebooks lag.
        let notebook = self.snapshot();
        self.autosaver.request_save(self.path.clone(), notebook);
        if let Err(e) = self.metadata.touch_notebook(&self.path, unix_now()) {
            eprintln!("metadata update failed: {e}");
        }
    }

    /// Starts placing a guide from a ruler, or moving an existing one.
    fn start_guide(&mut self, drag: GuideDrag) {
        self.guide_drag = Some(drag);
        self.guide_preview = None;
    }

    /// Follows the pointer during a guide drag. `local` is canvas-local.
    fn update_guide(&mut self, local: [f64; 2]) {
        let page = self.camera.screen_to_page(local);
        match self.guide_drag {
            Some(GuideDrag::Create { vertical }) => {
                self.guide_preview = Some(Guide { vertical, pos: axis_of(page, vertical) });
            }
            Some(GuideDrag::Move(index)) => {
                if let Some(guide) = self.guides.get_mut(index) {
                    guide.pos = axis_of(page, guide.vertical);
                }
            }
            None => {}
        }
    }

    /// Ends a guide drag. `inside` says whether the pointer is over the canvas. A new guide
    /// released over a ruler is discarded, and a moved guide released outside the canvas is removed.
    fn finish_guide(&mut self, inside: bool, now: Instant) {
        let drag = self.guide_drag.take();
        let preview = self.guide_preview.take();
        match drag {
            Some(GuideDrag::Create { .. }) => {
                if let (true, Some(guide)) = (inside, preview) {
                    self.guides.push(guide);
                    self.edited(now);
                }
            }
            Some(GuideDrag::Move(index)) => {
                if !inside && index < self.guides.len() {
                    self.guides.remove(index);
                }
                self.edited(now);
            }
            None => {}
        }
    }

    /// The guide under a canvas-local point, if guides are shown and one is close enough.
    fn guide_near(&self, local: [f64; 2]) -> Option<usize> {
        if !self.ui_state.guides_visible {
            return None;
        }
        self.guides.iter().position(|guide| {
            let distance = if guide.vertical {
                self.camera.page_to_screen([guide.pos, 0.0])[0] - local[0]
            } else {
                self.camera.page_to_screen([0.0, guide.pos])[1] - local[1]
            };
            distance.abs() <= GUIDE_GRAB_PX
        })
    }

    /// Pulls a drawing point onto the nearest guide on each axis, when snapping is on.
    fn snap(&self, point: StrokePoint) -> StrokePoint {
        if !self.ui_state.snap || self.guides.is_empty() {
            return point;
        }
        let tolerance = SNAP_PX / self.camera.zoom;
        let mut snapped = point;
        let verticals = self.guides.iter().filter(|g| g.vertical).map(|g| g.pos);
        if let Some(x) = guides::snap(point.x as f64, verticals, tolerance) {
            snapped.x = x as f32;
        }
        let horizontals = self.guides.iter().filter(|g| !g.vertical).map(|g| g.pos);
        if let Some(y) = guides::snap(point.y as f64, horizontals, tolerance) {
            snapped.y = y as f32;
        }
        snapped
    }

    fn undo(&mut self) {
        if matches!(self.gesture, Gesture::None) && self.doc.undo() {
            self.after_history(Instant::now());
        }
    }

    fn redo(&mut self) {
        if matches!(self.gesture, Gesture::None) && self.doc.redo() {
            self.after_history(Instant::now());
        }
    }

    fn delete_selection(&mut self) {
        if matches!(self.gesture, Gesture::None) && !self.selection.is_empty() {
            let ids = std::mem::take(&mut self.selection);
            self.doc.erase(&ids);
            self.after_history(Instant::now());
        }
    }

    fn zoom_by(&mut self, factor: f64) {
        if !matches!(self.gesture, Gesture::None) {
            return;
        }
        let c = self.chrome.canvas;
        self.camera.zoom_at(factor, [c.w as f64 / 2.0, c.h as f64 / 2.0]);
    }

    /// The whole notebook as it is saved on disk.
    fn snapshot(&self) -> Notebook {
        Notebook {
            title: self.title.clone(),
            strokes: self.doc.strokes().to_vec(),
            images: self.images.iter().map(|i| (**i).clone()).collect(),
            pattern: self.pattern,
            guides: self.guides.clone(),
        }
    }

    fn show_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now()));
    }

    /// Imports every page of a PDF below the existing content. Not undoable.
    fn import_pdf(&mut self, path: &Path, now: Instant) {
        let bottom = self
            .images
            .iter()
            .map(|i| i.rect.max[1])
            .chain(self.doc.strokes().iter().map(|s| s.bounds().max[1]))
            .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.max(v))));
        let top = bottom.map_or(0.0, |b| b + pdf::PAGE_GAP);
        let result = pdf::import_file(path, top).map_err(|e| e.to_string());
        match result {
            Ok(pages) => {
                let count = pages.len();
                let first_new = self.images.len();
                self.images.extend(pages.into_iter().map(Arc::new));
                self.commit_seq += 1;
                let seq = self.commit_seq;
                for image in &self.images[first_new..] {
                    for key in TileKey::covering(image.rect) {
                        self.tiles.mark_dirty(key, seq);
                    }
                }
                self.flush_requested = true;
                self.edited(now);
                self.show_toast(format!("Imported {count} page(s)"));
            }
            Err(e) => self.show_toast(format!("Import failed: {e}")),
        }
    }

    fn drop_file(&mut self, path: &Path) {
        let supported = path.extension().is_some_and(|e| {
            ["pdf", "png", "jpg", "jpeg"].iter().any(|ext| e.eq_ignore_ascii_case(ext))
        });
        if supported {
            self.import_pdf(path, Instant::now());
        } else {
            self.show_toast("Drop a PDF, PNG, or JPG file to import it");
        }
    }

    /// Changes the canvas background. Tiles are redrawn, and visible empty areas get tiles.
    fn set_pattern(&mut self, pattern: Pattern) {
        if pattern == self.pattern {
            return;
        }
        self.pattern = pattern;
        self.commit_seq += 1;
        self.tiles.mark_all_dirty(self.commit_seq);
        self.flush_requested = true;
        self.edited(Instant::now());
        self.show_toast(format!("Template: {}", pattern.label()));
    }

    /// Writes `<notebook>.pdf` next to the notebook file.
    fn export_pdf(&mut self) {
        let target = self.path.with_extension("pdf");
        let notebook = self.snapshot();
        let message = match pdf::export(&target, &notebook.strokes, &notebook.images, notebook.pattern) {
            Ok(()) => format!(
                "Exported {}",
                target.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
            ),
            Err(e) => format!("Export failed: {e}"),
        };
        self.show_toast(message);
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Tab(tab) => self.ui_state.tab = tab,
            Action::SetTool(tool) => self.ui_state.tool = tool,
            Action::SetColor(i) => self.ui_state.color = i.min(PEN_COLORS.len() - 1),
            Action::SetWidth(i) => self.ui_state.width = i.min(PEN_WIDTHS.len() - 1),
            Action::ZoomIn => self.zoom_by(1.25),
            Action::ZoomOut => self.zoom_by(0.8),
            Action::ZoomReset => {
                let factor = 1.0 / self.camera.zoom;
                self.zoom_by(factor);
            }
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::DeleteSelection => self.delete_selection(),
            Action::Save => {
                let notebook = self.snapshot();
                self.autosaver.request_save(self.path.clone(), notebook);
                self.show_toast("Saving...");
            }
            Action::ExportPdf => self.export_pdf(),
            Action::SetPattern(pattern) => self.set_pattern(pattern),
            Action::ToggleRulers => self.ui_state.rulers = !self.ui_state.rulers,
            Action::ToggleGuides => self.ui_state.guides_visible = !self.ui_state.guides_visible,
            Action::ToggleSnap => self.ui_state.snap = !self.ui_state.snap,
            Action::ClearGuides => {
                if !self.guides.is_empty() {
                    self.guides.clear();
                    self.edited(Instant::now());
                }
            }
        }
    }
}

/// Builds a rectangle from two corners, in any order.
fn normalize(a: [f64; 2], b: [f64; 2]) -> Rect {
    Rect::new([a[0].min(b[0]), a[1].min(b[1])], [a[0].max(b[0]), a[1].max(b[1])])
}

/// Points for a straight line, spaced about 2 page units apart.
fn line_points(a: StrokePoint, b: StrokePoint) -> Vec<StrokePoint> {
    let len = (b.x - a.x).hypot(b.y - a.y);
    let steps = ((len / 2.0).ceil() as usize).clamp(1, 20_000);
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            StrokePoint {
                x: a.x + (b.x - a.x) * t,
                y: a.y + (b.y - a.y) * t,
                pressure: 1.0,
            }
        })
        .collect()
}

struct App {
    path: PathBuf,
    producer: InputProducer,
    consumer: InputConsumer,
    /// Last cursor position, in window pixels.
    cursor: [f64; 2],
    left_drag: Option<Drag>,
    middle_down: bool,
    touch_id: Option<u64>,
    touch_last: [f64; 2],
    modifiers: ModifiersState,
    session: Option<Session>,
}

fn to_f32(p: [f64; 2]) -> [f32; 2] {
    [p[0] as f32, p[1] as f32]
}

impl App {
    fn new(path: PathBuf) -> Self {
        let (producer, consumer) = input_channel();
        Self {
            path,
            producer,
            consumer,
            cursor: [0.0, 0.0],
            left_drag: None,
            middle_down: false,
            touch_id: None,
            touch_last: [0.0, 0.0],
            modifiers: ModifiersState::empty(),
            session: None,
        }
    }

    /// Converts a window position to canvas-local coordinates.
    fn to_canvas(&self, p: [f64; 2]) -> [f64; 2] {
        match &self.session {
            Some(s) => [p[0] - s.chrome.canvas.x as f64, p[1] - s.chrome.canvas.y as f64],
            None => p,
        }
    }

    fn push_pointer(&self, phase: Phase, screen: [f64; 2], pressure: f32) {
        self.producer.push(InputEvent::Pointer(PointerSample {
            phase,
            screen,
            pressure,
        }));
    }

    fn request_redraw(&self) {
        if let Some(session) = &self.session {
            session.window.request_redraw();
        }
    }

    fn canvas_contains(&self, p: [f64; 2]) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.chrome.canvas.contains(to_f32(p)))
    }

    /// The ruler under a window point: `Some(true)` for the vertical ruler, `Some(false)` for the horizontal one.
    fn ruler_at(&self, p: [f64; 2]) -> Option<bool> {
        self.session.as_ref().and_then(|s| s.chrome.ruler_at(to_f32(p)))
    }

    fn chrome_hit(&self, p: [f64; 2]) -> Option<Action> {
        self.session.as_ref().and_then(|s| s.chrome.hit(to_f32(p)))
    }

    fn tool(&self) -> Tool {
        self.session.as_ref().map_or(Tool::Pen, |s| s.ui_state.tool)
    }

    fn apply(&mut self, action: Action) {
        if let Some(session) = self.session.as_mut() {
            session.apply(action);
        }
    }

    fn on_mouse_button(&mut self, pressed: bool, button: MouseButton) {
        match button {
            MouseButton::Left if pressed => {
                let p = self.cursor;
                if let Some(action) = self.chrome_hit(p) {
                    self.apply(action);
                    self.left_drag = Some(Drag::Chrome);
                } else if let Some(vertical) = self.ruler_at(p) {
                    if let Some(session) = self.session.as_mut() {
                        session.start_guide(GuideDrag::Create { vertical });
                    }
                    self.left_drag = Some(Drag::Guide);
                } else if self.canvas_contains(p) {
                    let local = self.to_canvas(p);
                    if self.tool() == Tool::Pan {
                        self.left_drag = Some(Drag::Pan);
                    } else if let Some(index) = self.session.as_ref().and_then(|s| s.guide_near(local)) {
                        if let Some(session) = self.session.as_mut() {
                            session.start_guide(GuideDrag::Move(index));
                        }
                        self.left_drag = Some(Drag::Guide);
                    } else {
                        self.left_drag = Some(Drag::Canvas);
                        self.push_pointer(Phase::Down, local, MOUSE_PRESSURE);
                    }
                }
            }
            MouseButton::Left => match self.left_drag.take() {
                Some(Drag::Canvas) => {
                    let local = self.to_canvas(self.cursor);
                    self.push_pointer(Phase::Up, local, MOUSE_PRESSURE);
                }
                Some(Drag::Guide) => {
                    let inside = self.canvas_contains(self.cursor);
                    if let Some(session) = self.session.as_mut() {
                        session.finish_guide(inside, Instant::now());
                    }
                }
                _ => {}
            },
            MouseButton::Middle => self.middle_down = pressed,
            _ => {}
        }
        self.request_redraw();
    }

    fn on_touch(&mut self, touch: Touch) {
        let screen = [touch.location.x, touch.location.y];
        let pressure = pressure_from_force(touch.force);
        match touch.phase {
            TouchPhase::Started => {
                if self.touch_id.is_none() {
                    self.touch_id = Some(touch.id);
                    self.touch_last = screen;
                    if let Some(action) = self.chrome_hit(screen) {
                        self.apply(action);
                    } else if self.canvas_contains(screen) && self.tool() != Tool::Pan {
                        let local = self.to_canvas(screen);
                        self.push_pointer(Phase::Down, local, pressure);
                    }
                }
            }
            TouchPhase::Moved => {
                if self.touch_id == Some(touch.id) {
                    if self.tool() == Tool::Pan {
                        self.producer.push(InputEvent::Pan {
                            dx: screen[0] - self.touch_last[0],
                            dy: screen[1] - self.touch_last[1],
                        });
                    } else {
                        let local = self.to_canvas(screen);
                        self.push_pointer(Phase::Move, local, pressure);
                    }
                    self.touch_last = screen;
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if self.touch_id == Some(touch.id) {
                    self.touch_id = None;
                    if self.tool() != Tool::Pan {
                        let local = self.to_canvas(screen);
                        self.push_pointer(Phase::Up, local, pressure);
                    }
                }
            }
        }
        self.request_redraw();
    }

    fn on_key(&mut self, key: &Key) {
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let action = match key {
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => Some(Action::DeleteSelection),
            Key::Named(NamedKey::Escape) => {
                if let Some(session) = self.session.as_mut() {
                    session.selection.clear();
                }
                None
            }
            Key::Character(c) => {
                let lower = c.to_lowercase();
                match (ctrl, shift, lower.as_str()) {
                    (true, false, "z") => Some(Action::Undo),
                    (true, true, "z") | (true, false, "y") => Some(Action::Redo),
                    (true, false, "s") => Some(Action::Save),
                    (true, false, "r") => Some(Action::ToggleRulers),
                    (true, false, "0") => Some(Action::ZoomReset),
                    (true, false, "=") | (true, false, "+") => Some(Action::ZoomIn),
                    (true, false, "-") => Some(Action::ZoomOut),
                    (false, false, "p") => Some(Action::SetTool(Tool::Pen)),
                    (false, false, "c") => Some(Action::SetTool(Tool::Pencil)),
                    (false, false, "m") => Some(Action::SetTool(Tool::Highlighter)),
                    (false, false, "b") => Some(Action::SetTool(Tool::Brush)),
                    (false, false, "l") => Some(Action::SetTool(Tool::Line)),
                    (false, false, "e") => Some(Action::SetTool(Tool::Eraser)),
                    (false, false, "s") => Some(Action::SetTool(Tool::Select)),
                    (false, false, "h") => Some(Action::SetTool(Tool::Pan)),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(action) = action {
            self.apply(action);
        }
        self.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.session.is_some() {
            return;
        }
        match Session::open(event_loop, self.path.clone()) {
            Ok(session) => self.session = Some(session),
            Err(e) => {
                eprintln!("failed to start InkCraft: {e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window_id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // Dropping the session drops the autosaver, which writes any pending save.
                self.session = None;
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(session) = self.session.as_mut() {
                    session.renderer.resize(size.width, size.height);
                }
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let events: Vec<InputEvent> = self.consumer.drain().collect();
                if let Some(session) = self.session.as_mut() {
                    if let Err(e) = session.step(events) {
                        eprintln!("frame failed: {e}");
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let last = self.cursor;
                self.cursor = [position.x, position.y];
                let hover = self.chrome_hit(self.cursor);
                if let Some(session) = self.session.as_mut() {
                    session.ui_state.hover = hover;
                }
                let local = self.to_canvas(self.cursor);
                let delta = [self.cursor[0] - last[0], self.cursor[1] - last[1]];
                if self.left_drag == Some(Drag::Canvas) {
                    self.push_pointer(Phase::Move, local, MOUSE_PRESSURE);
                }
                if self.left_drag == Some(Drag::Guide) {
                    if let Some(session) = self.session.as_mut() {
                        session.update_guide(local);
                    }
                }
                if self.left_drag == Some(Drag::Pan) || self.middle_down {
                    self.producer.push(InputEvent::Pan { dx: delta[0], dy: delta[1] });
                }
                self.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.on_mouse_button(state == ElementState::Pressed, button);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 50.0,
                };
                if lines != 0.0 && self.canvas_contains(self.cursor) {
                    self.producer.push(InputEvent::Zoom {
                        factor: 1.1f64.powf(lines),
                        anchor: self.to_canvas(self.cursor),
                    });
                    self.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && !event.repeat {
                    self.on_key(&event.logical_key);
                }
            }
            WindowEvent::DroppedFile(path) => {
                if let Some(session) = self.session.as_mut() {
                    session.drop_file(&path);
                }
                self.request_redraw();
            }
            WindowEvent::Touch(touch) => self.on_touch(touch),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(session) = &self.session {
            if session.is_busy() {
                session.window.request_redraw();
            }
        }
    }
}

/// Command-line options. With `--import`, `--export`, or `--template` the program runs without a window.
struct Args {
    path: PathBuf,
    import: Option<PathBuf>,
    export: Option<PathBuf>,
    template: Option<Pattern>,
    guides: Vec<Guide>,
}

fn parse_args() -> Result<Args, String> {
    let mut path = None;
    let mut import = None;
    let mut export = None;
    let mut template = None;
    let mut guides = Vec::new();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--import") => {
                import = Some(PathBuf::from(args.next().ok_or("--import needs a file")?));
            }
            Some("--export") => {
                export = Some(PathBuf::from(args.next().ok_or("--export needs an output file")?));
            }
            Some("--template") => {
                let name = args.next().ok_or("--template needs blank, ruled, grid, or dot")?;
                let name = name.to_string_lossy().into_owned();
                template = Some(Pattern::from_name(&name).ok_or(format!("unknown template {name}"))?);
            }
            Some("--guide") => {
                let spec = args.next().ok_or("--guide needs v:POS or h:POS")?;
                let spec = spec.to_string_lossy().into_owned();
                let (axis, pos) = spec.split_once(':').ok_or(format!("bad guide {spec}"))?;
                let pos: f64 = pos.parse().map_err(|_| format!("bad guide position in {spec}"))?;
                let vertical = match axis {
                    "v" => true,
                    "h" => false,
                    _ => return Err(format!("a guide starts with v: or h:, got {spec}")),
                };
                guides.push(Guide { vertical, pos });
            }
            Some(flag) if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => path = Some(PathBuf::from(arg)),
        }
    }
    Ok(Args {
        path: path.unwrap_or_else(|| default_data_dir().join("Untitled.ink")),
        import,
        export,
        template,
        guides,
    })
}

/// Imports, sets a template, and/or exports without opening a window.
fn run_headless(args: &Args) -> Result<(), Box<dyn Error>> {
    let mut notebook = if args.path.exists() {
        load_notebook(&args.path)?
    } else {
        Notebook {
            title: args
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into()),
            ..Default::default()
        }
    };
    if let Some(pattern) = args.template {
        notebook.pattern = pattern;
        println!("template: {}", pattern.label());
    }
    if !args.guides.is_empty() {
        notebook.guides.extend(args.guides.iter().copied());
        println!("added {} guide(s)", args.guides.len());
    }
    if let Some(file) = &args.import {
        let top = pdf::content_bounds(&notebook.strokes, &notebook.images)
            .map_or(0.0, |b| b.max[1] + pdf::PAGE_GAP);
        let pages = pdf::import_file(file, top)?;
        println!("imported {} page(s) from {}", pages.len(), file.display());
        notebook.images.extend(pages);
    }
    if args.template.is_some() || args.import.is_some() || !args.guides.is_empty() {
        save_notebook(&args.path, &notebook)?;
    }
    if let Some(out) = &args.export {
        pdf::export(out, &notebook.strokes, &notebook.images, notebook.pattern)?;
        println!("exported {}", out.display());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    if args.import.is_some() || args.export.is_some() || args.template.is_some() || !args.guides.is_empty() {
        return run_headless(&args);
    }
    let event_loop = EventLoop::new()?;
    let mut app = App::new(args.path);
    event_loop.run_app(&mut app)?;
    Ok(())
}
