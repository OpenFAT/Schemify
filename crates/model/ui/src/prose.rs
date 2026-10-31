//! The `prose` widget's model (D19, D21): markdown with inline `$…$` and display `$$…$$` LaTeX
//! math, parsed by the private `markdown` module and drawn by the view. Rules:
//! `docs/spec/docs-page.md` §2. This module holds what hosts need without drawing: the plot
//! fences of a source, image lines, and the metrics of a fenced code block (a windowed text
//! view places its lines itself).
//!
//! Contents:
//! - `pub use` [`Segment`], [`split_plots`], [`image_line`], [`ImageLine`] (from the private `markdown`)
//! - `pub const CODE_PX: RangeInclusive<u16>`, `pub const CODE_SCALE: f32`
//! - `pub struct CodeMetrics { pad, line, advance }`
//! - `pub fn code_metrics(ctx, px) -> Option<CodeMetrics>`; crate-only `code_pad(px) -> f32`
//! - `pub fn census(src) -> (usize, usize)`

use std::ops::RangeInclusive;

use egui::FontId;

use crate::markdown::{self, Block};

pub use crate::markdown::{ImageLine, Segment, image_line, split_plots};

/// Prose fonts (px) [`code_metrics`] answers for.
pub const CODE_PX: RangeInclusive<u16> = 8..=32;

/// Size of a fenced code block's monospace text relative to the prose font.
pub const CODE_SCALE: f32 = 0.92;

/// How a fenced code block lays out at one prose font size: a line of `cols` printable ASCII
/// characters is `advance * cols` wide and one row of `line` px, an empty line included.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CodeMetrics {
    /// Padding above, below and beside the text, in px.
    pub pad: f32,
    /// Height of one row in px; always a whole number.
    pub line: f32,
    /// Advance of every printable ASCII character in px (the face is monospace).
    pub advance: f32,
}

/// Code metrics at prose font `px` from `ctx`'s fonts ([`crate::font_definitions`]); `None`
/// outside [`CODE_PX`].
#[must_use]
pub fn code_metrics(ctx: &egui::Context, px: u16) -> Option<CodeMetrics> {
    if !CODE_PX.contains(&px) {
        return None;
    }
    let size = f32::from(px) * CODE_SCALE;
    let font = FontId::monospace(size);
    let (line, advance) = ctx.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, '0')));
    let m = CodeMetrics {
        pad: code_pad(f32::from(px)),
        line: line.ceil(),
        advance,
    };
    debug_assert!(m.advance > 0.0, "a monospace face has advances");
    debug_assert_eq!(m.line.fract(), 0.0, "rows stack with integer spacers");
    Some(m)
}

/// Padding around a fenced code block's text (each side) at prose font `px`.
pub(crate) fn code_pad(px: f32) -> f32 {
    debug_assert!(px > 0.0, "a font size");
    (px * 0.5).round()
}

/// What the view draws for markdown `src`: `(text runs, formulas)`. A text run is a heading,
/// paragraph, list item, quote, table cell, code block or footnote; a formula is an inline or
/// display formula RaTeX lays out (one outside its subset shows as source text instead).
#[must_use]
pub fn census(src: &str) -> (usize, usize) {
    let blocks = markdown::blocks(src);
    let notes: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Note(label, _) => Some(label.as_str()),
            _ => None,
        })
        .collect();
    let formulas_of = |text: &str| {
        let mut ok = |m: &str| crate::math::layout_math(m, 16.0).is_ok().then_some(0);
        markdown::inlines(text, &notes, &mut ok).maths.len()
    };
    let (mut runs, mut formulas) = (0, 0);
    for b in &blocks {
        let texts: Vec<&str> = match b {
            Block::Heading(_, t)
            | Block::Para(t)
            | Block::Quote(t)
            | Block::Item { text: t, .. }
            | Block::Note(_, t) => vec![t],
            Block::Table(head, rows) => head
                .iter()
                .chain(rows.iter().flatten())
                .map(String::as_str)
                .collect(),
            Block::Code(..) => {
                runs += 1;
                vec![]
            }
            Block::Math(m) => {
                formulas += usize::from(crate::math::layout_math(m, 16.0).is_ok());
                vec![]
            }
            Block::Rule | Block::Image(..) => vec![],
        };
        runs += texts.len();
        formulas += texts.iter().map(|t| formulas_of(t)).sum::<usize>();
    }
    (runs, formulas)
}
