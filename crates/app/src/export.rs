//! `export`: the drawn canvas tables (`render::canvas::CanvasPrims`) written as vector image
//! files — SVG, PDF (one page per drawing) and EPS, fitted to a [`Page`]. The PNG export is
//! the CPU raster of the same tables through `render::Painter` (`Editor::export_png`); these
//! writers walk the very tables that raster draws, so every format shows the same
//! primitives. Arcs are true arcs (SVG `A`, PostScript `arc`/`arcn`, PDF Béziers); PDF and
//! EPS texts print in the embedded UI faces (`schemify_ui::ui_fonts`), so non-ASCII text
//! prints as on the canvas.
//!
//! Data design:
//! 1. In: primitive tables in world milli-units (D33), their world bounds, a palette.
//!    Out: file bytes.
//! 2. How many: one drawing per page; up to `CanvasPrims::CAPACITY` primitives per kind.
//! 3. Widths: coordinates `i32` milli-units in; SVG keeps them exact (shortest decimal of
//!    the milli-unit, `format::format_coord`); PDF/PS page coordinates are points printed
//!    with two decimals.
//! 4. Access: one linear pass per primitive kind, in the order the canvas draws them.
//! 5. Lifetime: the output string only.
//! 6. Parallel: no (I/O-sized, cold path).
//!
//! Determinism (AGENTS rule 4): no hash maps, fixed decimal formatting; arc end and control
//! points are rounded to milli-units before printing, so platform `sin`/`cos` last-bit
//! differences do not reach the bytes; font tables are written from ordered maps.

use schemify_format::format_coord;
use schemify_render::canvas::{CanvasPrims, LAYERS, Palette};

use crate::scene::Scene;

/// How a drawing is colored: the canvas palette (layer colors, widths, fills) or black on
/// white, with or without a background rectangle.
#[derive(Clone, Debug)]
pub struct Look {
    /// Layer colors, widths, fills and the background.
    pub palette: Palette,
    /// Layer colors (xschem "Color Postscript/SVG"); `false` draws black on white.
    pub color: bool,
    /// No background rectangle (SVG only, xschem "Transparent SVG background").
    pub transparent: bool,
}

impl Look {
    /// `0xRRGGBB` of layer `l` as exported, or `None` when the layer is hidden (alpha 0).
    fn rgb(&self, l: u8) -> Option<u32> {
        let c = self.palette.color[usize::from(l) % LAYERS];
        if c & 0xff == 0 {
            return None;
        }
        Some(if self.color { c >> 8 } else { 0 })
    }

    fn background(&self) -> u32 {
        if self.color {
            self.palette.background >> 8
        } else {
            0x00ff_ffff
        }
    }

    fn filled(&self, l: u8) -> bool {
        self.palette.fill & (1 << (u32::from(l) % 32)) != 0
    }
}

/// The primitives of `sc` owned by objects with `keep[object]` set (the selection export).
///
/// # Panics
///
/// Panics if `keep` is not one flag per scene object.
pub fn subset(sc: &Scene, keep: &[bool]) -> CanvasPrims {
    assert_eq!(keep.len(), sc.objs.len(), "one flag per object");
    let on = |o: u32| keep.get(o as usize).copied().unwrap_or(false);
    let (p, o) = (&sc.prims, &sc.owner);
    let mut out = CanvasPrims::default();
    for i in 0..p.lines.x0.len() {
        if on(o.lines[i]) {
            let l = &p.lines;
            out.line(l.x0[i], l.y0[i], l.x1[i], l.y1[i], l.layer[i]);
        }
    }
    for i in 0..p.rects.x0.len() {
        if on(o.rects[i]) {
            let r = &p.rects;
            out.rect(r.x0[i], r.y0[i], r.x1[i], r.y1[i], r.layer[i]);
        }
    }
    for i in 0..p.arcs.cx.len() {
        if on(o.arcs[i]) {
            let a = &p.arcs;
            out.arc(a.cx[i], a.cy[i], a.r[i], a.start[i], a.sweep[i], a.layer[i]);
        }
    }
    for (polys, owners, closed) in [
        (&p.polylines, &o.polylines, false),
        (&p.polygons, &o.polygons, true),
    ] {
        let mut start = 0usize;
        for (k, &end) in polys.end.iter().enumerate() {
            let end = end as usize;
            if on(owners[k]) && end - start >= 2 {
                let pts: Vec<(i32, i32)> = (start..end).map(|j| (polys.x[j], polys.y[j])).collect();
                out.poly(&pts, closed, polys.layer[k]);
            }
            start = end;
        }
    }
    let t = &p.texts;
    let mut start = 0usize;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        if on(o.texts[i]) {
            let rot = t.rot.get(i).copied().unwrap_or(0);
            out.text_rot(
                t.x[i],
                t.y[i],
                t.size[i],
                t.layer[i],
                &t.text[start..end],
                rot,
            );
        }
        start = end;
    }
    assert!(out.len() <= p.len());
    out
}

/// World bounds `[x0, y0, x1, y1]` of everything in `prims` (`None` when empty).
pub fn bounds(prims: &CanvasPrims) -> Option<[i32; 4]> {
    let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    let mut grow = |x: i32, y: i32| {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    };
    let l = &prims.lines;
    for i in 0..l.x0.len() {
        grow(l.x0[i], l.y0[i]);
        grow(l.x1[i], l.y1[i]);
    }
    let r = &prims.rects;
    for i in 0..r.x0.len() {
        grow(r.x0[i], r.y0[i]);
        grow(r.x1[i], r.y1[i]);
    }
    let a = &prims.arcs;
    for i in 0..a.cx.len() {
        grow(a.cx[i] - a.r[i], a.cy[i] - a.r[i]);
        grow(a.cx[i] + a.r[i], a.cy[i] + a.r[i]);
    }
    for polys in [&prims.polylines, &prims.polygons] {
        for j in 0..polys.x.len() {
            grow(polys.x[j], polys.y[j]);
        }
    }
    let t = &prims.texts;
    let mut start = 0usize;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        let n = t.text[start..end].chars().count().max(1) as i64;
        let w = (i64::from(t.size[i]) * 6 * n / 10).min(i64::from(i32::MAX / 4)) as i32;
        grow(t.x[i], t.y[i]);
        grow(t.x[i].saturating_add(w), t.y[i].saturating_add(t.size[i]));
        start = end;
    }
    (b[0] <= b[2]).then_some(b)
}

/// Grows `b` by `pad` milli-units on every side (saturating).
pub fn padded(b: [i32; 4], pad: i32) -> [i32; 4] {
    [
        b[0].saturating_sub(pad),
        b[1].saturating_sub(pad),
        b[2].saturating_add(pad),
        b[3].saturating_add(pad),
    ]
}

fn units(m: i64) -> String {
    let mut s = String::new();
    format_coord(
        m.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        &mut s,
    );
    s
}

/// Arc end point at `deg_milli` (clockwise on screen, y down), rounded to milli-units.
fn arc_point(cx: i32, cy: i32, r: i32, deg_milli: i64) -> (i64, i64) {
    let t = (deg_milli as f64 / 1000.0).to_radians();
    let r = f64::from(r);
    (
        i64::from(cx) + (r * t.cos()).round() as i64,
        i64::from(cy) + (r * t.sin()).round() as i64,
    )
}

fn xml_escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
}

/// The drawing `prims` inside world bounds `b` as an SVG document in xschem units (one user
/// unit = one xschem unit). One SVG element per primitive, in canvas draw order (lines,
/// rects, arcs, polylines, polygons, texts), hidden layers left out.
///
/// # Panics
///
/// Panics if `b` is inverted.
pub fn svg(prims: &CanvasPrims, b: [i32; 4], look: &Look) -> String {
    use std::fmt::Write;
    assert!(b[0] <= b[2] && b[1] <= b[3], "inverted bounds");
    let (w, h) = (
        i64::from(b[2]) - i64::from(b[0]),
        i64::from(b[3]) - i64::from(b[1]),
    );
    let mut s = String::with_capacity(256 + prims.len() * 80);
    let _ = writeln!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{} {} {} {}\" width=\"{}\" height=\"{}\">",
        units(i64::from(b[0])),
        units(i64::from(b[1])),
        units(w.max(1)),
        units(h.max(1)),
        units(w.max(1)),
        units(h.max(1))
    );
    // Stroke width in user units: the palette's pixels at one pixel per xschem unit.
    let stroke = |l: u8| look.palette.width[usize::from(l) % LAYERS].max(0.1);
    if !look.transparent {
        let _ = writeln!(
            s,
            "<rect id=\"background\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#{:06x}\"/>",
            units(i64::from(b[0])),
            units(i64::from(b[1])),
            units(w.max(1)),
            units(h.max(1)),
            look.background()
        );
    }
    let l = &prims.lines;
    for i in 0..l.x0.len() {
        let Some(c) = look.rgb(l.layer[i]) else {
            continue;
        };
        let _ = writeln!(
            s,
            "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"#{c:06x}\" stroke-width=\"{}\"/>",
            units(l.x0[i].into()),
            units(l.y0[i].into()),
            units(l.x1[i].into()),
            units(l.y1[i].into()),
            stroke(l.layer[i])
        );
    }
    let r = &prims.rects;
    for i in 0..r.x0.len() {
        let Some(c) = look.rgb(r.layer[i]) else {
            continue;
        };
        let (x0, x1) = (r.x0[i].min(r.x1[i]), r.x0[i].max(r.x1[i]));
        let (y0, y1) = (r.y0[i].min(r.y1[i]), r.y0[i].max(r.y1[i]));
        let fill = if look.filled(r.layer[i]) {
            format!("#{c:06x}")
        } else {
            "none".to_owned()
        };
        let _ = writeln!(
            s,
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{fill}\" stroke=\"#{c:06x}\" stroke-width=\"{}\"/>",
            units(x0.into()),
            units(y0.into()),
            units(i64::from(x1) - i64::from(x0)),
            units(i64::from(y1) - i64::from(y0)),
            stroke(r.layer[i])
        );
    }
    let a = &prims.arcs;
    for i in 0..a.cx.len() {
        let Some(c) = look.rgb(a.layer[i]) else {
            continue;
        };
        let full = a.sweep[i].unsigned_abs() >= 360_000;
        let fill = if full && look.filled(a.layer[i]) {
            format!("#{c:06x}")
        } else {
            "none".to_owned()
        };
        if full {
            let _ = writeln!(
                s,
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"{fill}\" stroke=\"#{c:06x}\" stroke-width=\"{}\"/>",
                units(a.cx[i].into()),
                units(a.cy[i].into()),
                units(a.r[i].into()),
                stroke(a.layer[i])
            );
            continue;
        }
        let start = i64::from(a.start[i]);
        let end = start + i64::from(a.sweep[i]);
        let p0 = arc_point(a.cx[i], a.cy[i], a.r[i], start);
        let p1 = arc_point(a.cx[i], a.cy[i], a.r[i], end);
        let large = u8::from(a.sweep[i].unsigned_abs() > 180_000);
        let dir = u8::from(a.sweep[i] > 0);
        let _ = writeln!(
            s,
            "<path d=\"M {} {} A {} {} 0 {large} {dir} {} {}\" fill=\"none\" stroke=\"#{c:06x}\" stroke-width=\"{}\"/>",
            units(p0.0),
            units(p0.1),
            units(a.r[i].into()),
            units(a.r[i].into()),
            units(p1.0),
            units(p1.1),
            stroke(a.layer[i])
        );
    }
    for (polys, closed) in [(&prims.polylines, false), (&prims.polygons, true)] {
        let mut start = 0usize;
        for (k, &end) in polys.end.iter().enumerate() {
            let end = end as usize;
            let layer = polys.layer[k];
            let Some(c) = look.rgb(layer) else {
                start = end;
                continue;
            };
            let mut pts = String::new();
            for j in start..end {
                let _ = write!(
                    pts,
                    "{},{} ",
                    units(polys.x[j].into()),
                    units(polys.y[j].into())
                );
            }
            let fill = if closed && look.filled(layer) {
                format!("#{c:06x}")
            } else {
                "none".to_owned()
            };
            let tag = if closed { "polygon" } else { "polyline" };
            let _ = writeln!(
                s,
                "<{tag} points=\"{}\" fill=\"{fill}\" stroke=\"#{c:06x}\" stroke-width=\"{}\"/>",
                pts.trim_end(),
                stroke(layer)
            );
            start = end;
        }
    }
    let t = &prims.texts;
    let mut start = 0usize;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        let text = &t.text[start..end];
        start = end;
        let Some(c) = look.rgb(t.layer[i]) else {
            continue;
        };
        let mut esc = String::new();
        xml_escape(text, &mut esc);
        let (x, y) = (units(t.x[i].into()), units(t.y[i].into()));
        let rot = t.rot.get(i).copied().unwrap_or(0);
        let turn = if rot == 0 {
            String::new()
        } else {
            format!(" transform=\"rotate({} {x} {y})\"", u32::from(rot) * 90)
        };
        let _ = writeln!(
            s,
            "<text x=\"{x}\" y=\"{y}\" font-size=\"{}\" font-family=\"sans-serif\" dominant-baseline=\"hanging\" fill=\"#{c:06x}\"{turn}>{esc}</text>",
            units(t.size[i].into())
        );
    }
    s.push_str("</svg>\n");
    s
}

/// Paper sizes a vector export fits its drawing to: `(name, width, height)` in points
/// (1/72 in), portrait.
pub const PAPER: [(&str, f64, f64); 6] = [
    ("a4", 595.0, 842.0),
    ("a3", 842.0, 1191.0),
    ("a5", 420.0, 595.0),
    ("letter", 612.0, 792.0),
    ("legal", 612.0, 1008.0),
    ("tabloid", 792.0, 1224.0),
];

/// The page a PDF or EPS export is fitted to: a [`PAPER`] row and its orientation. The
/// default is A4 landscape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Page {
    /// Row of [`PAPER`].
    pub paper: u8,
    /// Portrait (else landscape).
    pub portrait: bool,
}

impl Page {
    /// Parses `<paper> [portrait|landscape]` (`a3`, `letter portrait`, `A4-portrait`;
    /// case-insensitive, landscape by default). `None` for an unknown paper or word.
    pub fn parse(s: &str) -> Option<Page> {
        let lower = s.trim().to_ascii_lowercase();
        let mut words = lower.split([' ', '-', '_']).filter(|w| !w.is_empty());
        let name = words.next()?;
        let paper = PAPER.iter().position(|p| p.0 == name)? as u8;
        let portrait = match words.next() {
            None | Some("landscape") => false,
            Some("portrait") => true,
            Some(_) => return None,
        };
        words.next().is_none().then_some(Page { paper, portrait })
    }

    /// Width and height in points.
    pub fn points(self) -> (f64, f64) {
        let (_, w, h) = PAPER[usize::from(self.paper) % PAPER.len()];
        if self.portrait { (w, h) } else { (h, w) }
    }

    /// The spelling [`Page::parse`] reads back (`a4 landscape`).
    pub fn name(self) -> String {
        let o = if self.portrait {
            "portrait"
        } else {
            "landscape"
        };
        format!("{} {o}", PAPER[usize::from(self.paper) % PAPER.len()].0)
    }
}

/// Page coordinates of world bounds `b` fitted (aspect kept) into a `page` of points with a
/// `margin`: `(scale points per milli-unit, x offset, y top)`. PDF and PostScript have y up.
fn fit(b: [i32; 4], page: (f64, f64), margin: f64) -> (f64, f64, f64) {
    let w = (f64::from(b[2]) - f64::from(b[0])).max(1.0);
    let h = (f64::from(b[3]) - f64::from(b[1])).max(1.0);
    let s = ((page.0 - 2.0 * margin) / w).min((page.1 - 2.0 * margin) / h);
    let ox = margin + ((page.0 - 2.0 * margin) - w * s) / 2.0;
    let top = page.1 - margin - ((page.1 - 2.0 * margin) - h * s) / 2.0;
    (s, ox, top)
}

/// Which page-description language a vector page is written in: PDF content-stream
/// operators or PostScript. Same geometry, two spellings.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Pdf,
    Ps,
}

/// Glyphs one face prints: glyph id → (advance in font units, a character it prints).
/// Ordered, so the font tables written from it are deterministic.
type Glyphs = std::collections::BTreeMap<u16, (u16, char)>;

/// Glyphs the texts of a document print, per [`schemify_ui::ui_fonts`] face; a face with
/// none is not embedded.
type Used = [Glyphs; 2];

/// PostScript names of the embedded UI faces (Type 42 resources, PDF `BaseFont`s), indexed
/// like [`schemify_ui::ui_fonts`]; a PDF page names face `f` `/F<f + 1>`.
const FONT_NAMES: [&str; 2] = ["SchemifyUI-NotoSans", "SchemifyUI-KaTeXMain"];

/// Cubic Bézier approximation of an arc (world milli-units, angles in milli-degrees
/// clockwise on screen, as [`arc_point`]): the start point, then three points per segment of
/// at most 90°, each rounded to a milli-unit (so platform `sin`/`cos`/`tan` last-bit
/// differences do not reach the bytes). Radial error below 0.03% of the radius.
fn arc_beziers(cx: i32, cy: i32, r: i32, start: i64, sweep: i64) -> Vec<(i32, i32)> {
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
fn page_ops(
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
    let width = |o: &mut String, l: u8| {
        let w = f64::from(look.palette.width[usize::from(l) % LAYERS]).max(0.1) * 0.5;
        let _ = match lang {
            Lang::Pdf => writeln!(o, "{w:.2} w"),
            Lang::Ps => writeln!(o, "{w:.2} setlinewidth"),
        };
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
        width(&mut o, l.layer[i]);
        let _ = writeln!(
            o,
            "{:.2} {:.2} {mv} {:.2} {:.2} {ln} {stroke}",
            px(l.x0[i]),
            py(l.y0[i]),
            px(l.x1[i]),
            py(l.y1[i])
        );
    }
    // One stroked (and, closed on a filled layer, filled) path built by the operators `body`.
    let finish = |o: &mut String, body: &str, closed: bool, layer: u8, c: u32| {
        let fill = closed && look.filled(layer);
        color(o, c, false);
        if fill {
            color(o, c, true);
        }
        width(o, layer);
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
    };
    let path = |o: &mut String, pts: &[(f64, f64)], closed: bool, layer: u8, c: u32| {
        let mut body = String::with_capacity(pts.len() * 16);
        for (k, p) in pts.iter().enumerate() {
            let op = if k == 0 { mv } else { ln };
            let _ = write!(body, "{:.2} {:.2} {op} ", p.0, p.1);
        }
        finish(o, &body, closed, layer, c);
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
        path(&mut o, &pts, true, r.layer[i], c);
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
        finish(&mut o, &body, full, a.layer[i], c);
    }
    for (polys, closed) in [(&prims.polylines, false), (&prims.polygons, true)] {
        let mut start = 0usize;
        for (k, &end) in polys.end.iter().enumerate() {
            let end = end as usize;
            if let Some(c) = look.rgb(polys.layer[k]) {
                let pts: Vec<(f64, f64)> = (start..end)
                    .map(|j| (px(polys.x[j]), py(polys.y[j])))
                    .collect();
                path(&mut o, &pts, closed, polys.layer[k], c);
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

/// ASCII85 (PDF `/ASCII85Decode`) of `bytes` with its `~>` end marker and a line break
/// every 80 characters, so a PDF carrying a font stays 7-bit text.
fn ascii85(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 5 / 4 + bytes.len() / 64 + 4);
    let mut col = 0usize;
    let mut push = |out: &mut String, c: u8| {
        out.push(char::from(c));
        col += 1;
        if col == 80 {
            out.push('\n');
            col = 0;
        }
    };
    for chunk in bytes.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        let mut v = u32::from_be_bytes(word);
        if chunk.len() == 4 && v == 0 {
            push(&mut out, b'z');
            continue;
        }
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = (v % 85) as u8 + b'!';
            v /= 85;
        }
        for &d in &digits[..chunk.len() + 1] {
            push(&mut out, d);
        }
    }
    out.push_str("~>");
    out
}

/// The `ToUnicode` CMap of the glyphs one face prints, so viewers extract and search the text.
fn to_unicode(glyphs: &Glyphs) -> String {
    use std::fmt::Write;
    let rows: Vec<(u16, char)> = glyphs
        .iter()
        .filter(|(g, _)| **g != 0)
        .map(|(g, (_, c))| (*g, *c))
        .collect();
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    // At most 100 entries per block (PDF 32000-1 §9.10.3).
    for block in rows.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", block.len());
        for &(g, c) in block {
            let _ = write!(s, "<{g:04X}> <");
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                let _ = write!(s, "{u:04X}");
            }
            s.push_str(">\n");
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

/// The PDF objects of UI face `face`, numbered from `first`: the `Type0` font (the one a
/// page names `/F<face + 1>`), its `CIDFontType2` (widths `W` of the glyphs it prints), the
/// font descriptor, the `FontFile2` stream (the whole face, ASCII85) and the `ToUnicode` CMap.
fn font_objects(first: usize, face: u8, glyphs: &Glyphs) -> [String; 5] {
    use std::fmt::Write;
    assert!(!glyphs.is_empty(), "only a face that prints is embedded");
    let name = FONT_NAMES[usize::from(face)];
    let info = schemify_ui::ui_font_info(face);
    let upem = u32::from(info.upem);
    let em = |v: i16| i64::from(v) * 1000 / i64::from(upem);
    let mut w = String::new();
    for (g, (adv, _)) in glyphs {
        let _ = write!(w, "{g} [{}] ", (u32::from(*adv) * 1000 + upem / 2) / upem);
    }
    let font = schemify_ui::ui_fonts()[usize::from(face)];
    let data = ascii85(font);
    let cmap = to_unicode(glyphs);
    let b = info.bbox;
    [
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>",
            first + 1,
            first + 4
        ),
        format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{name} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {} 0 R /CIDToGIDMap /Identity /W [{}] >>",
            first + 2,
            w.trim_end()
        ),
        format!(
            "<< /Type /FontDescriptor /FontName /{name} /Flags 32 /FontBBox [{} {} {} {}] /ItalicAngle 0 /Ascent {} /Descent {} /CapHeight {} /StemV 80 /FontFile2 {} 0 R >>",
            em(b[0]),
            em(b[1]),
            em(b[2]),
            em(b[3]),
            em(info.ascent),
            em(info.descent),
            em(info.cap_height),
            first + 3
        ),
        format!(
            "<< /Length {} /Length1 {} /Filter /ASCII85Decode >>\nstream\n{data}\nendstream",
            data.len() + 1,
            font.len()
        ),
        format!("<< /Length {} >>\nstream\n{cmap}endstream", cmap.len()),
    ]
}

/// One PDF page per `(prims, world bounds)` drawing, each fitted to `page`. Texts print in
/// the embedded UI faces (each a `Type0`/`CIDFontType2` font carrying the whole face as
/// `FontFile2`, glyph ids as 2-byte codes, a `ToUnicode` map), picked per character in the
/// canvas's fallback order, so non-ASCII text prints as on the canvas; a character neither
/// face has prints `.notdef`. Only faces that print are embedded (Noto Sans ~190 KB, KaTeX
/// Main ~65 KB as ASCII85); the file is 7-bit text.
///
/// # Panics
///
/// Panics if `pages` is empty or a bound is inverted.
pub fn pdf(pages: &[(&CanvasPrims, [i32; 4])], look: &Look, page: Page) -> Vec<u8> {
    use std::fmt::Write;
    assert!(!pages.is_empty(), "a PDF has at least one page");
    let size = page.points();
    let mut used = Used::default();
    let contents: Vec<String> = pages
        .iter()
        .map(|(prims, b)| {
            assert!(b[0] <= b[2] && b[1] <= b[3], "inverted bounds");
            page_ops(prims, *b, look, size, Lang::Pdf, &mut used)
        })
        .collect();
    // 1 catalog, 2 pages, (page, content) pairs from 3, then five objects per printing face.
    let mut next = 3 + 2 * pages.len();
    let mut fonts: [Option<usize>; 2] = [None; 2];
    let mut resources = String::new();
    for (f, glyphs) in used.iter().enumerate() {
        if !glyphs.is_empty() {
            fonts[f] = Some(next);
            let _ = write!(resources, "/F{} {next} 0 R ", f + 1);
            next += 5;
        }
    }
    let resources = if resources.is_empty() {
        "<< >>".to_owned()
    } else {
        format!("<< /Font << {resources}>> >>")
    };
    let mut objs: Vec<String> = Vec::with_capacity(next);
    let kids: Vec<String> = (0..pages.len())
        .map(|k| format!("{} 0 R", 3 + 2 * k))
        .collect();
    objs.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    objs.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    ));
    for (k, content) in contents.iter().enumerate() {
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources {resources} /Contents {} 0 R >>",
            size.0,
            size.1,
            4 + 2 * k
        ));
        objs.push(format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ));
    }
    for (f, first) in fonts.iter().enumerate() {
        if let Some(first) = *first {
            assert_eq!(objs.len() + 1, first, "font objects are numbered in order");
            objs.extend(font_objects(first, f as u8, &used[f]));
        }
    }
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objs.len());
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        let _ = write!(out, "{} 0 obj\n{body}\nendobj\n", i + 1);
    }
    let xref = out.len();
    let _ = write!(out, "xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1);
    for off in &offsets {
        let _ = writeln!(out, "{off:010} 00000 n ");
    }
    let _ = write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objs.len() + 1
    );
    assert_eq!(offsets.len(), next - 1);
    assert!(out.is_ascii(), "the PDF is 7-bit text");
    out.into_bytes()
}

/// Longest `sfnts` string of a Type 42 font: PostScript strings hold at most 65535 bytes,
/// and each must have an even length.
const SFNTS_MAX: usize = 65_534;

/// UI face `face` as a PostScript Type 42 font resource named `FONT_NAMES[face]`: the
/// TrueType file as `sfnts` hex strings broken at table boundaries, `CharStrings` naming every
/// glyph it prints (`/g<id>`) for `glyphshow`.
///
/// # Panics
///
/// Panics if a table of the face is longer than [`SFNTS_MAX`] (not the case for the shipped
/// faces: the largest table, Noto Sans's `glyf`, is 63.6 KB).
fn type42(face: u8, glyphs: &Glyphs) -> String {
    use std::fmt::Write;
    // ponytail: breaks at table boundaries only; split `glyf` at `loca` glyph boundaries if
    // a UI face ever grows a table over 64 KB.
    let name = FONT_NAMES[usize::from(face)];
    let font = schemify_ui::ui_fonts()[usize::from(face)];
    let info = schemify_ui::ui_font_info(face);
    let be16 = |o: usize| usize::from(u16::from_be_bytes([font[o], font[o + 1]]));
    let be32 = |o: usize| u32::from_be_bytes([font[o], font[o + 1], font[o + 2], font[o + 3]]);
    let tables = be16(4);
    assert!(
        tables > 0 && 12 + 16 * tables <= font.len(),
        "a table directory"
    );
    let mut cuts: Vec<usize> = (0..tables)
        .map(|i| be32(12 + 16 * i + 8) as usize)
        .collect();
    cuts.push(font.len());
    cuts.sort_unstable();
    cuts.dedup();
    let mut strings: Vec<&[u8]> = Vec::new();
    let mut from = 0usize;
    for (k, &cut) in cuts.iter().enumerate() {
        // Extend the current string while the next cut still fits.
        if cuts.get(k + 1).is_some_and(|&n| n - from <= SFNTS_MAX) {
            continue;
        }
        assert!(
            cut > from && cut - from <= SFNTS_MAX,
            "a font table over 64 KB"
        );
        strings.push(&font[from..cut]);
        from = cut;
    }
    assert_eq!(from, font.len(), "every byte of the face is in a string");
    let u = f64::from(info.upem);
    let b = info.bbox;
    let mut s = format!(
        "%%BeginResource: font {name}\n11 dict begin\n/FontName /{name} def\n/FontType 42 def\n\
         /FontMatrix [1 0 0 1 0 0] def\n/FontBBox [{:.4} {:.4} {:.4} {:.4}] def\n/PaintType 0 def\n\
         /Encoding 256 array 0 1 255 {{1 index exch /.notdef put}} for def\n\
         /CharStrings {} dict dup begin\n/.notdef 0 def\n",
        f64::from(b[0]) / u,
        f64::from(b[1]) / u,
        f64::from(b[2]) / u,
        f64::from(b[3]) / u,
        glyphs.len() + 1
    );
    for g in glyphs.keys().filter(|g| **g != 0) {
        let _ = writeln!(s, "/g{g} {g} def");
    }
    s.push_str("end def\n/sfnts [\n");
    for part in &strings {
        s.push('<');
        for (i, byte) in part.iter().enumerate() {
            if i > 0 && i % 64 == 0 {
                s.push('\n');
            }
            let _ = write!(s, "{byte:02X}");
        }
        // An odd-length string ends in a pad byte the interpreter drops (Type 42).
        if part.len() % 2 == 1 {
            s.push_str("00");
        }
        s.push_str(">\n");
    }
    s.push_str("] def\nFontName currentdict end definefont pop\n%%EndResource\n");
    s
}

/// The drawing as Encapsulated PostScript (Level 2), fitted into a `page`-sized bounding
/// box. Texts print in the embedded UI faces as Type 42 resources (only faces that print),
/// so non-ASCII text prints as on the canvas.
///
/// # Panics
///
/// Panics if `b` is inverted.
pub fn eps(prims: &CanvasPrims, b: [i32; 4], look: &Look, page: Page) -> String {
    assert!(b[0] <= b[2] && b[1] <= b[3], "inverted bounds");
    let size = page.points();
    let mut used = Used::default();
    let body = page_ops(prims, b, look, size, Lang::Ps, &mut used);
    let fonts: String = used
        .iter()
        .enumerate()
        .filter(|(_, glyphs)| !glyphs.is_empty())
        .map(|(f, glyphs)| type42(f as u8, glyphs))
        .collect();
    let out = format!(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 {} {}\n%%Creator: Schemify\n%%LanguageLevel: 2\n%%EndComments\n{fonts}gsave\n{body}grestore\nshowpage\n%%EOF\n",
        size.0 as i32, size.1 as i32
    );
    assert!(out.is_ascii(), "the EPS is 7-bit text");
    out
}

/// Primitive count per SVG element kind written by [`svg`]: `(lines, rects, circles + arc
/// paths, polylines + polygons, texts)` — the oracle of an SVG export test parses the file
/// back with this.
pub fn svg_counts(svg: &str) -> [usize; 5] {
    let count = |tag: &str| svg.matches(tag).count();
    [
        count("<line "),
        // The background rectangle is `<rect id=…`, primitives are `<rect x=…`.
        count("<rect x="),
        count("<circle ") + count("<path "),
        count("<polyline ") + count("<polygon "),
        count("<text "),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look() -> Look {
        Look {
            palette: Palette::xschem_dark(),
            color: true,
            transparent: true,
        }
    }

    /// Reference ASCII85 decoder (written from the format's definition, independent of the
    /// encoder): whitespace ignored, `z` = four zero bytes, `~>` ends.
    fn ascii85_decode(s: &str) -> Vec<u8> {
        let body = s.strip_suffix("~>").expect("end marker");
        let mut out = Vec::new();
        let mut group: Vec<u32> = Vec::new();
        for c in body.chars().filter(|c| !c.is_whitespace()) {
            if c == 'z' {
                assert!(group.is_empty(), "z only between groups");
                out.extend_from_slice(&[0; 4]);
                continue;
            }
            group.push(c as u32 - 33);
            if group.len() == 5 {
                let v = group.iter().fold(0u64, |a, &d| a * 85 + u64::from(d));
                out.extend_from_slice(&(v as u32).to_be_bytes());
                group.clear();
            }
        }
        if !group.is_empty() {
            let n = group.len() - 1;
            while group.len() < 5 {
                group.push(84);
            }
            let v = group.iter().fold(0u64, |a, &d| a * 85 + u64::from(d));
            out.extend_from_slice(&(v as u32).to_be_bytes()[..n]);
        }
        out
    }

    /// The body of PDF object `n`'s stream.
    fn stream_of(pdf: &str, n: usize) -> &str {
        let at = pdf.find(&format!("\n{n} 0 obj\n")).expect("object") + 1;
        let body = &pdf[at..];
        let s = body.find("stream\n").expect("a stream") + "stream\n".len();
        let e = body.find("endstream").expect("its end");
        &body[s..e]
    }

    /// The object number after `key` (`/ToUnicode 9 0 R` → 9).
    fn reference(pdf: &str, key: &str) -> usize {
        let at = pdf.find(key).unwrap_or_else(|| panic!("{key}")) + key.len();
        pdf[at..]
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    fn hex16(h: &[u8]) -> u16 {
        u16::from_str_radix(std::str::from_utf8(h).unwrap(), 16).unwrap()
    }

    /// The text of PDF object `n` (`n 0 obj` … `endobj`).
    fn object(pdf: &str, n: usize) -> &str {
        let at = pdf.find(&format!("\n{n} 0 obj\n")).expect("object") + 1;
        let end = pdf[at..].find("endobj").expect("its end") + at;
        &pdf[at..end]
    }

    /// The `ToUnicode` map of the page font `/F<k>` (glyph id → text), if the page names it.
    fn font_map(pdf: &str, k: usize) -> Option<std::collections::BTreeMap<u16, String>> {
        let fonts = &pdf[pdf.find("/Font << ")?..];
        let fonts = &fonts[..fonts.find(">>")?];
        let key = format!("/F{k} ");
        fonts.find(&key)?;
        let font = object(pdf, reference(fonts, &key));
        let cmap = stream_of(pdf, reference(font, "/ToUnicode "));
        let mut map = std::collections::BTreeMap::new();
        for line in cmap.lines().filter(|l| l.starts_with('<')) {
            let mut it = line.split_whitespace();
            let (g, u) = (it.next().unwrap(), it.next().unwrap());
            if g == "<0000>" && u == "<FFFF>" {
                continue; // the codespace range
            }
            let units: Vec<u16> = u[1..u.len() - 1].as_bytes().chunks(4).map(hex16).collect();
            map.insert(
                hex16(&g.as_bytes()[1..5]),
                String::from_utf16(&units).unwrap(),
            );
        }
        Some(map)
    }

    /// Every `BT … ET` text of `pdf`: its `/F<k> … Tf <…> Tj` runs decoded through the
    /// `ToUnicode` map of each run's font (a glyph without a mapping → U+FFFD).
    fn shown_texts(pdf: &str) -> Vec<String> {
        let maps = [font_map(pdf, 1), font_map(pdf, 2)];
        let mut out = Vec::new();
        for line in pdf.lines().filter(|l| l.starts_with("BT ")) {
            let mut font = 0usize;
            let mut text = String::new();
            for tok in line.split_whitespace() {
                if let Some(k) = tok.strip_prefix("/F") {
                    font = k.parse().unwrap();
                } else if let Some(hex) = tok.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
                    let map = maps[font - 1]
                        .as_ref()
                        .expect("the run's font is a resource");
                    for h in hex.as_bytes().chunks(4) {
                        text.push_str(map.get(&hex16(h)).map_or("\u{fffd}", String::as_str));
                    }
                }
            }
            out.push(text);
        }
        out
    }

    /// The decoded `sfnts` of every Type 42 font in `eps`, checking the string rules (even
    /// lengths under the limit) on the way.
    fn sfnts(eps: &str) -> Vec<Vec<u8>> {
        let mut fonts = Vec::new();
        let mut rest = eps;
        while let Some(at) = rest.find("/sfnts [\n") {
            let at = at + "/sfnts [\n".len();
            let end = rest[at..].find("] def").unwrap() + at;
            let mut bytes = Vec::new();
            for s in rest[at..end].split('>').filter(|s| s.contains('<')) {
                let hex: String = s[s.find('<').unwrap() + 1..]
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                assert!(
                    hex.len() % 4 == 0 && hex.len() / 2 <= SFNTS_MAX,
                    "{}",
                    hex.len()
                );
                for k in (0..hex.len()).step_by(2) {
                    bytes.push(u8::from_str_radix(&hex[k..k + 2], 16).unwrap());
                }
            }
            fonts.push(bytes);
            rest = &rest[end..];
        }
        fonts
    }

    #[test]
    fn ascii85_round_trips() {
        for n in 0..=13 {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(ascii85_decode(&ascii85(&bytes)), bytes, "{n} bytes");
        }
        assert_eq!(ascii85_decode(&ascii85(&[0, 0, 0, 0, 0])), [0; 5]);
        for font in schemify_ui::ui_fonts() {
            assert_eq!(ascii85_decode(&ascii85(font)), font);
            assert!(ascii85(font).lines().all(|l| l.len() <= 80));
        }
    }

    #[test]
    fn svg_writes_one_element_per_primitive() {
        let mut p = CanvasPrims::default();
        p.line(0, 0, 10_000, 0, 1);
        p.rect(0, 0, 5_000, 5_000, 4);
        p.arc(0, 0, 3_000, 0, 360_000, 4);
        p.arc(0, 0, 3_000, 0, 90_000, 4);
        p.poly(&[(0, 0), (1_000, 1_000), (2_000, 0)], true, 4);
        p.text(0, 0, 10_000, 3, "a<b");
        let s = svg(&p, [0, 0, 10_000, 10_000], &look());
        assert_eq!(svg_counts(&s), [1, 1, 2, 1, 1]);
        assert!(s.contains("a&lt;b"));
        assert!(s.contains("viewBox=\"0 0 10 10\""));
        // A true arc: one `A` command from (3, 0) to (0, 3), small, clockwise on screen.
        assert!(s.contains("d=\"M 3 0 A 3 3 0 0 1 0 3\""), "{s}");
        // Deterministic: the same tables give the same bytes.
        assert_eq!(s, svg(&p, [0, 0, 10_000, 10_000], &look()));
        // A hidden layer writes nothing.
        let mut hidden = look();
        hidden.palette.color[1] &= !0xff;
        assert_eq!(svg_counts(&svg(&p, [0, 0, 10_000, 10_000], &hidden))[0], 0);
    }

    #[test]
    fn pdf_has_one_page_per_drawing_and_a_valid_xref() {
        let mut p = CanvasPrims::default();
        p.line(0, 0, 10_000, 0, 1);
        p.text(0, 0, 5_000, 3, "(x)");
        let doc = pdf(
            &[(&p, [0, 0, 10_000, 10_000]), (&p, [0, 0, 1, 1])],
            &look(),
            Page::default(),
        );
        let text = String::from_utf8(doc).unwrap();
        assert!(text.starts_with("%PDF-1.4\n") && text.ends_with("%%EOF\n"));
        assert_eq!(text.matches("/Type /Page ").count(), 2);
        assert!(text.contains("/MediaBox [0 0 842 595]"), "A4 landscape");
        // The xref offsets point at the objects they name: 2 + 2 pages × 2 + 5 font objects.
        let xref = text.rfind("xref\n").unwrap();
        let rows: Vec<&str> = text[xref..]
            .lines()
            .skip(3)
            .take_while(|l| l.ends_with(" n "))
            .collect();
        assert_eq!(rows.len(), 11);
        for (i, line) in rows.iter().enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(
                text[off..].starts_with(&format!("{} 0 obj", i + 1)),
                "{line}"
            );
        }
        // Negative space: no texts, no font.
        let mut bare = CanvasPrims::default();
        bare.line(0, 0, 1_000, 0, 1);
        let doc = pdf(&[(&bare, [0, 0, 1_000, 1_000])], &look(), Page::default());
        let t = String::from_utf8(doc).unwrap();
        assert!(!t.contains("/Font") && !t.contains("FontFile2"));
    }

    /// Non-ASCII text prints: the texts decode back through the PDF's own `ToUnicode` maps to
    /// what the canvas shows (Noto Sans, then KaTeX Main for → and ≤), each embedded
    /// `FontFile2` is a UI face byte for byte, and the widths in `W` are the face's advances.
    /// A character neither face has prints `.notdef`.
    #[test]
    fn pdf_text_uses_the_embedded_ui_faces() {
        let mut p = CanvasPrims::default();
        let texts = ["R1 = 10 kΩ", "µA → ±5 °C", "(x) \\ y", "ΔV ≤ 3"];
        for (k, t) in texts.iter().enumerate() {
            p.text_rot(0, k as i32 * 10_000, 5_000, 3, t, (k % 4) as u8);
        }
        p.text(0, 50_000, 5_000, 3, "漢");
        let b = [0, 0, 50_000, 60_000];
        let doc = String::from_utf8(pdf(&[(&p, b)], &look(), Page::default())).unwrap();
        let shown = shown_texts(&doc);
        assert_eq!(&shown[..4], &texts[..]);
        assert_eq!(shown[4], "\u{fffd}", "outside both faces: .notdef");
        let mut files: Vec<Vec<u8>> = doc
            .match_indices("/FontFile2 ")
            .map(|(at, _)| {
                let data = stream_of(&doc, reference(&doc[at..], "/FontFile2 "));
                ascii85_decode(data.trim_end())
            })
            .collect();
        files.sort();
        let mut faces: Vec<Vec<u8>> = schemify_ui::ui_fonts().map(<[u8]>::to_vec).to_vec();
        faces.sort();
        assert_eq!(files, faces, "both faces embedded, byte for byte");
        let mut g = Vec::new();
        schemify_ui::ui_font_glyphs("Ω→", &mut g);
        for x in &g {
            let upem = u32::from(schemify_ui::ui_font_info(x.face).upem);
            let w = (u32::from(x.advance) * 1000 + upem / 2) / upem;
            let font = object(&doc, reference(&doc, &format!("/F{} ", x.face + 1)));
            let cid = object(&doc, reference(font, "/DescendantFonts ["));
            assert!(cid.contains(&format!("{} [{w}]", x.id)), "W row: {cid}");
        }
        assert!(!doc.contains("Helvetica"));
        // Negative space: an ASCII-only drawing embeds Noto Sans alone.
        let mut ascii = CanvasPrims::default();
        ascii.text(0, 0, 5_000, 3, "R1 10k");
        let one = String::from_utf8(pdf(&[(&ascii, b)], &look(), Page::default())).unwrap();
        assert_eq!(one.matches("/FontFile2 ").count(), 1);
        assert!(one.contains("/F1 ") && !one.contains("/F2 "));
        // Deterministic bytes.
        assert_eq!(
            doc.as_bytes(),
            &pdf(&[(&p, b)], &look(), Page::default())[..]
        );
    }

    #[test]
    fn eps_has_a_bounding_box_paths_and_a_type42_font() {
        let mut p = CanvasPrims::default();
        p.poly(&[(0, 0), (1_000, 0), (1_000, 1_000)], false, 4);
        let e = eps(&p, [0, 0, 1_000, 1_000], &look(), Page::default());
        assert!(e.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 842 595\n"));
        assert!(e.contains("lineto") && e.contains("stroke"));
        assert!(!e.contains("FontType 42"), "no texts, no font");
        p.text(0, 0, 500, 3, "Ω1");
        let page = Page::parse("letter portrait").unwrap();
        let e = eps(&p, [0, 0, 1_000, 1_000], &look(), page);
        assert!(e.contains("%%BoundingBox: 0 0 612 792\n"));
        // Only Noto Sans prints: one Type 42 font whose sfnts strings are its bytes.
        assert_eq!(sfnts(&e), [schemify_ui::ui_fonts()[0]]);
        p.text(0, 500, 500, 3, "→");
        let e = eps(&p, [0, 0, 1_000, 1_000], &look(), page);
        assert_eq!(sfnts(&e), schemify_ui::ui_fonts());
        // Every glyph shown is named in its face's CharStrings and selected before it shows.
        let mut g = Vec::new();
        schemify_ui::ui_font_glyphs("Ω1→", &mut g);
        assert_eq!(g[2].face, 1);
        for x in g {
            assert!(e.contains(&format!("/g{} {} def", x.id, x.id)));
            assert!(e.contains(&format!("/g{} glyphshow", x.id)));
        }
        assert!(e.contains("/SchemifyUI-KaTeXMain findfont"));
        assert!(!e.contains("Helvetica"));
    }

    /// Arcs are true arcs: Béziers whose ends are the arc's points and whose midpoints lie on
    /// the circle; PostScript turns a clockwise screen sweep with `arcn`.
    #[test]
    fn arcs_are_beziers_on_the_circle() {
        let r = 100_000;
        for (start, sweep, segs) in [
            (0, 90_000, 1),
            (30_000, 100_000, 2),
            (0, 360_000, 4),
            (45_000, -270_000, 3),
        ] {
            let pts = arc_beziers(1_000, -2_000, r, start, sweep);
            assert_eq!(pts.len(), 1 + 3 * segs, "{start} {sweep}");
            let p0 = arc_point(1_000, -2_000, r, start);
            let p1 = arc_point(1_000, -2_000, r, start + sweep);
            assert_eq!(pts[0], (p0.0 as i32, p0.1 as i32));
            assert_eq!(*pts.last().unwrap(), (p1.0 as i32, p1.1 as i32));
            for seg in pts.windows(4).step_by(3) {
                // The Bézier at t = 1/2.
                let b = |x: bool| {
                    let c = |i: usize| f64::from(if x { seg[i].0 } else { seg[i].1 });
                    0.125 * c(0) + 0.375 * c(1) + 0.375 * c(2) + 0.125 * c(3)
                };
                let d = ((b(true) - 1_000.0).powi(2) + (b(false) + 2_000.0).powi(2)).sqrt();
                assert!((d - f64::from(r)).abs() < f64::from(r) * 3e-4, "{d}");
            }
        }
        let mut p = CanvasPrims::default();
        p.arc(0, 0, 3_000, 0, 90_000, 4);
        let b = [-3_000, -3_000, 3_000, 3_000];
        let doc = String::from_utf8(pdf(&[(&p, b)], &look(), Page::default())).unwrap();
        let content = stream_of(&doc, 4);
        assert_eq!(content.matches(" c ").count(), 1, "{content}");
        assert_eq!(content.matches(" l ").count(), 0, "no polyline");
        let e = eps(&p, b, &look(), Page::default());
        assert!(e.contains(" 0.000 -90.000 arcn "), "{e}");
    }

    #[test]
    fn page_sizes_parse_and_print() {
        for paper in 0..PAPER.len() as u8 {
            for portrait in [false, true] {
                let p = Page { paper, portrait };
                assert_eq!(Page::parse(&p.name()), Some(p));
                let (w, h) = p.points();
                assert_eq!(w < h, portrait);
            }
        }
        let a3 = Page {
            paper: 1,
            portrait: false,
        };
        assert_eq!(Page::parse("A3"), Some(a3));
        let a4p = Page {
            paper: 0,
            portrait: true,
        };
        assert_eq!(Page::parse("a4-portrait"), Some(a4p));
        for bad in ["", "a7", "a4 sideways", "a4 portrait x", "portrait"] {
            assert_eq!(Page::parse(bad), None, "{bad:?}");
        }
        let mut p = CanvasPrims::default();
        p.line(0, 0, 1_000, 0, 1);
        let page = Page::parse("a3 portrait").unwrap();
        let doc = String::from_utf8(pdf(&[(&p, [0, 0, 1_000, 1_000])], &look(), page)).unwrap();
        assert!(doc.contains("/MediaBox [0 0 842 1191]"));
    }

    #[test]
    fn bounds_cover_every_primitive() {
        let mut p = CanvasPrims::default();
        assert_eq!(bounds(&p), None);
        p.line(-5, 2, 7, 3, 1);
        p.arc(100, 100, 10, 0, 360_000, 1);
        assert_eq!(bounds(&p), Some([-5, 2, 110, 110]));
        assert_eq!(padded([0, 0, 1, 1], 2), [-2, -2, 3, 3]);
    }
}
