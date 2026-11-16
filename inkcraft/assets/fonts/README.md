# Bundled fonts

- `bootstrap-icons.ttf`: Bootstrap Icons 1.13.1 (MIT). Glyph codepoints from its `bootstrap-icons.css`. See `LICENSE-bootstrap-icons.txt`.
- `NotoSans-Regular.ttf`, `NotoSans-SemiBold.ttf`: Noto Sans latin subset (SIL Open Font License 1.1), via `@fontsource/noto-sans` 5.3.0. See `LICENSE-noto-sans.txt`.

The fonts are embedded with `include_bytes!` so the UI looks the same on every platform.

Converted from the WOFF files to TTF, because Windows font loading (DirectWrite) does not read WOFF. The glyphs and tables are unchanged.
