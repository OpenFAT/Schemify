//! Compile: flat AST → stack bytecode.
//!
//! Done once per expression, so evaluation does no name lookup, no arity
//! check and no work on constant subexpressions:
//! - identifiers become input slots (first-occurrence order), `v()`/`i()`
//!   become signal slots, `ns.f()` become plugin slots;
//! - builtin names, arity and mode are checked (`X006`, `X008`);
//! - every constant subtree is folded by running the scalar evaluator on it.
//!   A folding error in a subtree that is always evaluated is reported here
//!   (static type and range checking); under a lazy branch it is left for
//!   runtime, so `false && 1/0` stays legal.
//!
//! Lazy operators compile to bracketed jumps (`Cond … Else … EndCond`,
//! `And … EndAnd`, `Or … EndOr`). The scalar evaluator jumps; the calculator
//! evaluator, given a column condition, runs both arms and selects per element.

use std::collections::BTreeMap;

use crate::eval::{self, Scratch, V};
use crate::parse::{Ast, BinOp, Node, parse};
use crate::{Code, Error, MAX_PLUGIN_ARGS, Span, Value};

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

/// Splits an [`SignalKind::Op`] signal name `<inst>[<param>]` into its
/// instance path and parameter (`("x1/m2", "gm")`).
///
/// # Panics
/// If `name` is not shaped so; the compiler only makes such names.
pub fn op_parts(name: &str) -> (&str, &str) {
    let (inst, rest) = name.rsplit_once('[').expect("an op signal name");
    let param = rest.strip_suffix(']').expect("an op signal name");
    assert!(!inst.is_empty() && !param.is_empty());
    (inst, param)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpKind {
    Const,
    Input,
    Signal,
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

/// Builtin functions (spec §5). Discriminants index [`BUILTINS`]; everything
/// from `V` up to `Op` is calculator-only (`Steps`, appended later, is scalar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    Abs,
    Sqrt,
    Exp,
    Ln,
    Log10,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Sinh,
    Cosh,
    Tanh,
    Floor,
    Ceil,
    Round,
    Int,
    Min,
    Max,
    Clamp,
    Sign,
    Pow,
    If,
    Snap,
    Eng,
    Str,
    Num,
    Len,
    Re,
    Im,
    Mag,
    Ph,
    Db,
    Conj,
    Cplx,
    SpectreSource,
    V,
    I,
    X,
    Deriv,
    Integ,
    Avg,
    Rms,
    Ymax,
    Ymin,
    Pp,
    At,
    Cross,
    Delay,
    Settle,
    Fft,
    GainMargin,
    PhaseMargin,
    Op,
    Steps,
}

const VARIADIC: u32 = u32::MAX;

/// `(name, builtin, min args, max args)`, in discriminant order.
pub(crate) const BUILTINS: [(&str, Builtin, u32, u32); 57] = [
    ("abs", Builtin::Abs, 1, 1),
    ("sqrt", Builtin::Sqrt, 1, 1),
    ("exp", Builtin::Exp, 1, 1),
    ("ln", Builtin::Ln, 1, 1),
    ("log10", Builtin::Log10, 1, 1),
    ("sin", Builtin::Sin, 1, 1),
    ("cos", Builtin::Cos, 1, 1),
    ("tan", Builtin::Tan, 1, 1),
    ("asin", Builtin::Asin, 1, 1),
    ("acos", Builtin::Acos, 1, 1),
    ("atan", Builtin::Atan, 1, 1),
    ("atan2", Builtin::Atan2, 2, 2),
    ("sinh", Builtin::Sinh, 1, 1),
    ("cosh", Builtin::Cosh, 1, 1),
    ("tanh", Builtin::Tanh, 1, 1),
    ("floor", Builtin::Floor, 1, 1),
    ("ceil", Builtin::Ceil, 1, 1),
    ("round", Builtin::Round, 1, 1),
    ("int", Builtin::Int, 1, 1),
    ("min", Builtin::Min, 2, VARIADIC),
    ("max", Builtin::Max, 2, VARIADIC),
    ("clamp", Builtin::Clamp, 3, 3),
    ("sign", Builtin::Sign, 1, 1),
    ("pow", Builtin::Pow, 2, 2),
    ("if", Builtin::If, 3, 3),
    ("snap", Builtin::Snap, 2, 2),
    ("eng", Builtin::Eng, 1, 2),
    ("str", Builtin::Str, 1, 1),
    ("num", Builtin::Num, 1, 1),
    ("len", Builtin::Len, 1, 1),
    ("re", Builtin::Re, 1, 1),
    ("im", Builtin::Im, 1, 1),
    ("mag", Builtin::Mag, 1, 1),
    ("ph", Builtin::Ph, 1, 1),
    ("db", Builtin::Db, 1, 1),
    ("conj", Builtin::Conj, 1, 1),
    ("cplx", Builtin::Cplx, 2, 2),
    ("spectre_source", Builtin::SpectreSource, 1, 1),
    ("v", Builtin::V, 1, 1),
    ("i", Builtin::I, 1, 1),
    ("x", Builtin::X, 0, 0),
    ("deriv", Builtin::Deriv, 1, 1),
    ("integ", Builtin::Integ, 1, 1),
    ("avg", Builtin::Avg, 1, 1),
    ("rms", Builtin::Rms, 1, 1),
    ("ymax", Builtin::Ymax, 1, 1),
    ("ymin", Builtin::Ymin, 1, 1),
    ("pp", Builtin::Pp, 1, 1),
    ("at", Builtin::At, 2, 2),
    ("cross", Builtin::Cross, 2, 4),
    ("delay", Builtin::Delay, 4, 4),
    ("settle", Builtin::Settle, 2, 2),
    ("fft", Builtin::Fft, 1, 1),
    ("gain_margin", Builtin::GainMargin, 1, 1),
    ("phase_margin", Builtin::PhaseMargin, 1, 1),
    ("op", Builtin::Op, 2, 2),
    ("steps", Builtin::Steps, 4, VARIADIC),
];

impl Builtin {
    pub(crate) fn from_id(id: u8) -> Builtin {
        let b = BUILTINS[id as usize].1;
        debug_assert_eq!(b as u8, id);
        b
    }

    pub(crate) fn name(self) -> &'static str {
        BUILTINS[self as usize].0
    }

    fn lookup(name: &str) -> Option<(Builtin, u32, u32)> {
        // `V(out)` / `I(v1)`: SPICE habit, the only case-insensitive names.
        let name = match name {
            "V" => "v",
            "I" => "i",
            n => n,
        };
        BUILTINS
            .iter()
            .find(|row| row.0 == name)
            .map(|&(_, b, lo, hi)| (b, lo, hi))
    }

    pub(crate) fn calc_only(self) -> bool {
        // `steps` was appended after the calculator block (discriminants are stable).
        self as u8 >= Builtin::V as u8 && self != Builtin::Steps
    }
}

/// `phys.*` constants (spec §5; SI 2019 exact values, CODATA 2022 for eps0).
fn phys(name: &str) -> Option<f64> {
    Some(match name {
        "k" => 1.380_649e-23,
        "q" => 1.602_176_634e-19,
        "h" => 6.626_070_15e-34,
        "c" => 299_792_458.0,
        "eps0" => 8.854_187_818_8e-12,
        "kelvin0" => 273.15,
        _ => return None,
    })
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
    pub(crate) pool: String,
    pub(crate) idents: Vec<Name>,
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

    /// Signals referenced by `v(…)`/`i(…)`, in the slot order
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
    /// its first occurrence.
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

    pub(crate) fn name(&self, n: Name) -> &str {
        &self.pool[n.start as usize..(n.start + n.len) as usize]
    }

    pub(crate) fn plugin_name(&self, slot: u32) -> String {
        let (ns, f) = self.plugins[slot as usize];
        format!("{}.{}", self.name(ns), self.name(f))
    }
}

/// Work item of the iterative code generator: `phase` counts children done.
#[derive(Clone, Copy)]
struct Frame {
    node: u32,
    phase: u32,
    /// Code index where this node's code starts (for folding).
    start: u32,
    /// Pending jump to patch (lazy operators).
    aux: u32,
    /// Under a lazy arm: folding errors are deferred to runtime.
    lazy: bool,
}

pub(crate) struct Compiler {
    prog: Program,
    idents: BTreeMap<String, u32>,
    signals: BTreeMap<(SignalKind, String), u32>,
    plugins: BTreeMap<(String, String), u32>,
    work: Vec<Frame>,
    /// Per finished node on the work path: is it a constant?
    done: Vec<bool>,
    scratch: Scratch,
}

impl Compiler {
    pub(crate) fn new(mode: Mode) -> Compiler {
        Compiler {
            prog: Program {
                ops: Vec::new(),
                spans: Vec::new(),
                consts: Vec::new(),
                pool: String::new(),
                idents: Vec::new(),
                ident_spans: Vec::new(),
                signals: Vec::new(),
                signal_spans: Vec::new(),
                plugins: Vec::new(),
                roots: Vec::new(),
                mode,
            },
            idents: BTreeMap::new(),
            signals: BTreeMap::new(),
            plugins: BTreeMap::new(),
            work: Vec::new(),
            done: Vec::new(),
            scratch: Scratch::default(),
        }
    }

    /// Emits a load of signal `(kind, name)`, adding it on first use.
    fn signal(&mut self, kind: SignalKind, name: &str, span: Span) {
        let next = self.prog.signals.len() as u32;
        let slot = *self.signals.entry((kind, name.to_string())).or_insert(next);
        if slot == next {
            let nm = self.intern(name);
            self.prog.signals.push((kind, nm));
            self.prog.signal_spans.push(span);
        }
        assert!((slot as usize) < self.prog.signals.len());
        self.emit(OpKind::Signal, 0, slot, span);
        self.done.push(false);
    }

    pub(crate) fn finish(self) -> Program {
        let p = self.prog;
        assert_eq!(p.ops.len(), p.spans.len());
        assert_eq!(p.idents.len(), p.ident_spans.len());
        assert_eq!(p.signals.len(), p.signal_spans.len());
        p
    }

    pub(crate) fn roots_len(&self) -> u32 {
        self.prog.roots.len() as u32
    }

    /// Parses and compiles `src[start..end]` as one more root.
    pub(crate) fn add_root(&mut self, src: &str, start: usize, end: usize) -> Result<(), Error> {
        let ast = parse(src, start, end)?;
        let code_start = self.prog.ops.len() as u32;
        self.work.clear();
        self.done.clear();
        self.work.push(Frame {
            node: ast.root,
            phase: 0,
            start: code_start,
            aux: 0,
            lazy: false,
        });
        while let Some(f) = self.work.pop() {
            self.step(&ast, src, f)?;
        }
        assert_eq!(self.done.len(), 1, "root did not produce exactly one value");
        self.prog
            .roots
            .push((code_start, self.prog.ops.len() as u32));
        Ok(())
    }

    fn emit(&mut self, kind: OpKind, sub: u8, arg: u32, span: Span) -> u32 {
        self.prog.ops.push(Op { kind, sub, arg });
        self.prog.spans.push(span);
        self.prog.ops.len() as u32 - 1
    }

    fn here(&self) -> u32 {
        self.prog.ops.len() as u32
    }

    fn intern(&mut self, s: &str) -> Name {
        intern(&mut self.prog.pool, s)
    }

    fn leaf_const(&mut self, v: V, span: Span) {
        let k = self.prog.consts.len() as u32;
        self.prog.consts.push(v);
        self.emit(OpKind::Const, 0, k, span);
        self.done.push(true);
    }

    fn input(&mut self, name: &str, span: Span) {
        let next = self.prog.idents.len() as u32;
        let slot = *self.idents.entry(name.to_string()).or_insert(next);
        if slot == next {
            let n = self.intern(name);
            self.prog.idents.push(n);
            self.prog.ident_spans.push(span);
        }
        self.emit(OpKind::Input, 0, slot, span);
        self.done.push(false);
    }

    fn child(&mut self, parent: Frame, next_phase: u32, child: u32, lazy: bool) {
        self.work.push(Frame {
            phase: next_phase,
            ..parent
        });
        let start = self.here();
        self.work.push(Frame {
            node: child,
            phase: 0,
            start,
            aux: 0,
            lazy: parent.lazy || lazy,
        });
    }

    fn step(&mut self, ast: &Ast, src: &str, mut f: Frame) -> Result<(), Error> {
        let n = f.node as usize;
        let span = ast.span[n];
        let text = &src[span.start as usize..span.end as usize];
        let (a, b) = (ast.a[n], ast.b[n]);
        if f.phase == 0 {
            f.start = self.here();
        }
        match ast.kind[n] {
            Node::Int => self.leaf_const(V::Int(ast.ints[a as usize]), span),
            Node::Num => self.leaf_const(V::Num(ast.nums[a as usize]), span),
            Node::True => self.leaf_const(V::Bool(true), span),
            Node::False => self.leaf_const(V::Bool(false), span),
            Node::Pi => self.leaf_const(V::Num(core::f64::consts::PI), span),
            Node::Str => {
                let s = &ast.pool[a as usize..(a + b) as usize];
                let name = self.intern(s);
                self.leaf_const(V::Str(name.start, name.len), span);
            }
            Node::Ident => self.input(text, span),
            Node::NsIdent => {
                let (ns, name) = text.split_once('.').expect("lexer guarantees a dot");
                if ns != "phys" {
                    self.input(text, span);
                } else {
                    let v = phys(name).ok_or_else(|| {
                        Error::new(
                            Code::X004,
                            span,
                            format!("unknown constant `{text}` (phys.k q h c eps0 kelvin0)"),
                        )
                    })?;
                    self.leaf_const(V::Num(v), span);
                }
            }
            Node::Neg | Node::Not => match f.phase {
                0 => self.child(f, 1, a, false),
                _ => {
                    let kind = if ast.kind[n] == Node::Neg {
                        OpKind::Neg
                    } else {
                        OpKind::Not
                    };
                    self.emit(kind, 0, 0, span);
                    self.finish_node(f, 1, true)?;
                }
            },
            Node::Bin(op @ (BinOp::And | BinOp::Or)) => {
                let (open, close) = if op == BinOp::And {
                    (OpKind::And, OpKind::EndAnd)
                } else {
                    (OpKind::Or, OpKind::EndOr)
                };
                match f.phase {
                    0 => self.child(f, 1, a, false),
                    1 => {
                        f.aux = self.emit(open, 0, 0, span);
                        self.child(f, 2, b, true);
                    }
                    _ => {
                        self.emit(close, 0, 0, span);
                        self.prog.ops[f.aux as usize].arg = self.here();
                        self.finish_node(f, 2, true)?;
                    }
                }
            }
            Node::Bin(op) => match f.phase {
                0 => self.child(f, 1, a, false),
                1 => self.child(f, 2, b, false),
                _ => {
                    self.emit(bin_op(op), 0, 0, span);
                    self.finish_node(f, 2, true)?;
                }
            },
            Node::Ternary => {
                let x = b as usize;
                self.ternary(f, [a, ast.extra[x], ast.extra[x + 1]], span)?;
            }
            Node::Call => self.call(ast, src, f, span)?,
            Node::NsCall => self.plugin_call(ast, src, f, span)?,
        }
        Ok(())
    }

    /// `c ? t : e` and `if(c, t, e)`: both arms are lazy.
    fn ternary(&mut self, mut f: Frame, [c, t, e]: [u32; 3], span: Span) -> Result<(), Error> {
        match f.phase {
            0 => self.child(f, 1, c, false),
            1 => {
                f.aux = self.emit(OpKind::Cond, 0, 0, span);
                self.child(f, 2, t, true);
            }
            2 => {
                let else_op = self.emit(OpKind::Else, 0, 0, span);
                self.prog.ops[f.aux as usize].arg = self.here();
                f.aux = else_op;
                self.child(f, 3, e, true);
            }
            _ => {
                let end = self.emit(OpKind::EndCond, 0, 0, span);
                self.prog.ops[f.aux as usize].arg = end;
                self.finish_node(f, 3, true)?;
            }
        }
        Ok(())
    }

    fn call(&mut self, ast: &Ast, src: &str, f: Frame, span: Span) -> Result<(), Error> {
        let n = f.node as usize;
        let x = ast.a[n] as usize;
        let argc = ast.b[n];
        let arg = |k: u32| ast.extra[x + 1 + k as usize];
        let name = &src[span.start as usize..ast.extra[x] as usize];
        // Looked up again each phase: `aux` belongs to `if`'s jump patching.
        let (builtin, lo, hi) = Builtin::lookup(name)
            .ok_or_else(|| Error::new(Code::X006, span, format!("unknown function `{name}`")))?;
        if f.phase == 0 {
            if argc < lo || argc > hi {
                let want = match (lo, hi) {
                    (lo, hi) if lo == hi => format!("{lo}"),
                    (lo, VARIADIC) => format!("at least {lo}"),
                    (lo, hi) => format!("{lo} to {hi}"),
                };
                return Err(Error::new(
                    Code::X008,
                    span,
                    format!("`{name}` takes {want} arguments, got {argc}"),
                ));
            }
            if builtin.calc_only() && self.prog.mode == Mode::Scalar {
                return Err(Error::new(
                    Code::X006,
                    span,
                    format!("`{name}` is only available in calculator mode"),
                ));
            }
        }
        match builtin {
            Builtin::If => return self.ternary(f, [arg(0), arg(1), arg(2)], span),
            Builtin::V | Builtin::I => {
                let a = arg(0) as usize;
                let a_span = ast.span[a];
                let net = match ast.kind[a] {
                    Node::Str => &ast.pool[ast.a[a] as usize..(ast.a[a] + ast.b[a]) as usize],
                    Node::Ident | Node::NsIdent => &src[a_span.start as usize..a_span.end as usize],
                    _ => {
                        return Err(Error::new(
                            Code::X003,
                            a_span,
                            format!("`{name}` takes a signal name (identifier or string)"),
                        ));
                    }
                };
                let kind = if builtin == Builtin::V {
                    SignalKind::V
                } else {
                    SignalKind::I
                };
                self.signal(kind, net, span);
            }
            Builtin::Op => {
                // Both arguments are names taken verbatim, like `v`/`i`'s.
                let text = |k: u32| {
                    let a = arg(k) as usize;
                    match ast.kind[a] {
                        Node::Str => {
                            Some(&ast.pool[ast.a[a] as usize..(ast.a[a] + ast.b[a]) as usize])
                        }
                        Node::Ident => {
                            Some(&src[ast.span[a].start as usize..ast.span[a].end as usize])
                        }
                        _ => None,
                    }
                };
                let inst = text(0).filter(|i| {
                    !i.is_empty() && !i.contains(|c: char| c.is_whitespace() || c == ']')
                });
                let param = text(1).filter(|p| {
                    !p.is_empty() && p.bytes().all(|c| c == b'_' || c.is_ascii_alphanumeric())
                });
                let (Some(inst), Some(param)) = (inst, param) else {
                    return Err(Error::new(
                        Code::X003,
                        span,
                        "`op` takes an instance path and a parameter name (`op(\"x1/m2\", \"gm\")`)",
                    ));
                };
                self.signal(SignalKind::Op, &format!("{inst}[{param}]"), span);
            }
            Builtin::X => {
                self.emit(OpKind::Axis, 0, 0, span);
                self.done.push(false);
            }
            _ if f.phase < argc => self.child(f, f.phase + 1, arg(f.phase), false),
            _ => {
                self.emit(OpKind::Call, builtin as u8, argc, span);
                self.finish_node(f, argc as usize, !builtin.calc_only())?;
            }
        }
        Ok(())
    }

    fn plugin_call(&mut self, ast: &Ast, src: &str, f: Frame, span: Span) -> Result<(), Error> {
        let n = f.node as usize;
        let x = ast.a[n] as usize;
        let argc = ast.b[n];
        let full = &src[span.start as usize..ast.extra[x] as usize];
        let (ns, name) = full.split_once('.').expect("lexer guarantees a dot");
        if f.phase == 0 {
            if ns == "phys" {
                return Err(Error::new(
                    Code::X006,
                    span,
                    format!("`{full}` is a constant, not a function"),
                ));
            }
            if argc as usize > MAX_PLUGIN_ARGS {
                return Err(Error::new(
                    Code::X008,
                    span,
                    format!("plugin calls take at most {MAX_PLUGIN_ARGS} arguments, got {argc}"),
                ));
            }
        }
        if f.phase < argc {
            self.child(f, f.phase + 1, ast.extra[x + 1 + f.phase as usize], false);
            return Ok(());
        }
        let next = self.prog.plugins.len() as u32;
        let slot = *self
            .plugins
            .entry((ns.to_string(), name.to_string()))
            .or_insert(next);
        if slot == next {
            let (a, b) = (self.intern(ns), self.intern(name));
            self.prog.plugins.push((a, b));
        }
        self.emit(OpKind::Plugin, argc as u8, slot, span);
        self.finish_node(f, argc as usize, false)
    }

    /// Pops the children's constness and folds the node if all are constant.
    fn finish_node(&mut self, f: Frame, nchild: usize, foldable: bool) -> Result<(), Error> {
        assert!(self.done.len() >= nchild, "child results missing");
        let base = self.done.len() - nchild;
        let all_const = self.done[base..].iter().all(|&c| c);
        self.done.truncate(base);
        if !(all_const && foldable) {
            self.done.push(false);
            return Ok(());
        }
        let end = self.here();
        let span = self.prog.spans[end as usize - 1];
        let folded = eval::run(
            &self.prog,
            f.start,
            end,
            &[],
            &mut eval::no_plugins(&self.prog),
            &mut self.scratch,
        );
        match folded {
            Ok(v) => {
                self.prog.ops.truncate(f.start as usize);
                self.prog.spans.truncate(f.start as usize);
                let v = match v {
                    V::Str(s, len) => {
                        let text = core::str::from_utf8(
                            &self.scratch.strs[s as usize..(s + len) as usize],
                        )
                        .expect("scratch strings are UTF-8");
                        let name = intern(&mut self.prog.pool, text);
                        V::Str(name.start, name.len)
                    }
                    v => v,
                };
                self.leaf_const(v, span);
                debug_assert_eq!(self.here(), f.start + 1);
                Ok(())
            }
            Err(e) if !f.lazy => Err(e),
            Err(_) => {
                self.done.push(false);
                Ok(())
            }
        }
    }
}

fn intern(pool: &mut String, s: &str) -> Name {
    let start = pool.len() as u32;
    pool.push_str(s);
    Name {
        start,
        len: s.len() as u32,
    }
}

fn bin_op(op: BinOp) -> OpKind {
    match op {
        BinOp::Eq => OpKind::Eq,
        BinOp::Ne => OpKind::Ne,
        BinOp::Lt => OpKind::Lt,
        BinOp::Le => OpKind::Le,
        BinOp::Gt => OpKind::Gt,
        BinOp::Ge => OpKind::Ge,
        BinOp::Add => OpKind::Add,
        BinOp::Sub => OpKind::Sub,
        BinOp::Mul => OpKind::Mul,
        BinOp::Div => OpKind::Div,
        BinOp::Rem => OpKind::Rem,
        BinOp::Pow => OpKind::Pow,
        BinOp::And | BinOp::Or => unreachable!("lazy operators compile to jumps"),
    }
}
