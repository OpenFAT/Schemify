//! The compiled form (spec §1, §4): a [`Program`] is stack bytecode plus the
//! slot tables a caller binds before evaluating (identifiers, signals, plugin
//! functions). Built by `compile`, run by `eval` and `calc`, never mutated
//! after.
//!
//! Contents:
//! - `pub enum Mode`, `pub enum SignalKind`, `pub struct Program`
//! - `pub(crate) enum OpKind`, `pub(crate) struct Op`, `pub(crate) struct Name`,
//!   `pub(crate) struct Signal`, `pub(crate) struct PluginFn`, `pub(crate) struct CodeRange`
//! - `pub struct OpParts`, `pub struct PluginFunction` (`Display` as `namespace.function`)
//! - `pub fn op_parts(name: &str) -> OpParts`
//! - `Program::compile(src: &str, mode: Mode) -> Result<Program, Error>`,
//!   `Program::idents(&self)`, `Program::signals(&self)`,
//!   `Program::signal_span(&self, k: usize) -> Span`, `Program::plugin_fns(&self)`,
//!   `Program::bind(&self, out, lookup) -> Result<(), Error>`,
//!   `Program::name(&self, n: Name) -> &str`, `Program::plugin_name(&self, slot: u32) -> String`

use core::fmt;

use crate::compile::Compiler;
use crate::eval::Scalar;
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

/// An [`SignalKind::Op`] signal name split by [`op_parts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpParts<'name> {
    /// The instance path (`x1/m2`).
    pub instance: &'name str,
    /// The device parameter (`gm`), as written.
    pub parameter: &'name str,
}

/// One plugin function slot of a [`Program`], from [`Program::plugin_fns`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginFunction<'program> {
    /// The namespace before the dot (`gmid`).
    pub namespace: &'program str,
    /// The function after the dot (`id`).
    pub function: &'program str,
}

impl fmt::Display for PluginFunction<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.namespace, self.function)
    }
}

/// A compiled expression (or the holes of a [`crate::Template`]): immutable,
/// cheap to evaluate repeatedly, `Send + Sync`.
#[derive(Clone, Debug)]
pub struct Program {
    pub(crate) ops: Vec<Op>,
    /// Parallel to `ops`; read only when an error is raised.
    pub(crate) spans: Vec<Span>,
    /// Folded constants and literals. `Scalar::Str` ranges point into `pool`.
    pub(crate) consts: Vec<Scalar>,
    /// Names and string constants, back to back.
    pub(crate) pool: String,
    pub(crate) idents: Vec<Name>,
    /// Parallel to `idents`: the first occurrence of each.
    pub(crate) ident_spans: Vec<Span>,
    pub(crate) signals: Vec<Signal>,
    /// Parallel to `signals`: the call that first names each one.
    pub(crate) signal_spans: Vec<Span>,
    /// One per plugin slot.
    pub(crate) plugins: Vec<PluginFn>,
    /// Code range per root: one for an expression, one per template hole.
    pub(crate) roots: Vec<CodeRange>,
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

/// One signal slot: `v(name)`, `i(name)` or `op(…)` with its `<inst>[<param>]` name.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Signal {
    pub kind: SignalKind,
    pub name: Name,
}

/// One plugin slot: `namespace.function(…)`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PluginFn {
    pub namespace: Name,
    pub function: Name,
}

/// The ops `[start, end)` of one root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CodeRange {
    pub start: u32,
    pub end: u32,
}

// The evaluator's hot loop reads one 8-byte op per step (data design 3).
const _: () = assert!(size_of::<Op>() == 8);

/// Splits an [`SignalKind::Op`] signal name `<inst>[<param>]` into its
/// instance path and parameter (`x1/m2[gm]` → `x1/m2`, `gm`).
///
/// # Panics
/// If `name` is not shaped so; the compiler only makes such names.
#[must_use]
pub fn op_parts(name: &str) -> OpParts<'_> {
    let (inst, rest) = name.rsplit_once('[').expect("an op signal name");
    let param = rest.strip_suffix(']').expect("an op signal name");
    assert!(!inst.is_empty(), "op signal without an instance");
    assert!(!param.is_empty(), "op signal without a parameter");
    OpParts {
        instance: inst,
        parameter: param,
    }
}

impl Program {
    /// Compiles one expression.
    ///
    /// # Errors
    /// Any lexical or parse error (`X001`, `X002`, `X009`), unknown function
    /// or calculator-only function in [`Mode::Scalar`] (`X006`), wrong arity
    /// (`X008`), unknown `phys.*` constant (`X004`), and any error raised by a
    /// constant subexpression that is always evaluated (`X003`, `X010`, …).
    ///
    /// # Panics
    /// If `src` is 4 GiB or longer (spans are u32).
    pub fn compile(src: &str, mode: Mode) -> Result<Program, Error> {
        assert!(
            src.len() <= u32::MAX as usize,
            "expression source longer than 4 GiB"
        );
        let mut compiler = Compiler::new(mode);
        compiler.add_root(src, 0, src.len())?;
        let program = compiler.finish();
        assert_eq!(program.roots.len(), 1, "an expression is one root");
        Ok(program)
    }

    /// Identifier names in input-slot order: the order [`Program::eval`]
    /// expects its inputs in. Each name appears once. `phys.*` constants are
    /// not identifiers; other namespaced names (`a.b`) are, verbatim.
    #[must_use]
    pub fn idents(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.idents.iter().map(|n| self.name(*n))
    }

    /// Signals referenced by `v(…)`/`i(…)`/`op(…)`, in the slot order
    /// [`Plot::signals`](crate::Plot::signals) must follow. Empty in scalar mode.
    #[must_use]
    pub fn signals(&self) -> impl ExactSizeIterator<Item = (SignalKind, &str)> + '_ {
        self.signals
            .iter()
            .map(|signal| (signal.kind, self.name(signal.name)))
    }

    /// The source span of the call that first names signal `k` (`v(out)`,
    /// `op("M1", "gm")`): callers that fail to bind a signal report it there.
    ///
    /// # Panics
    /// If `k >= signals().len()`.
    #[must_use]
    pub fn signal_span(&self, k: usize) -> Span {
        assert!(
            k < self.signal_spans.len(),
            "signal {k} of {}",
            self.signal_spans.len()
        );
        debug_assert_eq!(self.signal_spans.len(), self.signals.len());
        self.signal_spans[k]
    }

    /// Plugin functions in slot order: the slot number a plugin callback
    /// receives indexes this list. Non-empty means the value depends on
    /// plugins, so callers caching values per D20 key on it.
    #[must_use]
    pub fn plugin_fns(&self) -> impl ExactSizeIterator<Item = PluginFunction<'_>> + '_ {
        self.plugins.iter().map(|plugin| PluginFunction {
            namespace: self.name(plugin.namespace),
            function: self.name(plugin.function),
        })
    }

    /// Resolves every identifier through `lookup` into `out` (cleared first),
    /// ready to pass to [`Program::eval`]. Resolution order across scopes
    /// (spec §4) is the caller's: chain layers inside `lookup`.
    ///
    /// # Errors
    /// `X004` naming the first identifier `lookup` does not know, spanned at
    /// its first occurrence; `out` then holds the inputs before it.
    pub fn bind<'input>(
        &self,
        out: &mut Vec<Value<'input>>,
        mut lookup: impl FnMut(&str) -> Option<Value<'input>>,
    ) -> Result<(), Error> {
        debug_assert_eq!(self.idents.len(), self.ident_spans.len());
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
        debug_assert_eq!(out.len(), self.idents.len(), "one input per identifier");
        Ok(())
    }

    /// The pool text `n` names.
    pub(crate) fn name(&self, n: Name) -> &str {
        let (start, len) = (n.start as usize, n.len as usize);
        debug_assert!(start + len <= self.pool.len(), "name past the pool");
        &self.pool[start..start + len]
    }

    /// `namespace.function` of plugin slot `slot`, for error messages.
    pub(crate) fn plugin_name(&self, slot: u32) -> String {
        assert!(
            (slot as usize) < self.plugins.len(),
            "plugin slot {slot} of {}",
            self.plugins.len()
        );
        let plugin = self.plugins[slot as usize];
        let name = PluginFunction {
            namespace: self.name(plugin.namespace),
            function: self.name(plugin.function),
        }
        .to_string();
        debug_assert!(name.len() >= 3, "both parts are identifiers");
        name
    }
}
