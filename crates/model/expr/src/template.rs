//! Templates (spec §6): text with `{expr}` holes; `{{` and `}}` are literal
//! braces. All holes compile into one [`Program`] with one root per hole, so
//! a template binds its identifiers once, like a single expression.
//!
//! Contents:
//! - `pub struct Template`, `Template::parse(src: &str) -> Result<Template, Error>`,
//!   `Template::program(&self) -> &Program`, `Template::segments(&self)`,
//!   `Template::render(&self, inputs, scratch, out) -> Result<(), Error>`,
//!   `Template::render_with(&self, inputs, plugins, scratch, out) -> Result<(), Error>`
//! - `pub enum Segment<'template>`

use crate::compile::Compiler;
use crate::eval::{self, PluginValue, Scalar, Scratch};
use crate::program::{Mode, Program};
use crate::text;
use crate::{Code, Error, Span, Value};

/// A parsed template, rendered per instance.
#[derive(Clone, Debug)]
pub struct Template {
    program: Program,
    /// Literal text with escapes resolved.
    text: String,
    segments: Vec<StoredSegment>,
}

/// One piece of a parsed template, in source order (see [`Template::segments`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segment<'template> {
    /// Literal text with `{{`/`}}` resolved to one brace; never empty.
    Lit(&'template str),
    /// A hole.
    Hole {
        /// `Some(k)` when the hole is spelled as one bare identifier (`{ W }`,
        /// not `{(W)}` or `{W*1}`), `k` indexing [`Program::idents`].
        ident: Option<u32>,
    },
}

/// A [`Segment`] as stored: literal ranges index `text`, so one template is
/// two allocations plus its program.
#[derive(Clone, Copy, Debug)]
enum StoredSegment {
    /// Byte range `[start, end)` of `text`.
    Literal { start: u32, end: u32 },
    Hole {
        /// Root index in `program`.
        root: u32,
        /// The hole with its braces, for errors.
        span: Span,
        /// Index in `program.idents` of the identifier the hole is spelled
        /// as; [`NOT_IDENT`] for any other expression.
        ident: u32,
    },
}

/// `StoredSegment::Hole`'s identifier index when the hole is not one bare identifier.
const NOT_IDENT: u32 = u32::MAX;

impl Template {
    /// Parses `src` and compiles every hole in [`Mode::Scalar`].
    ///
    /// # Errors
    /// `X009` for an unclosed `{`, an empty hole, or a lone `}` (write `}}`);
    /// any compile error of a hole, spanned in `src`.
    ///
    /// # Panics
    /// If `src` is 4 GiB or longer (spans are u32).
    pub fn parse(src: &str) -> Result<Template, Error> {
        assert!(
            src.len() <= u32::MAX as usize,
            "template source longer than 4 GiB"
        );
        let mut compiler = Compiler::new(Mode::Scalar);
        let (mut text, mut segments) = (String::new(), Vec::new());
        let b = src.as_bytes();
        let mut lit_start = 0;
        let mut i = 0;
        let flush = |text: &str, segments: &mut Vec<StoredSegment>, lit_start: &mut usize| {
            if text.len() > *lit_start {
                segments.push(StoredSegment::Literal {
                    start: *lit_start as u32,
                    end: text.len() as u32,
                });
            }
            *lit_start = text.len();
        };
        while i < b.len() {
            match (b[i], b.get(i + 1)) {
                (b'{', Some(b'{')) | (b'}', Some(b'}')) => {
                    text.push(b[i] as char);
                    i += 2;
                }
                (b'{', _) => {
                    let end = hole_end(src, i)?;
                    if src[i + 1..end].trim().is_empty() {
                        return Err(Error::new(
                            Code::X009,
                            Span::new(i, end + 1),
                            "empty template hole",
                        ));
                    }
                    flush(&text, &mut segments, &mut lit_start);
                    let root = compiler.roots_len();
                    compiler.add_root(src, i + 1, end)?;
                    segments.push(StoredSegment::Hole {
                        root,
                        span: Span::new(i, end + 1),
                        ident: NOT_IDENT,
                    });
                    i = end + 1;
                }
                (b'}', _) => {
                    return Err(Error::new(
                        Code::X009,
                        Span::new(i, i + 1),
                        "lone `}` in template (write `}}`)",
                    ));
                }
                _ => {
                    let next = src[i..].find(['{', '}']).map_or(src.len(), |k| i + k);
                    text.push_str(&src[i..next]);
                    i = next;
                }
            }
        }
        flush(&text, &mut segments, &mut lit_start);
        debug_assert_eq!(lit_start, text.len(), "every literal byte is in a segment");
        let program = compiler.finish();
        let holes = segments
            .iter()
            .filter(|segment| matches!(segment, StoredSegment::Hole { .. }))
            .count();
        assert_eq!(program.roots.len(), holes, "one root per hole");
        // A hole whose trimmed text equals an input identifier is that identifier
        // alone: any other expression using it is longer than its name.
        for segment in &mut segments {
            if let StoredSegment::Hole { span, ident, .. } = segment {
                let hole = src[span.start as usize + 1..span.end as usize - 1].trim();
                if let Some(k) = program.idents().position(|id| id == hole) {
                    *ident = k as u32;
                }
            }
        }
        Ok(Template {
            program,
            text,
            segments,
        })
    }

    /// The compiled holes: use [`Program::idents`] and [`Program::bind`] to
    /// build the inputs for [`Template::render`]. In netlist context `name`,
    /// `cell`, `model`, `pins`, pin names, `params` and `file` (spec §6) are
    /// ordinary identifiers the caller binds.
    #[must_use]
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// The template as literal text and holes, in source order. Lets a caller
    /// that already holds the text of every bare-identifier hole (the
    /// netlister's params and nets) copy it without evaluating.
    pub fn segments(&self) -> impl Iterator<Item = Segment<'_>> + '_ {
        self.segments.iter().map(|segment| match *segment {
            StoredSegment::Literal { start, end } => {
                Segment::Lit(&self.text[start as usize..end as usize])
            }
            StoredSegment::Hole { ident, .. } => Segment::Hole {
                ident: (ident != NOT_IDENT).then_some(ident),
            },
        })
    }

    /// Renders onto `out`: literals verbatim, `int`/`num` as canonical number
    /// text (spec §7), `bool` as `true`/`false`, `str` verbatim.
    ///
    /// # Errors
    /// Any hole's evaluation error; `X003` for a `cnum` hole. On error `out`
    /// is restored to its length before the call.
    ///
    /// # Panics
    /// If `inputs.len() != program().idents().len()`.
    pub fn render(
        &self,
        inputs: &[Value<'_>],
        scratch: &mut Scratch,
        out: &mut String,
    ) -> Result<(), Error> {
        self.render_with(inputs, &mut eval::no_plugins(&self.program), scratch, out)
    }

    /// [`Template::render`] with plugin functions (see [`Program::eval_with`]).
    ///
    /// # Errors
    /// As [`Template::render`], plus whatever `plugins` returns.
    ///
    /// # Panics
    /// As [`Template::render`].
    pub fn render_with<P, R>(
        &self,
        inputs: &[Value<'_>],
        plugins: &mut P,
        scratch: &mut Scratch,
        out: &mut String,
    ) -> Result<(), Error>
    where
        P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
        R: PluginValue,
    {
        assert_eq!(
            inputs.len(),
            self.program.idents.len(),
            "one input per identifier"
        );
        let len0 = out.len();
        let r = self.render_inner(inputs, plugins, scratch, out);
        if r.is_err() {
            out.truncate(len0);
        }
        debug_assert!(out.len() >= len0, "rendering only appends");
        r
    }
}

// ---- private ----

impl Template {
    /// Evaluates every hole onto `out`; [`Template::render_with`] restores
    /// `out` on error.
    fn render_inner<P, R>(
        &self,
        inputs: &[Value<'_>],
        plugins: &mut P,
        scratch: &mut Scratch,
        out: &mut String,
    ) -> Result<(), Error>
    where
        P: FnMut(u32, &[Value<'_>]) -> Result<R, Error>,
        R: PluginValue,
    {
        for segment in &self.segments {
            match *segment {
                StoredSegment::Literal { start, end } => {
                    out.push_str(&self.text[start as usize..end as usize]);
                }
                StoredSegment::Hole { root, span, .. } => {
                    let code = self.program.roots[root as usize];
                    match eval::run(&self.program, code, inputs, plugins, scratch)? {
                        Scalar::Int(x) => text::write_int(out, x),
                        Scalar::Num(x) => text::write_num(out, x),
                        Scalar::Bool(b) => out.push_str(if b { "true" } else { "false" }),
                        Scalar::Str(s, len) => {
                            out.push_str(eval::str_at(&scratch.strings, s, len));
                        }
                        Scalar::Cnum(..) => {
                            return Err(Error::new(
                                Code::X003,
                                span,
                                "a cnum has no text form; use re/im/mag/ph",
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Index of the `}` closing the hole opened at `open`, skipping `}` inside
/// string literals (expressions themselves never contain braces).
fn hole_end(src: &str, open: usize) -> Result<usize, Error> {
    let b = src.as_bytes();
    debug_assert_eq!(b[open], b'{');
    let (mut i, mut in_str) = (open + 1, false);
    while i < b.len() {
        match b[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'}' if !in_str => {
                debug_assert!(i > open, "a hole closes after it opens");
                return Ok(i);
            }
            _ => {}
        }
        i += 1;
    }
    Err(Error::new(
        Code::X009,
        Span::new(open, src.len()),
        "unclosed `{` in template (write `{{` for a literal brace)",
    ))
}
