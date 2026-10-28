//! The page content both PostScript dialects share: the drawing fitted to the page, then
//! every primitive kind as path operators (PDF or PostScript spelling), texts in the
//! embedded faces with the glyphs each used.
//!
//! Contents:
//! - `pub(super) enum Lang`
//! - `pub(super) type Glyphs`
//! - `pub(super) type Used`
//! - `pub(super) fn fit`
//! - `pub(super) fn arc_beziers`
//! - `pub(super) fn page_ops`

use schemify_canvas::{CanvasPrims, LAYERS, LineStyle};

use super::{FONT_NAMES, Look, arc_point};

/// Which page-description language a vector page is written in: PDF content-stream
/// operators or PostScript. Same geometry, two spellings.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Lang {
    Pdf,
    Ps,
}

/// Glyphs one face prints: glyph id → (advance in font units, a character it prints).
/// Ordered, so the font tables written from it are deterministic.
pub(super) type Glyphs = std::collections::BTreeMap<u16, (u16, char)>;

/// Glyphs the texts of a document print, per [`schemify_ui::ui_fonts`] face; a face with
/// none is not embedded.
pub(super) type Used = [Glyphs; 2];

/// Page coordinates of world bounds `b` fitted (aspect kept) into a `page` of points with a
/// `margin`: `(scale points per milli-unit, x offset, y top)`. PDF and PostScript have y up.
pub(super) fn fit(b: [i32; 4], page: (f64, f64), margin: f64) -> (f64, f64, f64) {
    let w = (f64::from(b[2]) - f64::from(b[0])).max(1.0);
    let h = (f64::from(b[3]) - f64::from(b[1])).max(1.0);
    let s = ((page.0 - 2.0 * margin) / w).min((page.1 - 2.0 * margin) / h);
    let ox = margin + ((page.0 - 2.0 * margin) - w * s) / 2.0;
    let top = page.1 - margin - ((page.1 - 2.0 * margin) - h * s) / 2.0;
    (s, ox, top)
}

/// Cubic Bézier approximation of an arc (world milli-units, angles in milli-degrees
/// clockwise on screen, as [`arc_point`]): the start point, then three points per segment of
/// at most 90°, each rounded to a milli-unit (so platform `sin`/`cos`/`tan` last-bit
/// differences do not reach the bytes). Radial error below 0.03% of the radius.
pub(super) fn arc_beziers(cx: i32, cy: i32, r: i32, start: i64, sweep: i64) -> Vec<(i32, i32)> {
    let sweep = sweep.clamp(-360_000, 360_000);
    let n = sweep.unsigned_abs().div_ceil(90_000).max(1) as i64;
    let rf = f64::from(r);
    let at = |x: f64, y: f64| {
        let q = |v: f64| v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
        (q(f64::from(cx) + x), q(f64::from(cy) + y))
    };
    let end = |d: i64| {
        let p = arc_point(cx, cy, r, d);
        let q = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        (q(p.0), q(p.1))
    };
    let mut out = Vec::with_capacity(1 + 3 * n as usize);
    out.push(end(start));
    for k in 0..n {
        let d0 = start + sweep * k / n;
        let d1 = start + sweep * (k + 1) / n;
        let (a0, a1) = (
            (d0 as f64 / 1000.0).to_radians(),
            (d1 as f64 / 1000.0).to_radians(),
        );
        // Control points at 4/3 · tan(Δ/4) of the radius along the end tangents.
        let kk = 4.0 / 3.0 * ((a1 - a0) / 4.0).tan() * rf;
        let (c0, s0, c1, s1) = (a0.cos(), a0.sin(), a1.cos(), a1.sin());
        out.push(at(rf * c0 - kk * s0, rf * s0 + kk * c0));
        out.push(at(rf * c1 + kk * s1, rf * s1 - kk * c1));
        out.push(end(d1));
    }
    assert_eq!(out.len(), 1 + 3 * n as usize);
    out
}

/// Writes one page of `prims` (world bounds `b`) as content operators in `lang`; the glyphs
/// its texts print are added to `used`.
pub(super) fn page_ops(
    prims: &CanvasPrims,
    b: [i32; 4],
    look: &Look,
    page: (f64, f64),
    lang: Lang,
    used: &mut Used,
) -> String {
    use std::fmt::Write;
    assert!(
        page.0 > 48.0 && page.1 > 48.0,
        "a page wider than its margins"
    );
    let (s, ox, top) = fit(b, page, 24.0);
    let px = |x: i32| ox + (f64::from(x) - f64::from(b[0])) * s;
    let py = |y: i32| top - (f64::from(y) - f64::from(b[1])) * s;
    let mut o = String::with_capacity(prims.len() * 40 + 256);
    let color = |o: &mut String, c: u32, fill: bool| {
        let (r, g, bl) = (
            f64::from((c >> 16) & 0xff) / 255.0,
            f64::from((c >> 8) & 0xff) / 255.0,
            f64::from(c & 0xff) / 255.0,
        );
        let _ = match (lang, fill) {
            (Lang::Pdf, false) => writeln!(o, "{r:.3} {g:.3} {bl:.3} RG"),
            (Lang::Pdf, true) => writeln!(o, "{r:.3} {g:.3} {bl:.3} rg"),
            (Lang::Ps, _) => writeln!(o, "{r:.3} {g:.3} {bl:.3} setrgbcolor"),
        };
    };
    // Line width (and, for a dashed row, its dash, which `undash` resets after the stroke).
    let width = |o: &mut String, l: u8, st: LineStyle| {
        let w = f64::from(look.palette.width[usize::from(l) % LAYERS]).max(0.1) * 0.5;
        let lw = w * f64::from(st.weight());
        let _ = match lang {
            Lang::Pdf => writeln!(o, "{lw:.2} w"),
            Lang::Ps => writeln!(o, "{lw:.2} setlinewidth"),
        };
        if st.dash() > 0 {
            let d = 2.0 * f64::from(st.dash()) * w;
            let _ = match lang {
                Lang::Pdf => writeln!(o, "[{d:.2} {d:.2}] 0 d"),
                Lang::Ps => writeln!(o, "[{d:.2} {d:.2}] 0 setdash"),
            };
        }
    };
    let undash = |o: &mut String, st: LineStyle| {
        if st.dash() > 0 {
            o.push_str(if lang == Lang::Pdf {
                "[] 0 d\n"
            } else {
                "[] 0 setdash\n"
            });
        }
    };
    let (mv, ln, stroke, close) = match lang {
        Lang::Pdf => ("m", "l", "S", "h"),
        Lang::Ps => ("moveto", "lineto", "stroke", "closepath"),
    };
    // Background, then a clip to the drawing box (a view export cuts what lies outside).
    let (x0, y0, x1, y1) = (px(b[0]), py(b[3]), px(b[2]), py(b[1]));
    if look.color {
        color(&mut o, look.background(), true);
        let _ = match lang {
            Lang::Pdf => writeln!(o, "{x0:.2} {y0:.2} {:.2} {:.2} re f", x1 - x0, y1 - y0),
            Lang::Ps => writeln!(
                o,
                "newpath {x0:.2} {y0:.2} moveto {x1:.2} {y0:.2} lineto {x1:.2} {y1:.2} lineto {x0:.2} {y1:.2} lineto closepath fill"
            ),
        };
    }
    let _ = match lang {
        Lang::Pdf => writeln!(o, "{x0:.2} {y0:.2} {:.2} {:.2} re W n", x1 - x0, y1 - y0),
        Lang::Ps => writeln!(
            o,
            "newpath {x0:.2} {y0:.2} moveto {x1:.2} {y0:.2} lineto {x1:.2} {y1:.2} lineto {x0:.2} {y1:.2} lineto closepath clip newpath"
        ),
    };
    let l = &prims.lines;
    for i in 0..l.x0.len() {
        let Some(c) = look.rgb(l.layer[i]) else {
            continue;
        };
        color(&mut o, c, false);
        width(&mut o, l.layer[i], l.style[i]);
        let _ = writeln!(
            o,
            "{:.2} {:.2} {mv} {:.2} {:.2} {ln} {stroke}",
            px(l.x0[i]),
            py(l.y0[i]),
            px(l.x1[i]),
            py(l.y1[i])
        );
        undash(&mut o, l.style[i]);
    }
    // One stroked (and, closed on a filled layer, filled) path built by the operators `body`.
    let finish = |o: &mut String, body: &str, closed: bool, layer: u8, st: LineStyle, c: u32| {
        let fill = closed && (look.filled(layer) || st.fills());
        color(o, c, false);
        if fill {
            color(o, c, true);
        }
        width(o, layer, st);
        if lang == Lang::Ps {
            o.push_str("newpath ");
        }
        o.push_str(body);
        if closed {
            o.push_str(close);
            o.push(' ');
        }
        let end = match (lang, fill) {
            (Lang::Pdf, true) => "B",
            (Lang::Ps, true) => "gsave fill grestore stroke",
            _ => stroke,
        };
        let _ = writeln!(o, "{end}");
        undash(o, st);
    };
    let path =
        |o: &mut String, pts: &[(f64, f64)], closed: bool, layer: u8, st: LineStyle, c: u32| {
            let mut body = String::with_capacity(pts.len() * 16);
            for (k, p) in pts.iter().enumerate() {
                let op = if k == 0 { mv } else { ln };
                let _ = write!(body, "{:.2} {:.2} {op} ", p.0, p.1);
            }
            finish(o, &body, closed, layer, st, c);
        };
    let r = &prims.rects;
    for i in 0..r.x0.len() {
        let Some(c) = look.rgb(r.layer[i]) else {
            continue;
        };
        let pts = [
            (px(r.x0[i]), py(r.y0[i])),
            (px(r.x1[i]), py(r.y0[i])),
            (px(r.x1[i]), py(r.y1[i])),
            (px(r.x0[i]), py(r.y1[i])),
        ];
        path(&mut o, &pts, true, r.layer[i], r.style[i], c);
    }
    let a = &prims.arcs;
    for i in 0..a.cx.len() {
        let Some(c) = look.rgb(a.layer[i]) else {
            continue;
        };
        // True arcs: PostScript's `arc`/`arcn`, PDF cubic Béziers of at most 90° each.
        let sweep = i64::from(a.sweep[i]).clamp(-360_000, 360_000);
        let full = sweep.unsigned_abs() >= 360_000;
        let mut body = String::with_capacity(128);
        match lang {
            Lang::Ps => {
                // Page y is up: a clockwise screen sweep from `start` is a clockwise page turn
                // (`arcn`) from the negated angle.
                let start = i64::from(a.start[i]);
                let (d0, d1) = (-start, -(start + sweep));
                let op = if sweep >= 0 { "arcn" } else { "arc" };
                let _ = write!(
                    body,
                    "{:.2} {:.2} {:.2} {:.3} {:.3} {op} ",
                    px(a.cx[i]),
                    py(a.cy[i]),
                    f64::from(a.r[i]) * s,
                    d0 as f64 / 1000.0,
                    d1 as f64 / 1000.0
                );
            }
            Lang::Pdf => {
                let pts = arc_beziers(a.cx[i], a.cy[i], a.r[i], i64::from(a.start[i]), sweep);
                let _ = write!(body, "{:.2} {:.2} m ", px(pts[0].0), py(pts[0].1));
                for seg in pts[1..].chunks_exact(3) {
                    let _ = write!(
                        body,
                        "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c ",
                        px(seg[0].0),
                        py(seg[0].1),
                        px(seg[1].0),
                        py(seg[1].1),
                        px(seg[2].0),
                        py(seg[2].1)
                    );
                }
            }
        }
        finish(&mut o, &body, full, a.layer[i], a.style[i], c);
    }
    for (polys, closed) in [(&prims.polylines, false), (&prims.polygons, true)] {
        let mut start = 0usize;
        for (k, &end) in polys.end.iter().enumerate() {
            let end = end as usize;
            if let Some(c) = look.rgb(polys.layer[k]) {
                let pts: Vec<(f64, f64)> = (start..end)
                    .map(|j| (px(polys.x[j]), py(polys.y[j])))
                    .collect();
                path(&mut o, &pts, closed, polys.layer[k], polys.style[k], c);
            }
            start = end;
        }
    }
    let t = &prims.texts;
    let mut start = 0usize;
    let mut glyphs: Vec<schemify_ui::FontGlyph> = Vec::new();
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        let text = &t.text[start..end];
        start = end;
        let Some(c) = look.rgb(t.layer[i]) else {
            continue;
        };
        let size = (f64::from(t.size[i]) * s).max(0.5);
        // The anchor is the text's top-left; the baseline sits 0.8 em below it, in the
        // text's own (turned) frame. A clockwise screen turn is a negative page turn.
        let (x, y) = (px(t.x[i]), py(t.y[i]));
        let rot = t.rot.get(i).copied().unwrap_or(0) % 4;
        let (cos, sin) = [(1, 0), (0, -1), (-1, 0), (0, 1)][usize::from(rot)];
        // Glyphs of the embedded UI faces, one per character (no shaping), in runs of one
        // face each; the text position advances by the glyph widths across runs.
        glyphs.clear();
        schemify_ui::ui_font_glyphs(text, &mut glyphs);
        if glyphs.is_empty() {
            continue; // an empty text prints nothing and must not name a font
        }
        for (ch, g) in text.chars().zip(&glyphs) {
            used[usize::from(g.face)]
                .entry(g.id)
                .or_insert((g.advance, ch));
        }
        let mut shown = String::with_capacity(glyphs.len() * 16);
        let mut face = u8::MAX;
        for g in &glyphs {
            if g.face != face {
                face = g.face;
                let _ = match lang {
                    Lang::Pdf if shown.is_empty() => write!(shown, "/F{} {size:.2} Tf <", face + 1),
                    Lang::Pdf => write!(shown, "> Tj /F{} {size:.2} Tf <", face + 1),
                    Lang::Ps => write!(
                        shown,
                        "/{} findfont {size:.2} scalefont setfont ",
                        FONT_NAMES[usize::from(face)]
                    ),
                };
            }
            let _ = match (lang, g.id) {
                (Lang::Pdf, id) => write!(shown, "{id:04X}"),
                (Lang::Ps, 0) => write!(shown, "/.notdef glyphshow "),
                (Lang::Ps, id) => write!(shown, "/g{id} glyphshow "),
            };
        }
        if lang == Lang::Pdf {
            shown.push_str("> Tj ");
        }
        color(&mut o, c, true);
        let base = size * 0.8;
        let _ = match lang {
            Lang::Pdf => writeln!(
                o,
                "BT {cos} {sin} {} {cos} {x:.2} {y:.2} Tm 0 {:.2} Td {shown}ET",
                -sin, -base
            ),
            Lang::Ps => writeln!(
                o,
                "gsave {x:.2} {y:.2} translate {} rotate 0 {:.2} moveto {shown}grestore",
                -90 * i32::from(rot),
                -base
            ),
        };
    }
    o
}
