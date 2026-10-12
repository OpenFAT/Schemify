//! Schemify expressions ⇄ Virtuoso CDF/AEL value strings (spec `interop.md` §3.4), plus the
//! other value-shaped tables of the CDF edge: string-parameter literals, booleans and the
//! CDF `units` names (§6.3).
//!
//! Both directions are text rewrites that keep the source's spacing and parentheses, so a
//! value that needs no rewriting comes back byte for byte. Only `x^y` is restructured
//! (`pow(x,y)`), which is why the Schemify side is parsed into a span tree; the AEL side is
//! rewritten token by token.

use schemify_expr::{Mode, Program, Scratch, Value, write_num};

// Data design: inputs are single CDF values (bytes to a few KiB), one at a time; the token
// and node tables live for one call. Widths: spans index the value (`usize`, the caller's
// string); the node table is bounded by `NODES_MAX`.

/// Largest value this module rewrites, in bytes; longer values are refused (`expression`).
pub const VALUE_MAX: usize = 1 << 16;

/// Nodes per parsed expression (the expression language's own bound, expr.md §3).
const NODES_MAX: usize = 65_536;

/// Nesting depth per expression (expr.md §3).
const DEPTH_MAX: u32 = 256;

/// Inlined-variable chains followed before giving up (a cycle is an error in Schemify too).
const INLINE_DEPTH_MAX: u32 = 16;

/// What an identifier of a Schemify expression is, seen from the CDF value being written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Name {
    /// A parameter of the cell that contains the instance: `pPar("x")`.
    ParentParam,
    /// A parameter of the cell whose own CDF default is being written: `iPar("x")`.
    OwnParam,
    /// A bench variable: the bare name (an ADE design variable).
    BenchVar,
    /// A variable of the containing component, with its expression (Schemify syntax, in
    /// the same scope): inlined in parentheses, `needs_review: inlined-var`.
    CellVar(String),
    /// Nothing in scope: written bare (a design variable), `needs_review: design-variable`.
    Unknown,
}

/// A translated value and the review reason it carries, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translated {
    /// The value in the target syntax.
    pub text: String,
    /// `needs_review` reason (§12 class names: `inlined-var`, `design-variable`, `expression`).
    pub review: Option<&'static str>,
}

// ---------------------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------------------

/// Schemify number suffixes (expr.md §2, any case) → Spectre scale factors. `mil` is not
/// here: it folds into the number. Longest first, so `meg`/`mil` win over `m`.
const SUFFIX_TO_AEL: [(&str, &str); 11] = [
    ("meg", "M"),
    ("t", "T"),
    ("f", "f"),
    ("p", "p"),
    ("n", "n"),
    ("u", "u"),
    ("µ", "u"),
    ("μ", "u"),
    ("m", "m"),
    ("k", "k"),
    ("g", "G"),
];

/// Spectre scale factors (case-sensitive; research netlist-ade §1.5) → Schemify suffix,
/// or `None` for the factors Schemify lacks (`a c % _ K`-style ones fold into the number).
const SUFFIX_FROM_AEL: [(char, Option<&str>, f64); 14] = [
    ('T', Some("t"), 1e12),
    ('G', Some("g"), 1e9),
    ('M', Some("meg"), 1e6),
    ('K', Some("k"), 1e3),
    ('k', Some("k"), 1e3),
    ('_', None, 1.0),
    ('%', None, 1e-2),
    ('c', None, 1e-2),
    ('m', Some("m"), 1e-3),
    ('u', Some("u"), 1e-6),
    ('n', Some("n"), 1e-9),
    ('p', Some("p"), 1e-12),
    ('f', Some("f"), 1e-15),
    ('a', None, 1e-18),
];

/// Length of the numeric part (`digits[.digits][e[+-]digits]` or `.digits…`) at the start
/// of `s`, and whether it has an exponent. `None` when `s` does not start a number.
fn mantissa(s: &str) -> Option<(usize, bool)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let f = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if int_digits == 0 && i == f {
            return None;
        }
    } else if int_digits == 0 {
        return None;
    }
    let mut exp = false;
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let d = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > d {
            i = j;
            exp = true;
        }
    }
    Some((i, exp))
}

/// Writes `x` as canonical number text (expr.md §7): valid in both syntaxes.
fn canonical(x: f64) -> String {
    let mut s = String::new();
    write_num(&mut s, x);
    s
}

/// Rewrites one Schemify number literal (`1meg`, `2M`, `2.5mil`) as a Spectre number
/// (`1M`, `2m`, folded `63.5u`-equivalent canonical text). `None` for text that is not a
/// number literal.
pub fn number_to_ael(lit: &str) -> Option<String> {
    let (n, exp) = mantissa(lit)?;
    let (m, suffix) = lit.split_at(n);
    if suffix.is_empty() {
        return Some(m.to_owned());
    }
    let lower = suffix.to_lowercase();
    let value = |scale: f64| -> Option<String> {
        let v: f64 = m.parse().ok()?;
        let x = v * scale;
        x.is_finite().then(|| canonical(x))
    };
    if lower == "mil" {
        return value(25.4e-6);
    }
    let (_, to) = SUFFIX_TO_AEL.iter().find(|(s, _)| *s == lower)?;
    if exp {
        // Spectre ignores a scale factor after an exponent: fold instead.
        let scale = match *to {
            "T" => 1e12,
            "M" => 1e6,
            "G" => 1e9,
            "k" => 1e3,
            "m" => 1e-3,
            "u" => 1e-6,
            "n" => 1e-9,
            "p" => 1e-12,
            _ => 1e-15,
        };
        return value(scale);
    }
    Some(format!("{m}{to}"))
}

/// Rewrites one Spectre number (`1M`, `10e3`, `1pF`, `5a`) as a Schemify literal (`1meg`,
/// `10e3`, `1p`, `5e-18`). Unit letters after a scale factor are dropped (Spectre ignores
/// them). Returns the literal and the bytes consumed; `None` when `s` starts no number.
fn number_from_ael(s: &str) -> Option<(String, usize)> {
    let (n, exp) = mantissa(s)?;
    let m = &s[..n];
    // `5.` is legal in Spectre, not in Schemify.
    let m_fixed = if m.ends_with('.') {
        format!("{m}0")
    } else {
        m.to_owned()
    };
    let rest = &s[n..];
    let Some(c) = rest.chars().next() else {
        return Some((m_fixed, n));
    };
    let Some(&(_, to, scale)) = SUFFIX_FROM_AEL.iter().find(|(f, _, _)| *f == c) else {
        return Some((m_fixed, n));
    };
    let mut used = n + c.len_utf8();
    // Unit letters after the factor (`1pF`, `2uH`).
    while let Some(u) = s[used..].chars().next() {
        if u.is_ascii_alphabetic() {
            used += 1;
        } else {
            break;
        }
    }
    let text = match to {
        Some(t) if !exp => format!("{m_fixed}{t}"),
        _ => {
            let v: f64 = m_fixed.parse().ok()?;
            let x = if exp { v } else { v * scale };
            if !x.is_finite() {
                return None;
            }
            canonical(x)
        }
    };
    Some((text, used))
}

// ---------------------------------------------------------------------------------------
// Lexer (both syntaxes)
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tk {
    Num,
    Ident,
    /// `ns.name` (Schemify only).
    NsIdent,
    Str,
    Op,
    LParen,
    RParen,
    Comma,
}

#[derive(Clone, Copy, Debug)]
struct Tok {
    kind: Tk,
    start: usize,
    end: usize,
}

const OPS2: [&str; 7] = ["==", "!=", "<=", ">=", "&&", "||", "**"];
const OPS1: &str = "+-*/%^<>!?:";

/// Tokenizes `s`. `ael` selects Spectre numbers (unit letters after a factor) and `**`.
fn lex(s: &str, ael: bool) -> Option<Vec<Tok>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let kind = if c.is_ascii_digit()
            || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            if ael {
                let (_, used) = number_from_ael(&s[i..])?;
                i += used;
            } else {
                let (n, _) = mantissa(&s[i..])?;
                i += n;
                // Suffix letters (validated by number_to_ael later).
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80)
                {
                    i += 1;
                }
                // `µ` and `μ` are multi-byte; the loop above took them as >= 0x80 bytes.
                while i < b.len() && !s.is_char_boundary(i) {
                    i += 1;
                }
            }
            Tk::Num
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            if !ael
                && b.get(i) == Some(&b'.')
                && b.get(i + 1)
                    .is_some_and(|x| x.is_ascii_alphabetic() || *x == b'_')
            {
                i += 1;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                Tk::NsIdent
            } else {
                Tk::Ident
            }
        } else if c == b'"' {
            i += 1;
            loop {
                match b.get(i) {
                    None => return None,
                    Some(b'\\') => i += 2,
                    Some(b'"') => {
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            Tk::Str
        } else if c == b'(' {
            i += 1;
            Tk::LParen
        } else if c == b')' {
            i += 1;
            Tk::RParen
        } else if c == b',' {
            i += 1;
            Tk::Comma
        } else if i + 1 < b.len() && OPS2.contains(&&s[i..i + 2]) && (ael || &s[i..i + 2] != "**") {
            i += 2;
            Tk::Op
        } else if OPS1.as_bytes().contains(&c) {
            i += 1;
            Tk::Op
        } else {
            return None;
        };
        out.push(Tok {
            kind,
            start,
            end: i.min(b.len()),
        });
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------
// Schemify → AEL
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Nk {
    /// A token: number, identifier, string.
    Leaf(usize),
    /// Prefix operator (its token is in the node's span), operand.
    Un(usize),
    /// Left, operator token, right.
    Bin(usize, usize, usize),
    /// Condition, then, else.
    Tern(usize, usize, usize),
    /// Function-name token, arguments.
    Call(usize, Vec<usize>),
    /// Parenthesised expression.
    Group(usize),
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Tok>,
    i: usize,
    nodes: Vec<(Nk, usize, usize)>,
    depth: u32,
    /// Saw something AEL lacks (strings, `?:`, comparisons, logic, `%`, unknown calls).
    foreign: bool,
}

/// Functions with the same meaning in both languages; `ln` is AEL's `log`.
const SHARED_FNS: [&str; 22] = [
    "log10", "exp", "sqrt", "abs", "min", "max", "sin", "cos", "tan", "asin", "acos", "atan",
    "atan2", "sinh", "cosh", "tanh", "floor", "ceil", "int", "pow", "ln", "log",
];

impl Parser<'_> {
    fn text(&self, t: usize) -> &str {
        &self.src[self.toks[t].start..self.toks[t].end]
    }

    fn peek_op(&self, op: &str) -> bool {
        self.toks
            .get(self.i)
            .is_some_and(|t| t.kind == Tk::Op && &self.src[t.start..t.end] == op)
    }

    fn push(&mut self, k: Nk, start: usize, end: usize) -> Option<usize> {
        if self.nodes.len() >= NODES_MAX {
            return None;
        }
        self.nodes.push((k, start, end));
        Some(self.nodes.len() - 1)
    }

    fn span(&self, n: usize) -> (usize, usize) {
        (self.nodes[n].1, self.nodes[n].2)
    }

    fn expr(&mut self) -> Option<usize> {
        self.depth += 1;
        if self.depth > DEPTH_MAX {
            return None;
        }
        let c = self.binary(0)?;
        let r = if self.peek_op("?") {
            self.foreign = true;
            self.i += 1;
            let a = self.expr()?;
            if !self.peek_op(":") {
                return None;
            }
            self.i += 1;
            let b = self.expr()?;
            let (s, _) = self.span(c);
            let (_, e) = self.span(b);
            self.push(Nk::Tern(c, a, b), s, e)?
        } else {
            c
        };
        self.depth -= 1;
        Some(r)
    }

    /// Precedence climbing over the binary levels `|| && cmp +- */%`.
    fn binary(&mut self, level: u8) -> Option<usize> {
        const LEVELS: [&[&str]; 5] = [
            &["||"],
            &["&&"],
            &["==", "!=", "<", "<=", ">", ">="],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level as usize == LEVELS.len() {
            return self.unary();
        }
        let mut l = self.binary(level + 1)?;
        while let Some(t) = self.toks.get(self.i).copied() {
            let op = &self.src[t.start..t.end];
            if t.kind != Tk::Op || !LEVELS[level as usize].contains(&op) {
                break;
            }
            if level <= 2 || op == "%" {
                self.foreign = true;
            }
            let opi = self.i;
            self.i += 1;
            let r = self.binary(level + 1)?;
            let (s, _) = self.span(l);
            let (_, e) = self.span(r);
            l = self.push(Nk::Bin(l, opi, r), s, e)?;
            if level == 2 {
                break; // comparisons are non-associative
            }
        }
        Some(l)
    }

    fn unary(&mut self) -> Option<usize> {
        if self.peek_op("-") || self.peek_op("!") {
            if self.peek_op("!") {
                self.foreign = true;
            }
            let opi = self.i;
            self.i += 1;
            self.depth += 1;
            if self.depth > DEPTH_MAX {
                return None;
            }
            let x = self.unary()?;
            self.depth -= 1;
            let s = self.toks[opi].start;
            let (_, e) = self.span(x);
            return self.push(Nk::Un(x), s, e);
        }
        self.pow()
    }

    fn pow(&mut self) -> Option<usize> {
        let l = self.call()?;
        if self.peek_op("^") {
            let opi = self.i;
            self.i += 1;
            self.depth += 1;
            if self.depth > DEPTH_MAX {
                return None;
            }
            let r = self.unary()?;
            self.depth -= 1;
            let (s, _) = self.span(l);
            let (_, e) = self.span(r);
            return self.push(Nk::Bin(l, opi, r), s, e);
        }
        Some(l)
    }

    fn call(&mut self) -> Option<usize> {
        let t = *self.toks.get(self.i)?;
        match t.kind {
            Tk::Ident | Tk::NsIdent
                if self
                    .toks
                    .get(self.i + 1)
                    .is_some_and(|n| n.kind == Tk::LParen) =>
            {
                let name = self.i;
                if t.kind == Tk::NsIdent
                    || !SHARED_FNS.contains(&self.text(name))
                    || self.text(name) == "log"
                {
                    self.foreign = true;
                }
                self.i += 2;
                let mut args = Vec::new();
                if self.toks.get(self.i).is_some_and(|x| x.kind == Tk::RParen) {
                    self.i += 1;
                } else {
                    loop {
                        args.push(self.expr()?);
                        match self.toks.get(self.i).map(|x| x.kind) {
                            Some(Tk::Comma) => self.i += 1,
                            Some(Tk::RParen) => {
                                self.i += 1;
                                break;
                            }
                            _ => return None,
                        }
                    }
                }
                let e = self.toks[self.i - 1].end;
                self.push(Nk::Call(name, args), t.start, e)
            }
            Tk::LParen => {
                self.i += 1;
                let x = self.expr()?;
                if self.toks.get(self.i).map(|x| x.kind) != Some(Tk::RParen) {
                    return None;
                }
                self.i += 1;
                let e = self.toks[self.i - 1].end;
                self.push(Nk::Group(x), t.start, e)
            }
            Tk::Num | Tk::Ident | Tk::NsIdent | Tk::Str => {
                if t.kind == Tk::Str || t.kind == Tk::NsIdent {
                    self.foreign = true;
                }
                let txt = &self.src[t.start..t.end];
                if t.kind == Tk::Ident && (txt == "true" || txt == "false") {
                    self.foreign = true;
                }
                let k = self.i;
                self.i += 1;
                self.push(Nk::Leaf(k), t.start, t.end)
            }
            _ => None,
        }
    }
}

/// Parses a Schemify expression into a span tree; `None` on a syntax error.
fn parse_schemify(src: &str) -> Option<Parser<'_>> {
    let toks = lex(src, false)?;
    let mut p = Parser {
        src,
        toks,
        i: 0,
        nodes: Vec::new(),
        depth: 0,
        foreign: false,
    };
    p.expr()?;
    if p.i != p.toks.len() {
        return None;
    }
    Some(p)
}

struct Emit<'p, 'a, F> {
    p: &'p Parser<'a>,
    resolve: &'p mut F,
    review: Option<&'static str>,
    depth: u32,
    ok: bool,
}

impl<F: FnMut(&str) -> Name> Emit<'_, '_, F> {
    fn emit(&mut self, n: usize, out: &mut String) {
        let (kind, start, end) = self.p.nodes[n].clone();
        match kind {
            Nk::Leaf(t) => {
                let txt = self.p.text(t);
                match self.p.toks[t].kind {
                    Tk::Num => match number_to_ael(txt) {
                        Some(v) => out.push_str(&v),
                        None => self.ok = false,
                    },
                    Tk::Ident if txt == "pi" => out.push_str("3.141592653589793"),
                    Tk::Ident => self.ident(txt, out),
                    _ => self.ok = false,
                }
            }
            Nk::Bin(l, op, r) if self.p.text(op) == "^" => {
                out.push_str("pow(");
                self.emit(l, out);
                out.push(',');
                self.emit(r, out);
                out.push(')');
            }
            Nk::Call(name, args) => {
                let f = self.p.text(name);
                out.push_str(if f == "ln" { "log" } else { f });
                let mut pos = self.p.toks[name].end;
                for a in args {
                    let (s, e) = (self.p.nodes[a].1, self.p.nodes[a].2);
                    out.push_str(&self.p.src[pos..s]);
                    self.emit(a, out);
                    pos = e;
                }
                out.push_str(&self.p.src[pos..end]);
            }
            other => {
                let kids: Vec<usize> = match other {
                    Nk::Un(x) | Nk::Group(x) => vec![x],
                    Nk::Bin(l, _, r) => vec![l, r],
                    Nk::Tern(c, a, b) => vec![c, a, b],
                    Nk::Leaf(_) | Nk::Call(..) => unreachable!(),
                };
                let mut pos = start;
                for k in kids {
                    let (s, e) = (self.p.nodes[k].1, self.p.nodes[k].2);
                    out.push_str(&self.p.src[pos..s]);
                    self.emit(k, out);
                    pos = e;
                }
                out.push_str(&self.p.src[pos..end]);
            }
        }
    }

    fn ident(&mut self, name: &str, out: &mut String) {
        match (self.resolve)(name) {
            Name::ParentParam => {
                out.push_str("pPar(\"");
                out.push_str(name);
                out.push_str("\")");
            }
            Name::OwnParam => {
                out.push_str("iPar(\"");
                out.push_str(name);
                out.push_str("\")");
            }
            Name::BenchVar => out.push_str(name),
            Name::Unknown => {
                out.push_str(name);
                self.review.get_or_insert("design-variable");
            }
            Name::CellVar(expr) => {
                if self.depth >= INLINE_DEPTH_MAX {
                    self.ok = false;
                    return;
                }
                match to_ael_inner(&expr, self.resolve, self.depth + 1) {
                    Ok(t) => {
                        out.push('(');
                        out.push_str(&t.text);
                        out.push(')');
                        self.review = Some("inlined-var");
                    }
                    Err(_) => self.ok = false,
                }
            }
        }
    }
}

/// Translates a Schemify expression (a number-kind default or override) into a CDF/AEL
/// value: suffixes respelled (`1meg` → `1M`, `2M` → `2m`, `mil` folded), parameters as
/// `pPar`/`iPar`, bench variables bare, component variables inlined, `ln` → `log`,
/// `x^y` → `pow(x,y)`. An expression using anything AEL lacks (strings, `?:`, `if`,
/// comparisons, logic, `%`, plugin and other functions) is constant-folded when it has
/// no identifiers.
///
/// `resolve` classifies each identifier; it is called once per occurrence, in text order.
///
/// # Errors
///
/// `"expression"` when the value cannot be written as AEL (the instance then keeps the
/// CDF default, §3.4); the text is never partially translated.
pub fn to_ael(
    src: &str,
    mut resolve: impl FnMut(&str) -> Name,
) -> Result<Translated, &'static str> {
    to_ael_inner(src, &mut resolve, 0)
}

fn to_ael_inner<F: FnMut(&str) -> Name>(
    src: &str,
    resolve: &mut F,
    depth: u32,
) -> Result<Translated, &'static str> {
    if src.len() > VALUE_MAX {
        return Err("expression");
    }
    let p = parse_schemify(src).ok_or("expression")?;
    if p.foreign {
        return fold(src);
    }
    let root = p.nodes.len() - 1;
    let mut e = Emit {
        p: &p,
        resolve,
        review: None,
        depth,
        ok: true,
    };
    let mut out = String::with_capacity(src.len() + 8);
    e.emit(root, &mut out);
    if !e.ok {
        return fold(src);
    }
    assert!(!out.is_empty() || src.trim().is_empty());
    Ok(Translated {
        text: out,
        review: e.review,
    })
}

/// Canonical number text of `src` when it is an identifier-free numeric expression
/// (`(2)*(1e-6)` → `0.000002`); `None` otherwise.
pub fn fold_numeric(src: &str) -> Option<String> {
    fold(src).ok().map(|t| t.text)
}

/// Constant-folds an identifier-free numeric expression into canonical number text.
fn fold(src: &str) -> Result<Translated, &'static str> {
    let p = Program::compile(src, Mode::Scalar).map_err(|_| "expression")?;
    if p.idents().len() > 0 || p.plugin_fns().len() > 0 {
        return Err("expression");
    }
    let mut s = Scratch::default();
    let x = match p.eval(&[], &mut s).map_err(|_| "expression")? {
        Value::Int(i) => i as f64,
        Value::Num(x) => x,
        _ => return Err("expression"),
    };
    Ok(Translated {
        text: canonical(x),
        review: None,
    })
}

// ---------------------------------------------------------------------------------------
// AEL → Schemify
// ---------------------------------------------------------------------------------------

/// What an identifier of an AEL value is, on import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AelName {
    /// A parameter of the cell the value lives in (Schemify resolves it by name).
    Param,
    /// A bench variable (the value lives in a bench): kept bare.
    BenchVar,
    /// Neither: kept bare, `needs_review: design-variable`.
    Unknown,
}

/// AEL function names with a different Schemify spelling.
const FN_FROM_AEL: [(&str, &str); 1] = [("log", "ln")];

/// AEL/Spectre functions Schemify lacks: the value is reviewed (`expression`).
const AEL_ONLY_FNS: [&str; 6] = ["hypot", "fmod", "asinh", "acosh", "atanh", "atPar"];

/// Translates a CDF/AEL value into a Schemify expression: Spectre factors respelled
/// (`1M` → `1meg`, atto/centi folded), `pPar("x")` → `x`, `iPar("x")` → the text `ipar`
/// returns for `x` (callers parenthesize an instance value, §3.4), `log` → `ln`, `**` →
/// `^`; bare identifiers classified by `resolve`.
///
/// Returns `None` for text that does not lex (an unterminated string, a stray character).
/// The result is not guaranteed to compile (an AEL-only function is kept and reviewed as
/// `expression`); callers compile it and drop what does not.
pub fn from_ael(
    src: &str,
    mut resolve: impl FnMut(&str) -> AelName,
    mut ipar: impl FnMut(&str) -> Option<String>,
) -> Option<Translated> {
    if src.len() > VALUE_MAX {
        return None;
    }
    let toks = lex(src, true)?;
    let mut out = String::with_capacity(src.len());
    let mut review = None;
    let mut pos = 0;
    let mut k = 0;
    while k < toks.len() {
        let t = toks[k];
        out.push_str(&src[pos..t.start]);
        let txt = &src[t.start..t.end];
        pos = t.end;
        k += 1;
        match t.kind {
            Tk::Num => out.push_str(&number_from_ael(txt)?.0),
            Tk::Op if txt == "**" => out.push('^'),
            Tk::Ident => {
                let is_call = toks.get(k).is_some_and(|n| n.kind == Tk::LParen);
                // pPar("x") / iPar("x"): exactly name ( "x" ).
                let arg = (is_call
                    && toks.get(k + 1).is_some_and(|n| n.kind == Tk::Str)
                    && toks.get(k + 2).is_some_and(|n| n.kind == Tk::RParen))
                .then(|| {
                    let s = toks[k + 1];
                    src[s.start + 1..s.end - 1].to_owned()
                });
                match (txt, arg) {
                    ("pPar", Some(x)) => {
                        out.push_str(&x);
                        if resolve(&x) != AelName::Param {
                            review = Some("expression");
                        }
                        pos = toks[k + 2].end;
                        k += 3;
                    }
                    ("iPar", Some(x)) => {
                        match ipar(&x) {
                            Some(v) => out.push_str(&v),
                            None => {
                                out.push_str(&x);
                                review = Some("expression");
                            }
                        }
                        pos = toks[k + 2].end;
                        k += 3;
                    }
                    _ if is_call => {
                        let mapped = FN_FROM_AEL
                            .iter()
                            .find(|(a, _)| *a == txt)
                            .map_or(txt, |(_, s)| *s);
                        if AEL_ONLY_FNS.contains(&txt)
                            || txt == "dotPar"
                            || txt == "pPar"
                            || txt == "iPar"
                        {
                            review = Some("expression");
                        }
                        out.push_str(mapped);
                    }
                    _ => {
                        out.push_str(txt);
                        match resolve(txt) {
                            AelName::Param | AelName::BenchVar => {}
                            AelName::Unknown => {
                                review.get_or_insert("design-variable");
                            }
                        }
                    }
                }
            }
            _ => out.push_str(txt),
        }
    }
    out.push_str(&src[pos..]);
    Some(Translated { text: out, review })
}

// ---------------------------------------------------------------------------------------
// Literals, booleans, units (§3.4 strings, §6.3)
// ---------------------------------------------------------------------------------------

/// The literal text of a Schemify template without holes (`{{`/`}}` undoubled); `None`
/// when it has a hole or a lone brace (`needs_review: template`).
pub fn template_to_literal(t: &str) -> Option<String> {
    let mut out = String::with_capacity(t.len());
    let mut it = t.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' | '}' => {
                if it.peek() == Some(&c) {
                    it.next();
                    out.push(c);
                } else {
                    return None;
                }
            }
            c => out.push(c),
        }
    }
    Some(out)
}

/// A literal as a Schemify template (braces doubled).
pub fn literal_to_template(s: &str) -> String {
    s.replace('{', "{{").replace('}', "}}")
}

/// The CDF boolean default (`"t"`/`"nil"`) of a Schemify bool expression; `None` unless it
/// is the literal `true` or `false`.
pub fn bool_to_cdf(e: &str) -> Option<&'static str> {
    match e.trim() {
        "true" => Some("t"),
        "false" => Some("nil"),
        _ => None,
    }
}

/// A CDF boolean (`t nil TRUE FALSE`, any case) as a Schemify literal.
pub fn bool_from_cdf(v: &str) -> Option<&'static str> {
    match v.to_ascii_lowercase().as_str() {
        "t" | "true" | "1" | "yes" => Some("true"),
        "nil" | "false" | "0" | "no" | "" => Some("false"),
        _ => None,
    }
}

/// Schemify `unit` → CDF `units` (§6.3 table), first match wins on export.
pub const UNITS: [(&str, &str); 17] = [
    ("m", "lengthMetric"),
    ("mil", "lengthEnglish"),
    ("F", "capacitance"),
    ("Ohm", "resistance"),
    ("Ω", "resistance"),
    ("H", "inductance"),
    ("S", "conductance"),
    ("s", "time"),
    ("Hz", "frequency"),
    ("W", "power"),
    ("dB", "powerDB"),
    ("deg", "angle"),
    ("V", "voltage"),
    ("A", "current"),
    ("C", "temperature"),
    ("K", "temperature"),
    ("degC", "temperature"),
];

/// The CDF `units` name of a Schemify unit; `None` drops it.
pub fn unit_to_cdf(u: &str) -> Option<&'static str> {
    UNITS.iter().find(|(s, _)| *s == u).map(|(_, c)| *c)
}

/// The Schemify unit of a CDF `units` name (the first row naming it).
pub fn unit_from_cdf(c: &str) -> Option<&'static str> {
    UNITS.iter().find(|(_, x)| *x == c).map(|(s, _)| *s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(n: &str) -> Name {
        match n {
            "w" | "l" => Name::ParentParam,
            "own" => Name::OwnParam,
            "vdd" => Name::BenchVar,
            "k" => Name::CellVar("2*w".into()),
            "loop" => Name::CellVar("loop+1".into()),
            _ => Name::Unknown,
        }
    }

    #[test]
    fn export_table() {
        for (s, v) in [
            ("1meg", "1M"),
            ("1MEG", "1M"),
            ("2M", "2m"),
            ("2m", "2m"),
            ("1.5k", "1.5k"),
            ("1K", "1k"),
            ("3G", "3G"),
            ("4t", "4T"),
            ("10f", "10f"),
            ("7µ", "7u"),
            ("2mil", "5.08e-5"),
            ("1e3k", "1000000"),
            ("1e-6", "1e-6"),
            ("2*w + 1u", "2*pPar(\"w\") + 1u"),
            ("own*2", "iPar(\"own\")*2"),
            ("vdd/2", "vdd/2"),
            ("ln(w)", "log(pPar(\"w\"))"),
            ("w^2", "pow(pPar(\"w\"),2)"),
            ("2^3^2", "pow(2,pow(3,2))"),
            ("-(l)", "-(pPar(\"l\"))"),
            ("pow(w, 2)", "pow(pPar(\"w\"), 2)"),
            ("pi", "3.141592653589793"),
            ("1 > 0 ? 2 : 3", "2"),
            ("if(true, 1u, 2u)", "1e-6"),
            ("round(2.5)", "3"),
        ] {
            assert_eq!(to_ael(s, params).unwrap().text, v, "{s}");
        }
        assert_eq!(to_ael("k", params).unwrap().text, "(2*pPar(\"w\"))");
        assert_eq!(to_ael("k", params).unwrap().review, Some("inlined-var"));
        assert_eq!(to_ael("x", params).unwrap().review, Some("design-variable"));
        for bad in ["w > 1 ? 1 : 2", "\"s\"", "loop", "gmid.w(1)", "1 +", "1x"] {
            assert_eq!(to_ael(bad, params), Err("expression"), "{bad}");
        }
    }

    #[test]
    fn import_table() {
        let res = |n: &str| match n {
            "w" | "l" => AelName::Param,
            "VDD" => AelName::BenchVar,
            _ => AelName::Unknown,
        };
        let ipar = |n: &str| (n == "w").then(|| "(1u)".to_owned());
        for (v, s) in [
            ("1M", "1meg"),
            ("2m", "2m"),
            ("1pF", "1p"),
            ("5a", "5e-18"),
            ("3c", "0.03"),
            ("1K", "1k"),
            ("2T", "2t"),
            ("5.", "5.0"),
            ("pPar(\"w\")*2", "w*2"),
            ("iPar(\"w\") * 3", "(1u) * 3"),
            ("log(VDD)", "ln(VDD)"),
            ("2**3", "2^3"),
            ("1e-3", "1e-3"),
        ] {
            assert_eq!(from_ael(v, res, ipar).unwrap().text, s, "{v}");
        }
        assert_eq!(
            from_ael("foo*2", res, ipar).unwrap().review,
            Some("design-variable")
        );
        assert_eq!(
            from_ael("hypot(1,2)", res, ipar).unwrap().review,
            Some("expression")
        );
        assert!(from_ael("\"open", res, ipar).is_none());
    }

    #[test]
    fn literals_and_units() {
        assert_eq!(template_to_literal("a{{b}}").as_deref(), Some("a{b}"));
        assert_eq!(template_to_literal("a{b}"), None);
        assert_eq!(literal_to_template("a{b}"), "a{{b}}");
        assert_eq!(bool_to_cdf("true"), Some("t"));
        assert_eq!(bool_from_cdf("FALSE"), Some("false"));
        assert_eq!(unit_to_cdf("Ohm"), Some("resistance"));
        assert_eq!(unit_from_cdf("resistance"), Some("Ohm"));
        assert_eq!(unit_to_cdf("um"), None);
    }
}
