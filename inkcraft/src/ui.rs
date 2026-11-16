//! Application chrome, styled after Print Me Checks (Azurehawke/printmechecks):
//! a blue title and tab bar, a light grey ribbon of grouped buttons, a left
//! tool rail, a right details panel, and a blue status bar.
//!
//! Layout is recomputed from the current state on every frame, so one
//! [`Chrome`] value drives both painting and hit testing. Design sizes are in
//! logical pixels and are multiplied by the window scale factor.

use skia_safe::{
    surfaces, AlphaType, Canvas, Color, ColorType, Data, Font, FontMgr, ImageInfo, Paint, PaintStyle, Rect as SkRect, Typeface,
};

use crate::guides;
use crate::model::{Guide, Paper, Pattern};

pub const TAB_BAR_H: f32 = 34.0;
pub const RIBBON_BODY_H: f32 = 96.0;
pub const STATUS_BAR_H: f32 = 24.0;
pub const LEFT_RAIL_W: f32 = 46.0;
pub const DETAILS_W: f32 = 300.0;

const APP_NAME: &str = "InkCraft";
/// Pen colors, straight RGBA. Index 0 is the default.
pub const PEN_COLORS: [[u8; 4]; 5] = [
    [28, 28, 40, 255],
    [43, 87, 154, 255],
    [196, 43, 28, 255],
    [16, 124, 16, 255],
    [136, 23, 152, 255],
];
pub const PEN_COLOR_NAMES: [&str; 5] = ["Ink", "Blue", "Red", "Green", "Purple"];
/// Nib diameters in page units at full pressure.
pub const PEN_WIDTHS: [f32; 3] = [1.5, 3.0, 6.0];
pub const PEN_WIDTH_NAMES: [&str; 3] = ["Thin", "Medium", "Thick"];

// Palette from the Print Me Checks stylesheets.
const BRAND: u32 = 0x2b579a;
const RIBBON_BG: u32 = 0xf3f3f3;
const RAIL_BG: u32 = 0xf7f7f7;
const PANEL_BG: u32 = 0xf7f7f7;
const BORDER: u32 = 0xd6d6d6;
const CANVAS_BORDER: u32 = 0xc7c7c7;
const HOVER_BG: u32 = 0xe1e8f3;
const HOVER_BORDER: u32 = 0xc5d3e8;
const ON_BG: u32 = 0xd4e0f1;
const ON_BORDER: u32 = 0xa9bfe0;
const TEXT: u32 = 0x222222;
const RAIL_ICON: u32 = 0x333333;
const MUTED: u32 = 0x666666;
const PANEL_MUTED: u32 = 0x6c757d;
const PANEL_TEXT: u32 = 0x212529;
const DIVIDER: u32 = 0xe2e2e2;
const TAB_TEXT: u32 = 0xe8eef7;
const STATUS_TEXT: u32 = 0xf0f4fa;
const TOAST_BG: u32 = 0x323232;

// Bootstrap Icons codepoints (bootstrap-icons.css).
const ICON_PEN: char = '\u{f4c8}';
const ICON_HAND: char = '\u{f402}';
const ICON_ZOOM_IN: char = '\u{f62c}';
const ICON_ZOOM_OUT: char = '\u{f62d}';
const ICON_FIT: char = '\u{f14a}';
const ICON_UNDO: char = '\u{f117}';
const ICON_REDO: char = '\u{f116}';
const ICON_SAVE: char = '\u{f7d8}';
const ICON_NOTEBOOK: char = '\u{f444}';
const ICON_PENCIL: char = '\u{f4cb}';
const ICON_HIGHLIGHTER: char = '\u{f7f8}';
const ICON_BRUSH: char = '\u{f1d8}';
const ICON_ERASER: char = '\u{f331}';
const ICON_LINE: char = '\u{f657}';
const ICON_SELECT: char = '\u{f1b6}';
const ICON_TRASH: char = '\u{f78b}';
const ICON_PDF: char = '\u{f640}';
const ICON_BLANK: char = '\u{f392}';
const ICON_RULED: char = '\u{f478}';
const ICON_GRID_TEMPLATE: char = '\u{f3f9}';
const ICON_DOTS: char = '\u{f5d4}';
const ICON_PAGE: char = '\u{f38b}';
const ICON_PORTRAIT: char = '\u{f4e7}';
const ICON_LANDSCAPE: char = '\u{f4e4}';
const ICON_NO_PAGE: char = '\u{f63b}';
const ICON_RULERS: char = '\u{f523}';
const ICON_GUIDES: char = '\u{f304}';
const ICON_MAGNET: char = '\u{f779}';
/// Thickness of the ruler strips, in logical pixels.
const RULER_SIZE: f32 = 18.0;
const GUIDE_COLOR: u32 = 0x00a3c4;
const RULER_BG: u32 = 0xf3f4f6;
const RULER_INK: u32 = 0x5a5f66;

const REGULAR_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");
const SEMIBOLD_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSans-SemiBold.ttf");
const ICON_FONT: &[u8] = include_bytes!("../assets/fonts/bootstrap-icons.ttf");

/// Tabs along the top of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Home,
    Draw,
    View,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Home, Tab::Draw, Tab::View];

    fn label(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Draw => "Draw",
            Tab::View => "View",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Ink tools draw strokes. They differ only in how the stroke looks (see [`ink_look`]).
    Pen,
    Pencil,
    Highlighter,
    Brush,
    /// Drag to draw a straight line.
    Line,
    /// Drag over strokes to remove them whole.
    Eraser,
    /// Drag a rectangle to select strokes; Delete removes them.
    Select,
    /// Drag to move the view.
    Pan,
}

impl Tool {
    pub fn is_ink(self) -> bool {
        matches!(self, Tool::Pen | Tool::Pencil | Tool::Highlighter | Tool::Brush)
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Pen => "Pen",
            Tool::Pencil => "Pencil",
            Tool::Highlighter => "Highlighter",
            Tool::Brush => "Brush",
            Tool::Line => "Line",
            Tool::Eraser => "Eraser",
            Tool::Select => "Select",
            Tool::Pan => "Hand",
        }
    }
}

/// Color and width for a stroke drawn with `tool`, given the chosen color and
/// width. Each ink tool is a look on the same stroke format: the file format
/// stores only color and width, so these looks are baked in at commit time.
/// Returns `(rgba, width)`.
pub fn ink_look(tool: Tool, rgba: [u8; 4], base_width: f32) -> ([u8; 4], f32) {
    // (alpha multiplier, width multiplier)
    let (alpha, scale) = match tool {
        Tool::Pencil => (0.85, 0.55),
        Tool::Highlighter => (0.35, 4.5),
        Tool::Brush => (1.0, 2.2),
        _ => (1.0, 1.0),
    };
    let a = (rgba[3] as f32 * alpha).round() as u8;
    ([rgba[0], rgba[1], rgba[2], a], base_width * scale)
}

/// Everything a control can ask the application to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Tab(Tab),
    SetTool(Tool),
    SetColor(usize),
    SetWidth(usize),
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Undo,
    Redo,
    DeleteSelection,
    Save,
    ExportPdf,
    SetPattern(Pattern),
    /// `None` removes the artboard.
    SetPaper(Option<Paper>),
    SetLandscape(bool),
    ToggleRulers,
    ToggleGuides,
    ToggleSnap,
    ClearGuides,
}

/// Interactive state that affects how the chrome looks.
#[derive(Clone, Debug, PartialEq)]
pub struct UiState {
    pub tab: Tab,
    pub tool: Tool,
    pub color: usize,
    pub width: usize,
    pub hover: Option<Action>,
    pub rulers: bool,
    pub guides_visible: bool,
    pub snap: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            tab: Tab::Home,
            tool: Tool::Pen,
            color: 0,
            width: 1,
            hover: None,
            rulers: false,
            guides_visible: true,
            snap: true,
        }
    }
}

/// Read-only facts shown in the status bar and details panel.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub title: String,
    pub file_name: String,
    pub stroke_count: usize,
    pub zoom_percent: u32,
    pub save_status: String,
    pub can_undo: bool,
    pub can_redo: bool,
    pub toast: Option<String>,
    /// At least one stroke is selected.
    pub has_selection: bool,
    /// Rectangle being dragged in the Select tool, in window pixels.
    pub marquee: Option<PxRect>,
    /// Bounding boxes of selected strokes, in window pixels.
    pub selection: Vec<PxRect>,
    /// Background pattern of the notebook.
    pub pattern: Pattern,
    /// The artboard's size in points, and its paper size if it is a standard one.
    pub page: Option<String>,
    pub paper: Option<Paper>,
    /// The artboard is portrait (ignored without one).
    pub page_portrait: bool,
    /// Guides in the notebook, in points.
    pub guides: Vec<Guide>,
    /// Guide being dragged out of a ruler, not yet placed.
    pub guide_preview: Option<Guide>,
    /// Screen pixels per page unit, and the page point at the canvas's top-left.
    pub zoom: f64,
    pub pan: [f64; 2],
}

/// Rectangle in physical window pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PxRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl PxRect {
    pub fn contains(&self, p: [f32; 2]) -> bool {
        p[0] >= self.x && p[0] < self.x + self.w && p[1] >= self.y && p[1] < self.y + self.h
    }

    pub fn center(&self) -> [f32; 2] {
        [self.x + self.w / 2.0, self.y + self.h / 2.0]
    }

    /// `[x, y, w, h]` clamped to non-negative integers, for scissor rects.
    pub fn as_u32(&self) -> [u32; 4] {
        [
            self.x.max(0.0) as u32,
            self.y.max(0.0) as u32,
            self.w.max(0.0) as u32,
            self.h.max(0.0) as u32,
        ]
    }

    fn sk(&self) -> SkRect {
        SkRect::from_xywh(self.x, self.y, self.w, self.h)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Glyph {
    Icon(char),
    /// A line sample showing a nib width (in logical px).
    Stroke(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Tab(&'static str),
    /// Large icon with a label beneath, as in the Print Me Checks ribbon.
    Big { glyph: Glyph, label: &'static str },
    /// A color chip.
    Swatch([u8; 4]),
    /// Compact icon-only button for the left tool rail.
    Rail(char),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub rect: PxRect,
    pub action: Action,
    pub content: Content,
    pub selected: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupBox {
    pub label: &'static str,
    pub label_rect: PxRect,
    pub separator: PxRect,
}

/// Computed geometry for one frame.
#[derive(Clone, Debug)]
pub struct Chrome {
    pub size: (f32, f32),
    pub scale: f32,
    pub tab_bar: PxRect,
    pub ribbon: PxRect,
    pub rail: PxRect,
    pub panel: PxRect,
    pub status: PxRect,
    /// Drawing area. Transparent in the chrome layer, so strokes show through.
    pub canvas: PxRect,
    /// Rulers along the top and left edges of the canvas, when shown.
    pub ruler_top: Option<PxRect>,
    pub ruler_left: Option<PxRect>,
    pub buttons: Vec<Button>,
    pub groups: Vec<GroupBox>,
    pub rail_separators: Vec<PxRect>,
    /// X position where the tabs start, after the app name.
    tabs_x: f32,
}

impl Chrome {
    /// The action of the enabled control under `p`, if any.
    /// The ruler under a window point: `Some(true)` for the vertical ruler, `Some(false)` for the horizontal one.
    pub fn ruler_at(&self, p: [f32; 2]) -> Option<bool> {
        if self.ruler_left.is_some_and(|r| r.contains(p)) {
            return Some(true);
        }
        if self.ruler_top.is_some_and(|r| r.contains(p)) {
            return Some(false);
        }
        None
    }

    pub fn hit(&self, p: [f32; 2]) -> Option<Action> {
        self.buttons
            .iter()
            .rev()
            .find(|b| b.enabled && b.rect.contains(p))
            .map(|b| b.action)
    }
}

/// Loaded typefaces. Fonts are created per size on demand.
pub struct Fonts {
    regular: Typeface,
    semibold: Typeface,
    icons: Typeface,
}

impl Fonts {
    pub fn load() -> Result<Self, String> {
        let mgr = FontMgr::default();
        let load = |name: &str, bytes: &'static [u8]| {
            mgr.new_from_data(Data::new_copy(bytes), None)
                .ok_or_else(|| format!("failed to load bundled font {name}"))
        };
        Ok(Self {
            regular: load("NotoSans-Regular", REGULAR_FONT)?,
            semibold: load("NotoSans-SemiBold", SEMIBOLD_FONT)?,
            icons: load("bootstrap-icons", ICON_FONT)?,
        })
    }

    fn text(&self, size: f32) -> Font {
        Font::from_typeface(self.regular.clone(), size)
    }

    fn semibold(&self, size: f32) -> Font {
        Font::from_typeface(self.semibold.clone(), size)
    }

    fn icon(&self, size: f32) -> Font {
        Font::from_typeface(self.icons.clone(), size)
    }
}

fn text_width(font: &Font, s: &str) -> f32 {
    font.measure_str(s, None).0
}

/// Computes the chrome geometry for a window of `size` physical pixels.
pub fn layout(size: (u32, u32), scale: f32, state: &UiState, info: &Info, fonts: &Fonts) -> Chrome {
    let s = scale;
    let (w, h) = (size.0 as f32, size.1 as f32);
    let tab_h = TAB_BAR_H * s;
    let body_bottom = tab_h + RIBBON_BODY_H * s;
    let status_top = (h - STATUS_BAR_H * s).max(body_bottom);
    let rail_w = (LEFT_RAIL_W * s).min(w);
    let panel_w = (DETAILS_W * s).min(w * 0.35);
    let middle_h = (status_top - body_bottom).max(0.0);

    let ruler = if state.rulers { RULER_SIZE * s } else { 0.0 };
    let mut chrome = Chrome {
        size: (w, h),
        scale: s,
        tab_bar: PxRect { x: 0.0, y: 0.0, w, h: tab_h },
        ribbon: PxRect { x: 0.0, y: 0.0, w, h: body_bottom },
        rail: PxRect { x: 0.0, y: body_bottom, w: rail_w, h: middle_h },
        panel: PxRect { x: w - panel_w, y: body_bottom, w: panel_w, h: middle_h },
        status: PxRect { x: 0.0, y: status_top, w, h: h - status_top },
        canvas: PxRect {
            x: rail_w + ruler,
            y: body_bottom + ruler,
            w: (w - rail_w - panel_w - ruler).max(0.0),
            h: (middle_h - ruler).max(0.0),
        },
        ruler_top: state.rulers.then(|| PxRect {
            x: rail_w + ruler,
            y: body_bottom,
            w: (w - rail_w - panel_w - ruler).max(0.0),
            h: ruler,
        }),
        ruler_left: state.rulers.then(|| PxRect {
            x: rail_w,
            y: body_bottom + ruler,
            w: ruler,
            h: (middle_h - ruler).max(0.0),
        }),
        buttons: Vec::new(),
        groups: Vec::new(),
        rail_separators: Vec::new(),
        tabs_x: 0.0,
    };
    layout_tabs(&mut chrome, state, fonts);
    layout_ribbon(&mut chrome, state, info, fonts);
    layout_rail(&mut chrome, state);
    chrome
}

fn layout_tabs(c: &mut Chrome, state: &UiState, fonts: &Fonts) {
    let s = c.scale;
    let app_icon = fonts.icon(16.0 * s);
    let app_font = fonts.semibold(16.0 * s);
    let icon_w = text_width(&app_icon, &ICON_PEN.to_string());
    let name_w = text_width(&app_font, APP_NAME);
    // Matches the app-name block: 6px padding, 18px margin before the tabs.
    c.tabs_x = 16.0 * s + icon_w + 5.0 * s + name_w + 6.0 * s + 18.0 * s;

    let tab_font = fonts.semibold(14.0 * s);
    let mut x = c.tabs_x;
    for tab in Tab::ALL {
        let label = tab.label();
        let tw = text_width(&tab_font, label) + 32.0 * s;
        c.buttons.push(Button {
            rect: PxRect { x, y: 6.0 * s, w: tw, h: TAB_BAR_H * s - 6.0 * s },
            action: Action::Tab(tab),
            content: Content::Tab(label),
            selected: state.tab == tab,
            enabled: true,
        });
        x += tw + 2.0 * s;
    }
}

/// One control in a ribbon group, before it is placed.
struct Spec {
    action: Action,
    content: Content,
    selected: bool,
    enabled: bool,
}

struct GroupSpec {
    label: &'static str,
    items: Vec<Spec>,
}

fn big(action: Action, glyph: Glyph, label: &'static str, selected: bool, enabled: bool) -> Spec {
    Spec {
        action,
        content: Content::Big { glyph, label },
        selected,
        enabled,
    }
}

fn tool_button(tool: Tool, glyph: char, label: &'static str, state: &UiState) -> Spec {
    big(Action::SetTool(tool), Glyph::Icon(glyph), label, state.tool == tool, true)
}

fn template_button(pattern: Pattern, glyph: char, label: &'static str, info: &Info) -> Spec {
    big(Action::SetPattern(pattern), Glyph::Icon(glyph), label, info.pattern == pattern, true)
}

fn ribbon_specs(state: &UiState, info: &Info) -> Vec<GroupSpec> {
    match state.tab {
        Tab::Home => vec![
            GroupSpec {
                label: "History",
                items: vec![
                    big(Action::Undo, Glyph::Icon(ICON_UNDO), "Undo", false, info.can_undo),
                    big(Action::Redo, Glyph::Icon(ICON_REDO), "Redo", false, info.can_redo),
                ],
            },
            GroupSpec {
                label: "Edit",
                items: vec![big(
                    Action::DeleteSelection,
                    Glyph::Icon(ICON_TRASH),
                    "Delete",
                    false,
                    info.has_selection,
                )],
            },
            GroupSpec {
                label: "Save",
                items: vec![big(Action::Save, Glyph::Icon(ICON_SAVE), "Save", false, true)],
            },
            GroupSpec {
                label: "PDF",
                items: vec![big(Action::ExportPdf, Glyph::Icon(ICON_PDF), "Export", false, true)],
            },
        ],
        Tab::Draw => vec![
            GroupSpec {
                label: "Pens",
                items: vec![
                    tool_button(Tool::Pen, ICON_PEN, "Pen", state),
                    tool_button(Tool::Pencil, ICON_PENCIL, "Pencil", state),
                    tool_button(Tool::Highlighter, ICON_HIGHLIGHTER, "Highlight", state),
                    tool_button(Tool::Brush, ICON_BRUSH, "Brush", state),
                ],
            },
            GroupSpec {
                label: "Tools",
                items: vec![
                    tool_button(Tool::Line, ICON_LINE, "Line", state),
                    tool_button(Tool::Eraser, ICON_ERASER, "Eraser", state),
                    tool_button(Tool::Select, ICON_SELECT, "Select", state),
                    tool_button(Tool::Pan, ICON_HAND, "Hand", state),
                ],
            },
            GroupSpec {
                label: "Color",
                items: PEN_COLORS
                    .iter()
                    .enumerate()
                    .map(|(i, &rgba)| Spec {
                        action: Action::SetColor(i),
                        content: Content::Swatch(rgba),
                        selected: state.color == i,
                        enabled: true,
                    })
                    .collect(),
            },
            GroupSpec {
                label: "Width",
                items: PEN_WIDTHS
                    .iter()
                    .enumerate()
                    .map(|(i, &width)| {
                        big(Action::SetWidth(i), Glyph::Stroke(width), PEN_WIDTH_NAMES[i], state.width == i, true)
                    })
                    .collect(),
            },
            GroupSpec {
                label: "Template",
                items: vec![
                    template_button(Pattern::Blank, ICON_BLANK, "Blank", info),
                    template_button(Pattern::Ruled, ICON_RULED, "Ruled", info),
                    template_button(Pattern::Grid, ICON_GRID_TEMPLATE, "Grid", info),
                    template_button(Pattern::Dot, ICON_DOTS, "Dots", info),
                ],
            },
        ],
        Tab::View => vec![GroupSpec {
            label: "Zoom",
            items: vec![
                big(Action::ZoomIn, Glyph::Icon(ICON_ZOOM_IN), "Zoom in", false, true),
                big(Action::ZoomOut, Glyph::Icon(ICON_ZOOM_OUT), "Zoom out", false, true),
                big(Action::ZoomReset, Glyph::Icon(ICON_FIT), "100%", false, true),
            ],
        }, GroupSpec {
            label: "Guides",
            items: vec![
                big(Action::ToggleRulers, Glyph::Icon(ICON_RULERS), "Rulers", state.rulers, true),
                big(Action::ToggleGuides, Glyph::Icon(ICON_GUIDES), "Show", state.guides_visible, true),
                big(Action::ToggleSnap, Glyph::Icon(ICON_MAGNET), "Snap", state.snap, true),
                big(Action::ClearGuides, Glyph::Icon(ICON_TRASH), "Clear", false, !info.guides.is_empty()),
            ],
        }, GroupSpec {
            label: "Page",
            items: vec![
                big(Action::SetPaper(Some(Paper::Letter)), Glyph::Icon(ICON_PAGE), "Letter", info.paper == Some(Paper::Letter), true),
                big(Action::SetPaper(Some(Paper::Legal)), Glyph::Icon(ICON_PAGE), "Legal", info.paper == Some(Paper::Legal), true),
                big(Action::SetPaper(Some(Paper::Tabloid)), Glyph::Icon(ICON_PAGE), "Tabloid", info.paper == Some(Paper::Tabloid), true),
                big(Action::SetPaper(None), Glyph::Icon(ICON_NO_PAGE), "No page", info.page.is_none(), true),
                big(Action::SetLandscape(false), Glyph::Icon(ICON_PORTRAIT), "Portrait", info.page.is_some() && info.page_portrait, info.page.is_some()),
                big(Action::SetLandscape(true), Glyph::Icon(ICON_LANDSCAPE), "Landscape", info.page.is_some() && !info.page_portrait, info.page.is_some()),
            ],
        }],
    }
}

const BIG_MIN_W: f32 = 62.0;
const BIG_ICON_H: f32 = 26.0;
const BIG_LABEL_SIZE: f32 = 12.0;
const BIG_LINE_H: f32 = 13.8;
const SWATCH_SIZE: f32 = 24.0;
const RAIL_BUTTON: f32 = 34.0;

/// Logical-pixel size of a control; the caller multiplies by the scale.
fn content_size(content: &Content, fonts: &Fonts, s: f32) -> (f32, f32) {
    match content {
        Content::Big { label, .. } => {
            let font = fonts.text(BIG_LABEL_SIZE * s);
            let lines = label.split('\n').count() as f32;
            let text_w = label.split('\n').map(|l| text_width(&font, l)).fold(0.0, f32::max);
            let w = (text_w + 12.0 * s).max(BIG_MIN_W * s);
            let h = 4.0 * s + BIG_ICON_H * s + 3.0 * s + lines * BIG_LINE_H * s + 4.0 * s;
            (w, h)
        }
        Content::Swatch(_) => (SWATCH_SIZE * s, SWATCH_SIZE * s),
        Content::Rail(_) => (RAIL_BUTTON * s, RAIL_BUTTON * s),
        Content::Tab(_) => (0.0, 0.0),
    }
}

fn layout_ribbon(c: &mut Chrome, state: &UiState, info: &Info, fonts: &Fonts) {
    let s = c.scale;
    let gap = 4.0 * s;
    let label_h = 16.0 * s;
    let area_top = c.tab_bar.h + 6.0 * s;
    let area_bottom = c.ribbon.h - 2.0 * s - label_h;
    let label_font = fonts.text(11.5 * s);

    let mut x = 6.0 * s;
    for group in ribbon_specs(state, info) {
        let sizes: Vec<(f32, f32)> = group
            .items
            .iter()
            .map(|it| content_size(&it.content, fonts, s))
            .collect();
        let items_w: f32 = sizes.iter().map(|sz| sz.0).sum::<f32>()
            + gap * sizes.len().saturating_sub(1) as f32;
        let label_w = text_width(&label_font, group.label);
        let group_w = items_w.max(label_w) + 20.0 * s;

        let mut ix = x + 10.0 * s;
        for (item, (iw, ih)) in group.items.into_iter().zip(sizes) {
            let iy = area_top + (area_bottom - area_top - ih) / 2.0;
            c.buttons.push(Button {
                rect: PxRect { x: ix, y: iy, w: iw, h: ih },
                action: item.action,
                content: item.content,
                selected: item.selected,
                enabled: item.enabled,
            });
            ix += iw + gap;
        }

        c.groups.push(GroupBox {
            label: group.label,
            label_rect: PxRect {
                x,
                y: c.ribbon.h - label_h,
                w: group_w,
                h: label_h,
            },
            separator: PxRect {
                x: x + group_w,
                y: area_top - 6.0 * s,
                w: 1.0 * s,
                h: c.ribbon.h - area_top,
            },
        });
        x += group_w + 1.0 * s;
    }
}

fn layout_rail(c: &mut Chrome, state: &UiState) {
    let s = c.scale;
    let x = (c.rail.w - RAIL_BUTTON * s) / 2.0;
    let mut y = c.rail.y + 8.0 * s;
    let entries: Vec<Option<(Action, char, bool, bool)>> = vec![
        Some((Action::SetTool(Tool::Pen), ICON_PEN, state.tool.is_ink(), true)),
        Some((Action::SetTool(Tool::Eraser), ICON_ERASER, state.tool == Tool::Eraser, true)),
        Some((Action::SetTool(Tool::Select), ICON_SELECT, state.tool == Tool::Select, true)),
        Some((Action::SetTool(Tool::Pan), ICON_HAND, state.tool == Tool::Pan, true)),
        None,
        Some((Action::ZoomIn, ICON_ZOOM_IN, false, true)),
        Some((Action::ZoomOut, ICON_ZOOM_OUT, false, true)),
        Some((Action::ZoomReset, ICON_FIT, false, true)),
        None,
        Some((Action::Save, ICON_SAVE, false, true)),
    ];
    for entry in entries {
        match entry {
            Some((action, glyph, selected, enabled)) => {
                c.buttons.push(Button {
                    rect: PxRect { x, y, w: RAIL_BUTTON * s, h: RAIL_BUTTON * s },
                    action,
                    content: Content::Rail(glyph),
                    selected,
                    enabled,
                });
                y += RAIL_BUTTON * s + 3.0 * s;
            }
            None => {
                c.rail_separators.push(PxRect {
                    x: (c.rail.w - 26.0 * s) / 2.0,
                    y: y + 4.0 * s,
                    w: 26.0 * s,
                    h: 1.0 * s,
                });
                y += 9.0 * s + 3.0 * s;
            }
        }
    }
}

fn color(hex: u32, alpha: f32) -> Color {
    Color::from_argb(
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        (hex >> 16) as u8,
        (hex >> 8) as u8,
        hex as u8,
    )
}

fn paint_for(hex: u32, alpha: f32, style: PaintStyle, stroke_w: f32) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(hex, alpha));
    paint.set_style(style);
    if style == PaintStyle::Stroke {
        paint.set_stroke_width(stroke_w);
    }
    paint
}

fn fill_rect(canvas: &Canvas, r: PxRect, hex: u32) {
    canvas.draw_rect(r.sk(), &paint_for(hex, 1.0, PaintStyle::Fill, 0.0));
}

fn fill_round(canvas: &Canvas, r: PxRect, radius: f32, hex: u32, alpha: f32) {
    canvas.draw_round_rect(r.sk(), radius, radius, &paint_for(hex, alpha, PaintStyle::Fill, 0.0));
}

fn stroke_round(canvas: &Canvas, r: PxRect, radius: f32, hex: u32, alpha: f32, width: f32) {
    // Inset by half the stroke so the line sits inside the rectangle.
    let inset = width / 2.0;
    let inner = PxRect {
        x: r.x + inset,
        y: r.y + inset,
        w: r.w - width,
        h: r.h - width,
    };
    canvas.draw_round_rect(inner.sk(), radius, radius, &paint_for(hex, alpha, PaintStyle::Stroke, width));
}

fn draw_text_at(canvas: &Canvas, text: &str, x: f32, cy: f32, font: &Font, hex: u32, alpha: f32) {
    let m = font.metrics().1;
    let baseline = cy - (m.ascent + m.descent) / 2.0;
    canvas.draw_str(text, (x, baseline), font, &paint_for(hex, alpha, PaintStyle::Fill, 0.0));
}

fn draw_text_centered(canvas: &Canvas, text: &str, cx: f32, cy: f32, font: &Font, hex: u32, alpha: f32) {
    let w = text_width(font, text);
    draw_text_at(canvas, text, cx - w / 2.0, cy, font, hex, alpha);
}

fn draw_text_right(canvas: &Canvas, text: &str, right: f32, cy: f32, font: &Font, hex: u32, alpha: f32) -> f32 {
    let w = text_width(font, text);
    draw_text_at(canvas, text, right - w, cy, font, hex, alpha);
    w
}

/// Letter-spaced text, used for the uppercase panel headers.
fn draw_spaced(canvas: &Canvas, text: &str, x: f32, cy: f32, font: &Font, hex: u32, spacing: f32) {
    let mut cx = x;
    for ch in text.chars() {
        let s = ch.to_string();
        draw_text_at(canvas, &s, cx, cy, font, hex, 1.0);
        cx += text_width(font, &s) + spacing;
    }
}

fn paint_button(canvas: &Canvas, btn: &Button, state: &UiState, fonts: &Fonts, s: f32) {
    let hovered = btn.enabled && state.hover == Some(btn.action);
    let alpha = if btn.enabled { 1.0 } else { 0.4 };
    let r = btn.rect;

    match &btn.content {
        Content::Tab(_) => {}
        Content::Big { glyph, label } => {
            frame(canvas, r, btn.selected, hovered, alpha, 4.0 * s);
            let cx = r.x + r.w / 2.0;
            let icon_cy = r.y + 4.0 * s + BIG_ICON_H * s / 2.0;
            match glyph {
                Glyph::Icon(ch) => {
                    let font = fonts.icon(BIG_ICON_H * s);
                    draw_text_centered(canvas, &ch.to_string(), cx, icon_cy, &font, BRAND, alpha);
                }
                Glyph::Stroke(width) => {
                    let thickness = (width * s * 0.9).clamp(1.0 * s, 8.0 * s);
                    let half = 12.0 * s;
                    canvas.draw_line(
                        (cx - half, icon_cy),
                        (cx + half, icon_cy),
                        &paint_for(BRAND, alpha, PaintStyle::Stroke, thickness),
                    );
                }
            }
            let font = fonts.text(BIG_LABEL_SIZE * s);
            let first_line_cy = r.y + 4.0 * s + BIG_ICON_H * s + 3.0 * s + BIG_LINE_H * s / 2.0;
            for (i, line) in label.split('\n').enumerate() {
                let cy = first_line_cy + i as f32 * BIG_LINE_H * s;
                draw_text_centered(canvas, line, cx, cy, &font, TEXT, alpha);
            }
        }
        Content::Swatch(rgba) => {
            let [red, green, blue, a] = *rgba;
            let chip = PxRect {
                x: r.x + 4.0 * s,
                y: r.y + 4.0 * s,
                w: r.w - 8.0 * s,
                h: r.h - 8.0 * s,
            };
            let fill = Color::from_argb(a, red, green, blue);
            let mut paint = Paint::default();
            paint.set_anti_alias(true);
            paint.set_color(fill);
            canvas.draw_round_rect(chip.sk(), 3.0 * s, 3.0 * s, &paint);
            if btn.selected {
                stroke_round(canvas, r, 4.0 * s, BRAND, alpha, 2.0 * s);
            } else if hovered {
                stroke_round(canvas, r, 4.0 * s, HOVER_BORDER, alpha, 1.0 * s);
            }
        }
        Content::Rail(ch) => {
            frame(canvas, r, btn.selected, hovered, alpha, 4.0 * s);
            let font = fonts.icon(18.0 * s);
            let hex = if btn.selected { BRAND } else { RAIL_ICON };
            let [cx, cy] = r.center();
            draw_text_centered(canvas, &ch.to_string(), cx, cy, &font, hex, alpha);
        }
    }
}

fn frame(canvas: &Canvas, r: PxRect, selected: bool, hovered: bool, alpha: f32, radius: f32) {
    if selected {
        fill_round(canvas, r, radius, ON_BG, alpha);
        stroke_round(canvas, r, radius, ON_BORDER, alpha, 1.0);
    } else if hovered {
        fill_round(canvas, r, radius, HOVER_BG, alpha);
        stroke_round(canvas, r, radius, HOVER_BORDER, alpha, 1.0);
    }
}

fn paint_tab_bar(canvas: &Canvas, c: &Chrome, state: &UiState, fonts: &Fonts, s: f32) {
    fill_rect(canvas, c.tab_bar, BRAND);

    let white = 0xffffff;
    let app_cy = c.tab_bar.h * 0.5 + 2.0 * s;
    draw_text_at(canvas, &ICON_PEN.to_string(), 16.0 * s, app_cy, &fonts.icon(16.0 * s), white, 1.0);
    let icon_w = text_width(&fonts.icon(16.0 * s), &ICON_PEN.to_string());
    draw_text_at(
        canvas,
        APP_NAME,
        16.0 * s + icon_w + 5.0 * s,
        app_cy,
        &fonts.semibold(16.0 * s),
        white,
        1.0,
    );

    let tab_font = fonts.semibold(14.0 * s);
    let tab_font_regular = fonts.text(14.0 * s);
    for btn in c.buttons.iter().filter(|b| matches!(b.content, Content::Tab(_))) {
        let Content::Tab(label) = btn.content else { continue };
        let r = btn.rect;
        // Extend the active tab down so it merges with the ribbon body.
        let fill_rect_px = PxRect { h: r.h + 1.0 * s, ..r };
        let hovered = state.hover == Some(btn.action);
        if btn.selected {
            fill_round(canvas, fill_rect_px, 4.0 * s, RIBBON_BG, 1.0);
            draw_text_centered(canvas, label, r.x + r.w / 2.0, r.y + r.h / 2.0, &tab_font, BRAND, 1.0);
        } else {
            if hovered {
                fill_round(canvas, r, 4.0 * s, 0xffffff, 0.15);
            }
            draw_text_centered(canvas, label, r.x + r.w / 2.0, r.y + r.h / 2.0, &tab_font_regular, TAB_TEXT, 1.0);
        }
    }
}

fn paint_ribbon(canvas: &Canvas, c: &Chrome, state: &UiState, fonts: &Fonts, s: f32) {
    let body = PxRect {
        x: 0.0,
        y: c.tab_bar.h,
        w: c.ribbon.w,
        h: c.ribbon.h - c.tab_bar.h,
    };
    fill_rect(canvas, body, RIBBON_BG);
    fill_rect(canvas, PxRect { x: 0.0, y: c.ribbon.h - 1.0 * s, w: c.ribbon.w, h: 1.0 * s }, BORDER);

    let label_font = fonts.text(11.5 * s);
    for g in &c.groups {
        fill_rect(canvas, g.separator, BORDER);
        let [cx, cy] = g.label_rect.center();
        draw_text_centered(canvas, g.label, cx, cy, &label_font, MUTED, 1.0);
    }
    for btn in c.buttons.iter().filter(|b| matches!(b.content, Content::Big { .. } | Content::Swatch(_))) {
        paint_button(canvas, btn, state, fonts, s);
    }
}

fn paint_rail(canvas: &Canvas, c: &Chrome, state: &UiState, fonts: &Fonts, s: f32) {
    fill_rect(canvas, c.rail, RAIL_BG);
    fill_rect(
        canvas,
        PxRect { x: c.rail.x + c.rail.w - 1.0 * s, w: 1.0 * s, ..c.rail },
        BORDER,
    );
    for sep in &c.rail_separators {
        fill_rect(canvas, *sep, BORDER);
    }
    for btn in c.buttons.iter().filter(|b| matches!(b.content, Content::Rail(_))) {
        paint_button(canvas, btn, state, fonts, s);
    }
}

fn paint_canvas_frame(canvas: &Canvas, c: &Chrome, s: f32) {
    stroke_round(canvas, c.canvas, 0.0, CANVAS_BORDER, 1.0, 1.0 * s);
}

/// Pale strips with tick marks and labels along the top and left edges of the canvas.
fn paint_rulers(canvas: &Canvas, c: &Chrome, info: &Info, fonts: &Fonts, s: f32) {
    let font = fonts.text(9.5 * s);
    let step = guides::tick_step(info.zoom);
    if let Some(top) = c.ruler_top {
        fill_round(canvas, top, 0.0, RULER_BG, 1.0);
        let from = info.pan[0];
        let to = info.pan[0] + c.canvas.w as f64 / info.zoom;
        for v in guides::ticks(from, to, step) {
            let x = (c.canvas.x as f64 + (v - info.pan[0]) * info.zoom) as f32;
            let labeled = guides::is_labeled(v);
            let len = if labeled { 9.0 * s } else { 4.0 * s };
            let tick = PxRect { x, y: top.y + top.h - len, w: 1.0 * s, h: len };
            fill_round(canvas, tick, 0.0, RULER_INK, 1.0);
            if labeled {
                draw_text_at(canvas, &guides::format_inches(v), x + 2.0 * s, top.y + 5.0 * s, &font, RULER_INK, 1.0);
            }
        }
        let edge = PxRect { x: top.x, y: top.y + top.h - 1.0 * s, w: top.w, h: 1.0 * s };
        fill_round(canvas, edge, 0.0, CANVAS_BORDER, 1.0);
    }
    if let Some(left) = c.ruler_left {
        fill_round(canvas, left, 0.0, RULER_BG, 1.0);
        let from = info.pan[1];
        let to = info.pan[1] + c.canvas.h as f64 / info.zoom;
        for v in guides::ticks(from, to, step) {
            let y = (c.canvas.y as f64 + (v - info.pan[1]) * info.zoom) as f32;
            let labeled = guides::is_labeled(v);
            let len = if labeled { 9.0 * s } else { 4.0 * s };
            let tick = PxRect { x: left.x + left.w - len, y, w: len, h: 1.0 * s };
            fill_round(canvas, tick, 0.0, RULER_INK, 1.0);
            if labeled {
                // Labels run up the strip, so they are rotated a quarter turn.
                canvas.save();
                canvas.translate((left.x + 5.0 * s, y + 2.0 * s));
                canvas.rotate(-90.0, None);
                draw_text_at(canvas, &guides::format_inches(v), 0.0, 0.0, &font, RULER_INK, 1.0);
                canvas.restore();
            }
        }
        let edge = PxRect { x: left.x + left.w - 1.0 * s, y: left.y, w: 1.0 * s, h: left.h };
        fill_round(canvas, edge, 0.0, CANVAS_BORDER, 1.0);
    }
}

/// Guide lines across the canvas. The guide being dragged out of a ruler is always shown.
fn paint_guides(canvas: &Canvas, c: &Chrome, info: &Info, state: &UiState, s: f32) {
    let mut lines: Vec<Guide> = if state.guides_visible { info.guides.clone() } else { Vec::new() };
    lines.extend(info.guide_preview);
    let area = c.canvas;
    for g in lines {
        let rect = if g.vertical {
            let x = area.x + ((g.pos - info.pan[0]) * info.zoom) as f32;
            if x < area.x || x > area.x + area.w {
                continue;
            }
            PxRect { x, y: area.y, w: 1.0 * s, h: area.h }
        } else {
            let y = area.y + ((g.pos - info.pan[1]) * info.zoom) as f32;
            if y < area.y || y > area.y + area.h {
                continue;
            }
            PxRect { x: area.x, y, w: area.w, h: 1.0 * s }
        };
        fill_round(canvas, rect, 0.0, GUIDE_COLOR, 1.0);
    }
}

/// Selection boxes and the rubber-band rectangle, drawn over the canvas.
fn paint_selection(canvas: &Canvas, info: &Info, s: f32) {
    for rect in &info.selection {
        stroke_round(canvas, *rect, 0.0, BRAND, 1.0, 1.0 * s);
    }
    if let Some(marquee) = info.marquee {
        fill_round(canvas, marquee, 0.0, BRAND, 0.08);
        stroke_round(canvas, marquee, 0.0, BRAND, 1.0, 1.0 * s);
    }
}

fn paint_details(canvas: &Canvas, c: &Chrome, state: &UiState, info: &Info, fonts: &Fonts, s: f32) {
    let p = c.panel;
    fill_rect(canvas, p, PANEL_BG);
    fill_rect(canvas, PxRect { x: p.x, y: p.y, w: 1.0 * s, h: p.h }, BORDER);

    let pad = 14.0 * s;
    let x = p.x + pad;
    let width = p.w - 2.0 * pad;
    let header_font = fonts.semibold(11.5 * s);
    let body = fonts.text(14.0 * s);
    let muted = fonts.text(12.5 * s);
    let mut y = p.y + 12.0 * s;

    let header = |canvas: &Canvas, y: f32, text: &str| {
        draw_spaced(canvas, &text.to_uppercase(), x, y + 9.0 * s, &header_font, BRAND, 0.6 * s);
        y + 22.0 * s
    };
    let line = |canvas: &Canvas, y: f32, text: &str, font: &Font, hex: u32| {
        draw_text_at(canvas, text, x, y + 10.0 * s, font, hex, 1.0);
        y + 20.0 * s
    };
    let divider = |canvas: &Canvas, y: f32| {
        fill_rect(canvas, PxRect { x: p.x + pad, y, w: width, h: 1.0 * s }, DIVIDER);
        y + 14.0 * s
    };

    y = header(canvas, y, "Notebook");
    y = line(canvas, y, &info.title, &fonts.semibold(14.0 * s), PANEL_TEXT);
    y = line(canvas, y, &info.file_name, &muted, PANEL_MUTED);
    y = line(canvas, y, &format!("{} strokes", info.stroke_count), &body, PANEL_TEXT);
    y = divider(canvas, y);

    y = header(canvas, y, "Pen");
    let swatch = PEN_COLORS[state.color];
    let dot = Color::from_argb(swatch[3], swatch[0], swatch[1], swatch[2]);
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(dot);
    canvas.draw_circle((x + 7.0 * s, y + 9.0 * s), 6.0 * s, &paint);
    draw_text_at(
        canvas,
        PEN_COLOR_NAMES[state.color],
        x + 22.0 * s,
        y + 9.0 * s,
        &body,
        PANEL_TEXT,
        1.0,
    );
    y += 24.0 * s;
    y = line(canvas, y, &format!("{} width", PEN_WIDTH_NAMES[state.width]), &muted, PANEL_MUTED);
    y = divider(canvas, y);

    y = header(canvas, y, "View");
    y = line(canvas, y, &format!("Zoom {}%", info.zoom_percent), &body, PANEL_TEXT);
    let y = line(canvas, y, &format!("Tool: {}", state.tool.label()), &muted, PANEL_MUTED);
    let y = line(canvas, y, &format!("Template: {}", info.pattern.label()), &muted, PANEL_MUTED);
    if let Some(page) = &info.page {
        line(canvas, y, &format!("Page: {page}"), &muted, PANEL_MUTED);
    }
}

fn paint_status(canvas: &Canvas, c: &Chrome, info: &Info, fonts: &Fonts, s: f32) {
    let bar = c.status;
    fill_rect(canvas, bar, BRAND);
    let font = fonts.text(12.0 * s);
    let icon = fonts.icon(12.0 * s);
    let cy = bar.y + bar.h / 2.0;

    let left = 12.0 * s;
    draw_text_at(canvas, &ICON_NOTEBOOK.to_string(), left, cy, &icon, STATUS_TEXT, 1.0);
    let icon_w = text_width(&icon, &ICON_NOTEBOOK.to_string());
    let title_x = left + icon_w + 6.0 * s;
    draw_text_at(canvas, &info.title, title_x, cy, &font, STATUS_TEXT, 1.0);
    let title_w = text_width(&font, &info.title);
    draw_text_at(
        canvas,
        &format!("{} strokes", info.stroke_count),
        title_x + title_w + 18.0 * s,
        cy,
        &font,
        STATUS_TEXT,
        1.0,
    );

    let mut right = bar.w - 12.0 * s;
    right -= draw_text_right(canvas, &info.save_status, right, cy, &font, STATUS_TEXT, 1.0) + 18.0 * s;
    draw_text_right(canvas, &format!("Zoom {}%", info.zoom_percent), right, cy, &font, STATUS_TEXT, 1.0);
}

fn paint_toast(canvas: &Canvas, c: &Chrome, info: &Info, fonts: &Fonts, s: f32) {
    let Some(message) = &info.toast else { return };
    let font = fonts.text(14.0 * s);
    let w = text_width(&font, message) + 36.0 * s;
    let h = 34.0 * s;
    let rect = PxRect {
        x: (c.size.0 - w) / 2.0,
        y: c.status.y - 36.0 * s - h,
        w,
        h,
    };
    fill_round(canvas, rect, 6.0 * s, TOAST_BG, 1.0);
    let [cx, cy] = rect.center();
    draw_text_centered(canvas, message, cx, cy, &font, 0xffffff, 1.0);
}

/// Paints the whole chrome into a window-sized premultiplied RGBA8 buffer.
/// The canvas area is left transparent.
pub fn paint(c: &Chrome, state: &UiState, info: &Info, fonts: &Fonts) -> Option<Vec<u8>> {
    let (w, h) = (c.size.0 as i32, c.size.1 as i32);
    if w <= 0 || h <= 0 {
        return None;
    }
    let s = c.scale;
    let image = ImageInfo::new((w, h), ColorType::RGBA8888, AlphaType::Premul, None);
    let mut surface = surfaces::raster(&image, None, None)?;
    {
        let canvas = surface.canvas();
        canvas.clear(Color::TRANSPARENT);
        paint_ribbon(canvas, c, state, fonts, s);
        paint_tab_bar(canvas, c, state, fonts, s);
        paint_rail(canvas, c, state, fonts, s);
        paint_canvas_frame(canvas, c, s);
        paint_guides(canvas, c, info, state, s);
        paint_rulers(canvas, c, info, fonts, s);
        paint_selection(canvas, info, s);
        paint_details(canvas, c, state, info, fonts, s);
        paint_status(canvas, c, info, fonts, s);
        paint_toast(canvas, c, info, fonts, s);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    surface
        .read_pixels(&image, &mut pixels, (w * 4) as usize, (0, 0))
        .then_some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(can_undo: bool) -> Info {
        Info {
            title: "Floor plan".into(),
            file_name: "Floor plan.ink".into(),
            stroke_count: 12,
            zoom_percent: 100,
            save_status: "All changes saved".into(),
            can_undo,
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
            page: None,
            paper: None,
            page_portrait: true,
        }
    }

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    }

    #[test]
    fn rulers_inset_the_canvas_and_are_hit_testable() {
        let fonts = Fonts::load().unwrap();
        let on = UiState { rulers: true, ..UiState::default() };
        let off = layout((1280, 800), 1.0, &UiState::default(), &info(true), &fonts);
        let c = layout((1280, 800), 1.0, &on, &info(true), &fonts);
        assert_eq!(c.canvas.x, off.canvas.x + RULER_SIZE);
        assert_eq!(c.canvas.y, off.canvas.y + RULER_SIZE);
        let top = c.ruler_top.expect("top ruler");
        let left = c.ruler_left.expect("left ruler");
        assert_eq!(c.ruler_at([top.x + 5.0, top.y + 2.0]), Some(false));
        assert_eq!(c.ruler_at([left.x + 2.0, left.y + 5.0]), Some(true));
        assert_eq!(c.ruler_at([c.canvas.x + 5.0, c.canvas.y + 5.0]), None);
    }

    #[test]
    fn bundled_fonts_load() {
        Fonts::load().expect("bundled fonts should load");
    }

    #[test]
    fn canvas_sits_between_rail_and_details_panel() {
        let fonts = Fonts::load().unwrap();
        let c = layout((1280, 800), 1.0, &UiState::default(), &info(true), &fonts);
        assert_eq!(c.canvas.x, c.rail.w);
        assert_eq!(c.canvas.x + c.canvas.w, c.panel.x);
        assert_eq!(c.canvas.y, c.ribbon.h);
        assert_eq!(c.canvas.y + c.canvas.h, c.status.y);
        assert!(c.canvas.w > 800.0);
    }

    #[test]
    fn hit_test_respects_enabled_state() {
        let fonts = Fonts::load().unwrap();
        let on = layout((1280, 800), 1.0, &UiState::default(), &info(true), &fonts);
        let undo = on.buttons.iter().find(|b| b.action == Action::Undo).unwrap();
        assert_eq!(on.hit(undo.rect.center()), Some(Action::Undo));

        let off = layout((1280, 800), 1.0, &UiState::default(), &info(false), &fonts);
        let undo = off.buttons.iter().find(|b| b.action == Action::Undo).unwrap();
        assert_eq!(off.hit(undo.rect.center()), None);
    }

    #[test]
    fn hit_test_finds_tabs_and_swatches() {
        let fonts = Fonts::load().unwrap();
        let state = UiState { tab: Tab::Draw, ..UiState::default() };
        let c = layout((1280, 800), 1.0, &state, &info(false), &fonts);
        let blue = c.buttons.iter().find(|b| b.action == Action::SetColor(1)).unwrap();
        assert_eq!(c.hit(blue.rect.center()), Some(Action::SetColor(1)));
        let draw_tab = c.buttons.iter().find(|b| b.action == Action::Tab(Tab::Draw)).unwrap();
        assert_eq!(c.hit(draw_tab.rect.center()), Some(Action::Tab(Tab::Draw)));
    }

    #[test]
    fn paint_matches_print_me_checks_regions() {
        let fonts = Fonts::load().unwrap();
        let c = layout((1280, 800), 1.0, &UiState::default(), &info(true), &fonts);
        let pixels = paint(&c, &UiState::default(), &info(true), &fonts).expect("chrome paints");
        assert_eq!(pixels.len(), 1280 * 800 * 4);

        // Status bar is the brand blue, fully opaque.
        assert_eq!(pixel(&pixels, 1280, 640, 790), [0x2b, 0x57, 0x9a, 255]);
        // Tab bar is brand blue behind the app name.
        assert_eq!(pixel(&pixels, 1280, 1000, 4), [0x2b, 0x57, 0x9a, 255]);
        // The canvas is transparent so strokes and tiles show through.
        let cx = (c.canvas.x + c.canvas.w / 2.0) as u32;
        let cy = (c.canvas.y + c.canvas.h / 2.0) as u32;
        assert_eq!(pixel(&pixels, 1280, cx, cy)[3], 0);
    }

    #[test]
    fn scale_factor_scales_chrome_heights() {
        let fonts = Fonts::load().unwrap();
        let one = layout((1280, 800), 1.0, &UiState::default(), &info(true), &fonts);
        let two = layout((2560, 1600), 2.0, &UiState::default(), &info(true), &fonts);
        assert!((two.ribbon.h - one.ribbon.h * 2.0).abs() < 1e-3);
        assert!((two.canvas.x - one.canvas.x * 2.0).abs() < 1e-3);
    }
}
