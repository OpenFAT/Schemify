//! `prose` and `math` nodes drawn with egui: markdown blocks as wrapped rich text, inline and
//! display formulas laid out by RaTeX and painted as KaTeX glyphs, rules and paths.
//!
//! Contents: crate-only `prose_node(v, ui, n)`, `math_node(v, ui, n)`

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontFamily, FontId, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use super::{View, color, paint_paths};
use crate::fonts::{BOLD, ITALIC};
use crate::kinds::{Prop, StyleProp};
use crate::markdown::{self, BOLD as B, Block, CODE, ITALIC as I, Inline};
use crate::math::{FACE_NAMES, MathList, layout_math};
use crate::prose::{CODE_SCALE, code_pad};
use crate::state::UiState;
use crate::tables::NodeIx;

/// A `prose` node: its markdown at its font and colour, wrapped to the width; clickable when it
/// has a `command`.
pub(crate) fn prose_node(v: &mut View, ui: &mut Ui, n: NodeIx) {
    let src = v.prop(n, Prop::Value).to_owned();
    let (px, fg) = (v.font(n), color(v.style(n, StyleProp::Fg)));
    let sense = if v.t.nodes.props[Prop::Command as usize][usize::from(n)].is_none() {
        Sense::hover()
    } else {
        Sense::click()
    };
    let r = ui.scope_builder(UiBuilder::new().sense(sense), |ui| {
        ui.set_width(ui.available_width());
        prose(v.st, ui, &src, px, fg);
    });
    if r.response.clicked() {
        v.run(n, Prop::Command, "");
    }
    v.record(n, r.response.rect, None);
}

/// A `math` node: its LaTeX `value` at its font.
pub(crate) fn math_node(v: &mut View, ui: &mut Ui, n: NodeIx) {
    let src = v.prop(n, Prop::Value).to_owned();
    let (px, fg) = (v.font(n), color(v.style(n, StyleProp::Fg)));
    let r = math(v.st, ui, &src, px, fg);
    v.record(n, r.rect, None);
}

// ---- private ----

/// Formulas kept laid out; the cache is dropped whole when it fills.
const MATH_CACHED: usize = 1024;

/// Markdown `src` at `px` in `fg`.
// ponytail: parsed every frame; cache the blocks per source when docs pages get long.
fn prose(st: &mut UiState, ui: &mut Ui, src: &str, px: f32, fg: Color32) {
    let blocks = markdown::blocks(src);
    let notes: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Note(label, _) => Some(label.as_str()),
            _ => None,
        })
        .collect();
    ui.spacing_mut().item_spacing = vec2(0.0, (px * 0.6).round());
    for b in &blocks {
        match b {
            Block::Heading(level, text) => {
                let scale = [1.6, 1.35, 1.2, 1.1, 1.0, 1.0][usize::from(*level).clamp(1, 6) - 1];
                rich(st, ui, text, &notes, px * scale, fg, B);
            }
            Block::Para(text) => rich(st, ui, text, &notes, px, fg, 0),
            Block::Quote(text) => {
                ui.horizontal(|ui| {
                    let (bar, _) = ui.allocate_exact_size(vec2(3.0, px * 1.2), Sense::hover());
                    ui.painter().rect_filled(bar, 0.0, fg);
                    ui.add_space(px * 0.75);
                    ui.vertical(|ui| rich(st, ui, text, &notes, px, fg, I));
                });
            }
            Block::Item { depth, marker, text } => {
                ui.horizontal_top(|ui| {
                    ui.add_space(px * 1.5 * f32::from(*depth));
                    let m = ui.painter().layout_no_wrap(format!("{marker} "), FontId::proportional(px), fg);
                    ui.label(egui::WidgetText::Galley(m));
                    ui.vertical(|ui| rich(st, ui, text, &notes, px, fg, 0));
                });
            }
            Block::Code(_, text) => {
                let pad = code_pad(px) as i8;
                egui::Frame::new()
                    .fill(fg.gamma_multiply(0.06))
                    .inner_margin(egui::Margin::same(pad))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let font = FontId::monospace(px * CODE_SCALE);
                        ui.label(egui::RichText::new(text).font(font).color(fg));
                    });
            }
            Block::Math(src) => {
                ui.vertical_centered(|ui| {
                    math(st, ui, src, px, fg);
                });
            }
            Block::Table(head, rows) => {
                egui::Grid::new(ui.next_auto_id())
                    .spacing(vec2(px, px * 0.3))
                    .show(ui, |ui| {
                        if !head.is_empty() {
                            for c in head {
                                rich(st, ui, c, &notes, px, fg, B);
                            }
                            ui.end_row();
                        }
                        for r in rows {
                            for c in r {
                                rich(st, ui, c, &notes, px, fg, 0);
                            }
                            ui.end_row();
                        }
                    });
            }
            Block::Rule => {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, fg));
            }
            Block::Note(..) => {}
            Block::Image(alt, path) => match st.texture(ui.ctx(), path) {
                Some(tex) => {
                    let img = st.image(path).expect("a texture comes from an image");
                    let size = vec2(img.width as f32, img.height as f32);
                    let scale = (ui.available_width() / size.x).min(1.0);
                    ui.image((tex, size * scale));
                }
                None => rich(st, ui, alt, &notes, px, fg, I),
            },
        }
    }
    for (k, b) in blocks.iter().filter(|b| matches!(b, Block::Note(..))).enumerate() {
        if let Block::Note(_, text) = b {
            rich(st, ui, &format!("{}. {text}", k + 1), &notes, px * 0.85, fg, 0);
        }
    }
}

/// One inline run: wrapped rich text with its formulas inline, base style bits `base`.
fn rich(st: &mut UiState, ui: &mut Ui, src: &str, notes: &[&str], px: f32, fg: Color32, base: u8) {
    let mut sources: Vec<String> = Vec::new();
    let inl = markdown::inlines(src, notes, &mut |m: &str| {
        layout(st, m, px).is_ok().then(|| {
            sources.push(m.to_owned());
            (sources.len() - 1) as u32
        })
    });
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let mut at = 0usize;
        for &(off, id) in &inl.maths {
            let off = off as usize;
            if off > at {
                ui.label(job(&inl, at..off, px, fg, base));
            }
            math(st, ui, &sources[id as usize], px, fg);
            at = off;
        }
        if at < inl.text.len() {
            ui.label(job(&inl, at..inl.text.len(), px, fg, base));
        }
    });
}

/// The text of `inl` in `range` as a layout job, each span in its face.
fn job(inl: &Inline, range: std::ops::Range<usize>, px: f32, fg: Color32, base: u8) -> LayoutJob {
    let mut j = LayoutJob::default();
    let mut at = range.start;
    let style_at = |i: usize| {
        inl.spans
            .iter()
            .find(|s| (s.0 as usize) <= i && i < s.1 as usize)
            .map_or(0, |s| s.2)
    };
    while at < range.end {
        let bits = style_at(at) | base;
        let mut end = at + 1;
        while end < range.end && (style_at(end) | base) == bits {
            end += 1;
        }
        while !inl.text.is_char_boundary(end) {
            end += 1;
        }
        let family = if bits & CODE != 0 {
            FontFamily::Monospace
        } else if bits & B != 0 {
            FontFamily::Name(BOLD.into())
        } else if bits & I != 0 {
            FontFamily::Name(ITALIC.into())
        } else {
            FontFamily::Proportional
        };
        let mut f = TextFormat::simple(FontId::new(px, family), fg);
        if bits & CODE != 0 {
            f.background = fg.gamma_multiply(0.08);
        }
        j.append(&inl.text[at..end], 0.0, f);
        at = end;
    }
    j
}

/// The cached layout of formula `src` at `px`.
fn layout(st: &mut UiState, src: &str, px: f32) -> Result<Arc<MathList>, String> {
    let key = (src.to_owned(), px.to_bits());
    if let Some(m) = st.math.get(&key) {
        return m.clone();
    }
    if st.math.len() >= MATH_CACHED {
        st.math.clear();
    }
    let m = layout_math(src, px).map(Arc::new);
    st.math.insert(key, m.clone());
    m
}

/// Formula `src` at `px` in `fg`, allocated at its box (its LaTeX source in monospace when
/// RaTeX does not take it).
fn math(st: &mut UiState, ui: &mut Ui, src: &str, px: f32, fg: Color32) -> egui::Response {
    let m = match layout(st, src, px) {
        Ok(m) => m,
        Err(_) => return ui.label(egui::RichText::new(src).monospace().color(fg)),
    };
    let (rect, r) = ui.allocate_exact_size(vec2(m.width.max(1.0), (m.height + m.depth).max(1.0)), Sense::hover());
    let p = ui.painter();
    for k in 0..m.glyph_char.len() {
        let face = FACE_NAMES[usize::from(m.glyph_face[k])];
        let font = FontId::new(m.glyph_px[k], FontFamily::Name(face.into()));
        let g = p.layout_no_wrap(m.glyph_char[k].to_string(), font, fg);
        let base = g
            .rows
            .first()
            .and_then(|r| r.glyphs.first().map(|gl| r.pos.y + gl.pos.y))
            .unwrap_or(m.glyph_px[k] * 0.8);
        p.galley(pos2(rect.left() + m.glyph_x[k], rect.top() + m.glyph_y[k] - base), g, fg);
    }
    for &[x, y, w, h] in &m.rules {
        let rr = egui::Rect::from_min_size(pos2(rect.left() + x, rect.top() + y), vec2(w, h));
        p.rect_filled(rr, 0.0, fg);
    }
    paint_paths(p, &m.fills, &m.strokes, rect.min, 1.0, px * 0.04, fg);
    r
}
