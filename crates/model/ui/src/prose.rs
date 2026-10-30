//! The `prose` widget (D19, D21): markdown with inline `$…$` and display `$$…$$` LaTeX math
//! (parsed by the private `markdown` module), laid out at a given width into positioned
//! parley layouts, math boxes, rules, shades and images. The docs page renders every `prose`
//! doc block through it. Rules: `docs/spec/docs-page.md` §2. Pipe-table columns are sized by
//! their content (`column_widths`); an image not registered in the [`TextSystem`] shows its
//! alt text.
//!
//! Data design:
//! 1. In: markdown text, font px, width. Out: [`ProseList`] (layout ids + offsets, math ids +
//!    offsets, rules, shades, image ids + rects) in the [`TextSystem`] cache.
//! 2. How many: one list per (prose block, size, width); a page holds ≤ 2^12 blocks (doc
//!    capacity), a block tens of lines.
//! 3. Widths: logical-pixel `f32` offsets; layout and math ids `u32` into the text system.
//! 4. Access: built once per (source, width), painted linearly every frame.
//! 5. Lifetime: the text system cache (evicted with it).
//! 6. Parallel: no; shaping shares the font context.
//!
//! Contents:
//! - `pub use` [`Segment`], [`split_plots`], [`image_line`], [`ImageLine`] (from the private `markdown`)
//! - `pub const CODE_PX: RangeInclusive<u16>`
//! - `pub struct CodeMetrics { pad, line, advance }`, `pub struct ProseList`
//! - `pub fn code_metrics(ts, px) -> Option<CodeMetrics>`
//! - `pub(crate) fn measure_code(ts, px) -> Option<CodeMetrics>`
//! - `pub(crate) fn layout_prose(ts, src, px, width) -> ProseList`

use std::ops::RangeInclusive;

use parley::{
    Alignment, AlignmentOptions, FontFamily, FontStyle, FontWeight, InlineBox, InlineBoxKind,
    Layout, LineHeight, PositionedLayoutItem, StyleProperty, VerticalAlign,
};

use crate::markdown;
use crate::markdown::{BOLD, Block, CODE, COLUMNS_MAX, ITALIC, Inline};
use crate::text::{TextId, TextSystem};

pub use crate::markdown::{ImageLine, Segment, image_line, split_plots};

/// Prose fonts (px) whose code metrics [`TextSystem::new`] measures ([`code_metrics`]).
pub const CODE_PX: RangeInclusive<u16> = 8..=32;

/// How a fenced code block lays out at one prose font size, for callers that place its lines
/// themselves (a windowed text view): a line of `cols` printable ASCII characters (no tab) is
/// `advance * cols` wide, and when that fits the wrap width (the prose width less `2 * pad`)
/// it is exactly one row of `line` px, an empty line included. Pinned by the tests below.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CodeMetrics {
    /// Padding above, below and beside the text, in px.
    pub pad: f32,
    /// Height of one row in px; always a whole number (parley rounds line boxes).
    pub line: f32,
    /// Advance of every printable ASCII character in px (the face is monospace).
    pub advance: f32,
}

/// A laid-out prose block in logical pixels, origin at its top-left. Text layouts and math
/// lists live in the [`TextSystem`] that built it and are evicted with it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProseList {
    /// Layout id per text run (paragraph, heading, list body, table cell, code block).
    pub text_id: Vec<TextId>,
    /// Left edge of each text layout.
    pub text_x: Vec<f32>,
    /// Top edge of each text layout.
    pub text_y: Vec<f32>,
    /// Math id ([`TextSystem::math_list`]) per formula, inline or display.
    pub math_id: Vec<u32>,
    /// Left edge of each formula box.
    pub math_x: Vec<f32>,
    /// Top edge of each formula box (its baseline is at `top + MathList::height`).
    pub math_y: Vec<f32>,
    /// Filled rectangles `[x, y, w, h]` in the text color (rules, table grid, quote bars).
    pub rules: Vec<[f32; 4]>,
    /// Rectangles `[x, y, w, h]` drawn faintly behind code blocks.
    pub shades: Vec<[f32; 4]>,
    /// Image id ([`TextSystem::image`]) per drawn image.
    pub image_id: Vec<u32>,
    /// Where each image is drawn, `[x, y, w, h]` (scaled down to the width, never up).
    pub image_rect: Vec<[f32; 4]>,
    /// Widest line.
    pub width: f32,
    /// Total height.
    pub height: f32,
}

/// Size of a fenced code block's monospace text relative to the prose font.
const CODE_SCALE: f32 = 0.92;

/// Where a run goes: its top-left corner and the width it wraps at, in logical px.
#[derive(Clone, Copy, Debug)]
struct Slot {
    x: f32,
    y: f32,
    width: f32,
}

/// Code metrics at prose font `px`, from the table [`TextSystem::new`] measured; `None`
/// outside [`CODE_PX`] or when the measured row height is not a whole number (then rows can
/// not be stacked with integer spacers).
pub fn code_metrics(ts: &TextSystem, px: u16) -> Option<CodeMetrics> {
    let m = ts
        .code_metrics
        .get(usize::from(px.checked_sub(*CODE_PX.start())?))?;
    debug_assert!(CODE_PX.contains(&px));
    m.filter(|m| m.line.fract() == 0.0 && m.line > 0.0)
}

/// Measures [`CodeMetrics`] at prose font `px` by shaping two one-character rows around an
/// empty one, unwrapped.
pub(crate) fn measure_code(ts: &mut TextSystem, px: u16) -> Option<CodeMetrics> {
    assert!(px > 0, "a font size");
    let px = f32::from(px);
    let id = shape(ts, "0\n\n0", px * CODE_SCALE, 1.0e6);
    let l = ts.layout(id);
    let rows: Vec<f32> = l.lines().map(|r| r.metrics().line_height).collect();
    let advance = l.lines().next()?.metrics().advance;
    let m = (rows.len() == 3 && rows.iter().all(|&h| h == rows[0])).then(|| CodeMetrics {
        pad: code_pad(px),
        line: rows[0],
        advance,
    });
    debug_assert!(m.is_none_or(|m| m.advance > 0.0));
    m
}

/// Lays out markdown `src` at `px` pixels per em, wrapping at `width`. Images come from the
/// ones registered in `ts` ([`TextSystem::set_image`]); one that is not shows its alt text.
///
/// # Panics
///
/// Panics if `px` or `width` is not finite and positive.
pub(crate) fn layout_prose(ts: &mut TextSystem, src: &str, px: f32, width: f32) -> ProseList {
    assert!(px.is_finite(), "prose size must be finite");
    assert!(px > 0.0, "prose size must be positive");
    assert!(width.is_finite(), "prose width must be finite");
    assert!(width > 0.0, "prose width must be positive");
    let mut p = ProseList::default();
    let gap = (px * 0.6).round();
    let parsed = markdown::blocks(src);
    let notes: Vec<&str> = parsed
        .iter()
        .filter_map(|b| match b {
            Block::Note(label, _) => Some(label.as_str()),
            _ => None,
        })
        .collect();
    let notes = notes.as_slice();
    let mut y = 0.0f32;
    let mut first = true;
    for b in &parsed {
        if matches!(b, Block::Note(..)) {
            continue; // drawn at the end
        }
        if !first {
            y += gap;
        }
        first = false;
        match b {
            Block::Heading(level, text) => {
                let scale = [1.6, 1.35, 1.2, 1.1, 1.0, 1.0][usize::from(*level) - 1];
                let slot = Slot { x: 0.0, y, width };
                y += rich(ts, &mut p, text, px * scale, BOLD, slot, notes);
            }
            Block::Para(text) => {
                let slot = Slot { x: 0.0, y, width };
                y += rich(ts, &mut p, text, px, 0, slot, notes);
            }
            Block::Quote(text) => {
                let w = (width - px).max(px);
                let slot = Slot { x: px, y, width: w };
                let h = rich(ts, &mut p, text, px, ITALIC, slot, notes);
                p.rules.push([px * 0.25, y, 3.0, h]);
                y += h;
            }
            Block::Item {
                depth,
                marker,
                text,
            } => {
                let x = px * 1.5 * f32::from(*depth);
                let body = x + px * 1.6;
                let marker_slot = Slot {
                    x: x + px * 0.3,
                    y,
                    width: px * 1.3,
                };
                rich(ts, &mut p, marker, px, 0, marker_slot, notes);
                let w = (width - body).max(px);
                let slot = Slot {
                    x: body,
                    y,
                    width: w,
                };
                y += rich(ts, &mut p, text, px, 0, slot, notes);
            }
            Block::Code(_, text) => {
                let pad = code_pad(px);
                let id = shape(ts, text, px * CODE_SCALE, (width - 2.0 * pad).max(px));
                let h = ts.layout(id).height();
                p.shades.push([0.0, y, width, h + 2.0 * pad]);
                push_text(&mut p, ts, id, pad, y + pad);
                y += h + 2.0 * pad;
            }
            Block::Math(src) => match ts.math_uncached(src, px * 1.15) {
                Some(m) => {
                    let list = ts.math_list(m);
                    let (mw, mh) = (list.width, list.height + list.depth);
                    p.math_id.push(m);
                    p.math_x.push(((width - mw) / 2.0).max(0.0));
                    p.math_y.push(y);
                    p.width = p.width.max(mw);
                    y += mh;
                }
                None => {
                    let id = shape(ts, &format!("$${src}$$"), px, width);
                    y += push_text(&mut p, ts, id, 0.0, y);
                }
            },
            Block::Table(header, rows) => {
                let t = (header.as_slice(), rows.as_slice());
                y += table(ts, &mut p, t, px, y, width, notes);
            }
            Block::Rule => {
                p.rules.push([0.0, (y + px * 0.4).round(), width, 1.0]);
                y += px * 0.8;
            }
            Block::Image(alt, path) => match ts.image_id(path) {
                Some(id) => {
                    let img = ts.image(id);
                    let (iw, ih) = (img.width as f32, img.height as f32);
                    let s = (width / iw).min(1.0);
                    let (w, h) = ((iw * s).floor().max(1.0), (ih * s).floor().max(1.0));
                    p.image_id.push(id);
                    p.image_rect.push([0.0, y, w, h]);
                    p.width = p.width.max(w);
                    y += h;
                }
                None => {
                    let text = format!("[image: {alt}]");
                    let slot = Slot { x: 0.0, y, width };
                    y += rich(ts, &mut p, &text, px, ITALIC, slot, notes);
                }
            },
            Block::Note(..) => unreachable!("skipped above"),
        }
    }
    // Footnotes: a short rule, then one numbered line each, a little smaller.
    let defs = parsed.iter().filter_map(|b| match b {
        Block::Note(_, text) => Some(text),
        _ => None,
    });
    let small = (px * 0.9).max(1.0);
    for (k, text) in defs.enumerate() {
        if k == 0 {
            if !first {
                y += gap;
            }
            p.rules.push([0.0, y.round(), (width / 3.0).round(), 1.0]);
            y += gap;
        }
        let marker = format!("{}.", k + 1);
        let body = small * 2.0;
        let marker_slot = Slot {
            x: 0.0,
            y,
            width: body,
        };
        rich(ts, &mut p, &marker, small, 0, marker_slot, notes);
        let w = (width - body).max(small);
        let slot = Slot {
            x: body,
            y,
            width: w,
        };
        y += rich(ts, &mut p, text, small, 0, slot, notes);
    }
    p.height = y.ceil();
    p.width = p.width.min(width);
    debug_assert_eq!(p.text_id.len(), p.text_y.len());
    debug_assert_eq!(p.math_id.len(), p.math_y.len());
    debug_assert_eq!(p.image_id.len(), p.image_rect.len());
    p
}

// ---- private ----

/// Padding around a fenced code block's text (each side) at prose font `px`.
fn code_pad(px: f32) -> f32 {
    debug_assert!(px > 0.0, "a font size");
    let pad = (px * 0.5).round();
    debug_assert!(pad >= 0.0);
    pad
}

/// Lays out one inline run in `slot`; returns its height.
fn rich(
    ts: &mut TextSystem,
    p: &mut ProseList,
    src: &str,
    px: f32,
    style: u8,
    slot: Slot,
    notes: &[&str],
) -> f32 {
    debug_assert!(slot.width > 0.0, "a run wraps at a positive width");
    let texts = p.text_id.len();
    let (layout, inl) = shape_rich(ts, src, px, style, notes);
    let h = place(ts, p, layout, &inl, slot);
    debug_assert_eq!(p.text_id.len(), texts + 1, "one layout per run");
    h
}

/// Parses and shapes one inline run at `px` (lines not broken yet), its formulas laid out.
fn shape_rich(
    ts: &mut TextSystem,
    src: &str,
    px: f32,
    style: u8,
    notes: &[&str],
) -> (Layout<()>, Inline) {
    assert!(px > 0.0, "prose size must be positive");
    debug_assert_eq!(style & CODE, 0, "a whole run is never a code span");
    let inl = markdown::inlines(src, notes, &mut |m| ts.math_uncached(m, px));
    // Math boxes first: the builder below borrows the text system's contexts mutably.
    let boxes: Vec<(u32, f32, f32, f32)> = inl
        .maths
        .iter()
        .map(|&(index, m)| {
            let l = ts.math_list(m);
            (index, l.width, l.height, l.depth)
        })
        .collect();
    let (family, mono) = (ts.family().to_owned(), ts.mono().to_owned());
    let mut layout: Layout<()> = Layout::new();
    {
        let (fcx, lcx) = (&mut ts.font_cx, &mut ts.layout_cx);
        let mut b = lcx.ranged_builder(fcx, &inl.text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::from(family.as_str())));
        b.push_default(StyleProperty::FontSize(px));
        b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
            1.35,
        )));
        if style & BOLD != 0 {
            b.push_default(StyleProperty::FontWeight(FontWeight::BOLD));
        }
        if style & ITALIC != 0 {
            b.push_default(StyleProperty::FontStyle(FontStyle::Italic));
        }
        for &(s, e, f) in &inl.spans {
            let r = s as usize..e as usize;
            if f & BOLD != 0 {
                b.push(StyleProperty::FontWeight(FontWeight::BOLD), r.clone());
            }
            if f & ITALIC != 0 {
                b.push(StyleProperty::FontStyle(FontStyle::Italic), r.clone());
            }
            if f & CODE != 0 {
                b.push(
                    StyleProperty::FontFamily(FontFamily::from(mono.as_str())),
                    r,
                );
            }
        }
        for (k, &(index, w, h, d)) in boxes.iter().enumerate() {
            b.push_inline_box(InlineBox {
                id: k as u64,
                kind: InlineBoxKind::InFlow,
                index: index as usize,
                width: w,
                height: h + d,
                baseline: Some(h),
                vertical_align: VerticalAlign::BASELINE,
            });
        }
        b.build_into(&mut layout, &inl.text);
    }
    debug_assert_eq!(boxes.len(), inl.maths.len(), "one inline box per formula");
    (layout, inl)
}

/// Breaks a [`shape_rich`] layout at the slot's width, records it and its formulas at the
/// slot's corner; returns its height.
fn place(
    ts: &mut TextSystem,
    p: &mut ProseList,
    mut layout: Layout<()>,
    inl: &Inline,
    slot: Slot,
) -> f32 {
    debug_assert_eq!(p.math_id.len(), p.math_x.len(), "math columns in step");
    debug_assert_eq!(p.math_id.len(), p.math_y.len(), "math columns in step");
    let maths = p.math_id.len();
    layout.break_all_lines(Some(slot.width.max(1.0)));
    layout.align(Alignment::Start, AlignmentOptions::default());
    for line in layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::InlineBox(ib) = item {
                p.math_id.push(inl.maths[ib.id as usize].1);
                p.math_x.push(slot.x + ib.x);
                p.math_y.push(slot.y + ib.y);
            }
        }
    }
    debug_assert!(
        p.math_id.len() - maths <= inl.maths.len(),
        "each formula placed once"
    );
    let id = ts.push_layout(layout);
    push_text(p, ts, id, slot.x, slot.y)
}

/// Shapes verbatim monospace text (code blocks, formulas outside the subset), wrapping at
/// `width`.
fn shape(ts: &mut TextSystem, text: &str, px: f32, width: f32) -> TextId {
    assert!(px > 0.0, "code size must be positive");
    debug_assert!(width > 0.0, "code wraps at a positive width");
    let mono = ts.mono().to_owned();
    let mut layout: Layout<()> = Layout::new();
    {
        let (fcx, lcx) = (&mut ts.font_cx, &mut ts.layout_cx);
        let mut b = lcx.ranged_builder(fcx, text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::from(mono.as_str())));
        b.push_default(StyleProperty::FontSize(px));
        b.build_into(&mut layout, text);
    }
    layout.break_all_lines(Some(width.max(1.0)));
    layout.align(Alignment::Start, AlignmentOptions::default());
    ts.push_layout(layout)
}

/// Records text layout `id` at `(x, y)`; returns its height.
fn push_text(p: &mut ProseList, ts: &TextSystem, id: TextId, x: f32, y: f32) -> f32 {
    debug_assert!(x >= 0.0, "runs sit inside the block");
    let l = ts.layout(id);
    p.text_id.push(id);
    p.text_x.push(x);
    p.text_y.push(y);
    p.width = p.width.max(x + l.width());
    debug_assert_eq!(p.text_id.len(), p.text_x.len(), "text columns in step");
    debug_assert_eq!(p.text_id.len(), p.text_y.len(), "text columns in step");
    l.height()
}

/// Lays out a pipe table with a full grid; returns its height. Columns are as wide as their
/// widest cell (unwrapped) when the table fits `width`; otherwise [`column_widths`] shares
/// the width out and cells wrap.
fn table(
    ts: &mut TextSystem,
    p: &mut ProseList,
    (header, rows): (&[String], &[Vec<String>]),
    px: f32,
    y0: f32,
    width: f32,
    notes: &[&str],
) -> f32 {
    let cols = rows
        .iter()
        .map(Vec::len)
        .chain([header.len()])
        .max()
        .unwrap_or(0)
        .min(COLUMNS_MAX);
    if cols == 0 {
        return 0.0;
    }
    let pad = (px * 0.4).round();
    let all: Vec<(&[String], u8)> = (!header.is_empty())
        .then_some((header, BOLD))
        .into_iter()
        .chain(rows.iter().map(|r| (r.as_slice(), 0)))
        .collect();
    // Every cell is shaped once: its unwrapped width sizes the column, then it wraps there.
    let mut cells: Vec<(Layout<()>, Inline)> = Vec::with_capacity(all.len() * cols);
    let mut natural = vec![0.0f32; cols];
    for &(row, style) in &all {
        for (c, nat) in natural.iter_mut().enumerate() {
            let text = row.get(c).map_or("", String::as_str);
            let (layout, inl) = shape_rich(ts, text, px, style, notes);
            let w = layout.calculate_content_widths().max.ceil() + 2.0 * pad;
            *nat = nat.max(w);
            cells.push((layout, inl));
        }
    }
    let widths = column_widths(&natural, width.floor(), (px * 3.0).min(width / cols as f32));
    let mut xs = Vec::with_capacity(cols + 1);
    xs.push(0.0f32);
    for w in &widths {
        xs.push(xs[xs.len() - 1] + w);
    }
    debug_assert_eq!(xs.len(), cols + 1, "one edge per column and a closing one");
    let total = xs[cols];
    let mut y = y0;
    p.rules.push([0.0, y0, total, 1.0]);
    let mut cells = cells.into_iter();
    for _ in 0..all.len() {
        let mut h = 0.0f32;
        for (&w, &x) in widths.iter().zip(&xs) {
            let (layout, inl) = cells.next().expect("one shaped cell per column");
            let slot = Slot {
                x: x + pad,
                y: y + pad,
                width: (w - 2.0 * pad).max(1.0),
            };
            h = h.max(place(ts, p, layout, &inl, slot));
        }
        y += h + 2.0 * pad;
        p.rules.push([0.0, y, total, 1.0]);
    }
    debug_assert!(cells.next().is_none());
    for &x in &xs {
        p.rules.push([x, y0, 1.0, y - y0]);
    }
    p.width = p.width.max(total);
    debug_assert!(y >= y0, "rows stack downward");
    y - y0 + 1.0
}

/// Widths of table columns whose cells want `natural` widths, in a pane `width` wide: the
/// natural widths when their sum fits; otherwise every column narrower than an equal share of
/// what the others leave keeps its natural width, and the rest split the remainder in
/// proportion to their natural widths, each at least `min` (so the sum can exceed `width`
/// only when `min` forces it). Whole pixels.
///
/// # Panics
///
/// Panics on no columns or more than [`COLUMNS_MAX`].
fn column_widths(natural: &[f32], width: f32, min: f32) -> Vec<f32> {
    let n = natural.len();
    assert!(n > 0, "a table has a column");
    assert!(n <= COLUMNS_MAX, "at most COLUMNS_MAX columns");
    if natural.iter().sum::<f32>() <= width {
        return natural.iter().map(|w| w.ceil()).collect();
    }
    let mut fixed = vec![false; n];
    // Each pass fixes at least one more column or stops; they cannot all fix (their sum
    // would fit), so at most n − 1 passes.
    for _ in 0..n {
        let used: f32 = (0..n).filter(|&c| fixed[c]).map(|c| natural[c]).sum();
        let flex = fixed.iter().filter(|f| !**f).count();
        let share = (width - used) / flex as f32;
        let narrow: Vec<usize> = (0..n)
            .filter(|&c| !fixed[c] && natural[c] <= share)
            .collect();
        if narrow.is_empty() {
            break;
        }
        for c in narrow {
            fixed[c] = true;
        }
    }
    let used: f32 = (0..n).filter(|&c| fixed[c]).map(|c| natural[c]).sum();
    let flex_n = fixed.iter().filter(|f| !**f).count();
    assert!(flex_n > 0, "the columns overflow, so one stays flexible");
    let flex_sum: f32 = (0..n).filter(|&c| !fixed[c]).map(|c| natural[c]).sum();
    let left = (width - used).max(0.0);
    let widths: Vec<f32> = (0..n)
        .map(|c| {
            if fixed[c] {
                natural[c].ceil()
            } else if flex_sum > 0.0 {
                (left * natural[c] / flex_sum).floor().max(min)
            } else {
                (left / flex_n as f32).floor().max(min)
            }
        })
        .collect();
    debug_assert_eq!(widths.len(), n, "one width per column");
    debug_assert!(widths.iter().all(|w| w.fract() == 0.0), "whole pixels");
    widths
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The [`CodeMetrics`] contract at every measured size: printable ASCII rows that fit lay
    /// out one `line` each (empty rows too) at `pad`, `advance` per character; a row of words
    /// too wide wraps.
    #[test]
    fn code_metrics_place_every_fitting_row() {
        let mut ts = TextSystem::new();
        let ascii: String = (b' '..=b'~').map(char::from).collect();
        let rows = ["", ascii.as_str(), "x", "", "R1 a b 1k", "~~ `` ~~"];
        let cols = rows.iter().map(|r| r.len()).max().unwrap();
        for px in CODE_PX {
            let m = code_metrics(&ts, px).expect("whole rows at every measured size");
            let width = 2.0 * m.pad + m.advance * cols as f32 + 0.5;
            let src = format!("```\n{}\n```", rows.join("\n"));
            let p = layout_prose(&mut ts, &src, f32::from(px), width);
            assert_eq!(p.text_y, [m.pad], "px {px}");
            let l = ts.layout(p.text_id[0]);
            assert_eq!(l.len(), rows.len(), "px {px}: no row wrapped");
            for (row, line) in rows.iter().zip(l.lines()) {
                assert_eq!(line.metrics().line_height, m.line, "px {px}");
                let w = m.advance * row.len() as f32;
                assert!((line.metrics().advance - w).abs() < 0.01, "px {px} {row:?}");
            }
            let full = 2.0 * m.pad + m.line * rows.len() as f32;
            assert_eq!(p.height, full, "px {px}");
            // Negative space: a row of words wider than the wrap width wraps.
            let wide = "a ".repeat(cols / 2 + 1);
            let p = layout_prose(&mut ts, &format!("```\n{wide}x\n```"), f32::from(px), width);
            assert!(ts.layout(p.text_id[0]).len() > 1, "px {px}");
        }
        assert_eq!(code_metrics(&ts, CODE_PX.end() + 1), None);
        assert_eq!(code_metrics(&ts, CODE_PX.start() - 1), None);
    }

    #[test]
    fn layout_wraps_and_places_math_inline() {
        let mut ts = TextSystem::new();
        let src = "# Title\n\nA long paragraph that has to wrap around several times at this narrow width, with $g_m = 2 I_D / V_{ov}$ inline.\n\n$$\\frac{g_m}{I_D}$$\n\n| a | b |\n|---|---|\n| 1 | 2 |";
        let wide = layout_prose(&mut ts, src, 14.0, 900.0);
        let narrow = layout_prose(&mut ts, src, 14.0, 160.0);
        assert!(
            narrow.height > wide.height,
            "{} vs {}",
            narrow.height,
            wide.height
        );
        assert!(narrow.width <= 160.0 && wide.width <= 900.0);
        assert_eq!(wide.math_id.len(), 2, "one inline and one display formula");
        // The display formula is centred; the inline one sits inside its paragraph.
        assert!(wide.math_x[1] > 300.0);
        assert!(wide.math_y[0] > wide.text_y[0] && wide.math_y[0] < wide.text_y[2]);
        assert!(wide.rules.len() >= 4, "table grid");
        assert!(wide.text_y.windows(2).all(|w| w[0] <= w[1]));
    }

    /// Table columns take their content's width when the table fits, and share the pane
    /// (narrow columns kept whole, wide ones wrapping) when it does not.
    #[test]
    fn table_columns_follow_their_content() {
        let mut ts = TextSystem::new();
        let src = "| k | description |\n|---|---|\n| 1 | a long description that has to wrap at this narrow width |";
        let cols = |p: &ProseList| -> Vec<f32> {
            let vertical = p.rules.iter().filter(|r| r[2] == 1.0 && r[3] > 1.0);
            vertical.map(|r| r[0]).collect()
        };
        let wide = layout_prose(&mut ts, src, 14.0, 900.0);
        let x = cols(&wide);
        assert_eq!(x.len(), 3, "{x:?}");
        assert!(x[1] < 60.0, "the short column hugs its content: {x:?}");
        assert!(x[2] < 900.0, "a table that fits is not stretched: {x:?}");
        let narrow = layout_prose(&mut ts, src, 14.0, 300.0);
        let y = cols(&narrow);
        assert!(
            (y[1] - x[1]).abs() < 1.0,
            "the short column keeps its width: {y:?}"
        );
        assert!(y[2] <= 300.0, "{y:?}");
        assert!(narrow.height > wide.height, "the long column wraps");
    }

    #[test]
    fn column_widths_share_the_pane() {
        assert_eq!(
            column_widths(&[50.0, 100.0], 300.0, 10.0),
            vec![50.0, 100.0]
        );
        assert_eq!(
            column_widths(&[40.0, 400.0, 400.0], 400.0, 10.0),
            vec![40.0, 180.0, 180.0]
        );
        let w = column_widths(&[30.0, 90.0, 900.0], 300.0, 10.0);
        assert_eq!(w[..2], [30.0, 90.0]);
        assert!(w.iter().sum::<f32>() <= 300.0, "{w:?}");
        // `min` wins over the share: then the sum may overflow, but no column is below it.
        assert_eq!(
            column_widths(&[400.0, 4000.0], 100.0, 30.0),
            vec![30.0, 90.0]
        );
    }

    /// Footnotes go to the end of the block under a short rule, one numbered line each.
    #[test]
    fn footnotes_go_to_the_end_under_a_short_rule() {
        let mut ts = TextSystem::new();
        let src = "[^a]: First.\n\nBody[^a].\n\n[^b]: Second.";
        let p = layout_prose(&mut ts, src, 14.0, 300.0);
        assert_eq!(
            p.text_id.len(),
            5,
            "the body, then a marker and a text per note"
        );
        let rule = *p.rules.last().unwrap();
        assert!(rule[2] <= 100.0 && rule[1] > p.text_y[0], "{rule:?}");
        assert!(p.text_y[1] > rule[1] && p.text_y[3] > p.text_y[1]);
    }

    /// An image alone on its line is drawn from the registered image, scaled down to the width
    /// (never up); one that is not registered shows its alt text instead.
    #[test]
    fn images_scale_to_the_width_or_show_their_alt_text() {
        let mut ts = TextSystem::new();
        let src = "Before\n\n![plot](img/a.png)\n\nAfter";
        let missing = layout_prose(&mut ts, src, 14.0, 300.0);
        assert!(missing.image_id.is_empty());
        assert_eq!(missing.text_id.len(), 3, "the alt text stands in");
        let img = crate::Image::new("img/a.png", 400, 200, vec![255; 400 * 200 * 4]);
        assert!(ts.set_image(img));
        let p = layout_prose(&mut ts, src, 14.0, 300.0);
        let r = p.image_rect[0];
        assert_eq!(p.image_rect.len(), 1);
        assert_eq!((r[0], r[2], r[3]), (0.0, 300.0, 150.0));
        assert!(r[1] > p.text_y[0] && p.text_y[1] > r[1] + 149.0, "{r:?}");
        let wide = layout_prose(&mut ts, src, 14.0, 900.0);
        let r = wide.image_rect[0];
        assert_eq!((r[2], r[3]), (400.0, 200.0), "never scaled up");
    }
}
