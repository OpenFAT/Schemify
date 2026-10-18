//! Parser for UI contributions written as line records (format spec §2, plugin-abi §7).
//!
//! Records (column 0) and their attributes (indented):
//!
//! ```text
//! node <id> <kind>          # attrs: parent <id> | order <int> | <prop> <value>
//! set <id> <prop> <value>   # prop may also be `parent` or `order`
//! remove <id>
//! style <selector>          # attrs: <style-prop> <value>
//! key <chord> <command>
//! ```
//!
//! This is the body of a `contribute ui` / `contribute keymap` output, so the base UI and plugin
//! UI share one syntax and can be hand-written in user config.

// ponytail: a local lexer for the §2 subset (records, attrs, comments, quoted values). Switch to
// `schemify-format` once it lands; the record vocabulary below stays here.

use crate::strings::{Strings, Sym};
use crate::tables::{Chord, Contribution, Edit, Kind, Prop, StyleProp};

/// Why a record was rejected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RejectReason {
    /// Unterminated quote, bad escape, a verbatim line, or a stray character.
    Syntax,
    /// Record word is not `node`, `set`, `remove`, `style` or `key`.
    UnknownRecord,
    /// `node` kind is not a [`Kind`].
    UnknownKind,
    /// Attribute or `set` prop is not a [`Prop`] / [`StyleProp`] (or `parent`, `order`).
    UnknownProp,
    /// Value malformed for its prop (color, length, integer, chord).
    BadValue,
    /// Wrong number of values.
    Arity,
    /// Indented attribute with no accepted record above it.
    OrphanAttr,
}

/// One rejected line; the rest of the contribution is still applied (plugin-abi §8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reject {
    /// 1-based line number.
    pub line: u32,
    /// Reason.
    pub reason: RejectReason,
}

enum Ctx {
    None,
    Node(Sym),
    Style(Sym),
}

/// Parses contribution text into edits, interning every string into `strings`.
///
/// Each line is accepted or rejected on its own; attributes of a rejected record are rejected
/// with [`RejectReason::OrphanAttr`].
///
/// # Panics
///
/// Panics when the text holds more than [`Contribution::CAPACITY`] edits or more than 2^32
/// lines.
pub fn parse(text: &str, strings: &mut Strings) -> (Contribution, Vec<Reject>) {
    parse_into(text, strings, false)
}

/// Parses like [`parse`], but interns the free text of `label`, `value`, `tooltip` and `accel`
/// props into the live region of `strings` as one generation ([`Strings::begin_live`]): the
/// previous call's handles for text this one no longer uses die. Ids, commands, classes and
/// every other value stay permanent, so interaction state keyed by node id survives.
///
/// For a contribution re-parsed whenever it changes (the app's state contribution, which
/// carries the pointer coordinates): text that changes every frame does not grow `strings`.
/// The tables composed from the previous result must be recomposed before they are read again.
///
/// # Panics
///
/// As [`parse`]; also when one generation holds more than [`Strings::LIVE_CAPACITY`] strings.
pub fn parse_live(text: &str, strings: &mut Strings) -> (Contribution, Vec<Reject>) {
    strings.begin_live();
    let out = parse_into(text, strings, true);
    strings.end_live();
    out
}

fn parse_into(text: &str, strings: &mut Strings, live: bool) -> (Contribution, Vec<Reject>) {
    assert!(
        text.len() < u32::MAX as usize,
        "contribution text too large"
    );
    let mut out = Contribution::default();
    let mut rejects = Vec::new();
    let mut ctx = Ctx::None;
    let mut toks: Vec<String> = Vec::with_capacity(8);
    for (n, raw) in text.lines().enumerate() {
        let line = (n + 1) as u32;
        let body = raw.trim_start_matches([' ', '\t']);
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let indented = body.len() != raw.len();
        let result = tokenize(body, &mut toks).and_then(|()| {
            let t: Vec<&str> = toks.iter().map(String::as_str).collect();
            if indented {
                attr(&t, &ctx, strings, live, &mut out)
            } else {
                ctx = Ctx::None;
                record(&t, strings, live, &mut out).map(|c| ctx = c)
            }
        });
        if let Err(reason) = result {
            rejects.push(Reject { line, reason });
            if !indented {
                ctx = Ctx::None;
            }
        }
        assert!(
            out.edits.len() <= Contribution::CAPACITY,
            "contribution too large"
        );
    }
    (out, rejects)
}

fn record(
    t: &[&str],
    strings: &mut Strings,
    live: bool,
    out: &mut Contribution,
) -> Result<Ctx, RejectReason> {
    let arity = |n: usize| {
        if t.len() == n {
            Ok(())
        } else {
            Err(RejectReason::Arity)
        }
    };
    match t[0] {
        "node" => {
            arity(3)?;
            let kind = Kind::parse(t[2]).ok_or(RejectReason::UnknownKind)?;
            let id = strings.intern(t[1]);
            out.edits.push(Edit::Node { id, kind });
            Ok(Ctx::Node(id))
        }
        "set" => {
            arity(4)?;
            let id = strings.intern(t[1]);
            node_attr(id, t[2], t[3], strings, live, out)?;
            Ok(Ctx::None)
        }
        "remove" => {
            arity(2)?;
            out.edits.push(Edit::Remove {
                id: strings.intern(t[1]),
            });
            Ok(Ctx::None)
        }
        "style" => {
            arity(2)?;
            Ok(Ctx::Style(strings.intern(t[1])))
        }
        "key" => {
            arity(3)?;
            let chord = Chord::parse(t[1]).ok_or(RejectReason::BadValue)?;
            out.edits.push(Edit::Key {
                chord,
                command: strings.intern(t[2]),
            });
            Ok(Ctx::None)
        }
        _ => Err(RejectReason::UnknownRecord),
    }
}

fn attr(
    t: &[&str],
    ctx: &Ctx,
    strings: &mut Strings,
    live: bool,
    out: &mut Contribution,
) -> Result<(), RejectReason> {
    if t.len() != 2 {
        return Err(RejectReason::Arity);
    }
    match *ctx {
        Ctx::None => Err(RejectReason::OrphanAttr),
        Ctx::Node(id) => node_attr(id, t[0], t[1], strings, live, out),
        Ctx::Style(selector) => {
            let prop = StyleProp::parse(t[0]).ok_or(RejectReason::UnknownProp)?;
            let value = prop.parse_value(t[1]).ok_or(RejectReason::BadValue)?;
            out.edits.push(Edit::Style {
                selector,
                prop,
                value,
            });
            Ok(())
        }
    }
}

fn node_attr(
    id: Sym,
    name: &str,
    value: &str,
    strings: &mut Strings,
    live: bool,
    out: &mut Contribution,
) -> Result<(), RejectReason> {
    let edit = match name {
        "parent" => Edit::Parent {
            id,
            parent: strings.intern(value),
        },
        "order" => Edit::Order {
            id,
            order: value.parse().map_err(|_| RejectReason::BadValue)?,
        },
        _ => {
            let prop = Prop::parse(name).ok_or(RejectReason::UnknownProp)?;
            let text = matches!(
                prop,
                Prop::Label | Prop::Value | Prop::Tooltip | Prop::Accel
            );
            Edit::Set {
                id,
                prop,
                value: if live && text {
                    strings.intern_live(value)
                } else {
                    strings.intern(value)
                },
            }
        }
    };
    out.edits.push(edit);
    Ok(())
}

/// Splits one record or attribute line into values (bare or quoted, escapes resolved).
fn tokenize(body: &str, out: &mut Vec<String>) -> Result<(), RejectReason> {
    out.clear();
    let mut chars = body.chars().peekable();
    // Bounded by the line length: every iteration consumes at least one char.
    while let Some(&c) = chars.peek() {
        if c == ' ' || c == '\t' {
            chars.next();
            continue;
        }
        let mut tok = String::new();
        if c == '"' {
            chars.next();
            loop {
                match chars.next().ok_or(RejectReason::Syntax)? {
                    '"' => break,
                    '\\' => tok.push(match chars.next().ok_or(RejectReason::Syntax)? {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        't' => '\t',
                        'u' => unicode_escape(&mut chars)?,
                        _ => return Err(RejectReason::Syntax),
                    }),
                    q => tok.push(q),
                }
            }
        } else {
            if c == '#' || c == '|' {
                return Err(RejectReason::Syntax);
            }
            while let Some(&b) = chars.peek() {
                if b == ' ' || b == '\t' {
                    break;
                }
                if b == '"' || b == '\\' || b.is_control() {
                    return Err(RejectReason::Syntax);
                }
                tok.push(b);
                chars.next();
            }
        }
        out.push(tok);
    }
    if out.is_empty() {
        return Err(RejectReason::Syntax);
    }
    Ok(())
}

/// Longest length a style value holds, in px ([`StyleProp::parse_value`]).
pub const LENGTH_MAX: u32 = 1 << 14;

/// Rows `[start, end)` a virtualized list lays out (only those rows become nodes, D39's node
/// budget): `rows` rows `pitch` px apart, its scroll pane `offset` px past the first row. The
/// window is `len` rows starting `len / 8` rows above the first visible one, the start rounded
/// down to a multiple of 16 so a small scroll leaves the window, and so the contribution text,
/// unchanged. The rows outside it are [`spacer`]s.
///
/// # Panics
///
/// Panics if `pitch` is not positive or `len` is 0.
pub fn window(offset: f32, pitch: f32, rows: u32, len: u32) -> (u32, u32) {
    assert!(pitch > 0.0, "rows have a pitch");
    assert!(len > 0, "a window shows rows");
    let first = ((offset.max(0.0) / pitch) as u32).min(rows);
    let start = (first.saturating_sub(len / 8) / 16) * 16;
    let end = (start + len).min(rows);
    assert!(start <= end);
    assert!(end - start <= len);
    (start, end)
}

/// Appends `px` of empty column to contribution text `t`: `panel` nodes `{id}.{k}` under
/// `parent`, each with its own `style` record (the id with `.` as `-`, then `-full` for the
/// full-height ones), at most [`LENGTH_MAX`] tall. With the parent's child `gap` after each,
/// they span exactly `px`, the space of the rows they stand for when every row's pitch counts
/// its gap. Writes nothing for 0.
///
/// # Panics
///
/// Panics if `gap` is not below [`LENGTH_MAX`], or if the last piece (`px % LENGTH_MAX`) is
/// shorter than `gap`.
pub fn spacer(t: &mut String, parent: &str, id: &str, px: u32, gap: u32) {
    use std::fmt::Write as _;
    assert!(gap < LENGTH_MAX, "a gap shorter than a spacer piece");
    let style = id.replace('.', "-");
    let (full, rest) = (px / LENGTH_MAX, px % LENGTH_MAX);
    assert!(rest == 0 || rest >= gap, "the last piece holds its gap");
    if full > 0 {
        let _ = writeln!(t, "style {style}-full\n  height {}", LENGTH_MAX - gap);
    }
    for k in 0..full {
        let _ = writeln!(
            t,
            "node {id}.{k} panel\n  parent {parent}\n  style {style}-full"
        );
    }
    if rest > 0 {
        let _ = writeln!(
            t,
            "style {style}\n  height {}\nnode {id}.{full} panel\n  parent {parent}\n  style {style}",
            rest - gap
        );
    }
}

fn unicode_escape(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<char, RejectReason> {
    if chars.next() != Some('{') {
        return Err(RejectReason::Syntax);
    }
    let mut v: u32 = 0;
    for i in 0..=6 {
        match chars.next().ok_or(RejectReason::Syntax)? {
            '}' if i > 0 => return char::from_u32(v).ok_or(RejectReason::Syntax),
            h => v = v * 16 + h.to_digit(16).ok_or(RejectReason::Syntax)?,
        }
    }
    Err(RejectReason::Syntax)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_and_spacer_cover_the_rows() {
        assert_eq!(window(0.0, 19.0, 0, 64), (0, 0));
        assert_eq!(window(0.0, 19.0, 10, 64), (0, 10));
        assert_eq!(window(-5.0, 19.0, 1000, 64), (0, 64));
        // First visible row 152: 8 above, rounded down to 16.
        assert_eq!(window(152.0 * 19.0, 19.0, 1000, 64), (144, 208));
        assert_eq!(window(1e9, 19.0, 1000, 64), (992, 1000));
        let mut s = Strings::new();
        for (px, gap) in [
            (0, 0),
            (19, 1),
            (12 * 16_383, 0),
            (19 * 2000, 1),
            (1 << 15, 0),
        ] {
            let mut t = String::new();
            spacer(&mut t, "list", "list.gap", px, gap);
            let (c, rej) = parse(&t, &mut s);
            assert_eq!(rej, vec![]);
            let nodes = t.matches("\nnode ").count() + usize::from(t.starts_with("node"));
            let heights: u32 = t
                .lines()
                .filter_map(|l| l.trim().strip_prefix("height "))
                .map(|h| h.parse::<u32>().unwrap())
                .sum();
            let full = px / LENGTH_MAX;
            // Each `-full` style is written once and used by every full piece.
            let spanned = if full > 0 {
                heights + (full - 1) * (LENGTH_MAX - gap)
            } else {
                heights
            };
            assert_eq!(spanned + gap * nodes as u32, px, "{px} {gap}");
            assert_eq!(c.edits.is_empty(), px == 0);
        }
    }

    #[test]
    fn parses_nodes_attrs_styles_keys() {
        let mut s = Strings::new();
        let text = "# comment\nnode file.save item\n  parent menu.file\n  label \"Save \\\"it\\\"\"\n  order -3\n\nstyle item\n  bg 102030\n  dir row\nkey Ctrl+S file.save\nset file.save checked 1\nremove x\n";
        let (c, rej) = parse(text, &mut s);
        assert_eq!(rej, vec![]);
        let save = s.find("file.save").unwrap();
        assert_eq!(
            c.edits[0],
            Edit::Node {
                id: save,
                kind: Kind::Item
            }
        );
        assert_eq!(
            c.edits[1],
            Edit::Parent {
                id: save,
                parent: s.find("menu.file").unwrap()
            }
        );
        assert_eq!(
            c.edits[2],
            Edit::Set {
                id: save,
                prop: Prop::Label,
                value: s.find("Save \"it\"").unwrap()
            }
        );
        assert_eq!(
            c.edits[3],
            Edit::Order {
                id: save,
                order: -3
            }
        );
        let item = s.find("item").unwrap();
        assert_eq!(
            c.edits[4],
            Edit::Style {
                selector: item,
                prop: StyleProp::Bg,
                value: 0x102030ff
            }
        );
        assert_eq!(
            c.edits[5],
            Edit::Style {
                selector: item,
                prop: StyleProp::Dir,
                value: 1
            }
        );
        assert_eq!(
            c.edits[6],
            Edit::Key {
                chord: Chord::parse("ctrl-s").unwrap(),
                command: save
            }
        );
        assert_eq!(c.edits.len(), 9);
    }

    #[test]
    fn rejects_each_bad_line_and_keeps_the_rest() {
        let mut s = Strings::new();
        let text = "node a bogus_kind\n  label orphaned\nnode b item\n  colour red\n  label ok\nstyle x\n  bg \"#12\"\nfrob a\nkey Ctrl+ nope\nnode \"unterminated item\n  | verbatim\nnode c item extra\n";
        let (c, rej) = parse(text, &mut s);
        let reasons: Vec<(u32, RejectReason)> = rej.iter().map(|r| (r.line, r.reason)).collect();
        assert_eq!(
            reasons,
            vec![
                (1, RejectReason::UnknownKind),
                (2, RejectReason::OrphanAttr),
                (4, RejectReason::UnknownProp),
                (7, RejectReason::BadValue),
                (8, RejectReason::UnknownRecord),
                (9, RejectReason::BadValue),
                (10, RejectReason::Syntax),
                (11, RejectReason::Syntax),
                (12, RejectReason::Arity),
            ]
        );
        assert_eq!(c.edits.len(), 2, "node b + its label survive");
    }

    #[test]
    fn unicode_escapes() {
        let mut s = Strings::new();
        let (c, rej) = parse("node a label\n  label \"\\u{3a9}\\u{1F600}\"\n", &mut s);
        assert!(rej.is_empty());
        assert!(matches!(c.edits[1], Edit::Set { value, .. } if s.get(value) == "Ω😀"));
        let (_, rej) = parse("node a label\n  label \"\\u{}\"\n", &mut s);
        assert_eq!(rej.len(), 1);
    }
}
