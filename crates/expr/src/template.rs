//! Templates (spec §6): text with `{expr}` holes; `{{` and `}}` are literal
//! braces. All holes compile into one [`Program`] with one root per hole, so
//! a template binds its identifiers once, like a single expression.

use core::fmt::Write;

use crate::compile::{Compiler, Mode, Program};
use crate::eval::{self, Scratch, V};
use crate::text::fmt_num;
use crate::{Code, Error, Span, Value};

/// A parsed template, rendered per instance.
#[derive(Clone, Debug)]
pub struct Template {
    program: Program,
    /// Literal text with escapes resolved.
    text: String,
    segs: Vec<Seg>,
}

#[derive(Clone, Copy, Debug)]
enum Seg {
    /// Byte range of `text`.
    Lit(u32, u32),
    /// Root index in `program`, and the hole's span for errors.
    Hole(u32, Span),
}

impl Template {
    /// Parses `src` and compiles every hole in [`Mode::Scalar`].
    ///
    /// # Errors
    /// `X009` for an unclosed `{`, an empty hole, or a lone `}` (write `}}`);
    /// any compile error of a hole, spanned in `src`.
    pub fn parse(src: &str) -> Result<Template, Error> {
        let mut c = Compiler::new(Mode::Scalar);
        let (mut text, mut segs) = (String::new(), Vec::new());
        let b = src.as_bytes();
        let mut lit_start = 0;
        let mut i = 0;
        let flush = |text: &str, segs: &mut Vec<Seg>, lit_start: &mut usize| {
            if text.len() > *lit_start {
                segs.push(Seg::Lit(*lit_start as u32, text.len() as u32));
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
                    flush(&text, &mut segs, &mut lit_start);
                    let root = c.roots_len();
                    c.add_root(src, i + 1, end)?;
                    segs.push(Seg::Hole(root, Span::new(i, end + 1)));
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
        flush(&text, &mut segs, &mut lit_start);
        let program = c.finish();
        assert_eq!(
            program.roots.len(),
            segs.iter().filter(|s| matches!(s, Seg::Hole(..))).count()
        );
        Ok(Template {
            program,
            text,
            segs,
        })
    }

    /// The compiled holes: use [`Program::idents`] and [`Program::bind`] to
    /// build the inputs for [`Template::render`]. In netlist context `name`,
    /// `cell`, `model`, `pins`, pin names, `params` and `file` (spec §6) are
    /// ordinary identifiers the caller binds.
    pub fn program(&self) -> &Program {
        &self.program
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
    pub fn render_with<P>(
        &self,
        inputs: &[Value<'_>],
        plugins: &mut P,
        scratch: &mut Scratch,
        out: &mut String,
    ) -> Result<(), Error>
    where
        P: FnMut(u32, &[Value<'_>]) -> Result<Value<'static>, Error>,
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
        r
    }

    fn render_inner<P>(
        &self,
        inputs: &[Value<'_>],
        plugins: &mut P,
        scratch: &mut Scratch,
        out: &mut String,
    ) -> Result<(), Error>
    where
        P: FnMut(u32, &[Value<'_>]) -> Result<Value<'static>, Error>,
    {
        for seg in &self.segs {
            match *seg {
                Seg::Lit(a, b) => out.push_str(&self.text[a as usize..b as usize]),
                Seg::Hole(root, span) => {
                    let (start, end) = self.program.roots[root as usize];
                    match eval::run(&self.program, start, end, inputs, plugins, scratch)? {
                        V::Int(x) => write!(out, "{x}").expect("String write"),
                        V::Num(x) => fmt_num(out, x).expect("String write"),
                        V::Bool(b) => out.push_str(if b { "true" } else { "false" }),
                        V::Str(s, len) => out.push_str(
                            core::str::from_utf8(&scratch.strs[s as usize..(s + len) as usize])
                                .expect("UTF-8"),
                        ),
                        V::Cnum(..) => {
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
            b'}' if !in_str => return Ok(i),
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
