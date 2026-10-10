//! Compiled expressions with their scope already resolved ([`Progs`]), the owned value
//! type every graph node holds ([`Val`]), and the one evaluation step (D20 plugin
//! calls with the `cached` fallback).
//!
//! Scope references are resolved once per program, not once per use: a program's
//! identifiers become [`Ref`]s into "the params, then the vars" of the scope the
//! program is written in (expr spec §4). A graph then turns refs into node indices
//! with arithmetic, which is what keeps a 10^5-instance cell cheap to build.

use schemify_expr::{
    Code as XCode, Error as XError, Mode, Program, Scratch, Span, Template, Value,
};

/// A scope reference: param `k` is `k`, var `v` is `VAR | v`.
pub(crate) type Ref = u32;
/// Tag bit of a var reference.
pub(crate) const VAR: Ref = 1 << 31;
/// "No such index" in optional `u32` columns.
pub(crate) const NONE: u32 = u32::MAX;

/// A value as elab stores it: owned, so it outlives the evaluation scratch.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    /// `int`.
    Int(i64),
    /// `num` (finite).
    Num(f64),
    /// `bool`.
    Bool(bool),
    /// `cnum`.
    Cnum(f64, f64),
    /// `str`.
    Str(Box<str>),
    /// This node's own expression failed (reported as `E008`).
    Err(Box<XError>),
    /// A node this one depends on failed; reported there, not here.
    Upstream,
}

impl Val {
    /// Returns the value as an expression input; `None` for the two error states.
    pub fn value(&self) -> Option<Value<'_>> {
        Some(match self {
            Val::Int(i) => Value::Int(*i),
            Val::Num(x) => Value::Num(*x),
            Val::Bool(b) => Value::Bool(*b),
            Val::Cnum(re, im) => Value::Cnum { re: *re, im: *im },
            Val::Str(s) => Value::Str(s),
            Val::Err(_) | Val::Upstream => return None,
        })
    }

    /// Copies an evaluation result out of the scratch it borrows.
    pub fn from_value(v: Value<'_>) -> Val {
        match v {
            Value::Int(i) => Val::Int(i),
            Value::Num(x) => Val::Num(x),
            Value::Bool(b) => Val::Bool(b),
            Value::Cnum { re, im } => Val::Cnum(re, im),
            Value::Str(s) => Val::Str(s.into()),
        }
    }

    /// Returns whether this is one of the two error states.
    pub fn is_err(&self) -> bool {
        matches!(self, Val::Err(_) | Val::Upstream)
    }

    /// Returns the value as `f64` for gating (`int` promotes); `None` otherwise.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Val::Int(i) => Some(*i as f64),
            Val::Num(x) => Some(*x),
            _ => None,
        }
    }

    /// Appends the canonical text (expr spec §7): numbers canonical, `bool` as
    /// `true`/`false`, `str` verbatim, `cnum` as `re+imi`.
    ///
    /// # Panics
    ///
    /// Panics on the error states (they have no text; check [`Val::is_err`]).
    pub fn write_text(&self, out: &mut String) {
        match self {
            Val::Int(i) => schemify_expr::write_int(out, *i),
            Val::Num(x) => schemify_expr::write_num(out, *x),
            Val::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Val::Str(s) => out.push_str(s),
            Val::Cnum(re, im) => {
                schemify_expr::write_num(out, *re);
                if *im >= 0.0 {
                    out.push('+');
                }
                schemify_expr::write_num(out, *im);
                out.push('i');
            }
            Val::Err(_) | Val::Upstream => panic!("an error has no text"),
        }
    }

    /// Parses a `cached <value>` attribute (D20): a number, `true`/`false`, else text.
    pub(crate) fn parse_cached(text: &str) -> Val {
        match text {
            "true" => Val::Bool(true),
            "false" => Val::Bool(false),
            _ => match schemify_expr::parse_num(text) {
                Ok(x) if x.fract() == 0.0 && x.abs() < 9.0e15 => Val::Int(x as i64),
                Ok(x) => Val::Num(x),
                Err(_) => Val::Str(text.into()),
            },
        }
    }
}

/// How a program's text is read (format spec §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// Expression positions (number/int/bool params, vars, bounds).
    Expr,
    /// Template positions (string/choice params).
    Template,
}

#[derive(Clone, Debug)]
pub(crate) enum Code {
    Expr(Program),
    Tmpl(Template),
    /// Compile error, or `X004` from scope resolution.
    Bad(Box<XError>),
}

/// A table of compiled programs with resolved scope references (CSR) and the D20
/// `cached` value of the record each came from.
#[derive(Clone, Debug, Default)]
pub(crate) struct Progs {
    pub(crate) code: Vec<Code>,
    pub(crate) ref_start: Vec<u32>,
    pub(crate) refs: Vec<Ref>,
    pub(crate) cached: Vec<Option<Val>>,
}

impl Progs {
    /// Compiles `text` and resolves its identifiers with `lookup` (name → [`Ref`]).
    /// Returns the program index. Errors are stored as [`Code::Bad`], never returned:
    /// a bad expression is a finding about the design, not a failure of elab.
    pub(crate) fn push(
        &mut self,
        text: &str,
        syntax: Syntax,
        cached: Option<Val>,
        lookup: impl Fn(&str) -> Option<Ref>,
    ) -> u32 {
        if self.ref_start.is_empty() {
            self.ref_start.push(0);
        }
        let compiled = match syntax {
            Syntax::Expr => Program::compile(text, Mode::Scalar).map(Code::Expr),
            Syntax::Template => Template::parse(text).map(Code::Tmpl),
        };
        let code = match compiled {
            Err(e) => Code::Bad(Box::new(e)),
            Ok(code) => {
                let prog = match &code {
                    Code::Expr(p) => p,
                    Code::Tmpl(t) => t.program(),
                    Code::Bad(_) => unreachable!(),
                };
                let mut dummy = Vec::new();
                let bound = prog.bind(&mut dummy, |name| {
                    lookup(name).map(|r| {
                        self.refs.push(r);
                        Value::Bool(false)
                    })
                });
                match bound {
                    Ok(()) => code,
                    Err(e) => {
                        self.refs
                            .truncate(*self.ref_start.last().expect("seeded") as usize);
                        Code::Bad(Box::new(e))
                    }
                }
            }
        };
        self.code.push(code);
        self.ref_start.push(self.refs.len() as u32);
        self.cached.push(cached);
        assert_eq!(self.code.len() + 1, self.ref_start.len());
        (self.code.len() - 1) as u32
    }

    pub(crate) fn refs(&self, p: u32) -> &[Ref] {
        let p = p as usize;
        &self.refs[self.ref_start[p] as usize..self.ref_start[p + 1] as usize]
    }

    /// Evaluates program `p` on `inputs` (one per ref, in ref order). Returns the value
    /// and whether it is *unverified* (D20: a plugin function was missing and the
    /// record's `cached` value stands in for the whole expression).
    pub(crate) fn eval<F: Funcs>(
        &self,
        p: u32,
        inputs: &[Value<'_>],
        funcs: &mut F,
        scratch: &mut Scratch,
        text: &mut String,
    ) -> (Val, bool) {
        let mut missing = false;
        let result = match &self.code[p as usize] {
            Code::Bad(e) => return (Val::Err(e.clone()), false),
            Code::Expr(prog) => {
                let mut call =
                    |slot: u32, args: &[Value<'_>]| plugin(prog, slot, args, funcs, &mut missing);
                prog.eval_with(inputs, &mut call, scratch)
                    .map(Val::from_value)
            }
            Code::Tmpl(t) => {
                let mut call = |slot: u32, args: &[Value<'_>]| {
                    plugin(t.program(), slot, args, funcs, &mut missing)
                };
                text.clear();
                t.render_with(inputs, &mut call, scratch, text)
                    .map(|()| Val::Str(text.as_str().into()))
            }
        };
        match (result, &self.cached[p as usize]) {
            (Ok(v), _) => (v, false),
            (Err(_), Some(c)) if missing => (c.clone(), true),
            (Err(e), _) => (Val::Err(Box::new(e)), false),
        }
    }
}

/// The plugin function registry (D20): returns `None` when `"ns.f"` is not loaded,
/// else the call's result. Must be pure. Implemented for every matching closure.
pub trait Funcs: FnMut(&str, &[Value<'_>]) -> Option<Result<Value<'static>, XError>> {}
impl<F: FnMut(&str, &[Value<'_>]) -> Option<Result<Value<'static>, XError>>> Funcs for F {}

/// A registry with no plugin functions loaded: every `ns.f(...)` falls back to its
/// record's `cached` value (unverified) or fails with `X006`.
pub fn no_funcs(_name: &str, _args: &[Value<'_>]) -> Option<Result<Value<'static>, XError>> {
    None
}

fn plugin<F: Funcs>(
    prog: &Program,
    slot: u32,
    args: &[Value<'_>],
    funcs: &mut F,
    missing: &mut bool,
) -> Result<Value<'static>, XError> {
    let (ns, f) = prog
        .plugin_fns()
        .nth(slot as usize)
        .expect("slot from this program");
    let name = format!("{ns}.{f}");
    match funcs(&name, args) {
        Some(r) => r,
        None => {
            *missing = true;
            Err(XError::new(
                XCode::X006,
                Span::default(),
                format!("plugin function `{name}` is not loaded"),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(name: &str) -> Option<Ref> {
        match name {
            "w" => Some(0),
            "k" => Some(VAR | 2),
            _ => None,
        }
    }

    #[test]
    fn programs_resolve_scope_once_and_report_unknowns() {
        let mut p = Progs::default();
        let a = p.push("2*w + k", Syntax::Expr, None, lookup);
        let b = p.push("w + nope", Syntax::Expr, None, lookup);
        let c = p.push("M{w}", Syntax::Template, None, lookup);
        let d = p.push("1 +", Syntax::Expr, None, lookup);
        assert_eq!(p.refs(a), &[0, VAR | 2]);
        assert!(matches!(&p.code[b as usize], Code::Bad(e) if e.code == XCode::X004));
        assert!(p.refs(b).is_empty());
        assert_eq!(p.refs(c), &[0]);
        assert!(matches!(&p.code[d as usize], Code::Bad(e) if e.code == XCode::X009));
        let (mut s, mut t) = (Scratch::default(), String::new());
        let (v, unverified) = p.eval(
            a,
            &[Value::Num(0.5), Value::Int(1)],
            &mut no_funcs,
            &mut s,
            &mut t,
        );
        assert_eq!((v, unverified), (Val::Num(2.0), false));
        let (v, _) = p.eval(c, &[Value::Int(3)], &mut no_funcs, &mut s, &mut t);
        assert_eq!(v, Val::Str("M3".into()));
    }

    #[test]
    fn missing_plugin_uses_cached_value_and_flags_it() {
        let mut p = Progs::default();
        let with = p.push("gmid.width(w)", Syntax::Expr, Some(Val::Num(2.5)), lookup);
        let without = p.push("gmid.width(w)", Syntax::Expr, None, lookup);
        let (mut s, mut t) = (Scratch::default(), String::new());
        let inputs = [Value::Num(1.0)];
        assert_eq!(
            p.eval(with, &inputs, &mut no_funcs, &mut s, &mut t),
            (Val::Num(2.5), true)
        );
        let (v, u) = p.eval(without, &inputs, &mut no_funcs, &mut s, &mut t);
        assert!(matches!(v, Val::Err(e) if e.code == XCode::X006) && !u);
        let mut loaded = |name: &str, args: &[Value<'_>]| {
            assert_eq!(name, "gmid.width");
            let Value::Num(x) = args[0] else { panic!() };
            Some(Ok(Value::Num(x * 3.0)))
        };
        assert_eq!(
            p.eval(with, &inputs, &mut loaded, &mut s, &mut t),
            (Val::Num(3.0), false)
        );
    }

    #[test]
    fn cached_text_parses_by_shape_and_canonical_text_round_trips() {
        assert_eq!(Val::parse_cached("2"), Val::Int(2));
        assert_eq!(Val::parse_cached("2.5u"), Val::Num(2.5e-6));
        assert_eq!(Val::parse_cached("true"), Val::Bool(true));
        assert_eq!(Val::parse_cached("nfet"), Val::Str("nfet".into()));
        let mut s = String::new();
        Val::Num(2.5e-12).write_text(&mut s);
        assert_eq!(s, "2.5e-12");
    }
}
