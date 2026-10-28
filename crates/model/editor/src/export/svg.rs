//! SVG: one `<g>` per layer in canvas draw order, coordinates as exact milli-unit decimals,
//! arcs as `A` commands, texts escaped. Also [`svg_counts`], the element census tests read.
//!
//! Contents:
//! - `pub(super) fn units`
//! - `pub fn svg`
//! - `pub fn svg_counts`

use std::fmt::Write as _;

use schemify_canvas::{CanvasPrims, LAYERS, LineStyle};

use super::{Look, arc_point};

pub(super) fn units(m: i64) -> String {
    let mut s = String::new();
    let clamped = i32::try_from(m.clamp(i64::from(i32::MIN), i64::from(i32::MAX)))
        .expect("clamped to the i32 range");
    schemify_format::format_coord(clamped, &mut s);
    assert!(!s.is_empty(), "every coordinate prints a digit");
    s
}

/// The drawing `prims` inside world bounds `b` as an SVG document in xschem units (one user
/// unit = one xschem unit). One SVG element per primitive, in canvas draw order (lines,
/// rects, arcs, polylines, polygons, texts), hidden layers left out.
///
/// # Panics
///
/// Panics if `b` is inverted, or the text columns of `prims` are out of step.
#[must_use]
pub fn svg(prims: &CanvasPrims, b: [i32; 4], look: &Look) -> String {
    assert!(b[0] <= b[2], "inverted bounds: x0 {} > x1 {}", b[0], b[2]);
    assert!(b[1] <= b[3], "inverted bounds: y0 {} > y1 {}", b[1], b[3]);
    assert_eq!(
        prims.texts.end.len(),
        prims.texts.x.len(),
        "text columns in step"
    );
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
    // Stroke attributes in user units: the palette's pixels at one pixel per xschem unit,
    // times the row's weight; a styled row also gets round caps/joins and its dashes.
    let stroke = |l: u8, st: LineStyle| {
        let w = look.palette.width[usize::from(l) % LAYERS].max(0.1);
        let mut a = format!("stroke-width=\"{}\"", w * f32::from(st.weight()));
        if st != LineStyle::PLAIN {
            a.push_str(" stroke-linecap=\"round\" stroke-linejoin=\"round\"");
        }
        if st.dash() > 0 {
            let d = 2.0 * f32::from(st.dash()) * w;
            let _ = write!(a, " stroke-dasharray=\"{d} {d}\"");
        }
        a
    };
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
            "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"#{c:06x}\" {}/>",
            units(i64::from(l.x0[i])),
            units(i64::from(l.y0[i])),
            units(i64::from(l.x1[i])),
            units(i64::from(l.y1[i])),
            stroke(l.layer[i], l.style[i])
        );
    }
    let r = &prims.rects;
    for i in 0..r.x0.len() {
        let Some(c) = look.rgb(r.layer[i]) else {
            continue;
        };
        let (x0, x1) = (r.x0[i].min(r.x1[i]), r.x0[i].max(r.x1[i]));
        let (y0, y1) = (r.y0[i].min(r.y1[i]), r.y0[i].max(r.y1[i]));
        let fill = if look.filled(r.layer[i]) || r.style[i].fills() {
            format!("#{c:06x}")
        } else {
            "none".to_owned()
        };
        let _ = writeln!(
            s,
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{fill}\" stroke=\"#{c:06x}\" {}/>",
            units(i64::from(x0)),
            units(i64::from(y0)),
            units(i64::from(x1) - i64::from(x0)),
            units(i64::from(y1) - i64::from(y0)),
            stroke(r.layer[i], r.style[i])
        );
    }
    let a = &prims.arcs;
    for i in 0..a.cx.len() {
        let Some(c) = look.rgb(a.layer[i]) else {
            continue;
        };
        let full = a.sweep[i].unsigned_abs() >= 360_000;
        let fill = if full && (look.filled(a.layer[i]) || a.style[i].fills()) {
            format!("#{c:06x}")
        } else {
            "none".to_owned()
        };
        if full {
            let _ = writeln!(
                s,
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"{fill}\" stroke=\"#{c:06x}\" {}/>",
                units(i64::from(a.cx[i])),
                units(i64::from(a.cy[i])),
                units(i64::from(a.r[i])),
                stroke(a.layer[i], a.style[i])
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
            "<path d=\"M {} {} A {} {} 0 {large} {dir} {} {}\" fill=\"none\" stroke=\"#{c:06x}\" {}/>",
            units(p0.0),
            units(p0.1),
            units(i64::from(a.r[i])),
            units(i64::from(a.r[i])),
            units(p1.0),
            units(p1.1),
            stroke(a.layer[i], a.style[i])
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
                    units(i64::from(polys.x[j])),
                    units(i64::from(polys.y[j]))
                );
            }
            let fill = if closed && (look.filled(layer) || polys.style[k].fills()) {
                format!("#{c:06x}")
            } else {
                "none".to_owned()
            };
            let tag = if closed { "polygon" } else { "polyline" };
            let _ = writeln!(
                s,
                "<{tag} points=\"{}\" fill=\"{fill}\" stroke=\"#{c:06x}\" {}/>",
                pts.trim_end(),
                stroke(layer, polys.style[k])
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
        let (x, y) = (units(i64::from(t.x[i])), units(i64::from(t.y[i])));
        let rot = t.rot.get(i).copied().unwrap_or(0);
        let turn = if rot == 0 {
            String::new()
        } else {
            format!(" transform=\"rotate({} {x} {y})\"", u32::from(rot) * 90)
        };
        let _ = writeln!(
            s,
            "<text x=\"{x}\" y=\"{y}\" font-size=\"{}\" font-family=\"sans-serif\" dominant-baseline=\"hanging\" fill=\"#{c:06x}\"{turn}>{esc}</text>",
            units(i64::from(t.size[i]))
        );
    }
    s.push_str("</svg>\n");
    assert!(s.starts_with("<svg "));
    s
}

/// Primitive count per SVG element kind written by [`svg`]: `(lines, rects, circles + arc
/// paths, polylines + polygons, texts)` — the oracle of an SVG export test parses the file
/// back with this.
#[must_use]
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

// ---- private ----

fn xml_escape(s: &str, out: &mut String) {
    let before = out.len();
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
    let escaped = &out[before..];
    assert!(
        !escaped.contains(['<', '>', '"']),
        "markup characters are escaped"
    );
}
