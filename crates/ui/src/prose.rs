//! The `prose` widget (D19, D21): a markdown subset with inline `$…$` and display `$$…$$` LaTeX
//! math, laid out at a given width into positioned parley layouts, math boxes and rules. The
//! docs page renders every `prose` doc block through it. Rules: `docs/spec/docs-page.md` §2.
//!
//! Subset: ATX headings (`#`…`######`), paragraphs, `*em*`/`_em_`, `**strong**`/`__strong__`,
//! `` `code` `` spans, fenced code blocks, `-`/`*`/`+`/`1.` list items (two spaces of indent per
//! level), `>` quotes, `---` rules, pipe tables (columns sized by their content), links shown
//! as their text (emphasis inside them nests), footnotes (`[^a]` and `[^a]: text`), images alone
//! on a line (`![alt](path)`, drawn from the app-registered [`crate::Image`]), backslash
//! escapes. `_` emphasis never opens or closes inside a word, so `gm_id_in` stays literal.
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

use parley::{
    Alignment, AlignmentOptions, FontFamily, FontStyle, FontWeight, InlineBox, InlineBoxKind,
    Layout, PositionedLayoutItem, StyleProperty, VerticalAlign,
};

use crate::text::{TextId, TextSystem};

/// Lines of one source the parser looks at; longer sources are cut (a doc block is far smaller).
pub const LINES_MAX: usize = 1 << 14;
/// Table columns laid out; extra cells are dropped.
pub const COLUMNS_MAX: usize = 16;

/// Inline style bits of a span.
const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const CODE: u8 = 4;

/// One parsed markdown block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// `#` heading, level 1–6, with its inline text.
    Heading(u8, String),
    /// Paragraph: consecutive lines joined by spaces.
    Para(String),
    /// List item at nesting `depth` with its marker (`•` or `3.`).
    Item {
        /// Nesting level (two leading spaces per level).
        depth: u8,
        /// Bullet or number text.
        marker: String,
        /// Item text (continuation lines joined).
        text: String,
    },
    /// `>` quote text.
    Quote(String),
    /// Fenced code block: info string and verbatim text.
    Code(String, String),
    /// Display math source (`$$…$$`).
    Math(String),
    /// Pipe table: header cells (empty when the table has no separator row) and body rows.
    Table(Vec<String>, Vec<Vec<String>>),
    /// `---` horizontal rule.
    Rule,
    /// Footnote definition `[^label]: text` (indented lines continue it). Drawn at the end of
    /// the block, numbered in definition order; `[^label]` in text shows that number.
    Note(String, String),
    /// An image alone on its line, `![alt](path)`: alt text and the project-relative path.
    Image(String, String),
}

/// Parses the block structure of `src`. Never fails: anything unrecognised is paragraph text.
///
/// Cost: O(len); allocates the block texts.
pub fn blocks(src: &str) -> Vec<Block> {
    let lines: Vec<&str> = src.lines().take(LINES_MAX).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() {
            i += 1;
            continue;
        }
        if let Some(fence) = ["```", "~~~"].into_iter().find(|f| t.starts_with(f)) {
            let lang = t[3..].trim().to_owned();
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                body.push(lines[i]);
                i += 1;
            }
            i += 1; // closing fence (or past the end)
            out.push(Block::Code(lang, body.join("\n")));
            continue;
        }
        if let Some(rest) = t.strip_prefix("$$") {
            if let Some(one) = rest.strip_suffix("$$") {
                out.push(Block::Math(one.trim().to_owned()));
                i += 1;
                continue;
            }
            let mut body = vec![rest];
            i += 1;
            while i < lines.len() {
                let l = lines[i].trim();
                i += 1;
                if let Some(head) = l.strip_suffix("$$") {
                    body.push(head);
                    break;
                }
                body.push(l);
            }
            out.push(Block::Math(body.join("\n").trim().to_owned()));
            continue;
        }
        if let Some((level, text)) = heading(t) {
            out.push(Block::Heading(level, text.to_owned()));
            i += 1;
            continue;
        }
        if is_rule(t) {
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        if t.starts_with('|') {
            let mut rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('|') {
                rows.push(cells(lines[i].trim()));
                i += 1;
            }
            let sep = rows
                .get(1)
                .is_some_and(|r| r.iter().all(|c| is_separator(c)));
            let header = if sep { rows.remove(0) } else { Vec::new() };
            if sep {
                rows.remove(0);
            }
            out.push(Block::Table(header, rows));
            continue;
        }
        if let Some((label, text)) = note_def(t) {
            let mut text = text.to_owned();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                let lt = l.trim();
                if lt.is_empty() || l.len() - l.trim_start().len() < 2 || starts_block(lt) {
                    break;
                }
                text.push(' ');
                text.push_str(lt);
                i += 1;
            }
            out.push(Block::Note(label.to_owned(), text));
            continue;
        }
        if let Some((alt, path)) = image_line(t) {
            out.push(Block::Image(alt.to_owned(), path.to_owned()));
            i += 1;
            continue;
        }
        if let Some((depth, marker, text)) = item(line) {
            let indent = line.len() - line.trim_start().len();
            let mut text = text.to_owned();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                let lt = l.trim();
                let deeper = l.len() - l.trim_start().len() > indent;
                if lt.is_empty() || !deeper || item(l).is_some() || starts_block(lt) {
                    break;
                }
                text.push(' ');
                text.push_str(lt);
                i += 1;
            }
            out.push(Block::Item {
                depth,
                marker,
                text,
            });
            continue;
        }
        if t.starts_with('>') {
            let mut text = String::new();
            while i < lines.len() && lines[i].trim().starts_with('>') {
                let l = lines[i].trim()[1..].trim();
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(l);
                i += 1;
            }
            out.push(Block::Quote(text));
            continue;
        }
        let mut text = t.to_owned();
        i += 1;
        while i < lines.len() {
            let l = lines[i].trim();
            if l.is_empty() || starts_block(l) || item(lines[i]).is_some() {
                break;
            }
            text.push(' ');
            text.push_str(l);
            i += 1;
        }
        out.push(Block::Para(text));
    }
    debug_assert!(out.len() <= lines.len());
    out
}

fn heading(t: &str) -> Option<(u8, &str)> {
    let level = t.bytes().take_while(|&b| b == b'#').count();
    let rest = &t[level..];
    ((1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' ')))
        .then(|| (level as u8, rest.trim().trim_end_matches('#').trim_end()))
}

fn is_rule(t: &str) -> bool {
    let c = t.as_bytes()[0];
    matches!(c, b'-' | b'*' | b'_')
        && t.bytes().filter(|&b| b != b' ').all(|b| b == c)
        && t.bytes().filter(|&b| b == c).count() >= 3
}

fn starts_block(t: &str) -> bool {
    t.starts_with("```")
        || t.starts_with("~~~")
        || t.starts_with("$$")
        || t.starts_with('|')
        || t.starts_with('>')
        || heading(t).is_some()
        || is_rule(t)
        || note_def(t).is_some()
        || image_line(t).is_some()
}

/// A footnote definition line `[^label]: text`: `(label, text)`. Labels have no spaces or
/// brackets.
fn note_def(t: &str) -> Option<(&str, &str)> {
    let (label, text) = t.strip_prefix("[^")?.split_once("]:")?;
    let ok = !label.is_empty() && !label.contains(|c: char| c.is_whitespace() || c == '[');
    ok.then(|| (label, text.trim()))
}

/// A line that is exactly one image, `![alt](path)` (an optional title after the path is
/// ignored): `(alt, path)`.
pub fn image_line(t: &str) -> Option<(&str, &str)> {
    let (alt, target) = t.strip_prefix("![")?.strip_suffix(')')?.split_once("](")?;
    let path = target.split_whitespace().next()?;
    (!alt.contains(']') && !target.contains(')')).then_some((alt, path))
}

/// A list item line: `(depth, marker, text)`.
fn item(line: &str) -> Option<(u8, String, &str)> {
    let t = line.trim_start();
    let depth = ((line.len() - t.len()) / 2).min(7) as u8;
    for b in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(b) {
            return Some((depth, "\u{2022}".to_owned(), rest.trim()));
        }
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &t[digits..];
    if (1..=9).contains(&digits) && (rest.starts_with(". ") || rest.starts_with(") ")) {
        return Some((depth, format!("{}.", &t[..digits]), rest[2..].trim()));
    }
    None
}

fn cells(t: &str) -> Vec<String> {
    let inner = t.trim_start_matches('|');
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner
        .split('|')
        .take(COLUMNS_MAX)
        .map(|c| c.trim().to_owned())
        .collect()
}

fn is_separator(c: &str) -> bool {
    let c = c.trim_matches(':');
    !c.is_empty() && c.bytes().all(|b| b == b'-')
}

/// Plain text of an inline run, with style spans and math anchors (byte offsets into `text`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inline {
    /// Text with markup removed (math sources removed too; they sit at anchors).
    pub text: String,
    /// `(start, end, style bits)`, non-overlapping, ascending.
    pub spans: Vec<(u32, u32, u8)>,
    /// `(byte offset, value returned by the math callback)` per inline formula.
    pub maths: Vec<(u32, u32)>,
}

/// Parses inline markup. `notes` are the block's footnote labels in number order: `[^label]`
/// becomes its number in superscript digits (an unknown label stays literal). `math` lays out
/// one `$…$` source and returns its id, or `None` to keep the source as literal text (a
/// formula outside the LaTeX subset is shown raw, never dropped).
pub fn inlines(src: &str, notes: &[&str], math: &mut impl FnMut(&str) -> Option<u32>) -> Inline {
    let mut out = Inline::default();
    let mut flags = 0u8;
    let mut span_start = 0u32;
    let chars: Vec<char> = src.chars().collect();
    // The open link: indices of its `]` and `)`. Its text is parsed like any other, so
    // emphasis nests inside; the target is skipped when the scan reaches the `]`.
    let mut link: Option<(usize, usize)> = None;
    let mut i = 0;
    // Each iteration consumes ≥ 1 char.
    while i < chars.len() {
        if let Some((k, e)) = link
            && i >= k
        {
            link = None;
            if i == k {
                i = e + 1;
                continue;
            }
            // A code span or formula ran past the `]`: the target shows as text.
        }
        let c = chars[i];
        let prev = i.checked_sub(1).map(|p| chars[p]);
        match c {
            '\\' if chars.get(i + 1).is_some_and(|n| n.is_ascii_punctuation()) => {
                out.text.push(chars[i + 1]);
                i += 2;
            }
            '`' => match chars[i + 1..].iter().position(|&n| n == '`') {
                Some(len) => {
                    let at = out.text.len() as u32;
                    restyle(&mut out, &mut span_start, flags, flags | CODE);
                    out.text.extend(&chars[i + 1..i + 1 + len]);
                    restyle(&mut out, &mut span_start, flags | CODE, flags);
                    debug_assert!(out.text.len() as u32 >= at);
                    i += len + 2;
                }
                None => {
                    out.text.push('`');
                    i += 1;
                }
            },
            '$' => {
                let double = chars.get(i + 1) == Some(&'$');
                let open = if double { 2 } else { 1 };
                let body = &chars[i + open..];
                // Pandoc's rule keeps prices literal: the closer is not escaped and (single
                // `$`) not followed by a digit.
                let close = (0..body.len()).find(|&k| {
                    body[k] == '$'
                        && (k == 0 || body[k - 1] != '\\')
                        && if double {
                            body.get(k + 1) == Some(&'$')
                        } else {
                            !body.get(k + 1).is_some_and(char::is_ascii_digit)
                        }
                });
                let ok = close.filter(|&k| k > 0 && !body[0].is_whitespace());
                let id = ok.and_then(|k| math(&body[..k].iter().collect::<String>()));
                match (ok, id) {
                    (Some(k), Some(id)) => {
                        out.maths.push((out.text.len() as u32, id));
                        i += open + k + open;
                    }
                    _ => {
                        out.text.push('$');
                        i += 1;
                    }
                }
            }
            '*' | '_' => {
                let run = chars[i..].iter().take_while(|&&n| n == c).count();
                let next = chars.get(i + run).copied();
                let word = |ch: Option<char>| ch.is_some_and(char::is_alphanumeric);
                let can_open =
                    next.is_some_and(|n| !n.is_whitespace()) && (c == '*' || !word(prev));
                let can_close =
                    prev.is_some_and(|p| !p.is_whitespace()) && (c == '*' || !word(next));
                let mut used = 0;
                for (len, bit) in [(2usize, BOLD), (1, ITALIC)] {
                    if run - used < len {
                        continue;
                    }
                    let on = flags & bit != 0;
                    if (on && can_close) || (!on && can_open) {
                        let to = flags ^ bit;
                        restyle(&mut out, &mut span_start, flags, to);
                        flags = to;
                        used += len;
                    }
                }
                out.text.extend(&chars[i + used..i + run]);
                i += run;
            }
            '[' => {
                if chars.get(i + 1) == Some(&'^')
                    && let Some(k) = chars[i..].iter().position(|&n| n == ']').map(|k| i + k)
                    && let Some(n) = notes
                        .iter()
                        .position(|l| chars[i + 2..k].iter().copied().eq(l.chars()))
                {
                    push_superscript(&mut out.text, n + 1);
                    i = k + 1;
                    continue;
                }
                match link_at(&chars, i) {
                    Some(ke) if link.is_none() => link = Some(ke),
                    _ => out.text.push('['),
                }
                i += 1;
            }
            // An image inside text shows its alt text; only one alone on a line is drawn.
            '!' if chars.get(i + 1) == Some(&'[') => match link_at(&chars, i + 1) {
                Some((k, e)) => {
                    out.text.extend(&chars[i + 2..k]);
                    i = e + 1;
                }
                None => {
                    out.text.push('!');
                    i += 1;
                }
            },
            _ => {
                out.text.push(c);
                i += 1;
            }
        }
    }
    restyle(&mut out, &mut span_start, flags, 0);
    debug_assert!(out.spans.windows(2).all(|w| w[0].1 <= w[1].0));
    out
}

/// The link `[text](target)` whose `[` is `chars[at]`: indices of its `]` and `)`.
fn link_at(chars: &[char], at: usize) -> Option<(usize, usize)> {
    debug_assert_eq!(chars.get(at), Some(&'['));
    let k = at + chars[at..].iter().position(|&n| n == ']')?;
    if chars.get(k + 1) != Some(&'(') {
        return None;
    }
    let e = k + chars[k..].iter().position(|&n| n == ')')?;
    Some((k, e))
}

/// Appends `n` in superscript digits (`¹²`): a footnote reference.
fn push_superscript(out: &mut String, n: usize) {
    const DIGITS: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    assert!(n > 0, "footnotes count from 1");
    for d in n.to_string().bytes() {
        out.push(DIGITS[usize::from(d - b'0')]);
    }
}

/// Closes the span styled `from` at the current end and starts one styled `to`.
fn restyle(out: &mut Inline, start: &mut u32, from: u8, to: u8) {
    let end = out.text.len() as u32;
    if from != 0 && end > *start {
        out.spans.push((*start, end, from));
    }
    if from != to {
        *start = end;
    }
}

/// One piece of a prose source after [`split_plots`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    /// Markdown for the prose widget.
    Prose(&'a str),
    /// The calculator expression of a ```` ```plot ```` fence (an embedded graph).
    Plot(&'a str),
}

/// Splits `src` at ```` ```plot ```` fences, so the page puts a graph where the fence is and
/// renders the text around it as prose. An unclosed fence runs to the end; whitespace-only
/// prose between fences is dropped.
///
/// Cost: one pass over the lines.
pub fn split_plots(src: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut from = 0;
    let mut at = 0;
    // Inside a fence: (is a plot fence, byte offset of its first body line).
    let mut fence: Option<(bool, usize)> = None;
    for line in src.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let t = line.trim();
        let is_fence = t.starts_with("```") || t.starts_with("~~~");
        match fence {
            None if is_fence => {
                let plot = t[3..].trim() == "plot";
                if plot {
                    push_prose(&mut out, &src[from..start]);
                }
                fence = Some((plot, at));
            }
            Some((plot, body)) if is_fence => {
                if plot {
                    out.push(Segment::Plot(src[body..start].trim()));
                    from = at;
                }
                fence = None;
            }
            _ => {}
        }
    }
    if let Some((true, body)) = fence {
        out.push(Segment::Plot(src[body..].trim()));
        from = src.len();
    }
    push_prose(&mut out, &src[from..]);
    debug_assert!(at == src.len());
    out
}

fn push_prose<'a>(out: &mut Vec<Segment<'a>>, s: &'a str) {
    if !s.trim().is_empty() {
        out.push(Segment::Prose(s));
    }
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

/// Lays out markdown `src` at `px` pixels per em, wrapping at `width`. Images come from the
/// ones registered in `ts` ([`TextSystem::set_image`]); one that is not shows its alt text.
///
/// # Panics
///
/// Panics if `px` or `width` is not finite and positive.
pub(crate) fn layout_prose(ts: &mut TextSystem, src: &str, px: f32, width: f32) -> ProseList {
    assert!(px > 0.0 && px.is_finite(), "prose size must be positive");
    assert!(
        width > 0.0 && width.is_finite(),
        "prose width must be positive"
    );
    let mut p = ProseList::default();
    let gap = (px * 0.6).round();
    let parsed = blocks(src);
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
                y += rich(ts, &mut p, text, px * scale, BOLD, (0.0, y), width, notes);
            }
            Block::Para(text) => y += rich(ts, &mut p, text, px, 0, (0.0, y), width, notes),
            Block::Quote(text) => {
                let w = (width - px).max(px);
                let h = rich(ts, &mut p, text, px, ITALIC, (px, y), w, notes);
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
                rich(
                    ts,
                    &mut p,
                    marker,
                    px,
                    0,
                    (x + px * 0.3, y),
                    px * 1.3,
                    notes,
                );
                let w = (width - body).max(px);
                y += rich(ts, &mut p, text, px, 0, (body, y), w, notes);
            }
            Block::Code(_, text) => {
                let pad = (px * 0.5).round();
                let id = shape(ts, text, px * 0.92, (width - 2.0 * pad).max(px));
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
                    y += rich(ts, &mut p, &text, px, ITALIC, (0.0, y), width, notes);
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
        rich(ts, &mut p, &marker, small, 0, (0.0, y), body, notes);
        let w = (width - body).max(small);
        y += rich(ts, &mut p, text, small, 0, (body, y), w, notes);
    }
    p.height = y.ceil();
    p.width = p.width.min(width);
    debug_assert_eq!(p.text_id.len(), p.text_y.len());
    debug_assert_eq!(p.math_id.len(), p.math_y.len());
    debug_assert_eq!(p.image_id.len(), p.image_rect.len());
    p
}

/// Lays out one inline run at `at`, wrapping at `width`; returns its height.
#[allow(clippy::too_many_arguments)]
fn rich(
    ts: &mut TextSystem,
    p: &mut ProseList,
    src: &str,
    px: f32,
    style: u8,
    at: (f32, f32),
    width: f32,
    notes: &[&str],
) -> f32 {
    let (layout, inl) = shape_rich(ts, src, px, style, notes);
    place(ts, p, layout, &inl, at, width)
}

/// Parses and shapes one inline run at `px` (lines not broken yet), its formulas laid out.
fn shape_rich(
    ts: &mut TextSystem,
    src: &str,
    px: f32,
    style: u8,
    notes: &[&str],
) -> (Layout<()>, Inline) {
    let inl = inlines(src, notes, &mut |m| ts.math_uncached(m, px));
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
        let (fcx, lcx) = ts.contexts();
        let mut b = lcx.ranged_builder(fcx, &inl.text, 1.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::from(family.as_str())));
        b.push_default(StyleProperty::FontSize(px));
        b.push_default(StyleProperty::LineHeight(
            parley::LineHeight::FontSizeRelative(1.35),
        ));
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
    (layout, inl)
}

/// Breaks a [`shape_rich`] layout at `width`, records it and its formulas at `at`; returns
/// its height.
fn place(
    ts: &mut TextSystem,
    p: &mut ProseList,
    mut layout: Layout<()>,
    inl: &Inline,
    at: (f32, f32),
    width: f32,
) -> f32 {
    layout.break_all_lines(Some(width.max(1.0)));
    layout.align(Alignment::Start, AlignmentOptions::default());
    for line in layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::InlineBox(ib) = item {
                p.math_id.push(inl.maths[ib.id as usize].1);
                p.math_x.push(at.0 + ib.x);
                p.math_y.push(at.1 + ib.y);
            }
        }
    }
    let id = ts.push_layout(layout);
    push_text(p, ts, id, at.0, at.1)
}

/// Shapes verbatim monospace text (code blocks, formulas outside the subset), wrapping at
/// `width`.
fn shape(ts: &mut TextSystem, text: &str, px: f32, width: f32) -> TextId {
    let mono = ts.mono().to_owned();
    let mut layout: Layout<()> = Layout::new();
    {
        let (fcx, lcx) = ts.contexts();
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
    let l = ts.layout(id);
    p.text_id.push(id);
    p.text_x.push(x);
    p.text_y.push(y);
    p.width = p.width.max(x + l.width());
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
    let total = xs[cols];
    let mut y = y0;
    p.rules.push([0.0, y0, total, 1.0]);
    let mut cells = cells.into_iter();
    for _ in 0..all.len() {
        let mut h = 0.0f32;
        for (&w, &x) in widths.iter().zip(&xs) {
            let (layout, inl) = cells.next().expect("one shaped cell per column");
            let at = (x + pad, y + pad);
            h = h.max(place(ts, p, layout, &inl, at, (w - 2.0 * pad).max(1.0)));
        }
        y += h + 2.0 * pad;
        p.rules.push([0.0, y, total, 1.0]);
    }
    debug_assert!(cells.next().is_none());
    for &x in &xs {
        p.rules.push([x, y0, 1.0, y - y0]);
    }
    p.width = p.width.max(total);
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
    assert!(n > 0 && n <= COLUMNS_MAX, "1..=COLUMNS_MAX columns");
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
    (0..n)
        .map(|c| {
            if fixed[c] {
                natural[c].ceil()
            } else if flex_sum > 0.0 {
                (left * natural[c] / flex_sum).floor().max(min)
            } else {
                (left / flex_n as f32).floor().max(min)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_cover_the_subset() {
        let src = "# Sizing\n\nThe input pair sets $g_m$.\nSecond line.\n\n- one\n  more\n  - nested\n2. two\n\n```spice\n.op\n```\n\n$$\ng_m = \\frac{2 I_D}{V_{ov}}\n$$\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\n\n> note\n\n---\n";
        let b = blocks(src);
        assert_eq!(b[0], Block::Heading(1, "Sizing".into()));
        assert_eq!(
            b[1],
            Block::Para("The input pair sets $g_m$. Second line.".into())
        );
        assert_eq!(
            b[2],
            Block::Item {
                depth: 0,
                marker: "\u{2022}".into(),
                text: "one more".into()
            }
        );
        assert!(matches!(&b[3], Block::Item { depth: 1, text, .. } if text == "nested"));
        assert!(matches!(&b[4], Block::Item { marker, .. } if marker == "2."));
        assert_eq!(b[5], Block::Code("spice".into(), ".op".into()));
        assert_eq!(b[6], Block::Math("g_m = \\frac{2 I_D}{V_{ov}}".into()));
        assert_eq!(
            b[7],
            Block::Table(
                vec!["a".into(), "b".into()],
                vec![vec!["1".into(), "2".into()]]
            )
        );
        assert_eq!(b[8], Block::Quote("note".into()));
        assert_eq!(b[9], Block::Rule);
        assert_eq!(b.len(), 10);
        // Negative space: nothing but text is a paragraph; empty input has no blocks.
        assert_eq!(blocks("#nospace"), vec![Block::Para("#nospace".into())]);
        assert!(blocks("\n\n  \n").is_empty());
    }

    #[test]
    fn inlines_style_and_anchor_math() {
        let mut n = 0;
        let mut math = |s: &str| {
            n += 1;
            (s != "bad").then_some(n)
        };
        let i = inlines(
            "**gm** is *large* for `W` at $g_m$; $bad$ stays, gm_id_in too, \\*x\\* and [doc](u)",
            &[],
            &mut math,
        );
        assert_eq!(
            i.text,
            "gm is large for W at ; $bad$ stays, gm_id_in too, *x* and doc"
        );
        assert_eq!(i.spans[0], (0, 2, BOLD));
        assert_eq!(i.spans[1], (6, 11, ITALIC));
        assert_eq!(i.spans[2], (16, 17, CODE));
        assert_eq!(i.spans.len(), 3);
        assert_eq!(i.maths, vec![(21, 1)]);
        // Prices stay literal: the closer is followed by a digit.
        assert!(
            inlines("from $5 to $10", &[], &mut |_| Some(0))
                .maths
                .is_empty()
        );
    }

    /// Links keep their text's markup (emphasis nests); footnote references become superscript
    /// numbers in definition order; an unknown label and an image inside text stay text.
    #[test]
    fn links_nest_emphasis_and_notes_number() {
        let i = inlines(
            "see [**bold** _it_ link](u) and[^b] then[^a], [^zz] ![fig](f.png)",
            &["a", "b"],
            &mut |_| None,
        );
        assert_eq!(i.text, "see bold it link and² then¹, [^zz] fig");
        assert_eq!(i.spans, vec![(4, 8, BOLD), (9, 11, ITALIC)]);
        // Negative space: `[x]` without a target and an unclosed `[` are literal.
        assert_eq!(inlines("[x] and [y", &[], &mut |_| None).text, "[x] and [y");
        let b = blocks("Text[^n].\n\n[^n]: The note,\n  continued.\n![plot](img/a.png)\n");
        assert_eq!(b[0], Block::Para("Text[^n].".into()));
        assert_eq!(b[1], Block::Note("n".into(), "The note, continued.".into()));
        assert_eq!(b[2], Block::Image("plot".into(), "img/a.png".into()));
        assert_eq!(b.len(), 3);
        assert!(
            matches!(&blocks("![a](b) and text")[0], Block::Para(_)),
            "an image with text around it is a paragraph"
        );
        assert!(
            matches!(&blocks("[^a b]: x")[0], Block::Para(_)),
            "labels have no spaces"
        );
    }

    #[test]
    fn plots_split_out_in_place() {
        let s = "Intro\n```plot\nv(out)\n```\nAfter\n```spice\n```plot inside code\n```\n";
        let parts = split_plots(s);
        assert_eq!(parts[0], Segment::Prose("Intro\n"));
        assert_eq!(parts[1], Segment::Plot("v(out)"));
        assert!(matches!(parts[2], Segment::Prose(p) if p.starts_with("After")));
        assert_eq!(parts.len(), 3);
        assert_eq!(split_plots("```plot\nx()"), vec![Segment::Plot("x()")]);
        assert!(split_plots("").is_empty());
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
