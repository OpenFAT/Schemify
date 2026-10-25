//! Vector icons as data (the `icon` prop, D6: a plugin restyles icons the way it restyles
//! everything else): SVG path data in a [`ICON_BOX`]-unit square, parsed into the verb/point
//! tables the math widget already draws.
//!
//! Syntax: one or more paths separated by `;`. A path is stroked (line icons), or filled when
//! it starts with `F`. Path data takes the SVG commands `M L H V C Q Z` (absolute) and
//! `m l h v c q z` (relative), numbers separated by spaces or commas, implicit repeats (extra
//! pairs after `M` are lines). Example, a play triangle beside a bar:
//! `"F M4 2 L14 8 L4 14 Z; M1 2 V14"`.

use crate::math::{MathPaths, Verb};

/// Side of the icon coordinate square (units are scaled to the row by the renderer).
pub const ICON_BOX: f32 = 16.0;

/// Stroke width of stroked paths, in icon units.
pub const ICON_STROKE: f32 = 1.5;

/// Points one icon may hold (bounds parsing and drawing).
pub const ICON_POINTS_MAX: usize = 512;

/// A parsed icon: filled and stroked paths in icon units.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Icon {
    /// Paths drawn filled.
    pub fill: MathPaths,
    /// Paths drawn with a [`ICON_STROKE`]-unit stroke.
    pub stroke: MathPaths,
}

/// Parses icon data (module docs). `None` on any syntax error, a non-finite number, an empty
/// icon or more than [`ICON_POINTS_MAX`] points: the caller then shows the label.
pub fn parse(src: &str) -> Option<Icon> {
    let mut icon = Icon::default();
    let mut points = 0usize;
    for part in src.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (fill, data) = match part.strip_prefix('F') {
            Some(rest) => (true, rest),
            None => (false, part),
        };
        let out = if fill {
            &mut icon.fill
        } else {
            &mut icon.stroke
        };
        let before = out.pt.len();
        path(data, out)?;
        points += out.pt.len() - before;
        if points > ICON_POINTS_MAX {
            return None;
        }
        out.verb_end.push(out.verb.len() as u32);
    }
    let empty = icon.fill.verb.is_empty() && icon.stroke.verb.is_empty();
    debug_assert_eq!(
        icon.fill.verb_end.last().copied().unwrap_or(0) as usize,
        icon.fill.verb.len()
    );
    (!empty).then_some(icon)
}

/// One token of path data.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Tok {
    Cmd(u8),
    Num(f32),
}

fn tokens(data: &str) -> Option<Vec<Tok>> {
    let b = data.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    // Bounded: every pass consumes at least one byte.
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() || c == b',' {
            i += 1;
        } else if b"MLHVCQZmlhvcqz".contains(&c) {
            out.push(Tok::Cmd(c));
            i += 1;
        } else if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            let mut dot = c == b'.';
            while i < b.len() && (b[i].is_ascii_digit() || (b[i] == b'.' && !dot)) {
                dot |= b[i] == b'.';
                i += 1;
            }
            let v: f32 = data[start..i].parse().ok()?;
            if !v.is_finite() {
                return None;
            }
            out.push(Tok::Num(v));
        } else {
            return None;
        }
    }
    Some(out)
}

/// Appends one path's verbs and absolute points to `out`.
fn path(data: &str, out: &mut MathPaths) -> Option<()> {
    let toks = tokens(data)?;
    let mut k = 0;
    let (mut cur, mut start) = ([0.0f32; 2], [0.0f32; 2]);
    let mut cmd = 0u8;
    let mut open = false;
    let num = |k: &mut usize| match toks.get(*k) {
        Some(Tok::Num(v)) => {
            *k += 1;
            Some(*v)
        }
        _ => None,
    };
    // Bounded: every pass consumes at least one token or returns.
    while k < toks.len() {
        if let Tok::Cmd(c) = toks[k] {
            cmd = c;
            k += 1;
        } else if cmd == 0 || cmd.eq_ignore_ascii_case(&b'z') {
            return None; // numbers before any command, or after Z
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { cur } else { [0.0, 0.0] };
        let pt = |k: &mut usize| Some([base[0] + num(&mut *k)?, base[1] + num(&mut *k)?]);
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let p = pt(&mut k)?;
                out.verb.push(Verb::Move);
                out.pt.push(p);
                (cur, start, open) = (p, p, true);
                cmd = if rel { b'l' } else { b'L' }; // extra pairs are lines
            }
            b'L' | b'H' | b'V' if open => {
                let p = match cmd.to_ascii_uppercase() {
                    b'L' => pt(&mut k)?,
                    b'H' => [base[0] + num(&mut k)?, cur[1]],
                    _ => [cur[0], base[1] + num(&mut k)?],
                };
                out.verb.push(Verb::Line);
                out.pt.push(p);
                cur = p;
            }
            b'C' if open => {
                let (a, b, c) = (pt(&mut k)?, pt(&mut k)?, pt(&mut k)?);
                out.verb.push(Verb::Cubic);
                out.pt.extend([a, b, c]);
                cur = c;
            }
            b'Q' if open => {
                let (a, b) = (pt(&mut k)?, pt(&mut k)?);
                out.verb.push(Verb::Quad);
                out.pt.extend([a, b]);
                cur = b;
            }
            b'Z' if open => {
                out.verb.push(Verb::Close);
                (cur, open) = (start, false);
            }
            _ => return None, // a drawing command before `M`
        }
    }
    // Exit: every verb but `Close` consumed at least one point.
    debug_assert!(out.pt.len() >= out.verb.iter().filter(|v| **v != Verb::Close).count());
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strokes_fills_relative_and_implicit_lines() {
        let i =
            parse("M1 2 3 4 h2 v-1 Z; F m4,2 l10 6 L4 14 z; M0 0 C1 1 2 2 3 3 Q4 4 5 5").unwrap();
        assert_eq!(i.stroke.verb_end, [5, 8]);
        assert_eq!(
            i.stroke.verb[..5],
            [Verb::Move, Verb::Line, Verb::Line, Verb::Line, Verb::Close]
        );
        assert_eq!(
            i.stroke.pt[..4],
            [[1.0, 2.0], [3.0, 4.0], [5.0, 4.0], [5.0, 3.0]]
        );
        assert_eq!(i.fill.verb_end, [4]);
        assert_eq!(i.fill.pt, [[4.0, 2.0], [14.0, 8.0], [4.0, 14.0]]);
        assert_eq!(i.stroke.pt.len(), 4 + 1 + 3 + 2);
    }

    #[test]
    fn rejects_bad_data() {
        for bad in [
            "",
            " ; ",
            "L1 2",
            "M1",
            "M1 2 x",
            "M1 2 Z 3 4",
            "1 2",
            "M1e9 2",
            "M--1 2",
            "Fx",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
        let long = format!("M0 0{}", " L1 1".repeat(ICON_POINTS_MAX));
        assert_eq!(parse(&long), None, "over the point cap");
    }
}
