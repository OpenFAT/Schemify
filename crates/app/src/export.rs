//! `export`: the drawn canvas tables (`render::canvas::CanvasPrims`) written as vector image
//! files — SVG, PDF (one page per drawing) and EPS. The PNG export is the CPU raster of the
//! same tables through `render::Painter` (`Editor::export_png`); these writers walk the very
//! tables that raster draws, so every format shows the same primitives.
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
//! Determinism (AGENTS rule 4): no hash maps, fixed decimal formatting; arc end points are
//! rounded to milli-units before printing, so platform `sin`/`cos` last-bit differences do
//! not reach the bytes.

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

/// Writes one page of `prims` (world bounds `b`) as content operators in `lang`.
fn page_ops(prims: &CanvasPrims, b: [i32; 4], look: &Look, page: (f64, f64), lang: Lang) -> String {
    use std::fmt::Write;
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
    let path = |o: &mut String, pts: &[(f64, f64)], closed: bool, layer: u8, c: u32| {
        let fill = closed && look.filled(layer);
        color(o, c, false);
        if fill {
            color(o, c, true);
        }
        width(o, layer);
        if lang == Lang::Ps {
            o.push_str("newpath ");
        }
        for (k, p) in pts.iter().enumerate() {
            let op = if k == 0 { mv } else { ln };
            let _ = write!(o, "{:.2} {:.2} {op} ", p.0, p.1);
        }
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
        // Polyline approximation: one segment per 6° (at most 61 points per arc).
        let sweep = i64::from(a.sweep[i]).clamp(-360_000, 360_000);
        let n = (sweep.unsigned_abs() / 6_000).clamp(2, 60);
        let pts: Vec<(f64, f64)> = (0..=n)
            .map(|k| {
                let d = i64::from(a.start[i]) + sweep * k as i64 / n as i64;
                let q = arc_point(a.cx[i], a.cy[i], a.r[i], d);
                (
                    px(q.0.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32),
                    py(q.1.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32),
                )
            })
            .collect();
        path(&mut o, &pts, sweep.unsigned_abs() >= 360_000, a.layer[i], c);
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
        let mut esc = String::with_capacity(text.len());
        for ch in text.chars() {
            match ch {
                '(' | ')' | '\\' => {
                    esc.push('\\');
                    esc.push(ch);
                }
                c if c.is_ascii() && !c.is_control() => esc.push(c),
                _ => esc.push('?'),
            }
        }
        color(&mut o, c, true);
        let base = size * 0.8;
        let _ = match lang {
            Lang::Pdf => writeln!(
                o,
                "BT /F1 {size:.2} Tf {cos} {sin} {} {cos} {x:.2} {y:.2} Tm 0 {:.2} Td ({esc}) Tj ET",
                -sin, -base
            ),
            Lang::Ps => writeln!(
                o,
                "gsave {x:.2} {y:.2} translate {} rotate /Helvetica findfont {size:.2} scalefont setfont 0 {:.2} moveto ({esc}) show grestore",
                -90 * i32::from(rot),
                -base
            ),
        };
    }
    o
}

/// A4 landscape in points.
pub const A4: (f64, f64) = (842.0, 595.0);

/// One PDF page per `(prims, world bounds)` drawing, each fitted to an A4 landscape page.
/// Texts use the standard Helvetica font (not embedded), so the file has no font data.
///
/// # Panics
///
/// Panics if `pages` is empty or a bound is inverted.
pub fn pdf(pages: &[(&CanvasPrims, [i32; 4])], look: &Look) -> Vec<u8> {
    use std::fmt::Write;
    assert!(!pages.is_empty(), "a PDF has at least one page");
    let mut objs: Vec<String> = Vec::with_capacity(3 + 2 * pages.len());
    // 1 catalog, 2 pages, 3 font, then (page, content) pairs.
    let kids: Vec<String> = (0..pages.len())
        .map(|k| format!("{} 0 R", 4 + 2 * k))
        .collect();
    objs.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    objs.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    ));
    objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());
    for (k, (prims, b)) in pages.iter().enumerate() {
        assert!(b[0] <= b[2] && b[1] <= b[3], "inverted bounds");
        let content = page_ops(prims, *b, look, A4, Lang::Pdf);
        objs.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            A4.0,
            A4.1,
            5 + 2 * k
        ));
        objs.push(format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ));
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
    assert_eq!(offsets.len(), 3 + 2 * pages.len());
    out.into_bytes()
}

/// The drawing as Encapsulated PostScript, fitted into an A4 landscape bounding box.
///
/// # Panics
///
/// Panics if `b` is inverted.
pub fn eps(prims: &CanvasPrims, b: [i32; 4], look: &Look) -> String {
    assert!(b[0] <= b[2] && b[1] <= b[3], "inverted bounds");
    let body = page_ops(prims, b, look, A4, Lang::Ps);
    format!(
        "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 {} {}\n%%Creator: Schemify\n%%EndComments\ngsave\n{body}grestore\nshowpage\n%%EOF\n",
        A4.0 as i32, A4.1 as i32
    )
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
        let doc = pdf(&[(&p, [0, 0, 10_000, 10_000]), (&p, [0, 0, 1, 1])], &look());
        let text = String::from_utf8(doc).unwrap();
        assert!(text.starts_with("%PDF-1.4\n") && text.ends_with("%%EOF\n"));
        assert_eq!(text.matches("/Type /Page ").count(), 2);
        assert!(text.contains("\\(x\\)"), "parentheses escaped");
        // The xref offsets point at the objects they name.
        let xref = text.rfind("xref\n").unwrap();
        for (i, line) in text[xref..].lines().skip(3).take(7).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(
                text[off..].starts_with(&format!("{} 0 obj", i + 1)),
                "{line}"
            );
        }
    }

    #[test]
    fn eps_has_a_bounding_box_and_paths() {
        let mut p = CanvasPrims::default();
        p.poly(&[(0, 0), (1_000, 0), (1_000, 1_000)], false, 4);
        let e = eps(&p, [0, 0, 1_000, 1_000], &look());
        assert!(e.starts_with("%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 842 595\n"));
        assert!(e.contains("lineto") && e.contains("stroke"));
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
