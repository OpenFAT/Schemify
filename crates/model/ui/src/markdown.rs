//! Markdown for the `prose` widget (D19, D21): the block structure and the inline markup of
//! the subset `docs/spec/docs-page.md` §2 names, parsed into owned rows the layout in
//! [`crate::prose`] places. Never fails: anything unrecognised is paragraph text.
//!
//! Subset: ATX headings (`#`…`######`), paragraphs, `*em*`/`_em_`, `**strong**`/`__strong__`,
//! `` `code` `` spans, fenced code blocks, `-`/`*`/`+`/`1.` list items (two spaces of indent per
//! level), `>` quotes, `---` rules, pipe tables, links shown as their text (emphasis inside them
//! nests), footnotes (`[^a]` and `[^a]: text`), images alone on a line (`![alt](path)`),
//! backslash escapes, inline `$…$` and display `$$…$$` math. `_` emphasis never opens or closes
//! inside a word, so `gm_id_in` stays literal.
//!
//! Contents:
//! - `pub(crate) const LINES_MAX`, `pub(crate) const COLUMNS_MAX`
//! - `pub(crate) const BOLD`, `pub(crate) const ITALIC`, `pub(crate) const CODE`
//! - `pub(crate) enum Block`, `pub(crate) struct Inline`, `pub enum Segment<'src>`
//! - `pub(crate) fn blocks(src) -> Vec<Block>`
//! - `pub fn image_line(t) -> Option<(&str, &str)>`
//! - `pub(crate) fn inlines(src, notes, math) -> Inline`
//! - `pub fn split_plots(src) -> Vec<Segment<'_>>`

/// Lines of one source the parser looks at; longer sources are cut (a doc block is far smaller).
pub(crate) const LINES_MAX: usize = 1 << 14;
/// Table columns parsed and laid out; extra cells are dropped.
pub(crate) const COLUMNS_MAX: usize = 16;

/// Inline style bit of a span: bold.
pub(crate) const BOLD: u8 = 1;
/// Inline style bit of a span: italic.
pub(crate) const ITALIC: u8 = 2;
/// Inline style bit of a span: a `code` span (monospace).
pub(crate) const CODE: u8 = 4;

/// One parsed markdown block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Block {
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

/// Plain text of an inline run, with style spans and math anchors (byte offsets into `text`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Inline {
    /// Text with markup removed (math sources removed too; they sit at anchors).
    pub text: String,
    /// `(start, end, style bits)`, non-overlapping, ascending.
    pub spans: Vec<(u32, u32, u8)>,
    /// `(byte offset, value returned by the math callback)` per inline formula.
    pub maths: Vec<(u32, u32)>,
}

/// One piece of a prose source after [`split_plots`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment<'src> {
    /// Markdown for the prose widget.
    Prose(&'src str),
    /// The calculator expression of a ```` ```plot ```` fence (an embedded graph).
    Plot(&'src str),
}

/// A link `[text](target)` in a run of chars: indices of its `]` and its `)`.
#[derive(Clone, Copy, Debug)]
struct Link {
    close: usize,
    end: usize,
}

/// A footnote definition line `[^label]: text`, split.
#[derive(Clone, Copy, Debug)]
struct NoteDef<'line> {
    label: &'line str,
    text: &'line str,
}

/// Parses the block structure of `src`. Never fails: anything unrecognised is paragraph text.
///
/// Cost: O(len); allocates the block texts.
pub(crate) fn blocks(src: &str) -> Vec<Block> {
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
        if let Some(NoteDef { label, text }) = note_def(t) {
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

/// A line that is exactly one image, `![alt](path)` (an optional title after the path is
/// ignored): `(alt, path)`.
#[must_use]
pub fn image_line(t: &str) -> Option<(&str, &str)> {
    let (alt, target) = t.strip_prefix("![")?.strip_suffix(')')?.split_once("](")?;
    let path = target.split_whitespace().next()?;
    (!alt.contains(']') && !target.contains(')')).then_some((alt, path))
}

/// Parses inline markup. `notes` are the block's footnote labels in number order: `[^label]`
/// becomes its number in superscript digits (an unknown label stays literal). `math` lays out
/// one `$…$` source and returns its id, or `None` to keep the source as literal text (a
/// formula outside the LaTeX subset is shown raw, never dropped).
pub(crate) fn inlines(
    src: &str,
    notes: &[&str],
    math: &mut impl FnMut(&str) -> Option<u32>,
) -> Inline {
    let mut out = Inline::default();
    let mut flags = 0u8;
    let mut span_start = 0u32;
    let chars: Vec<char> = src.chars().collect();
    // The open link: indices of its `]` and `)`. Its text is parsed like any other, so
    // emphasis nests inside; the target is skipped when the scan reaches the `]`.
    let mut link: Option<Link> = None;
    let mut i = 0;
    // Each iteration consumes ≥ 1 char.
    while i < chars.len() {
        if let Some(Link { close: k, end: e }) = link
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
                Some(l) => {
                    out.text.extend(&chars[i + 2..l.close]);
                    i = l.end + 1;
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

/// Splits `src` at ```` ```plot ```` fences, so the page puts a graph where the fence is and
/// renders the text around it as prose. An unclosed fence runs to the end; whitespace-only
/// prose between fences is dropped.
///
/// Cost: one pass over the lines.
#[must_use]
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

// ---- private ----

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

/// A footnote definition line `[^label]: text`. Labels have no spaces or brackets.
fn note_def(t: &str) -> Option<NoteDef<'_>> {
    let (label, text) = t.strip_prefix("[^")?.split_once("]:")?;
    let ok = !label.is_empty() && !label.contains(|c: char| c.is_whitespace() || c == '[');
    ok.then(|| NoteDef {
        label,
        text: text.trim(),
    })
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

/// The link `[text](target)` whose `[` is `chars[at]`.
fn link_at(chars: &[char], at: usize) -> Option<Link> {
    debug_assert_eq!(chars.get(at), Some(&'['));
    let k = at + chars[at..].iter().position(|&n| n == ']')?;
    if chars.get(k + 1) != Some(&'(') {
        return None;
    }
    let e = k + chars[k..].iter().position(|&n| n == ')')?;
    debug_assert!(at < k, "the `]` follows the `[`");
    debug_assert!(k + 1 < e, "the `(` sits between `]` and `)`");
    Some(Link { close: k, end: e })
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

fn push_prose<'src>(out: &mut Vec<Segment<'src>>, s: &'src str) {
    if !s.trim().is_empty() {
        out.push(Segment::Prose(s));
    }
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
}
