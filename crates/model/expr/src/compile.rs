//! Compile (spec §3–§5): a parsed [`Ast`] → [`Program`] bytecode.
//!
//! Done once per expression, so evaluation does no name lookup, no arity
//! check and no work on constant subexpressions:
//! - identifiers become input slots (first-occurrence order), `v()`/`i()`/`op()`
//!   become signal slots, `ns.f()` become plugin slots;
//! - builtin names, arity and mode are checked (`X006`, `X008`);
//! - every constant subtree is folded by running the scalar evaluator on it.
//!   A folding error in a subtree that is always evaluated is reported here
//!   (spec §4 static checking); under a lazy branch it is left for runtime, so
//!   `false && 1/0` stays legal.
//!
//! Code generation is an explicit work stack, so a 65 536-node chain never
//! recurses.
//!
//! Contents:
//! - `pub(crate) struct Compiler`
//! - `Compiler::new(mode: Mode) -> Compiler`,
//!   `Compiler::add_root(&mut self, src: &str, start: usize, end: usize) -> Result<(), Error>`,
//!   `Compiler::roots_len(&self) -> u32`, `Compiler::finish(self) -> Program`

use std::collections::BTreeMap;

use crate::builtin::{Builtin, VARIADIC, phys};
use crate::eval::{self, Scratch, V};
use crate::parse::{Ast, BinOp, Node, parse};
use crate::program::{Mode, Name, Op, OpKind, Program, SignalKind};
use crate::{Code, Error, MAX_PLUGIN_ARGS, Span};

/// Builds one [`Program`] from one or more roots (a template adds one per
/// hole). Lives for one compile call.
pub(crate) struct Compiler {
    prog: Program,
    /// Name → slot, so each identifier, signal and plugin gets one slot.
    idents: BTreeMap<String, u32>,
    signals: BTreeMap<(SignalKind, String), u32>,
    plugins: BTreeMap<(String, String), u32>,
    work: Vec<Frame>,
    /// Per finished node on the work path: is it a constant?
    done: Vec<bool>,
    /// The folding evaluator's memory.
    scratch: Scratch,
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

impl Compiler {
    /// An empty compiler for `mode`.
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

    /// Parses and compiles `src[start..end]` as one more root. Spans stay
    /// absolute offsets into `src`.
    ///
    /// # Errors
    /// As [`Program::compile`]; the compiler is then unusable.
    pub(crate) fn add_root(&mut self, src: &str, start: usize, end: usize) -> Result<(), Error> {
        let ast = parse(src, start, end)?;
        let code_start = self.here();
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
        assert!(self.here() > code_start, "a root emits code");
        self.prog.roots.push((code_start, self.here()));
        Ok(())
    }

    /// Roots compiled so far: the index the next [`Compiler::add_root`] gets.
    pub(crate) fn roots_len(&self) -> u32 {
        self.prog.roots.len() as u32
    }

    /// The finished program.
    pub(crate) fn finish(self) -> Program {
        let p = self.prog;
        assert_eq!(p.ops.len(), p.spans.len());
        assert_eq!(p.idents.len(), p.ident_spans.len());
        assert_eq!(p.signals.len(), p.signal_spans.len());
        p
    }
}

// ---- private ----

impl Compiler {
    fn step(&mut self, ast: &Ast, src: &str, mut f: Frame) -> Result<(), Error> {
        let n = f.node as usize;
        let span = ast.span[n];
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
                let name = intern(&mut self.prog.pool, ast.str_lit(n));
                self.leaf_const(V::Str(name.start, name.len), span);
            }
            Node::Ident => self.input(ast.text(src, n), span),
            Node::NsIdent => {
                let text = ast.text(src, n);
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
                    let neg = ast.kind[n] == Node::Neg;
                    self.emit(if neg { OpKind::Neg } else { OpKind::Not }, 0, 0, span);
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

    fn here(&self) -> u32 {
        self.prog.ops.len() as u32
    }

    fn emit(&mut self, kind: OpKind, sub: u8, arg: u32, span: Span) -> u32 {
        self.prog.ops.push(Op { kind, sub, arg });
        self.prog.spans.push(span);
        debug_assert_eq!(self.prog.ops.len(), self.prog.spans.len());
        self.here() - 1
    }

    fn leaf_const(&mut self, v: V, span: Span) {
        let k = self.prog.consts.len() as u32;
        self.prog.consts.push(v);
        self.emit(OpKind::Const, 0, k, span);
        self.done.push(true);
    }

    /// Emits a load of identifier `name`, adding its slot on first use.
    fn input(&mut self, name: &str, span: Span) {
        let next = self.prog.idents.len() as u32;
        let slot = *self.idents.entry(name.to_string()).or_insert(next);
        if slot == next {
            let n = intern(&mut self.prog.pool, name);
            self.prog.idents.push(n);
            self.prog.ident_spans.push(span);
        }
        assert!((slot as usize) < self.prog.idents.len());
        self.emit(OpKind::Input, 0, slot, span);
        self.done.push(false);
    }

    /// Emits a load of signal `(kind, name)`, adding its slot on first use.
    fn signal(&mut self, kind: SignalKind, name: &str, span: Span) {
        let next = self.prog.signals.len() as u32;
        let slot = *self.signals.entry((kind, name.to_string())).or_insert(next);
        if slot == next {
            let nm = intern(&mut self.prog.pool, name);
            self.prog.signals.push((kind, nm));
            self.prog.signal_spans.push(span);
        }
        assert!((slot as usize) < self.prog.signals.len());
        self.emit(OpKind::Signal, 0, slot, span);
        self.done.push(false);
    }

    /// Re-queues `parent` at `next_phase`, then compiles `child` first.
    fn child(&mut self, parent: Frame, next_phase: u32, child: u32, lazy: bool) {
        debug_assert!(next_phase > parent.phase);
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
        let arg = |k: u32| ast.extra[x + 1 + k as usize] as usize;
        let name = &src[span.start as usize..ast.extra[x] as usize];
        // Looked up again each phase: `aux` belongs to `if`'s jump patching.
        let (builtin, lo, hi) = Builtin::lookup(name)
            .ok_or_else(|| Error::new(Code::X006, span, format!("unknown function `{name}`")))?;
        if f.phase == 0 {
            check_call(self.prog.mode, builtin, name, argc, (lo, hi), span)?;
        }
        match builtin {
            Builtin::If => {
                let args = [arg(0), arg(1), arg(2)].map(|a| a as u32);
                return self.ternary(f, args, span);
            }
            Builtin::V | Builtin::I => {
                let a = arg(0);
                let net = match ast.kind[a] {
                    Node::Str => ast.str_lit(a),
                    Node::Ident | Node::NsIdent => ast.text(src, a),
                    _ => {
                        return Err(Error::new(
                            Code::X003,
                            ast.span[a],
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
                let text = |a: usize| match ast.kind[a] {
                    Node::Str => Some(ast.str_lit(a)),
                    Node::Ident => Some(ast.text(src, a)),
                    _ => None,
                };
                let inst = text(arg(0)).filter(|i| {
                    !i.is_empty() && !i.contains(|c: char| c.is_whitespace() || c == ']')
                });
                let param = text(arg(1)).filter(|p| {
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
            _ if f.phase < argc => self.child(f, f.phase + 1, arg(f.phase) as u32, false),
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
            let (a, b) = (
                intern(&mut self.prog.pool, ns),
                intern(&mut self.prog.pool, name),
            );
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
                        let text = eval::str_at(&self.scratch.strs, s, len);
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

/// Arity (`X008`) and mode (`X006`) of a builtin call with `argc` arguments.
fn check_call(
    mode: Mode,
    builtin: Builtin,
    name: &str,
    argc: u32,
    (lo, hi): (u32, u32),
    span: Span,
) -> Result<(), Error> {
    debug_assert!(lo <= hi);
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
    if builtin.calc_only() && mode == Mode::Scalar {
        return Err(Error::new(
            Code::X006,
            span,
            format!("`{name}` is only available in calculator mode"),
        ));
    }
    Ok(())
}

fn intern(pool: &mut String, s: &str) -> Name {
    let start = pool.len() as u32;
    pool.push_str(s);
    debug_assert!(pool.len() <= u32::MAX as usize, "program pool past 4 GiB");
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
