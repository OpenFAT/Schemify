//! The one expression language (D12): parameters, derived params, conditions,
//! netlist templates, docs variables, outputs/specs and the waveform calculator.
//!
//! Pure and total. Owns no tables. Normative spec: `docs/spec/expr.md`.
//!
//! # Pipeline
//!
//! `source → lex → parse (flat SoA node table) → compile (bytecode, constant
//! folding, arity checks) → eval`. Compile once, evaluate many times: the same
//! [`Program`] is evaluated per instance and per sweep point.
//!
//! Identifiers are not looked up during evaluation. [`Program::idents`] lists
//! them once; the caller resolves each name to a value source when it builds
//! its dependency graph and then hands [`Program::eval`] one [`Value`] per
//! identifier, in that order. [`Program::bind`] does the resolution for callers
//! that hold a name lookup.
//!
//! ```
//! use schemify_expr::{Mode, Program, Scratch, Value};
//!
//! let p = Program::compile("2*w + 1u", Mode::Scalar).unwrap();
//! let mut inputs = Vec::new();
//! p.bind(&mut inputs, |name| (name == "w").then_some(Value::Num(0.5e-6))).unwrap();
//!
//! let mut scratch = Scratch::default();
//! assert_eq!(p.eval(&inputs, &mut scratch).unwrap(), Value::Num(2e-6));
//! ```
//!
//! Templates (`{expr}` holes) render with canonical number text (spec §7):
//!
//! ```
//! use schemify_expr::{Scratch, Template, Value};
//!
//! let t = Template::parse("R{name} {pins} r={r*2}").unwrap();
//! let mut inputs = Vec::new();
//! t.program().bind(&mut inputs, |n| match n {
//!     "name" => Some(Value::Str("1")),
//!     "pins" => Some(Value::Str("a b")),
//!     "r" => Some(Value::Num(1.25e3)),
//!     _ => None,
//! }).unwrap();
//! let mut out = String::new();
//! t.render(&inputs, &mut Scratch::default(), &mut out).unwrap();
//! assert_eq!(out, "R1 a b r=2500");
//! ```
#![forbid(unsafe_code)]

// Data design (the six DOD questions):
// 1. In: expression text (files cap it at 2^30 bytes). Out: a `Program`, once;
//    then a `Value`, a column or rendered text per evaluation.
// 2. Many: one program per expression in a project (elab's `expr_graph`),
//    evaluated per instance and per sweep point, so evaluation is the hot loop.
// 3. Widths: spans u32 (sources < 4 GiB); AST child indices u32 (≤ 65 536
//    nodes); an op is u8 kind + u8 sub + u32 arg = 8 bytes; a value is 24
//    bytes (i64 | f64 | (f64, f64) | (u32, u32) string range, plus tag).
// 4. Access: the evaluator walks ops in order reading kind and operand
//    together (AoS); spans sit in a parallel column read only on errors.
// 5. Lifetime: the AST lives for one compile call; a `Program` lives with its
//    expression; a `Scratch` lives per thread and is reused, so the scalar
//    path does not allocate after warm-up (strings excepted).
// 6. Parallel: programs are immutable (`Send + Sync`); one `Scratch` per
//    thread evaluates disjoint instances independently.

mod calc;
mod compile;
#[doc(hidden)]
pub mod dmath;
mod eval;
mod fft;
mod graph;
mod lex;
mod parse;
mod template;
mod text;

use core::fmt;

pub use calc::{AxisOut, CalcResult, ColKind, ColOut, Column, Plot};
pub use compile::{Mode, Program, SignalKind};
pub use eval::{PluginValue, Scratch};
pub use graph::{Cycle, Deps, affected, reverse, topo_order, topo_order_all};
pub use template::Template;
pub use text::{parse_num, write_int, write_num};

/// Maximum AST nodes per expression (spec §3); more is [`Code::X002`].
pub const MAX_NODES: usize = 65_536;
/// Maximum syntactic nesting depth per expression (spec §3); more is [`Code::X002`].
pub const MAX_DEPTH: u32 = 256;
/// Maximum byte length of any string value (spec §1); longer is [`Code::X002`].
pub const MAX_STR_BYTES: usize = 1 << 20;
/// Maximum arguments to a plugin function `ns.f(…)`; more is [`Code::X008`].
pub const MAX_PLUGIN_ARGS: usize = 16;

/// A scalar value: what identifiers are bound to and what scalar evaluation returns.
///
/// `Str` borrows: inputs borrow from the caller's tables, results borrow from
/// the [`Scratch`] they were evaluated in (copy them out before the next
/// evaluation reuses it). NaN and infinities are never valid values (`X010`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    /// `int`: i64; promotes to `num` on overflow and on `/`.
    Int(i64),
    /// `num`: a finite f64.
    Num(f64),
    /// `bool`.
    Bool(bool),
    /// `cnum`: complex with finite parts.
    Cnum {
        /// Real part.
        re: f64,
        /// Imaginary part.
        im: f64,
    },
    /// `str`: UTF-8, at most [`MAX_STR_BYTES`].
    Str(&'a str),
}

/// Expression error codes (spec §8). Only [`Code::X007`] is a warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    /// Bad number literal or SI suffix.
    X001,
    /// Expression too large or too deep, or a string over [`MAX_STR_BYTES`].
    X002,
    /// Type error.
    X003,
    /// Unknown identifier.
    X004,
    /// Dependency cycle.
    X005,
    /// Unknown function, calculator-only function in scalar mode, or missing plugin.
    X006,
    /// Pin/param name shadowing in a template (warning; detected by the caller).
    X007,
    /// Wrong arity.
    X008,
    /// Parse error.
    X009,
    /// NaN or infinity produced (also: a measurement with no answer, such as no crossing).
    X010,
    /// Column length or sweep-axis mismatch.
    X011,
}

impl Code {
    /// Returns the code as written in reports (`"X004"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::X001 => "X001",
            Code::X002 => "X002",
            Code::X003 => "X003",
            Code::X004 => "X004",
            Code::X005 => "X005",
            Code::X006 => "X006",
            Code::X007 => "X007",
            Code::X008 => "X008",
            Code::X009 => "X009",
            Code::X010 => "X010",
            Code::X011 => "X011",
        }
    }

    /// Returns true for codes that are reported but do not block (`X007`).
    pub const fn is_warning(self) -> bool {
        matches!(self, Code::X007)
    }
}

/// Byte range `[start, end)` in the source text that was compiled (for a
/// [`Template`], the whole template text). Empty when the error has no source
/// position (dependency cycles, caller-made errors).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    /// First byte.
    pub start: u32,
    /// One past the last byte.
    pub end: u32,
}

impl Span {
    pub(crate) fn new(start: usize, end: usize) -> Span {
        assert!(start <= end, "span start {start} after end {end}");
        assert!(end <= u32::MAX as usize, "source longer than 4 GiB");
        Span {
            start: start as u32,
            end: end as u32,
        }
    }

    pub(crate) fn join(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// An expression error: code, where, and a human-readable message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Spec §8 code.
    pub code: Code,
    /// Offending source range; empty when not tied to source.
    pub span: Span,
    /// Message naming the offending token, identifier, function or cycle.
    pub msg: String,
}

impl Error {
    /// Builds an error; plugin callbacks use this to report their own failures
    /// (a missing plugin is [`Code::X006`]).
    pub fn new(code: Code, span: Span, msg: impl Into<String>) -> Error {
        Error {
            code,
            span,
            msg: msg.into(),
        }
    }

    /// Replaces an empty span (errors raised away from the source) with `span`.
    pub(crate) fn at(mut self, span: Span) -> Error {
        if self.span == Span::default() {
            self.span = span;
        }
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at {}..{}: {}",
            self.code.as_str(),
            self.span.start,
            self.span.end,
            self.msg
        )
    }
}

impl std::error::Error for Error {}
