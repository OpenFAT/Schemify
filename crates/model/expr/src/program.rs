//! The compiled form (spec §1, §4): a [`Program`] is stack bytecode plus the
//! slot tables a caller binds before evaluating (identifiers, signals, plugin
//! functions). Built by `compile`, run by `eval` and `calc`, never mutated
//! after.
//!
//! Contents:
//! - `pub enum Mode`, `pub enum SignalKind`, `pub struct Program`
//! - `pub(crate) enum OpKind`, `pub(crate) struct Op`, `pub(crate) struct Name`
//! - `pub fn op_parts(name: &str) -> (&str, &str)`
//! - `Program::compile(src: &str, mode: Mode) -> Result<Program, Error>`,
//!   `Program::idents(&self)`, `Program::signals(&self)`,
//!   `Program::signal_span(&self, k: usize) -> Span`, `Program::plugin_fns(&self)`,
//!   `Program::bind(&self, out, lookup) -> Result<(), Error>`,
//!   `Program::name(&self, n: Name) -> &str`, `Program::plugin_name(&self, slot: u32) -> String`

use crate::compile::Compiler;
use crate::eval::V;
use crate::{Code, Error, Span, Value};

/// Evaluation mode an expression is compiled for (spec §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Params, vars, templates, specs: no columns; calculator-only functions are `X006`.
    Scalar,
    /// Outputs and graph traces: columns over a result set's sweep axis.
    Calc,
}

/// Kind of a result signal referenced as `v(net)` or `i(dev)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SignalKind {
    /// `v(net)`: node voltage.
    V,
    /// `i(dev)`: device current.
    I,
    /// `op(inst, param)`: a device's DC operating-point value, one number; the
    /// signal name is `<inst>[<param>]` (split it with [`op_parts`]).
    Op,
}

/// A compiled expression (or the holes of a [`crate::Template`]): immutable,
/// cheap to evaluate repeatedly, `Send + Sync`.
#[derive(Clone, Debug)]
pub struct Program {
    pub(crate) ops: Vec<Op>,
    /// Parallel to `ops`; read only when an error is raised.
    pub(crate) spans: Vec<Span>,
    /// Folded constants and literals. `V::Str` ranges point into `pool`.
    pub(crate) consts: Vec<V>,
    /// Names and string constants, back to back.
    pub(crate) pool: String,
    pub(crate) idents: Vec<Name>,
    /// Parallel to `idents`: the first occurrence of each.
    pub(crate) ident_spans: Vec<Span>,
    pub(crate) signals: Vec<(SignalKind, Name)>,
    /// Parallel to `signals`: the call that first names each one.
    pub(crate) signal_spans: Vec<Span>,
    /// `(namespace, function)` per plugin slot.
    pub(crate) plugins: Vec<(Name, Name)>,
    /// Code range per root: one for an expression, one per template hole.
    pub(crate) roots: Vec<(u32, u32)>,
    pub(crate) mode: Mode,
}

/// Bytecode operation. Lazy operators are bracketed jumps (`Cond … Else …
/// EndCond`, `And … EndAnd`, `Or … EndOr`): the scalar evaluator jumps; the
/// calculator, given a column condition, runs both arms and selects per element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpKind {
    /// Pushes `consts[arg]`.
    Const,
    /// Pushes `inputs[arg]`.
    Input,
    /// Pushes signal slot `arg` (calculator only).
    Signal,
    /// Pushes the sweep axis `x()` (calculator only).
    Axis,
    Neg,
    Not,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Pops a bool; false jumps to `arg` (start of the else arm).
    Cond,
    /// End of the then arm; jumps to `arg` (the matching `EndCond`).
    Else,
    EndCond,
    /// Pops a bool; false pushes false and jumps to `arg` (past `EndAnd`).
    And,
    /// Checks the right operand is bool.
    EndAnd,
    /// Pops a bool; true pushes true and jumps to `arg` (past `EndOr`).
    Or,
    EndOr,
    /// `sub` = builtin id, `arg` = argc.
    Call,
    /// `sub` = argc, `arg` = plugin slot.
    Plugin,
}

/// One instruction. AoS: the evaluator reads kind and operands together.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Op {
    pub kind: OpKind,
    pub sub: u8,
    pub arg: u32,
}

/// Byte range of a name or string constant in [`Program::pool`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Name {
    pub start: u32,
    pub len: u32,
}

// The evaluator's hot loop reads one 8-byte op per step (data design 3).
const _: () = assert!(size_of::<Op>() == 8);

/// Splits an [`SignalKind::Op`] signal name `<inst>[<param>]` into its
/// instance path and parameter (`("x1/m2", "gm")`).
///
/// # Panics
/// If `name` is not shaped so; the compiler only makes such names.
pub fn op_parts(name: &str) -> (&str, &str) {
    let (inst, rest) = name.rsplit_once('[').expect("an op signal name");
    let param = rest.strip_suffix(']').expect("an op signal name");
    assert!(!inst.is_empty(), "op signal without an instance");
    assert!(!param.is_empty(), "op signal without a parameter");
    (inst, param)
}

impl Program {
    /// Compiles one expression.
    ///
    /// # Errors
    /// Any lexical or parse error (`X001`, `X002`, `X009`), unknown function
    /// or calculator-only function in [`Mode::Scalar`] (`X006`), wrong arity
    /// (`X008`), unknown `phys.*` constant (`X004`), and any error raised by a
    /// constant subexpression that is always evaluated (`X003`, `X010`, …).
    pub fn compile(src: &str, mode: Mode) -> Result<Program, Error> {
        let mut c = Compiler::new(mode);
        c.add_root(src, 0, src.len())?;
        let p = c.finish();
        assert_eq!(p.roots.len(), 1);
        Ok(p)
    }

    /// Identifier names in input-slot order: the order [`Program::eval`]
    /// expects its inputs in. Each name appears once. `phys.*` constants are
    /// not identifiers; other namespaced names (`a.b`) are, verbatim.
    pub fn idents(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.idents.iter().map(|n| self.name(*n))
    }

    /// Signals referenced by `v(…)`/`i(…)`/`op(…)`, in the slot order
    /// [`Plot::signals`](crate::Plot::signals) must follow. Empty in scalar mode.
    pub fn signals(&self) -> impl ExactSizeIterator<Item = (SignalKind, &str)> + '_ {
        self.signals.iter().map(|&(k, n)| (k, self.name(n)))
    }

    /// The source span of the call that first names signal `k` (`v(out)`,
    /// `op("M1", "gm")`): callers that fail to bind a signal report it there.
    ///
    /// # Panics
    /// If `k >= signals().len()`.
    pub fn signal_span(&self, k: usize) -> Span {
        self.signal_spans[k]
    }

    /// Plugin functions `(namespace, name)` in slot order: the slot number a
    /// plugin callback receives indexes this list. Non-empty means the value
    /// depends on plugins, so callers caching values per D20 key on it.
    pub fn plugin_fns(&self) -> impl ExactSizeIterator<Item = (&str, &str)> + '_ {
        self.plugins
            .iter()
            .map(|&(ns, f)| (self.name(ns), self.name(f)))
    }

    /// Resolves every identifier through `lookup` into `out` (cleared first),
    /// ready to pass to [`Program::eval`]. Resolution order across scopes
    /// (spec §4) is the caller's: chain layers inside `lookup`.
    ///
    /// # Errors
    /// `X004` naming the first identifier `lookup` does not know, spanned at
    /// its first occurrence; `out` then holds the inputs before it.
    pub fn bind<'a>(
        &self,
        out: &mut Vec<Value<'a>>,
        mut lookup: impl FnMut(&str) -> Option<Value<'a>>,
    ) -> Result<(), Error> {
        out.clear();
        for (i, name) in self.idents().enumerate() {
            let v = lookup(name).ok_or_else(|| {
                Error::new(
                    Code::X004,
                    self.ident_spans[i],
                    format!("unknown identifier `{name}`"),
                )
            })?;
            out.push(v);
        }
        assert_eq!(out.len(), self.idents.len());
        Ok(())
    }

    /// The pool text `n` names.
    pub(crate) fn name(&self, n: Name) -> &str {
        let (start, len) = (n.start as usize, n.len as usize);
        debug_assert!(start + len <= self.pool.len());
        &self.pool[start..start + len]
    }

    /// `ns.f` of plugin slot `slot`, for error messages.
    pub(crate) fn plugin_name(&self, slot: u32) -> String {
        let (ns, f) = self.plugins[slot as usize];
        format!("{}.{}", self.name(ns), self.name(f))
    }
}
