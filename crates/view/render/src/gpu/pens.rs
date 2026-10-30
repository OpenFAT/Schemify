//! Pens: a row's (palette layer, line style, closed) in, its colour and style word out, or
//! nothing for a dead row or a hidden layer. Resolved once per distinct pen and cached per
//! palette ([`Pens`]), so a pack resolves each pen once instead of once per row.
//!
//! Six questions: in, a row's (layer, style, closed); out, its colour and style word or
//! nothing. 32 × 128 × 2 entries (64 KiB), mostly unused (a document has a few dozen pens).
//! Read once per row per pack, written once per distinct pen. Lives with its
//! [`super::InstanceRows`]; cleared when the palette changes. Serial.
//!
//! Contents:
//! - `pub(super) struct Pen`
//! - `pub(super) struct Pens`, `Pens::use_palette`, `Pens::get`
//! - `pub(super) fn style_word(q16: u32, dash: u8, flags: u32) -> u32`
//! - `pub(super) fn pen(palette: &Palette, layer: u8, style: LineStyle, closed: bool) -> Option<Pen>`

use schemify_canvas::{LAYERS, LineStyle, Palette};

use super::{FILL, PARITY};

/// What a live row is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Pen {
    /// RGBA bytes in memory order.
    pub(super) color: u32,
    /// The row's style word ([`style_word`]).
    pub(super) style: u32,
}

/// The pens of one palette: per (layer, style byte, closed) what [`pen`] returns.
#[derive(Debug, Default)]
pub(super) struct Pens {
    palette: Option<Palette>,
    /// `RESOLVED | colour << 24 | style word`, or `RESOLVED | DRAWS_NOTHING`, or 0 (not yet).
    lut: Vec<u64>,
}

/// Style bytes a [`LineStyle`] spans (weight, dash and fill bits).
const STYLES: usize = 128;

const _: () = assert!(LineStyle::DASH_MAX < 16, "the dash is a nibble");

impl Pens {
    const RESOLVED: u64 = 1 << 63;
    const DRAWS_NOTHING: u64 = 1 << 62;

    /// Resolves pens from `palette` from now on (forgetting the old ones when it differs).
    pub(super) fn use_palette(&mut self, palette: &Palette) {
        if self.palette.as_ref() != Some(palette) {
            self.palette = Some(palette.clone());
            self.lut.clear();
            self.lut.resize(LAYERS * STYLES * 2, 0);
        }
        assert_eq!(self.lut.len(), LAYERS * STYLES * 2, "a palette in use");
        debug_assert!(
            self.palette.as_ref() == Some(palette),
            "pens of this palette"
        );
    }

    /// The pen of a row on `layer` in `style` ([`pen`]), from the cache.
    ///
    /// # Panics
    ///
    /// Panics before [`Pens::use_palette`].
    #[inline]
    pub(super) fn get(&mut self, layer: u8, style: LineStyle, closed: bool) -> Option<Pen> {
        let l = usize::from(layer);
        if l >= LAYERS {
            return None; // a dead row: no palette layer
        }
        let st = usize::from(style.0);
        assert!(st < STYLES, "a style byte");
        let at = (l * STYLES + st) * 2 + usize::from(closed);
        let mut e = self.lut[at];
        if e == 0 {
            let palette = self.palette.as_ref().expect("a palette in use");
            e = Self::RESOLVED
                | match pen(palette, layer, style, closed) {
                    Some(p) => u64::from(p.color) << 24 | u64::from(p.style),
                    None => Self::DRAWS_NOTHING,
                };
            self.lut[at] = e;
        }
        debug_assert!(e & Self::RESOLVED != 0, "a resolved entry");
        if e & Self::DRAWS_NOTHING != 0 {
            return None;
        }
        // A style word is 24 bits: 16 of width, a dash nibble, a flags nibble.
        let p = Pen {
            color: (e >> 24) as u32,
            style: (e & 0xff_ffff) as u32,
        };
        debug_assert!(p.style & 0xffff > 0, "a drawn row has a width");
        Some(p)
    }
}

/// A row's style word: stroke width in 1/16 px, dash, flags.
///
/// # Panics
///
/// Panics on a field past its bits.
pub(super) fn style_word(q16: u32, dash: u8, flags: u32) -> u32 {
    assert!(q16 <= 0xffff, "a width in 16 bits");
    assert!(dash < 16, "a dash in a nibble");
    assert!(flags < 16, "flags in a nibble");
    let word = q16 | (u32::from(dash) << 16) | (flags << 20);
    debug_assert!(word < 1 << 24, "a style word fits `Pens`' 24 bits");
    word
}

/// A row's colour (RGBA bytes in memory order) and style word ([`style_word`]) on `layer` in
/// `style`; `closed` lets the layer's or the style's fill apply. `None` for a dead row
/// ([`schemify_canvas::LAYER_DEAD`]) or a hidden layer (colour alpha 0): nothing drawn.
pub(super) fn pen(palette: &Palette, layer: u8, style: LineStyle, closed: bool) -> Option<Pen> {
    let l = usize::from(layer);
    if l >= LAYERS || palette.color[l] & 0xff == 0 {
        return None;
    }
    let w = f64::from(palette.width[l]) * f64::from(style.weight());
    assert!(w > 0.0, "a drawn layer has a width, layer {l} has {w}");
    let q = ((w * 16.0).round() as u32).clamp(1, 0xffff);
    let fill = closed && (palette.fill & (1 << l) != 0 || style.fills());
    let mut f = 0;
    if style != LineStyle::PLAIN {
        f |= PARITY;
    }
    if fill {
        f |= FILL;
    }
    Some(Pen {
        color: palette.color[l].swap_bytes(),
        style: style_word(q, style.dash(), f),
    })
}
