//! `export`: the drawn canvas tables (`schemify_canvas::CanvasPrims`) written as vector image
//! files — SVG, PDF (one page per drawing) and EPS, fitted to a [`Page`]. The PNG export is
//! the CPU raster of the same tables in `schemify-app` (`Editor::export_png`); these
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
//!
//! Contents:
//! - `pub struct Look`
//! - `pub const PAPER`
//! - `pub struct Page`, `Page::parse`, `Page::points`, `Page::name`
//! - `pub fn subset`
//! - `pub fn bounds`
//! - `pub fn padded`

mod eps;
mod ops;
mod pdf;
mod svg;

use schemify_canvas::{CanvasPrims, LAYER_DEAD, LAYERS, Palette};

use crate::scene::Scene;

pub use eps::eps;
pub use pdf::pdf;
pub use svg::{svg, svg_counts};

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

// `Page::paper` is a `u8` row of `PAPER`.
const _: () = assert!(PAPER.len() <= u8::MAX as usize);

/// PostScript names of the embedded UI faces (Type 42 resources, PDF `BaseFont`s), indexed
/// like [`schemify_ui::ui_fonts`]; a PDF page names face `f` `/F<f + 1>`.
const FONT_NAMES: [&str; 2] = ["SchemifyUI-NotoSans", "SchemifyUI-KaTeXMain"];

/// The primitives of `sc` owned by objects with `keep[object]` set (the selection export).
///
/// # Panics
///
/// Panics if `keep` is not one flag per scene object, or the scene's owner columns are out
/// of step with its primitives.
#[must_use]
pub fn subset(sc: &Scene, keep: &[bool]) -> CanvasPrims {
    assert_eq!(keep.len(), sc.objs.len(), "one flag per object");
    let on = |o: u32| keep.get(o as usize).copied().unwrap_or(false);
    let (p, o) = (&sc.prims, &sc.owner);
    assert_eq!(o.lines.len(), p.lines.x0.len(), "one owner per line");
    assert_eq!(o.rects.len(), p.rects.x0.len(), "one owner per rect");
    assert_eq!(o.arcs.len(), p.arcs.cx.len(), "one owner per arc");
    assert_eq!(o.texts.len(), p.texts.x.len(), "one owner per text");
    let mut out = CanvasPrims::default();
    for i in 0..p.lines.x0.len() {
        if on(o.lines[i]) {
            let l = &p.lines;
            out.line_styled(l.x0[i], l.y0[i], l.x1[i], l.y1[i], l.layer[i], l.style[i]);
        }
    }
    for i in 0..p.rects.x0.len() {
        if on(o.rects[i]) {
            let r = &p.rects;
            out.rect_styled(r.x0[i], r.y0[i], r.x1[i], r.y1[i], r.layer[i], r.style[i]);
        }
    }
    for i in 0..p.arcs.cx.len() {
        if on(o.arcs[i]) {
            let a = &p.arcs;
            let (start, sweep) = (a.start[i], a.sweep[i]);
            out.arc_styled(
                a.cx[i], a.cy[i], a.r[i], start, sweep, a.layer[i], a.style[i],
            );
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
                out.poly_styled(&pts, closed, polys.layer[k], polys.style[k]);
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

/// World bounds `[x0, y0, x1, y1]` of every live row of `prims` (`None` when there is none).
///
/// # Panics
///
/// The text columns of `prims` out of step.
#[must_use]
pub fn bounds(prims: &CanvasPrims) -> Option<[i32; 4]> {
    let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    let mut grow = |x: i32, y: i32| {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    };
    let live = |layer: &[u8], i: usize| layer[i] != LAYER_DEAD;
    let l = &prims.lines;
    for i in 0..l.x0.len() {
        if live(&l.layer, i) {
            grow(l.x0[i], l.y0[i]);
            grow(l.x1[i], l.y1[i]);
        }
    }
    let r = &prims.rects;
    for i in 0..r.x0.len() {
        if live(&r.layer, i) {
            grow(r.x0[i], r.y0[i]);
            grow(r.x1[i], r.y1[i]);
        }
    }
    let a = &prims.arcs;
    for i in 0..a.cx.len() {
        if live(&a.layer, i) {
            grow(a.cx[i] - a.r[i], a.cy[i] - a.r[i]);
            grow(a.cx[i] + a.r[i], a.cy[i] + a.r[i]);
        }
    }
    for polys in [&prims.polylines, &prims.polygons] {
        for k in 0..polys.end.len() {
            if !live(&polys.layer, k) {
                continue;
            }
            let start = if k == 0 { 0 } else { polys.end[k - 1] as usize };
            for j in start..polys.end[k] as usize {
                grow(polys.x[j], polys.y[j]);
            }
        }
    }
    let t = &prims.texts;
    assert_eq!(t.end.len(), t.x.len(), "text columns in step");
    let mut start = 0usize;
    for i in 0..t.x.len() {
        let end = t.end[i] as usize;
        if !live(&t.layer, i) {
            start = end;
            continue;
        }
        let n = t.text[start..end].chars().count().max(1) as i64;
        let w = (i64::from(t.size[i]) * 6 * n / 10).min(i64::from(i32::MAX / 4)) as i32;
        grow(t.x[i], t.y[i]);
        grow(t.x[i].saturating_add(w), t.y[i].saturating_add(t.size[i]));
        start = end;
    }
    let found = b[0] <= b[2];
    assert_eq!(found, b[1] <= b[3], "x and y grow together");
    found.then_some(b)
}

/// Grows `b` by `pad` milli-units on every side (saturating).
///
/// # Panics
///
/// A negative `pad`, or `b` is not ordered (`x0 ≤ x1`, `y0 ≤ y1`).
#[must_use]
pub fn padded(b: [i32; 4], pad: i32) -> [i32; 4] {
    assert!(pad >= 0, "a padding grows the box, got {pad}");
    assert!(b[0] <= b[2], "bounds from `bounds` are ordered");
    assert!(b[1] <= b[3], "bounds from `bounds` are ordered");
    [
        b[0].saturating_sub(pad),
        b[1].saturating_sub(pad),
        b[2].saturating_add(pad),
        b[3].saturating_add(pad),
    ]
}

impl Page {
    /// Parses `<paper> [portrait|landscape]` (`a3`, `letter portrait`, `A4-portrait`;
    /// case-insensitive, landscape by default). `None` for an unknown paper or word.
    #[must_use]
    pub fn parse(s: &str) -> Option<Page> {
        let lower = s.trim().to_ascii_lowercase();
        let mut words = lower.split([' ', '-', '_']).filter(|w| !w.is_empty());
        let name = words.next()?;
        let paper = u8::try_from(PAPER.iter().position(|p| p.0 == name)?).ok()?;
        let portrait = match words.next() {
            None | Some("landscape") => false,
            Some("portrait") => true,
            Some(_) => return None,
        };
        words.next().is_none().then_some(Page { paper, portrait })
    }

    /// Width and height in points.
    #[must_use]
    pub fn points(self) -> (f64, f64) {
        debug_assert!(
            usize::from(self.paper) < PAPER.len(),
            "a page from `parse` or `default`"
        );
        let (_, w, h) = PAPER[usize::from(self.paper) % PAPER.len()];
        if self.portrait { (w, h) } else { (h, w) }
    }

    /// The spelling [`Page::parse`] reads back (`a4 landscape`).
    #[must_use]
    pub fn name(self) -> String {
        let o = if self.portrait {
            "portrait"
        } else {
            "landscape"
        };
        format!("{} {o}", PAPER[usize::from(self.paper) % PAPER.len()].0)
    }
}

// ---- private ----

impl Look {
    /// `0xRRGGBB` of layer `l` as exported, or `None` when the layer is hidden (alpha 0) or
    /// the row dead (`LAYER_DEAD`, a scene's removed row).
    fn rgb(&self, l: u8) -> Option<u32> {
        if l == LAYER_DEAD {
            return None;
        }
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

/// Arc end point at `deg_milli` (clockwise on screen, y down), rounded to milli-units.
fn arc_point(cx: i32, cy: i32, r: i32, deg_milli: i64) -> (i64, i64) {
    assert!(r >= 0, "an arc radius is a length, got {r}");
    let t = (deg_milli as f64 / 1000.0).to_radians();
    let r = f64::from(r);
    (
        i64::from(cx) + (r * t.cos()).round() as i64,
        i64::from(cy) + (r * t.sin()).round() as i64,
    )
}

#[cfg(test)]
mod tests {
    use super::eps::SFNTS_MAX;
    use super::ops::arc_beziers;
    use super::pdf::ascii85;
    use super::*;
    use schemify_canvas::LineStyle;

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
            let units: Vec<u16> = u.as_bytes()[1..u.len() - 1].chunks(4).map(hex16).collect();
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
                    hex.len().is_multiple_of(4) && hex.len() / 2 <= SFNTS_MAX,
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
    fn exports_honour_stroke_weight_and_dash() {
        let mut p = CanvasPrims::default();
        p.line(0, 0, 10_000, 0, 1);
        let plain = svg(&p, [0, 0, 10_000, 10_000], &look());
        assert!(!plain.contains("linecap") && !plain.contains("dasharray"));
        p.line_styled(0, 5_000, 10_000, 5_000, 1, LineStyle::new(2, 3));
        let s = svg(&p, [0, 0, 10_000, 10_000], &look());
        let w = look().palette.width[1];
        assert!(s.contains(&format!("stroke-width=\"{}\"", w * 2.0)), "{s}");
        assert!(
            s.contains(&format!("stroke-dasharray=\"{0} {0}\"", 6.0 * w)),
            "{s}"
        );
        assert!(s.contains("stroke-linecap=\"round\""));
        // PDF: the dash is set for the dashed row only and reset after it.
        let pages = [(&p, [0, 0, 10_000, 10_000])];
        let doc = String::from_utf8_lossy(&pdf(&pages, &look(), Page::default())).into_owned();
        assert_eq!(doc.matches("] 0 d\n").count(), 2, "set once, reset once");
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
        // The table keyword starts a line (`startxref` also ends in `xref`).
        let xref = text.rfind("\nxref\n").unwrap() + 1;
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
