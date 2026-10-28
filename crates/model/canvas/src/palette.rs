//! How rows are coloured: the per-layer [`Palette`] and the recolor [`Marks`] drawn over a
//! [`CanvasPrims`](crate::CanvasPrims). Layer numbering follows xschem's Layers menu (format
//! spec §4 `layer`); the colours are read off xschem's screen, never its source (D1).
//!
//! Contents:
//! - `pub struct Palette { color, width, fill, background, grid }`, `Palette::xschem_dark`
//! - `pub enum PrimKind { Line, Rect, Arc, Polyline, Polygon }`
//! - `pub struct Marks { kind, index, color }`, `Marks::push`

use crate::LAYERS;

/// Per-layer drawing style: variation by data, one row per layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// Stroke/fill/text color, `0xRRGGBBAA`.
    pub color: [u32; LAYERS],
    /// Stroke width in logical pixels.
    pub width: [f32; LAYERS],
    /// Bit `l` set: closed shapes on layer `l` are also filled.
    pub fill: u32,
    /// Canvas background.
    pub background: u32,
    /// Grid dots.
    pub grid: u32,
}

/// Primitive kind for [`Marks`]: the tables a mark can recolor (labels cannot be marked).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrimKind {
    /// Row of [`CanvasPrims::lines`](crate::CanvasPrims::lines).
    Line,
    /// Row of [`CanvasPrims::rects`](crate::CanvasPrims::rects).
    Rect,
    /// Row of [`CanvasPrims::arcs`](crate::CanvasPrims::arcs).
    Arc,
    /// Row of [`CanvasPrims::polylines`](crate::CanvasPrims::polylines).
    Polyline,
    /// Row of [`CanvasPrims::polygons`](crate::CanvasPrims::polygons).
    Polygon,
}

/// Recolored primitives drawn on top: selection highlight, visual-diff colors, annotation
/// markers. One table covers all three; group rows by color for fewer draw calls. The three
/// columns stay equally long.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    /// Which table.
    pub kind: Vec<PrimKind>,
    /// Row in that table.
    pub index: Vec<u32>,
    /// Color, `0xRRGGBBAA`.
    pub color: Vec<u32>,
}

/// The xschem dark scheme, layers 0–21 (0 BG, 1 wire, 2 grid, 3 text, 4 symbol, 5 pin); every
/// later layer is grey.
const XSCHEM_DARK: [u32; 22] = [
    0x0000_00ff,
    0x00cc_ffff,
    0x5050_50ff,
    0xd0d0_d0ff,
    0x88dd_00ff,
    0xbb22_00ff,
    0x00a0_ffff,
    0xff70_ffff,
    0xffff_00ff,
    0x00ff_80ff,
    0xff80_00ff,
    0x8080_ffff,
    0xff40_40ff,
    0x40ff_ffff,
    0xc0c0_00ff,
    0xa040_ffff,
    0x40a0_40ff,
    0xffa0_a0ff,
    0xa0a0_ffff,
    0xe0e0_e0ff,
    0x80c0_ffff,
    0xffc0_40ff,
];

const _: () = assert!(XSCHEM_DARK.len() <= LAYERS);

impl Palette {
    /// The xschem dark scheme as it appears on screen: pins (layer 5) fill, every width is
    /// 1 px.
    // ponytail: hard-coded rows until the palette is a `ui_styles` selector set (`layer.N`).
    pub fn xschem_dark() -> Palette {
        let mut color = [0x8888_88ff; LAYERS];
        color[..XSCHEM_DARK.len()].copy_from_slice(&XSCHEM_DARK);
        Palette {
            color,
            width: [1.0; LAYERS],
            fill: 1 << 5,
            background: 0x0000_00ff,
            grid: 0x7070_70ff,
        }
    }
}

impl Marks {
    /// Appends a mark. `index` must be a row of `kind`'s table in the
    /// [`CanvasPrims`](crate::CanvasPrims) the marks are drawn over (the renderer and
    /// [`CanvasDiff`](crate::CanvasDiff) panic on a row past its table's end).
    ///
    /// # Panics
    ///
    /// If the columns are out of step.
    pub fn push(&mut self, kind: PrimKind, index: u32, color: u32) {
        assert_eq!(self.index.len(), self.kind.len(), "index column in step");
        assert_eq!(self.color.len(), self.kind.len(), "color column in step");
        self.kind.push(kind);
        self.index.push(index);
        self.color.push(color);
    }
}
