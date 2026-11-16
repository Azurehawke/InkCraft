# InkCraft

GPU-accelerated handwriting and sketching app in Rust. This is the first skeleton:
a desktop build on `winit` + `wgpu`, with Skia rasterizing on the CPU and a tile
cache on the GPU.

## Build and run

Requires a recent stable Rust toolchain (tested with 1.97) and a graphics driver
that `wgpu` can use.

    cargo run -p inkcraft -- path/to/Notebook.ink

With no argument the notebook is `Untitled.ink` in `$INKCRAFT_HOME`, or
`~/.inkcraft` if that is unset.

Controls: left mouse or pen draws. Middle-drag pans. Mouse wheel zooms around the
cursor. Pan and zoom are ignored while a stroke is in progress.

## Test

    cargo test
    cargo clippy --all-targets

## Layout

| File | Responsibility |
|------|----------------|
| `inkcraft/src/main.rs` | winit `ApplicationHandler`, input translation, frame loop |
| `inkcraft/src/renderer.rs` | Skia rasterization, tile scheduler and raster worker, wgpu compositing |
| `inkcraft/src/spatial.rs` | `rstar` index over stroke bounds for viewport queries |
| `inkcraft/src/input.rs` | Bounded input queue, pressure mapping, Catmull-Rom smoothing |
| `inkcraft/src/storage.rs` | `.ink` binary format, atomic writes, debounced autosave, SQLite metadata |
| `inkcraft/src/model.rs` | Document types (`Notebook`, `Stroke`) and `Camera` |
| `inkcraft/src/lib.rs` | Module wiring so logic can be tested without a display |

## Rendering model

* Strokes are stored as raw accepted samples. Smoothing is a pure function of
  those samples, so files never contain derived geometry.
* A stroke that has just been committed, or has not yet been rasterized into
  every tile it touches, is drawn through a viewport-sized overlay.
* Settled ink is rasterized in 512-unit page tiles on a background thread and
  uploaded to `wgpu` textures. Each frame draws only the tiles that intersect
  the viewport.
* Tiles are rasterized when a stroke ends, or once the page has been idle for
  300 ms.

## File format

`INK1` magic, `u16` format version, `u64` payload length, then a
bincode 2 (standard config) body. Saves write `*.ink.tmp`, fsync, then rename.

Version 5 (current) adds `page`, the artboard size. Version 4 adds `guides`, the guide lines. Version 3 adds `pattern`,
the background template (blank, ruled, grid, dots). Version 2 adds `images`,
the imported pages. Each entry is a page
rectangle plus its PNG bytes. Versions 1 to 3 still load. The migration is
covered by a unit test. Files saved by this version can't be read
by older builds.

## Deviations from the spec

These are deliberate or forced by the dependencies:

* **bincode 3.0.0 is not used.** It is a tombstone release on crates.io. Its
  README says development has ceased and its `lib.rs` is a compile error. This
  build uses `bincode` 2.0.1, the last working release. Consider `postcard` or
  `rkyv` for new data.
* **Skia rasterizes on the CPU.** `skia-safe` has its own GPU backends. Sharing a
  `wgpu` device with them means reaching into raw Vulkan, Metal or GL handles.
  The current path rasterizes on the CPU into a texture upload.
* **No stylus tilt, and pressure only where winit reports it.** winit 0.30 has no
  tablet event type. Pressure comes from touch `Force` when the platform reports
  it, otherwise full pressure. Real pen pressure on Windows (WM_POINTER or
  Wintab) and Linux (Wayland tablet or XInput2) needs a platform layer.
* **"Bincode compression" is not implemented.** bincode does not compress. A
  zstd or lz4 layer would be a separate step.
* **Smoothing lags by one pen sample.** A Catmull-Rom segment needs the next
  sample as its tangent control point.
* **Tiles are rasterized at zoom 1.** When zoomed in, the textures are magnified
  and look soft. Re-rasterizing at zoom buckets is a follow-up.
* **Android is not built.** winit's Android path and `skia-safe`'s Android
  build need their own setup.
* **Single page.** The model is one infinite canvas. Multi-page notebooks are
  not implemented.
* **Autosave clones the notebook.** Each committed stroke clones the whole
  document for the save worker. Fine for small notebooks; incremental saves are
  the next step.

## Verified

* `cargo test`: 38 unit tests pass, including the PDF export, a PDF import of the
  exported file (one page, PNG output), and loading a version 1 file. They cover smoothing, pressure mapping,
  camera zoom anchoring, R-tree culling, the file format (round trip, bad magic,
  unsupported version, truncation), atomic save and load, debounced autosave
  under a burst of requests, the SQLite hierarchy and tag queries, Skia tile
  rasterization, and the tile scheduler's dirty and cached sequencing.
* `cargo clippy --all-targets`: no warnings.
* End to end on Linux under Xvfb with synthetic pointer and wheel input: strokes
  rendered through wgpu, zoom anchored at the cursor, and the notebook autosaved
  with a valid `INK1` header and a `notebooks` row in SQLite.

## Not verified

Windows, macOS, Android, real pen hardware, HiDPI scaling, and performance on
large notebooks.

## Interface

The chrome follows the look of Print Me Checks (Azurehawke/printmechecks), using the same palette and layout:

- Blue title and tab bar (`#2b579a`) with Home, Draw, and View tabs.
- Light grey ribbon (`#f3f3f3`) with grouped large-icon buttons and small group labels.
- 46px left tool rail (`#f7f7f7`) with Pen, Eraser, Select, Hand, zoom, and save.
- 300px right details panel with uppercase blue section headers.
- Blue status bar with the notebook title, stroke count, zoom, and save state.
- Workspace surround `#e9eaec`, with the page drawn on white.

Chrome is drawn by Skia (`src/ui.rs`) from fonts bundled in `assets/fonts/`: Bootstrap Icons (MIT) and Noto Sans (OFL 1.1). Their license texts are included. The widgets are custom: there is no DOM and no CSS.

## Tools (Samsung Notes-style)

| Where | Tool | What it does |
| --- | --- | --- |
| Draw > Pens | Pen | Solid ink, full width, pressure-sensitive. |
| Draw > Pens | Pencil | Thinner and slightly translucent. |
| Draw > Pens | Highlight | Wide and translucent, in the chosen color. |
| Draw > Pens | Brush | Wider, with pressure applied to the wider nib. |
| Draw > Tools | Line | Drag for a straight line. |
| Draw > Tools | Eraser | Drag over strokes; each stroke it touches is removed whole. Hit strokes turn red until release. |
| Draw > Tools | Select | Drag a rectangle to select strokes fully inside it, or click one. Delete removes the selection. |
| Draw > Tools | Hand | Drag to pan. Middle-drag also pans. |
| Home > Edit | Delete | Removes the selected strokes. |

Color (5) and width (3) apply to every ink tool. Each ink tool is a look on the same stroke format: the file stores only color and width, so the pencil and highlighter looks are baked in when the stroke is committed. Existing `.ink` files load unchanged.

Every edit (draw, line, erase, delete) is one undo step. `src/document.rs` owns the strokes, the spatial index, and the undo/redo history, and rebuilds the index after each change because stroke ids shift.

Keyboard: Ctrl+Z undo, Ctrl+Y or Ctrl+Shift+Z redo, Ctrl+S save, Ctrl+0 100%, Ctrl+plus/minus zoom, P pen, C pencil, M highlighter, B brush, L line, E eraser, S select, H hand, Delete removes the selection, Escape clears it.

## Not yet implemented (Samsung Notes features)

- Freeform lasso selection, and moving, resizing, or rotating selected strokes.
- Shape recognition (hold to snap a stroke to a line, circle, or rectangle).
- Text boxes, handwriting-to-text recognition, and inserting images.
- Page templates (ruled, grid, dot), a ruler, a color picker, and an opacity slider.
- Pen effects (shaking or fountain), and a partial-stroke eraser.
- Print Me Checks features such as check details, accounts, and print preview do not apply to InkCraft.

Verified: 33 unit tests (document edits and undo, hit testing, chrome layout and hit testing, paint colors), clippy with `-D warnings`, and an Xvfb run with injected pointer and keyboard input. The run drew a pen stroke, a line, and a highlighter, erased the line, undid the erase, selected and deleted the highlighter, and produced screenshots of each step.
Not verified: Windows, macOS, Android, pen hardware, HiDPI.

## PDF import and export

Export writes one PDF page per imported page. Strokes are drawn on each page
they overlap, and strokes outside every imported page go on one extra page. A
notebook with no imports exports as a single page sized to its content. Strokes
are vector paths. Imported pages are embedded as images.

Import renders each page at 2x with `hayro`, a pure-Rust PDF rasterizer, and
places the pages in a column below existing content. Text is rendered as shapes,
not kept as text. Pages are stored as PNG.

How to use it:

* Drag a PDF onto the window to import it. The pages appear below the content.
* Home > PDF > Export writes `<notebook>.pdf` next to the `.ink` file.
* From the command line, without a window:

  ```
  inkcraft Notes.ink --import handout.pdf --export Notes-out.pdf
  ```

  `--import` creates the notebook if it doesn't exist. `--export` runs after
  `--import`, so one command can do both.

Limits:

* Importing isn't undoable. Imported pages can't be removed yet.
* A 40-page A4 PDF imported at 2x adds about 13 MB to the `.ink` file. Storing JPEG or a lower scale would make it smaller.
* Debug builds render pages slowly: importing 40 pages took about 17 seconds. Release builds are much faster.
* Autosave copies every page image on each edit. Large imports will make saves slower until images are stored by reference.
* No native file dialog yet. Drag-and-drop and the command line are the ways in.
* Verified: headless import and export of a TeX-generated 40-page PDF (checked with `pdfinfo`, `qpdf --check`, and `pdftoppm`), the in-app Export button under Xvfb, and imported pages drawn in the canvas. Drag-and-drop was not exercised, because synthetic X input can't simulate a file drop.

## Page templates

Home and Draw both have a Template group: Blank, Ruled, Grid, and Dots. The
pattern covers the whole canvas and is saved with the notebook. It is also
drawn in PDF export. Strokes sit above it. Imported PDF pages, PNG files, and
JPEG files are opaque, so they hide the pattern beneath them.

Image templates: a PNG or JPEG dropped on the window (or passed with `--import`)
becomes one page, fitted to A4 width (595 points) with its height kept in
proportion. Write on top of it, and export it with the rest of the notebook.

Command line:

```
inkcraft Notes.ink --template ruled
inkcraft Notes.ink --template grid --import scan.jpg --export Notes.pdf
```

Limits:

* Templates aren't undoable as separate steps. Changing the template is a
  change to the notebook, not an undoable edit.
* Image templates are stored as the original file bytes. A large photo makes the
  `.ink` file large, the same as PDF pages.
* Verified: headless template and image import (PNG and JPEG) with PDF export
  and qpdf checks, and in the app: Ruled, Grid, and Blank switch live, and a
  stroke is drawn over the ruled template.

## Rulers and guides

View > Guides > Rulers (or Ctrl+R) shows rulers along the top and left edges of
the canvas, in page units (points). Labels appear every 5 ticks, and the tick
spacing follows the zoom level.

* Drag from the top ruler to place a horizontal guide. Drag from the left ruler
  to place a vertical guide. Releasing over a ruler cancels the placement.
* Drag a guide line to move it. Drag it out of the canvas to delete it.
* Snap (on by default) pulls drawn points within 6 screen pixels of a guide onto
  it, on that axis. Pen, Pencil, Highlighter, Brush, and Line are affected.
* Show hides and shows the guides. Clear removes all guides.

Guides are saved with the notebook but are never exported to PDF.

Command line: `--guide v:2` adds a vertical guide at 2 in, and
`--guide h:3.5` adds a horizontal guide at 3.5 in. These can be repeated.

## Page sizes

The artboard is a white sheet with a hard drop shadow on a grey pasteboard,
the way VectorCraft draws its artboards. The sheet starts at the upper-left
corner, which is where the rulers start at zero. Sizes are Letter (8.5 × 11 in),
Legal (8.5 × 14 in) and Tabloid (11 × 17 in), in portrait or landscape. A
new notebook starts as Letter portrait; "No page" removes the sheet and leaves an
unbounded canvas.

Rulers are measured in inches. Coordinates are still stored in points (72 per
inch). Command line: `--page letter`, `--page legal,landscape` or `--page tabloid`.

Not yet done: PDF export still uses the imported pages' sizes, not the artboard.

Limits:

* Guide placement and moves aren't undoable.
* A guide is grabbed when the pointer is within 4 screen pixels of it, so a
  stroke that starts on a guide moves the guide instead of drawing.
* Guides can't be dragged with touch or a pen tablet yet; the mouse path is the
  only one wired up.
* Verified: the Guides ribbon group, guides drawn in the right places, ruler
  drag to create, snapping of a stroke's start point, moving a guide, and
  deleting one by dragging it off the canvas. All under Xvfb with synthetic
  mouse input.

