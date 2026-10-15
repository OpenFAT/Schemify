//! A test-only interpreter for the SKILL subset the interop plugin's `.il` scripts are
//! written in (`docs/spec/interop.md` §5.1), plus the reader both sides of the `.sfyx`
//! exchange format share.
//!
//! No SKILL implementation runs without Cadence (research: `virtuoso-tooling-prior-art`),
//! so the scripts are written in **prefix form only** — `(f a b)`, never `f(a b)` or infix,
//! `(getSGq obj attr)` for `obj~>attr` — which SKILL reads everywhere and which this reader
//! can check exactly: a C-style call, an operator, a scale-suffixed or zero-padded number is
//! a read error here, so it cannot slip into a shipped script.
//!
//! Semantics follow the *SKILL Language Reference* (ICADVM20.1): dynamic scope, `nil` is the
//! empty list and false, `case` compares with `equal` and never evaluates keys, `foreach`
//! returns its input list, `substring` is 1-based, `lineread` parses without evaluating and
//! returns `t` for a blank line and `nil` at end of file. Cadence functions (`db*`, `sch*`,
//! `cdf*`, `mae*`) go to the [`Host`] — the mock Virtuoso of `mockdb.rs`.
//!
//! ponytail: the oracle is our reading of the manual, not a real Virtuoso. Goldens captured
//! from the self-test (`SfySelfTest`) on a real installation replace it when available.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::rc::Rc;

pub mod mockdb;

/// A SKILL value. `Nil` is also the empty list; a `List` is never empty.
#[derive(Clone, Debug)]
pub enum V {
    Nil,
    T,
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Sym(Rc<str>),
    List(Rc<Vec<V>>),
    Port(u32),
    Table(u32),
    /// A host (mock database) object.
    Obj(u32),
    /// The value of a missing table key without a default.
    Unbound,
}

impl V {
    pub fn str(s: &str) -> V {
        V::Str(Rc::from(s))
    }
    pub fn sym(s: &str) -> V {
        V::Sym(Rc::from(s))
    }
    pub fn list(v: Vec<V>) -> V {
        if v.is_empty() {
            V::Nil
        } else {
            V::List(Rc::new(v))
        }
    }
    pub fn truthy(&self) -> bool {
        !matches!(self, V::Nil)
    }
    pub fn items(&self) -> &[V] {
        match self {
            V::List(v) => v,
            _ => &[],
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            V::Str(s) | V::Sym(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            V::Int(i) => Some(*i),
            _ => None,
        }
    }
    fn is_list(&self) -> bool {
        matches!(self, V::Nil | V::List(_))
    }
}

/// Structural equality as SKILL's `equal` (numbers of different types are not equal).
pub fn equal(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Nil, V::Nil) | (V::T, V::T) | (V::Unbound, V::Unbound) => true,
        (V::Int(x), V::Int(y)) => x == y,
        (V::Float(x), V::Float(y)) => x == y,
        (V::Str(x), V::Str(y)) | (V::Sym(x), V::Sym(y)) => x == y,
        (V::List(x), V::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| equal(p, q))
        }
        (V::Port(x), V::Port(y)) | (V::Table(x), V::Table(y)) | (V::Obj(x), V::Obj(y)) => x == y,
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------------------

/// Longest string literal SKILL's parser reads (*SKILL Language User Guide*: 8191).
pub const STRING_LITERAL_MAX: usize = 8191;

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
    line: u32,
}

/// Reads every form of `src` (prefix subset). Errors name the 1-based line.
pub fn read_all(src: &str) -> Result<Vec<V>, String> {
    let mut r = Reader {
        s: src.as_bytes(),
        i: 0,
        line: 1,
    };
    let mut out = Vec::new();
    loop {
        r.skip_ws()?;
        if r.i >= r.s.len() {
            return Ok(out);
        }
        out.push(r.form()?);
    }
}

fn is_sym_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'?' | b'@')
}

fn is_delim(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'(' | b')' | b';' | b'"')
}

impl Reader<'_> {
    fn err(&self, msg: &str) -> String {
        format!("line {}: {msg}", self.line)
    }

    fn skip_ws(&mut self) -> Result<(), String> {
        while self.i < self.s.len() {
            let b = self.s[self.i];
            if b == b'\n' {
                self.line += 1;
                self.i += 1;
            } else if b.is_ascii_whitespace() {
                self.i += 1;
            } else if b == b';' {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else if b == b'/' && self.s.get(self.i + 1) == Some(&b'*') {
                let start = self.line;
                self.i += 2;
                loop {
                    if self.i + 1 >= self.s.len() {
                        return Err(format!("line {start}: unterminated /* comment"));
                    }
                    if self.s[self.i] == b'\n' {
                        self.line += 1;
                    }
                    if self.s[self.i] == b'*' && self.s[self.i + 1] == b'/' {
                        self.i += 2;
                        break;
                    }
                    self.i += 1;
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    fn form(&mut self) -> Result<V, String> {
        let b = self.s[self.i];
        match b {
            b'(' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws()?;
                    if self.i >= self.s.len() {
                        return Err(self.err("unbalanced ("));
                    }
                    if self.s[self.i] == b')' {
                        self.i += 1;
                        return Ok(V::list(items));
                    }
                    items.push(self.form()?);
                }
            }
            b')' => Err(self.err("unbalanced )")),
            b'\'' => {
                self.i += 1;
                self.skip_ws()?;
                if self.i >= self.s.len() {
                    return Err(self.err("quote at end of input"));
                }
                let q = self.form()?;
                Ok(V::list(vec![V::sym("quote"), q]))
            }
            b'"' => self.string(),
            b'-' | b'0'..=b'9' | b'.' => self.number(),
            _ if is_sym_byte(b) => {
                let start = self.i;
                while self.i < self.s.len() && is_sym_byte(self.s[self.i]) {
                    self.i += 1;
                }
                let name = std::str::from_utf8(&self.s[start..self.i]).unwrap();
                if self.s.get(self.i) == Some(&b'(') {
                    return Err(self.err(&format!(
                        "C-style call `{name}(`: write `({name} …)` (prefix form only)"
                    )));
                }
                if self.i < self.s.len() && !is_delim(self.s[self.i]) {
                    return Err(self.err(&format!(
                        "character `{}` after `{name}`: operators and infix syntax are not allowed",
                        self.s[self.i] as char
                    )));
                }
                Ok(match name {
                    "nil" => V::Nil,
                    "t" => V::T,
                    _ => V::sym(name),
                })
            }
            _ => Err(self.err(&format!(
                "character `{}`: operators and infix syntax are not allowed",
                b as char
            ))),
        }
    }

    fn string(&mut self) -> Result<V, String> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&b) = self.s.get(self.i) else {
                return Err(self.err("unterminated string"));
            };
            self.i += 1;
            match b {
                b'"' => break,
                b'\n' => {
                    self.line += 1;
                    out.push(b'\n');
                }
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else {
                        return Err(self.err("unterminated string"));
                    };
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'v' => out.push(0x0b),
                        b'b' => out.push(0x08),
                        b'r' => out.push(b'\r'),
                        b'f' => out.push(0x0c),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.s.get(self.i) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.i += 1;
                                    }
                                    _ => break,
                                }
                            }
                            if v == 0 || v > 255 {
                                return Err(self.err("octal escape outside 1..=255"));
                            }
                            out.push(v as u8);
                        }
                        b'\n' => self.line += 1, // line continuation
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
        if out.len() > STRING_LITERAL_MAX {
            return Err(self.err("string literal longer than SKILL's 8191-character limit"));
        }
        let s = String::from_utf8(out).map_err(|_| self.err("string is not UTF-8"))?;
        Ok(V::str(&s))
    }

    fn number(&mut self) -> Result<V, String> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        let digits = self.i;
        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            self.i += 1;
        }
        let int_digits = self.i - digits;
        let mut float = false;
        if self.s.get(self.i) == Some(&b'.') {
            float = true;
            self.i += 1;
            let frac = self.i;
            while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if self.i == frac {
                return Err(self.err("a digit must follow `.`"));
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E'))
            && matches!(self.s.get(self.i + 1), Some(b'0'..=b'9' | b'-' | b'+'))
        {
            float = true;
            self.i += 2;
            while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).unwrap();
        if text == "-" || (int_digits == 0 && !float) {
            return Err(self.err("operators and infix syntax are not allowed (`-`)"));
        }
        if self.i < self.s.len() && !is_delim(self.s[self.i]) {
            return Err(self.err(&format!(
                "`{text}{}…`: scale suffixes and symbols starting with a digit are not allowed",
                self.s[self.i] as char
            )));
        }
        if !float && int_digits > 1 && self.s[digits] == b'0' {
            return Err(self.err(&format!("`{text}`: a leading 0 makes SKILL read octal")));
        }
        if float {
            text.parse::<f64>()
                .map(V::Float)
                .map_err(|e| self.err(&e.to_string()))
        } else {
            text.parse::<i64>()
                .map(V::Int)
                .map_err(|e| self.err(&e.to_string()))
        }
    }
}

/// Writes `v` in SKILL's default print form (`print`, `%L`).
pub fn print(v: &V, out: &mut String) {
    match v {
        V::Nil => out.push_str("nil"),
        V::T => out.push('t'),
        V::Int(i) => write!(out, "{i}").unwrap(),
        V::Float(f) => {
            // SKILL prints a float with 7 significant digits and always as a float: 2.0.
            let g = fmt_g(*f, 7);
            out.push_str(&g);
            if !g.contains(['.', 'e', 'i', 'n']) {
                out.push_str(".0");
            }
        }
        V::Str(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    _ => out.push(c),
                }
            }
            out.push('"');
        }
        V::Sym(s) => out.push_str(s),
        V::List(items) => {
            out.push('(');
            for (k, x) in items.iter().enumerate() {
                if k > 0 {
                    out.push(' ');
                }
                print(x, out);
            }
            out.push(')');
        }
        V::Port(p) => write!(out, "port:{p}").unwrap(),
        V::Table(t) => write!(out, "table:{t}").unwrap(),
        V::Obj(o) => write!(out, "db:{o}").unwrap(),
        V::Unbound => out.push_str("unbound"),
    }
}

fn fmt_g(f: f64, digits: usize) -> String {
    let s = format!("{:.*e}", digits.saturating_sub(1), f);
    // Shortest of fixed / scientific with `digits` significant digits.
    let v: f64 = s.parse().unwrap_or(f);
    let fixed = format!("{v}");
    if fixed.len() <= s.len() { fixed } else { s }
}

// ---------------------------------------------------------------------------------------
// Interpreter
// ---------------------------------------------------------------------------------------

/// The Cadence side of the interpreter: functions it does not know and `~>` on objects.
pub trait Host {
    /// Calls Cadence function `name`; `None` when the host does not define it.
    fn call(&mut self, name: &str, args: &[V]) -> Option<Result<V, String>>;
    /// `obj~>attr`.
    fn get(&mut self, obj: u32, attr: &str) -> Result<V, String>;
    /// `obj~>attr = value`.
    fn set(&mut self, obj: u32, attr: &str, value: V) -> Result<V, String>;
    /// True when [`Host::call`] defines `name` (what `fboundp` sees of the host).
    fn bound(&self, _name: &str) -> bool {
        false
    }
}

enum Flow {
    Err(String),
    Return(V),
}

impl From<String> for Flow {
    fn from(s: String) -> Flow {
        Flow::Err(s)
    }
}

impl From<&str> for Flow {
    fn from(s: &str) -> Flow {
        Flow::Err(s.to_owned())
    }
}

type R = Result<V, Flow>;

struct Proc {
    req: Vec<Rc<str>>,
    opt: Vec<(Rc<str>, V)>,
    key: Vec<(Rc<str>, V)>,
    rest: Option<Rc<str>>,
    body: Vec<V>,
}

enum Port {
    Out {
        path: String,
        buf: String,
        open: bool,
    },
    In {
        lines: Vec<String>,
        next: usize,
    },
    Closed,
}

/// The interpreter. Files live in [`Interp::files`] (absolute paths as keys); `printf`
/// output collects in [`Interp::log`].
pub struct Interp<H> {
    pub host: H,
    pub files: BTreeMap<String, String>,
    pub log: String,
    globals: HashMap<Rc<str>, V>,
    stack: Vec<(Rc<str>, V)>,
    funcs: HashMap<Rc<str>, Rc<Proc>>,
    ports: Vec<Port>,
    tables: Vec<(V, Vec<(V, V)>)>,
    steps: u64,
}

/// Evaluation steps before a run is declared runaway.
pub const STEPS_MAX: u64 = 200_000_000;

const SPECIAL: &[&str] = &[
    "quote",
    "progn",
    "setq",
    "let",
    "prog",
    "return",
    "if",
    "when",
    "unless",
    "cond",
    "case",
    "foreach",
    "for",
    "while",
    "and",
    "or",
    "procedure",
    "lambda",
    "setof",
    "exists",
    "errset",
    "getSGq",
    "setSGq",
    "dbGetq",
    "dbSetq",
    "unwindProtect",
    "getqq",
    "boundp",
    "gets",
];

const BUILTINS: &[&str] = &[
    "list",
    "cons",
    "car",
    "cdr",
    "cadr",
    "caddr",
    "cadddr",
    "cddr",
    "caar",
    "cdar",
    "nth",
    "nthelem",
    "length",
    "append",
    "append1",
    "reverse",
    "member",
    "assoc",
    "copy",
    "equal",
    "eq",
    "nequal",
    "neq",
    "null",
    "not",
    "atom",
    "listp",
    "stringp",
    "fixp",
    "floatp",
    "numberp",
    "symbolp",
    "plus",
    "difference",
    "times",
    "quotient",
    "minus",
    "add1",
    "sub1",
    "lessp",
    "greaterp",
    "leqp",
    "geqp",
    "zerop",
    "abs",
    "round",
    "fix",
    "float",
    "max",
    "min",
    "mod",
    "strcat",
    "strlen",
    "substring",
    "sprintf",
    "fprintf",
    "printf",
    "outfile",
    "infile",
    "close",
    "lineread",
    "parseString",
    "symbolToString",
    "stringToSymbol",
    "isCallable",
    "getVersion",
    "drain",
    "atoi",
    "sort",
    "alphalessp",
    "buildString",
    "mapcar",
    "funcall",
    "apply",
    "makeTable",
    "arrayref",
    "setarray",
    "error",
    "warn",
    "sstatus",
    "getShellEnvVar",
    "simplifyFilename",
    "isFile",
    "upperCase",
    "lowerCase",
    "strcmp",
    "last",
    "xCoord",
    "yCoord",
    "lowerLeft",
    "upperRight",
    "evenp",
    "oddp",
    "fboundp",
    "evalstring",
    "linereadstring",
    "charToInt",
    "intToChar",
    "atof",
    // Retarget (sfy_retarget.il): pure math, work dirs.
    "log",
    "expt",
    "createDir",
];

/// Every function name the interpreter defines itself (special forms and builtins).
pub fn defined_by_interp() -> impl Iterator<Item = &'static str> {
    SPECIAL.iter().chain(BUILTINS.iter()).copied()
}

impl<H: Host> Interp<H> {
    pub fn new(host: H) -> Self {
        Interp {
            host,
            files: BTreeMap::new(),
            log: String::new(),
            globals: HashMap::new(),
            stack: Vec::new(),
            funcs: HashMap::new(),
            ports: Vec::new(),
            tables: Vec::new(),
            steps: 0,
        }
    }

    /// Reads and evaluates every form of `src`; returns the last value.
    pub fn load(&mut self, src: &str) -> Result<V, String> {
        let forms = read_all(src)?;
        let mut last = V::Nil;
        for f in &forms {
            last = self.eval_top(f)?;
        }
        Ok(last)
    }

    /// Evaluates one form at top level.
    pub fn eval_top(&mut self, f: &V) -> Result<V, String> {
        match self.eval(f) {
            Ok(v) => Ok(v),
            Err(Flow::Err(e)) => Err(e),
            Err(Flow::Return(_)) => Err("return outside prog".into()),
        }
    }

    /// Calls procedure or builtin `name` with already evaluated `args`.
    pub fn call(&mut self, name: &str, args: Vec<V>) -> Result<V, String> {
        match self.apply_fn(name, args) {
            Ok(v) => Ok(v),
            Err(Flow::Err(e)) => Err(e),
            Err(Flow::Return(_)) => Err("return outside prog".into()),
        }
    }

    pub fn set_global(&mut self, name: &str, v: V) {
        self.globals.insert(Rc::from(name), v);
    }

    /// Names of every procedure defined so far.
    pub fn procedures(&self) -> Vec<String> {
        let mut v: Vec<String> = self.funcs.keys().map(|k| k.to_string()).collect();
        v.sort();
        v
    }

    fn lookup(&self, name: &str) -> Result<V, Flow> {
        if let Some((_, v)) = self.stack.iter().rev().find(|(n, _)| &**n == name) {
            return Ok(v.clone());
        }
        self.globals
            .get(name)
            .cloned()
            .ok_or_else(|| Flow::Err(format!("unbound variable {name}")))
    }

    fn assign(&mut self, name: &str, v: V) {
        if let Some(slot) = self.stack.iter_mut().rev().find(|(n, _)| &**n == name) {
            slot.1 = v;
        } else {
            self.globals.insert(Rc::from(name), v);
        }
    }

    fn body(&mut self, forms: &[V]) -> R {
        let mut last = V::Nil;
        for f in forms {
            last = self.eval(f)?;
        }
        Ok(last)
    }

    fn eval(&mut self, x: &V) -> R {
        self.steps += 1;
        if self.steps > STEPS_MAX {
            return Err(Flow::Err("evaluation step budget exhausted".into()));
        }
        match x {
            V::Sym(s) => self.lookup(s),
            V::List(items) => {
                let items = items.clone();
                let V::Sym(head) = &items[0] else {
                    return Err(Flow::Err(
                        "call of a non-symbol (redundant parentheses?)".into(),
                    ));
                };
                self.form(head, &items[1..])
            }
            other => Ok(other.clone()),
        }
    }

    fn sym_arg<'a>(&self, x: &'a V, what: &str) -> Result<&'a str, Flow> {
        match x {
            V::Sym(s) => Ok(s),
            _ => Err(Flow::Err(format!("{what}: expected a symbol"))),
        }
    }

    fn form(&mut self, head: &str, a: &[V]) -> R {
        match head {
            "quote" => Ok(a.first().cloned().unwrap_or(V::Nil)),
            "progn" => self.body(a),
            "setq" => {
                if !a.len().is_multiple_of(2) || a.is_empty() {
                    return Err(Flow::Err("setq: needs symbol/value pairs".into()));
                }
                let mut v = V::Nil;
                for pair in a.chunks(2) {
                    let n = self.sym_arg(&pair[0], "setq")?.to_owned();
                    v = self.eval(&pair[1])?;
                    self.assign(&n, v.clone());
                }
                Ok(v)
            }
            "let" | "prog" => {
                let Some(vars) = a.first() else {
                    return Err(Flow::Err(format!("{head}: missing variable list")));
                };
                let base = self.stack.len();
                for b in vars.items() {
                    let (name, init) = match b {
                        V::Sym(s) => (s.clone(), V::Nil),
                        V::List(p) if p.len() == 2 => {
                            let s = self.sym_arg(&p[0], head)?;
                            let s: Rc<str> = Rc::from(s);
                            (s, self.eval(&p[1])?)
                        }
                        _ => return Err(Flow::Err(format!("{head}: bad binding"))),
                    };
                    self.stack.push((name, init));
                }
                let r = self.body(&a[1..]);
                self.stack.truncate(base);
                match (head, r) {
                    ("prog", Err(Flow::Return(v))) => Ok(v),
                    ("prog", Ok(_)) => Ok(V::Nil),
                    (_, r) => r,
                }
            }
            "return" => {
                let v = match a.first() {
                    Some(x) => self.eval(x)?,
                    None => V::Nil,
                };
                Err(Flow::Return(v))
            }
            "if" => {
                // (if c a b) or (if c then a… else b…)
                let Some(c) = a.first() else {
                    return Err(Flow::Err("if: missing condition".into()));
                };
                let cond = self.eval(c)?.truthy();
                if matches!(a.get(1), Some(V::Sym(s)) if &**s == "then") {
                    let else_at = a
                        .iter()
                        .position(|x| matches!(x, V::Sym(s) if &**s == "else"));
                    let (then_part, else_part) = match else_at {
                        Some(k) => (&a[2..k], &a[k + 1..]),
                        None => (&a[2..], &a[a.len()..]),
                    };
                    return if cond {
                        self.body(then_part)
                    } else {
                        self.body(else_part)
                    };
                }
                if a.len() > 3 {
                    return Err(Flow::Err(
                        "if: more than one else form without `then`".into(),
                    ));
                }
                if cond {
                    a.get(1).map_or(Ok(V::Nil), |x| self.eval(x))
                } else {
                    a.get(2).map_or(Ok(V::Nil), |x| self.eval(x))
                }
            }
            "when" | "unless" => {
                let Some(c) = a.first() else {
                    return Err(Flow::Err(format!("{head}: missing condition")));
                };
                let c = self.eval(c)?.truthy();
                if c == (head == "when") {
                    self.body(&a[1..])
                } else {
                    Ok(V::Nil)
                }
            }
            "cond" => {
                for clause in a {
                    let items = clause.items();
                    if items.is_empty() {
                        return Err(Flow::Err("cond: empty clause".into()));
                    }
                    let c = self.eval(&items[0])?;
                    if c.truthy() {
                        return if items.len() == 1 {
                            Ok(c)
                        } else {
                            self.body(&items[1..])
                        };
                    }
                }
                Ok(V::Nil)
            }
            "case" => {
                let Some(k) = a.first() else {
                    return Err(Flow::Err("case: missing key form".into()));
                };
                let key = self.eval(k)?;
                for clause in &a[1..] {
                    let items = clause.items();
                    if items.is_empty() {
                        return Err(Flow::Err("case: empty clause".into()));
                    }
                    let hit = match &items[0] {
                        V::T => true,
                        V::List(keys) => keys.iter().any(|x| equal(x, &key)),
                        atom => equal(atom, &key),
                    };
                    if hit {
                        return self.body(&items[1..]);
                    }
                }
                Ok(V::Nil)
            }
            "foreach" | "setof" | "exists" => {
                if a.len() < 3 {
                    return Err(Flow::Err(format!("{head}: needs var, list and body")));
                }
                let var: Rc<str> = Rc::from(self.sym_arg(&a[0], head)?);
                let list = self.eval(&a[1])?;
                if !list.is_list() {
                    return Err(Flow::Err(format!("{head}: not a list")));
                }
                let base = self.stack.len();
                self.stack.push((var, V::Nil));
                let mut kept = Vec::new();
                let mut result = Ok(list.clone());
                let elems = list.items().to_vec();
                for (k, e) in elems.iter().enumerate() {
                    self.stack[base].1 = e.clone();
                    match self.body(&a[2..]) {
                        Ok(v) => {
                            if head == "setof" && v.truthy() {
                                kept.push(e.clone());
                            }
                            if head == "exists" && v.truthy() {
                                result = Ok(V::list(elems[k..].to_vec()));
                                break;
                            }
                        }
                        Err(f) => {
                            result = Err(f);
                            break;
                        }
                    }
                    if head == "exists" && k + 1 == elems.len() {
                        result = Ok(V::Nil);
                    }
                }
                self.stack.truncate(base);
                match head {
                    "setof" => result.map(|_| V::list(kept)),
                    "exists" if elems.is_empty() => Ok(V::Nil),
                    _ => result,
                }
            }
            "for" => {
                if a.len() < 3 {
                    return Err(Flow::Err("for: needs var, from, to".into()));
                }
                let var: Rc<str> = Rc::from(self.sym_arg(&a[0], "for")?);
                let lo = self.eval(&a[1])?.as_int().ok_or("for: integer bounds")?;
                let hi = self.eval(&a[2])?.as_int().ok_or("for: integer bounds")?;
                let base = self.stack.len();
                self.stack.push((var, V::Nil));
                let mut r = Ok(V::T);
                for i in lo..=hi {
                    self.stack[base].1 = V::Int(i);
                    if let Err(f) = self.body(&a[3..]) {
                        r = Err(f);
                        break;
                    }
                }
                self.stack.truncate(base);
                r
            }
            "while" => {
                let Some(c) = a.first() else {
                    return Err(Flow::Err("while: missing condition".into()));
                };
                while self.eval(c)?.truthy() {
                    self.body(&a[1..])?;
                }
                Ok(V::T)
            }
            "and" => {
                let mut v = V::T;
                for x in a {
                    v = self.eval(x)?;
                    if !v.truthy() {
                        return Ok(V::Nil);
                    }
                }
                Ok(v)
            }
            "or" => {
                for x in a {
                    let v = self.eval(x)?;
                    if v.truthy() {
                        return Ok(v);
                    }
                }
                Ok(V::Nil)
            }
            "procedure" => {
                let sig = a.first().map(V::items).unwrap_or(&[]);
                let Some(V::Sym(name)) = sig.first() else {
                    return Err(Flow::Err(
                        "procedure: (procedure (name args…) body…)".into(),
                    ));
                };
                let p = self.parse_params(&sig[1..])?;
                let p = Proc {
                    body: a[1..].to_vec(),
                    ..p
                };
                self.funcs.insert(name.clone(), Rc::new(p));
                Ok(V::Sym(name.clone()))
            }
            "lambda" => {
                // Stored as an anonymous procedure; the value is a symbol naming it.
                let p = self.parse_params(a.first().map(V::items).unwrap_or(&[]))?;
                let p = Proc {
                    body: a[1..].to_vec(),
                    ..p
                };
                let name: Rc<str> = Rc::from(format!("lambda#{}", self.funcs.len()).as_str());
                self.funcs.insert(name.clone(), Rc::new(p));
                Ok(V::Sym(name))
            }
            "errset" => {
                let Some(x) = a.first() else {
                    return Err(Flow::Err("errset: missing form".into()));
                };
                let depth = self.stack.len();
                // errset.errset (the property errset of the symbol errset, read with
                // `getqq`) holds the caught error; a form that completes clears it, so a
                // later errset (an unwindProtect cleanup, say) loses an earlier message.
                match self.eval(x) {
                    Ok(v) => {
                        self.set_global("errset.errset", V::Nil);
                        Ok(V::list(vec![v]))
                    }
                    Err(Flow::Err(e)) => {
                        self.stack.truncate(depth);
                        self.set_global("errset.errset", V::list(vec![V::str(&e)]));
                        Ok(V::Nil)
                    }
                    Err(r) => Err(r),
                }
            }
            // (getqq sym prop): the property prop of symbol sym, unevaluated; the scripts read
            // only errset.errset, kept as the global of that dotted name.
            "getqq" => match a {
                [V::Sym(x), V::Sym(y)] => Ok(self
                    .globals
                    .get(format!("{x}.{y}").as_str())
                    .cloned()
                    .unwrap_or(V::Nil)),
                _ => Err(Flow::Err("getqq: (getqq symbol property)".into())),
            },
            "unwindProtect" => {
                if a.len() != 2 {
                    return Err(Flow::Err(
                        "unwindProtect: (unwindProtect body cleanup)".into(),
                    ));
                }
                let r = self.eval(&a[0]);
                self.eval(&a[1])?;
                r
            }
            "boundp" => {
                let v = match a.first() {
                    Some(x) => self.eval(x)?,
                    None => return Err(Flow::Err("boundp: missing symbol".into())),
                };
                let name = self.sym_arg(&v, "boundp")?;
                Ok(if self.lookup(name).is_ok() {
                    V::T
                } else {
                    V::Nil
                })
            }
            "gets" => {
                // (gets var port): a syntax form (*SKILL Language Reference*): stores the next
                // line, keeping its "\n", into `var`; nil at end of file.
                if a.len() != 2 {
                    return Err(Flow::Err("gets: (gets var port)".into()));
                }
                let var = self.sym_arg(&a[0], "gets")?.to_owned();
                let V::Port(k) = self.eval(&a[1])? else {
                    return Err(Flow::Err("gets: expected a port".into()));
                };
                let Some(Port::In { lines, next }) = self.ports.get_mut(k as usize) else {
                    return Err(Flow::Err("gets: not an open input port".into()));
                };
                let v = match lines.get(*next) {
                    Some(l) => {
                        *next += 1;
                        V::str(l)
                    }
                    None => V::Nil,
                };
                self.assign(&var, v.clone());
                Ok(v)
            }
            "getSGq" | "dbGetq" => {
                if a.len() != 2 {
                    return Err(Flow::Err(format!("{head}: (getSGq obj attr)")));
                }
                let obj = self.eval(&a[0])?;
                let attr = self.sym_arg(&a[1], head)?.to_owned();
                self.get_attr(&obj, &attr)
            }
            "setSGq" | "dbSetq" => {
                if a.len() != 3 {
                    return Err(Flow::Err(format!("{head}: (setSGq obj value attr)")));
                }
                let obj = self.eval(&a[0])?;
                let v = self.eval(&a[1])?;
                let attr = self.sym_arg(&a[2], head)?.to_owned();
                match obj {
                    V::Obj(id) => Ok(self.host.set(id, &attr, v)?),
                    _ => Err(Flow::Err(format!("{head}: not a database object"))),
                }
            }
            _ => {
                // Ordinary call: evaluate arguments, keeping `?key` markers unevaluated.
                let mut args = Vec::with_capacity(a.len());
                for x in a {
                    match x {
                        V::Sym(s) if s.starts_with('?') => args.push(x.clone()),
                        _ => args.push(self.eval(x)?),
                    }
                }
                self.apply_fn(head, args)
            }
        }
    }

    fn get_attr(&mut self, obj: &V, attr: &str) -> R {
        match obj {
            V::Nil => Ok(V::Nil),
            V::Obj(id) => Ok(self.host.get(*id, attr)?),
            V::List(items) => {
                // `~>` maps over a list of objects.
                let items = items.clone();
                let mut out = Vec::with_capacity(items.len());
                for x in items.iter() {
                    out.push(self.get_attr(x, attr)?);
                }
                Ok(V::list(out))
            }
            _ => Err(Flow::Err(format!("~>{attr}: not an object or list"))),
        }
    }

    fn parse_params(&mut self, params: &[V]) -> Result<Proc, Flow> {
        let mut p = Proc {
            req: vec![],
            opt: vec![],
            key: vec![],
            rest: None,
            body: vec![],
        };
        let mut mode = 0; // 0 required, 1 @optional, 2 @key, 3 @rest
        for x in params {
            match x {
                V::Sym(s) if &**s == "@optional" => mode = 1,
                V::Sym(s) if &**s == "@key" => mode = 2,
                V::Sym(s) if &**s == "@rest" => mode = 3,
                V::Sym(s) => match mode {
                    0 => p.req.push(s.clone()),
                    1 => p.opt.push((s.clone(), V::Nil)),
                    2 => p.key.push((s.clone(), V::Nil)),
                    _ => p.rest = Some(s.clone()),
                },
                V::List(d) if d.len() == 2 && mode == 1 || mode == 2 => {
                    let d = x.items();
                    let s: Rc<str> = Rc::from(self.sym_arg(&d[0], "procedure")?);
                    if mode == 1 {
                        p.opt.push((s, d[1].clone()))
                    } else {
                        p.key.push((s, d[1].clone()))
                    }
                }
                _ => return Err(Flow::Err("procedure: bad parameter".into())),
            }
        }
        if !p.opt.is_empty() && !p.key.is_empty() {
            return Err(Flow::Err(
                "procedure: @optional and @key are mutually exclusive".into(),
            ));
        }
        Ok(p)
    }

    fn call_proc(&mut self, name: &str, p: Rc<Proc>, args: Vec<V>) -> R {
        let mut pos = Vec::new();
        let mut keys: Vec<(String, V)> = Vec::new();
        let mut it = args.into_iter();
        while let Some(x) = it.next() {
            match &x {
                V::Sym(s) if s.starts_with('?') => {
                    let v = it
                        .next()
                        .ok_or_else(|| format!("{name}: {s} without a value"))?;
                    keys.push((s[1..].to_owned(), v));
                }
                _ => pos.push(x),
            }
        }
        if pos.len() < p.req.len() || (p.rest.is_none() && pos.len() > p.req.len() + p.opt.len()) {
            return Err(Flow::Err(format!(
                "{name}: called with {} arguments, takes {}{}",
                pos.len(),
                p.req.len(),
                if p.opt.is_empty() {
                    String::new()
                } else {
                    format!("..{}", p.req.len() + p.opt.len())
                }
            )));
        }
        let base = self.stack.len();
        let mut pos = pos.into_iter();
        for n in &p.req {
            self.stack.push((n.clone(), pos.next().unwrap()));
        }
        for (n, d) in &p.opt {
            let v = match pos.next() {
                Some(v) => v,
                None => self.eval(d)?,
            };
            self.stack.push((n.clone(), v));
        }
        for (n, d) in &p.key {
            let v = match keys.iter().position(|(k, _)| k == &**n) {
                Some(i) => keys.swap_remove(i).1,
                None => self.eval(d)?,
            };
            self.stack.push((n.clone(), v));
        }
        if let Some((k, _)) = keys.first() {
            self.stack.truncate(base);
            return Err(Flow::Err(format!("{name}: unknown keyword ?{k}")));
        }
        if let Some(r) = &p.rest {
            self.stack.push((r.clone(), V::list(pos.collect())));
        }
        let r = self.body(&p.body);
        self.stack.truncate(base);
        match r {
            // The call chain, innermost first, for the test's failure message.
            Err(Flow::Err(e)) => Err(Flow::Err(format!("{e}\n  in {name}"))),
            r => r,
        }
    }

    fn fn_value(&self, f: &V) -> Result<String, Flow> {
        match f {
            V::Sym(s) => Ok(s.to_string()),
            _ => Err(Flow::Err("expected a function (symbol or lambda)".into())),
        }
    }

    fn apply_fn(&mut self, name: &str, args: Vec<V>) -> R {
        if let Some(p) = self.funcs.get(name).cloned() {
            return self.call_proc(name, p, args);
        }
        if let Some(r) = self.builtin(name, &args) {
            return r;
        }
        match self.host.call(name, &args) {
            Some(r) => Ok(r?),
            None => Err(Flow::Err(format!("undefined function {name}"))),
        }
    }

    fn port_out(&mut self, p: &V) -> Result<&mut String, Flow> {
        match p {
            V::Port(k) => match self.ports.get_mut(*k as usize) {
                Some(Port::Out {
                    buf, open: true, ..
                }) => Ok(buf),
                _ => Err(Flow::Err("not an open output port".into())),
            },
            _ => Err(Flow::Err("expected a port".into())),
        }
    }

    fn builtin(&mut self, name: &str, a: &[V]) -> Option<R> {
        let r = (|| -> R {
            let n = a.len();
            let need = |k: usize| -> Result<(), Flow> {
                if n == k {
                    Ok(())
                } else {
                    Err(Flow::Err(format!("{name}: takes {k} arguments, got {n}")))
                }
            };
            let int = |v: &V| -> Result<i64, Flow> {
                v.as_int()
                    .ok_or_else(|| Flow::Err(format!("{name}: expected an integer")))
            };
            let num = |v: &V| -> Result<f64, Flow> {
                match v {
                    V::Int(i) => Ok(*i as f64),
                    V::Float(f) => Ok(*f),
                    _ => Err(Flow::Err(format!("{name}: expected a number"))),
                }
            };
            let text = |v: &V| -> Result<String, Flow> {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Flow::Err(format!("{name}: expected a string")))
            };
            let list = |v: &V| -> Result<Vec<V>, Flow> {
                if v.is_list() {
                    Ok(v.items().to_vec())
                } else {
                    Err(Flow::Err(format!("{name}: expected a list")))
                }
            };
            let arith = |f: fn(i64, i64) -> Option<i64>, g: fn(f64, f64) -> f64| -> R {
                if a.is_empty() {
                    return Err(Flow::Err(format!("{name}: no arguments")));
                }
                let mut acc = a[0].clone();
                for x in &a[1..] {
                    acc = match (&acc, x) {
                        (V::Int(p), V::Int(q)) => V::Int(f(*p, *q).ok_or_else(|| {
                            format!("{name}: integer overflow or division by zero")
                        })?),
                        _ => V::Float(g(num(&acc)?, num(x)?)),
                    };
                }
                if a.len() == 1 {
                    num(&acc)?;
                }
                Ok(acc)
            };
            let cmp = |f: fn(f64, f64) -> bool| -> R {
                need(2)?;
                Ok(if f(num(&a[0])?, num(&a[1])?) {
                    V::T
                } else {
                    V::Nil
                })
            };
            Ok(match name {
                "list" => V::list(a.to_vec()),
                "cons" => {
                    need(2)?;
                    let mut v = vec![a[0].clone()];
                    v.extend(list(&a[1])?);
                    V::list(v)
                }
                "car" => {
                    need(1)?;
                    list(&a[0])?.first().cloned().unwrap_or(V::Nil)
                }
                "cdr" => {
                    need(1)?;
                    V::list(list(&a[0])?.into_iter().skip(1).collect())
                }
                "cadr" => {
                    need(1)?;
                    list(&a[0])?.get(1).cloned().unwrap_or(V::Nil)
                }
                "caddr" => {
                    need(1)?;
                    list(&a[0])?.get(2).cloned().unwrap_or(V::Nil)
                }
                "cadddr" => {
                    need(1)?;
                    list(&a[0])?.get(3).cloned().unwrap_or(V::Nil)
                }
                "cddr" => {
                    need(1)?;
                    V::list(list(&a[0])?.into_iter().skip(2).collect())
                }
                "caar" => {
                    need(1)?;
                    let f = list(&a[0])?.first().cloned().unwrap_or(V::Nil);
                    list(&f)?.first().cloned().unwrap_or(V::Nil)
                }
                "cdar" => {
                    need(1)?;
                    let f = list(&a[0])?.first().cloned().unwrap_or(V::Nil);
                    V::list(list(&f)?.into_iter().skip(1).collect())
                }
                "last" => {
                    need(1)?;
                    let l = list(&a[0])?;
                    l.last().map_or(V::Nil, |x| V::list(vec![x.clone()]))
                }
                "nth" => {
                    need(2)?;
                    let k = int(&a[0])?;
                    list(&a[1])?
                        .get(k.max(0) as usize)
                        .cloned()
                        .unwrap_or(V::Nil)
                }
                "nthelem" => {
                    need(2)?;
                    let k = int(&a[0])?;
                    if k < 1 {
                        V::Nil
                    } else {
                        list(&a[1])?.get(k as usize - 1).cloned().unwrap_or(V::Nil)
                    }
                }
                "length" => {
                    need(1)?;
                    match &a[0] {
                        V::Str(_) => {
                            return Err(Flow::Err("length: use strlen for strings".into()));
                        }
                        V::Table(t) => V::Int(self.tables[*t as usize].1.len() as i64),
                        v => V::Int(list(v)?.len() as i64),
                    }
                }
                "append" => {
                    let mut v = Vec::new();
                    for x in a {
                        v.extend(list(x)?);
                    }
                    V::list(v)
                }
                "append1" => {
                    need(2)?;
                    let mut v = list(&a[0])?;
                    v.push(a[1].clone());
                    V::list(v)
                }
                "reverse" => {
                    need(1)?;
                    let mut v = list(&a[0])?;
                    v.reverse();
                    V::list(v)
                }
                "copy" => {
                    need(1)?;
                    a[0].clone()
                }
                "member" => {
                    need(2)?;
                    let l = list(&a[1])?;
                    match l.iter().position(|x| equal(x, &a[0])) {
                        Some(k) => V::list(l[k..].to_vec()),
                        None => V::Nil,
                    }
                }
                "assoc" => {
                    need(2)?;
                    list(&a[1])?
                        .into_iter()
                        .find(|x| x.items().first().is_some_and(|k| equal(k, &a[0])))
                        .unwrap_or(V::Nil)
                }
                "equal" => {
                    need(2)?;
                    if equal(&a[0], &a[1]) { V::T } else { V::Nil }
                }
                "nequal" => {
                    need(2)?;
                    if equal(&a[0], &a[1]) { V::Nil } else { V::T }
                }
                "eq" | "neq" => {
                    need(2)?;
                    let same = match (&a[0], &a[1]) {
                        (V::Str(_), _) | (_, V::Str(_)) => {
                            return Err(Flow::Err(format!("{name}: strings compare with equal")));
                        }
                        (x, y) => equal(x, y),
                    };
                    if same == (name == "eq") { V::T } else { V::Nil }
                }
                "null" | "not" => {
                    need(1)?;
                    if a[0].truthy() { V::Nil } else { V::T }
                }
                "atom" => {
                    need(1)?;
                    if matches!(a[0], V::List(_)) {
                        V::Nil
                    } else {
                        V::T
                    }
                }
                "listp" => {
                    need(1)?;
                    if a[0].is_list() { V::T } else { V::Nil }
                }
                "stringp" => {
                    need(1)?;
                    if matches!(a[0], V::Str(_)) {
                        V::T
                    } else {
                        V::Nil
                    }
                }
                "fixp" => {
                    need(1)?;
                    if matches!(a[0], V::Int(_)) {
                        V::T
                    } else {
                        V::Nil
                    }
                }
                "floatp" => {
                    need(1)?;
                    if matches!(a[0], V::Float(_)) {
                        V::T
                    } else {
                        V::Nil
                    }
                }
                "numberp" => {
                    need(1)?;
                    if matches!(a[0], V::Int(_) | V::Float(_)) {
                        V::T
                    } else {
                        V::Nil
                    }
                }
                "symbolp" => {
                    need(1)?;
                    if matches!(a[0], V::Sym(_) | V::Nil | V::T) {
                        V::T
                    } else {
                        V::Nil
                    }
                }
                "plus" => arith(|x, y| x.checked_add(y), |x, y| x + y)?,
                "difference" => arith(|x, y| x.checked_sub(y), |x, y| x - y)?,
                "times" => arith(|x, y| x.checked_mul(y), |x, y| x * y)?,
                "quotient" => arith(|x, y| x.checked_div(y), |x, y| x / y)?,
                "minus" => {
                    need(1)?;
                    match &a[0] {
                        V::Int(i) => V::Int(-i),
                        v => V::Float(-num(v)?),
                    }
                }
                "add1" => {
                    need(1)?;
                    match &a[0] {
                        V::Int(i) => V::Int(i + 1),
                        v => V::Float(num(v)? + 1.0),
                    }
                }
                "sub1" => {
                    need(1)?;
                    match &a[0] {
                        V::Int(i) => V::Int(i - 1),
                        v => V::Float(num(v)? - 1.0),
                    }
                }
                "abs" => {
                    need(1)?;
                    match &a[0] {
                        V::Int(i) => V::Int(i.abs()),
                        v => V::Float(num(v)?.abs()),
                    }
                }
                "lessp" => cmp(|x, y| x < y)?,
                "greaterp" => cmp(|x, y| x > y)?,
                "leqp" => cmp(|x, y| x <= y)?,
                "geqp" => cmp(|x, y| x >= y)?,
                "zerop" => {
                    need(1)?;
                    if num(&a[0])? == 0.0 { V::T } else { V::Nil }
                }
                "evenp" => {
                    need(1)?;
                    if int(&a[0])? % 2 == 0 { V::T } else { V::Nil }
                }
                "oddp" => {
                    need(1)?;
                    if int(&a[0])? % 2 != 0 { V::T } else { V::Nil }
                }
                "round" => {
                    need(1)?;
                    let f = num(&a[0])?;
                    // SKILL rounds half away from zero and clamps to 32 bits with a warning.
                    let r = f.round();
                    if r.abs() > i32::MAX as f64 {
                        return Err(Flow::Err("round: outside the 32-bit range".into()));
                    }
                    V::Int(r as i64)
                }
                "fix" => {
                    need(1)?;
                    V::Int(num(&a[0])?.trunc() as i64)
                }
                "float" => {
                    need(1)?;
                    V::Float(num(&a[0])?)
                }
                "max" | "min" => {
                    if a.is_empty() {
                        return Err(Flow::Err(format!("{name}: no arguments")));
                    }
                    let mut best = a[0].clone();
                    for x in &a[1..] {
                        let better = if name == "max" {
                            num(x)? > num(&best)?
                        } else {
                            num(x)? < num(&best)?
                        };
                        if better {
                            best = x.clone();
                        }
                    }
                    best
                }
                "mod" => {
                    need(2)?;
                    let d = int(&a[1])?;
                    if d == 0 {
                        return Err(Flow::Err("mod: division by zero".into()));
                    }
                    V::Int(int(&a[0])? % d)
                }
                "strcat" => {
                    let mut s = String::new();
                    for x in a {
                        s.push_str(&text(x)?);
                    }
                    V::str(&s)
                }
                "strlen" => {
                    need(1)?;
                    V::Int(text(&a[0])?.len() as i64)
                }
                "substring" => {
                    if !(2..=3).contains(&n) {
                        return Err(Flow::Err("substring: (substring s start [len])".into()));
                    }
                    let s = text(&a[0])?;
                    let b = s.as_bytes();
                    let len = b.len() as i64;
                    let mut start = int(&a[1])?;
                    if start < 0 {
                        start += len + 1;
                    }
                    if start < 1 || start > len {
                        return Ok(V::Nil);
                    }
                    let take = if n == 3 { int(&a[2])?.max(0) } else { len };
                    let end = (start - 1 + take).min(len);
                    let piece = &b[(start - 1) as usize..end as usize];
                    V::str(
                        std::str::from_utf8(piece)
                            .map_err(|_| "substring: splits a UTF-8 character".to_owned())?,
                    )
                }
                "upperCase" => {
                    need(1)?;
                    V::str(&text(&a[0])?.to_uppercase())
                }
                "lowerCase" => {
                    need(1)?;
                    V::str(&text(&a[0])?.to_lowercase())
                }
                "strcmp" => {
                    need(2)?;
                    V::Int(match text(&a[0])?.cmp(&text(&a[1])?) {
                        std::cmp::Ordering::Less => -1,
                        std::cmp::Ordering::Equal => 0,
                        _ => 1,
                    })
                }
                "alphalessp" => {
                    need(2)?;
                    if text(&a[0])? < text(&a[1])? {
                        V::T
                    } else {
                        V::Nil
                    }
                }
                "symbolToString" => {
                    need(1)?;
                    V::str(&text(&a[0])?)
                }
                "stringToSymbol" => {
                    need(1)?;
                    V::sym(&text(&a[0])?)
                }
                "isCallable" => {
                    need(1)?;
                    let V::Sym(n) = &a[0] else {
                        return Err(Flow::Err("isCallable: not a symbol".into()));
                    };
                    let known = self.funcs.contains_key(&**n)
                        || defined_by_interp().any(|b| b == &**n)
                        || mockdb::SIGNATURES.iter().any(|(f, _)| f == &&**n);
                    if known { V::T } else { V::Nil }
                }
                "getVersion" => V::str("sfy test interpreter"),
                "drain" => {
                    need(1)?;
                    V::T
                }
                "atoi" => {
                    need(1)?;
                    text(&a[0])?
                        .trim()
                        .parse::<i64>()
                        .map(V::Int)
                        .unwrap_or(V::Nil)
                }
                "buildString" => {
                    if !(1..=2).contains(&n) {
                        return Err(Flow::Err("buildString: (buildString list [glue])".into()));
                    }
                    let glue = if n == 2 { text(&a[1])? } else { " ".into() };
                    let parts: Result<Vec<String>, Flow> = list(&a[0])?.iter().map(&text).collect();
                    V::str(&parts?.join(&glue))
                }
                "parseString" => {
                    if !(1..=2).contains(&n) {
                        return Err(Flow::Err("parseString: (parseString s [breaks])".into()));
                    }
                    let s = text(&a[0])?;
                    let breaks = if n == 2 { text(&a[1])? } else { " \t\n".into() };
                    let parts: Vec<V> = if breaks.is_empty() {
                        s.chars().map(|c| V::str(&c.to_string())).collect()
                    } else {
                        s.split(|c| breaks.contains(c))
                            .filter(|p| !p.is_empty())
                            .map(V::str)
                            .collect()
                    };
                    V::list(parts)
                }
                "xCoord" | "car_point" => {
                    need(1)?;
                    list(&a[0])?.first().cloned().unwrap_or(V::Nil)
                }
                "yCoord" => {
                    need(1)?;
                    list(&a[0])?.get(1).cloned().unwrap_or(V::Nil)
                }
                "lowerLeft" => {
                    need(1)?;
                    list(&a[0])?.first().cloned().unwrap_or(V::Nil)
                }
                "upperRight" => {
                    need(1)?;
                    list(&a[0])?.get(1).cloned().unwrap_or(V::Nil)
                }
                "sprintf" => {
                    if n < 2 {
                        return Err(Flow::Err("sprintf: (sprintf nil fmt args…)".into()));
                    }
                    if a[0].truthy() {
                        return Err(Flow::Err(
                            "sprintf: first argument must be nil (no variable form)".into(),
                        ));
                    }
                    V::str(&format(name, &text(&a[1])?, &a[2..])?)
                }
                "fprintf" => {
                    if n < 2 {
                        return Err(Flow::Err("fprintf: (fprintf port fmt args…)".into()));
                    }
                    let s = format(name, &text(&a[1])?, &a[2..])?;
                    self.port_out(&a[0])?.push_str(&s);
                    V::T
                }
                "printf" => {
                    if n < 1 {
                        return Err(Flow::Err("printf: (printf fmt args…)".into()));
                    }
                    let s = format(name, &text(&a[0])?, &a[1..])?;
                    self.log.push_str(&s);
                    V::T
                }
                "warn" => {
                    if n < 1 {
                        return Err(Flow::Err("warn: (warn fmt args…)".into()));
                    }
                    let s = format(name, &text(&a[0])?, &a[1..])?;
                    self.log.push_str("*WARNING* ");
                    self.log.push_str(&s);
                    V::Nil
                }
                "error" => {
                    if n < 1 {
                        return Err(Flow::Err("error: (error fmt args…)".into()));
                    }
                    return Err(Flow::Err(format(name, &text(&a[0])?, &a[1..])?));
                }
                "outfile" => {
                    if !(1..=2).contains(&n) {
                        return Err(Flow::Err("outfile: (outfile path [mode])".into()));
                    }
                    let path = text(&a[0])?;
                    if !path.starts_with('/') {
                        return Err(Flow::Err(format!(
                            "outfile: {path} is relative (SKILL path lookup); pass an absolute path"
                        )));
                    }
                    let mode = if n == 2 { text(&a[1])? } else { "w".into() };
                    let buf = match mode.as_str() {
                        "w" => String::new(),
                        "a" => self.files.get(&path).cloned().unwrap_or_default(),
                        m => return Err(Flow::Err(format!("outfile: mode {m}"))),
                    };
                    self.ports.push(Port::Out {
                        path,
                        buf,
                        open: true,
                    });
                    V::Port(self.ports.len() as u32 - 1)
                }
                "infile" => {
                    need(1)?;
                    let path = text(&a[0])?;
                    if !path.starts_with('/') {
                        return Err(Flow::Err(format!(
                            "infile: {path} is relative (SKILL path lookup); pass an absolute path"
                        )));
                    }
                    match self.files.get(&path) {
                        None => V::Nil,
                        Some(s) => {
                            let lines = s.split_inclusive('\n').map(str::to_owned).collect();
                            self.ports.push(Port::In { lines, next: 0 });
                            V::Port(self.ports.len() as u32 - 1)
                        }
                    }
                }
                "close" => {
                    need(1)?;
                    let V::Port(k) = a[0] else {
                        return Err(Flow::Err("close: expected a port".into()));
                    };
                    let p = std::mem::replace(&mut self.ports[k as usize], Port::Closed);
                    match p {
                        Port::Out { path, buf, .. } => {
                            self.files.insert(path, buf);
                        }
                        Port::In { .. } => {}
                        Port::Closed => return Err(Flow::Err("close: port already closed".into())),
                    }
                    V::T
                }
                "lineread" => {
                    need(1)?;
                    let V::Port(k) = a[0] else {
                        return Err(Flow::Err("lineread: expected a port".into()));
                    };
                    let Some(Port::In { lines, next }) = self.ports.get_mut(k as usize) else {
                        return Err(Flow::Err("lineread: not an open input port".into()));
                    };
                    if *next >= lines.len() {
                        return Ok(V::Nil);
                    }
                    let mut text = String::new();
                    loop {
                        if *next >= lines.len() {
                            return Err(Flow::Err(
                                "lineread: unbalanced parentheses at end of file".into(),
                            ));
                        }
                        text.push_str(&lines[*next]);
                        *next += 1;
                        if balanced(&text) {
                            break;
                        }
                    }
                    let forms = read_all(&text).map_err(|e| format!("lineread: {e}"))?;
                    if forms.is_empty() {
                        V::T
                    } else {
                        V::list(forms)
                    }
                }
                "fboundp" => {
                    need(1)?;
                    let f = text(&a[0])?;
                    let known = self.funcs.contains_key(f.as_str())
                        || defined_by_interp().any(|b| b == f)
                        || self.host.bound(&f);
                    if known { V::T } else { V::Nil }
                }
                "evalstring" => {
                    // Reads and evaluates every form of the string (the loader's only use: a
                    // PDK's CDF callback, spec §5.3); the value of the last.
                    need(1)?;
                    let forms = read_all(&text(&a[0])?).map_err(|e| format!("evalstring: {e}"))?;
                    let mut last = V::Nil;
                    for f in &forms {
                        last = self.eval(f)?;
                    }
                    last
                }
                "linereadstring" => {
                    // Parses (never evaluates) the first form of the string.
                    need(1)?;
                    let forms =
                        read_all(&text(&a[0])?).map_err(|e| format!("linereadstring: {e}"))?;
                    forms.into_iter().next().unwrap_or(V::Nil)
                }
                "charToInt" => {
                    // The first byte of the symbol's name (a byte-string SKILL's character code).
                    need(1)?;
                    let V::Sym(s) = &a[0] else {
                        return Err(Flow::Err("charToInt: expected a symbol".into()));
                    };
                    match s.as_bytes().first() {
                        Some(b) => V::Int(i64::from(*b)),
                        None => return Err(Flow::Err("charToInt: empty symbol".into())),
                    }
                }
                "intToChar" => {
                    need(1)?;
                    let c = int(&a[0])?;
                    if !(1..=127).contains(&c) {
                        return Err(Flow::Err(format!(
                            "intToChar: {c} is outside 1..127 (no NUL, ASCII only here)"
                        )));
                    }
                    V::sym(&char::from(c as u8).to_string())
                }
                "isFile" => {
                    need(1)?;
                    if self.files.contains_key(&text(&a[0])?) {
                        V::T
                    } else {
                        V::Nil
                    }
                }
                "simplifyFilename" => {
                    need(1)?;
                    a[0].clone()
                }
                "getShellEnvVar" => {
                    need(1)?;
                    V::Nil
                }
                "sstatus" => {
                    need(2)?;
                    V::T
                }
                "makeTable" => {
                    if !(1..=2).contains(&n) {
                        return Err(Flow::Err("makeTable: (makeTable name [default])".into()));
                    }
                    self.tables
                        .push((if n == 2 { a[1].clone() } else { V::Unbound }, Vec::new()));
                    V::Table(self.tables.len() as u32 - 1)
                }
                "arrayref" => {
                    need(2)?;
                    let V::Table(t) = a[0] else {
                        return Err(Flow::Err("arrayref: expected a table".into()));
                    };
                    let (def, rows) = &self.tables[t as usize];
                    rows.iter()
                        .find(|(k, _)| equal(k, &a[1]))
                        .map_or_else(|| def.clone(), |(_, v)| v.clone())
                }
                "setarray" => {
                    need(3)?;
                    let V::Table(t) = a[0] else {
                        return Err(Flow::Err("setarray: expected a table".into()));
                    };
                    let rows = &mut self.tables[t as usize].1;
                    match rows.iter_mut().find(|(k, _)| equal(k, &a[1])) {
                        Some(slot) => slot.1 = a[2].clone(),
                        None => rows.push((a[1].clone(), a[2].clone())),
                    }
                    a[2].clone()
                }
                "funcall" => {
                    if n < 1 {
                        return Err(Flow::Err("funcall: missing function".into()));
                    }
                    let f = self.fn_value(&a[0])?;
                    return self.apply_fn(&f, a[1..].to_vec());
                }
                "apply" => {
                    need(2)?;
                    let f = self.fn_value(&a[0])?;
                    let args = list(&a[1])?;
                    return self.apply_fn(&f, args);
                }
                "mapcar" => {
                    need(2)?;
                    let f = self.fn_value(&a[0])?;
                    let mut out = Vec::new();
                    for x in list(&a[1])? {
                        out.push(self.apply_fn(&f, vec![x])?);
                    }
                    V::list(out)
                }
                "sort" => {
                    need(2)?;
                    let mut v = list(&a[0])?;
                    let f = if a[1].truthy() {
                        Some(self.fn_value(&a[1])?)
                    } else {
                        None
                    };
                    // Stable merge sort (SKILL's is a recursive merge sort).
                    let mut err = None;
                    let mut keyed: Vec<V> = std::mem::take(&mut v);
                    let mut i = 1;
                    while i < keyed.len() {
                        let mut j = i;
                        while j > 0 {
                            let before = match &f {
                                None => text(&keyed[j])? < text(&keyed[j - 1])?,
                                Some(f) => {
                                    let lt = self
                                        .apply_fn(f, vec![keyed[j].clone(), keyed[j - 1].clone()]);
                                    match lt {
                                        Ok(x) => {
                                            x.truthy() && {
                                                // j may precede j-1 only if j-1 cannot precede j (strict order).
                                                match self.apply_fn(
                                                    f,
                                                    vec![keyed[j - 1].clone(), keyed[j].clone()],
                                                ) {
                                                    Ok(y) => !y.truthy(),
                                                    Err(e) => {
                                                        err = Some(e);
                                                        false
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            err = Some(e);
                                            false
                                        }
                                    }
                                }
                            };
                            if !before {
                                break;
                            }
                            keyed.swap(j, j - 1);
                            j -= 1;
                        }
                        if let Some(e) = err.take() {
                            return Err(e);
                        }
                        i += 1;
                    }
                    V::list(keyed)
                }
                // Retarget builtins (SKILL Language Reference ICADVM20.1).
                "log" => {
                    need(1)?;
                    let x = num(&a[0])?;
                    if x <= 0.0 {
                        return Err(Flow::Err("log: not a positive number".into()));
                    }
                    V::Float(x.ln())
                }
                "expt" => {
                    need(2)?;
                    match (&a[0], &a[1]) {
                        (V::Int(0), V::Int(0)) => V::Float(1.0),
                        (V::Int(b), V::Int(e)) if *e >= 0 => V::Int(
                            u32::try_from(*e)
                                .ok()
                                .and_then(|e| b.checked_pow(e))
                                .ok_or("expt: integer overflow")?,
                        ),
                        (b, e) => V::Float(num(b)?.powf(num(e)?)),
                    }
                }
                "atof" => {
                    if !(1..=2).contains(&n) {
                        return Err(Flow::Err("atof: (atof s [t])".into()));
                    }
                    // strtod: leading white space skipped; the longest numeric prefix, or with
                    // `t` the whole string.
                    let s = text(&a[0])?;
                    let s = s.trim_start();
                    let strict = n == 2 && a[1].truthy();
                    let parsed = if strict {
                        s.parse::<f64>().ok()
                    } else {
                        (1..=s.len())
                            .rev()
                            .filter(|&k| s.is_char_boundary(k))
                            .find_map(|k| s[..k].parse::<f64>().ok())
                    };
                    parsed.map_or(V::Nil, V::Float)
                }
                "createDir" => {
                    // Files are keyed by absolute path; directories need no existence.
                    need(1)?;
                    text(&a[0])?;
                    V::T
                }
                _ => return Err(Flow::Err(String::from("\u{0}not builtin"))),
            })
        })();
        match r {
            Err(Flow::Err(e)) if e == "\u{0}not builtin" => None,
            r => Some(r),
        }
    }
}

/// True when every `(` outside strings and comments in `text` is closed.
fn balanced(text: &str) -> bool {
    let mut depth = 0i64;
    let mut in_str = false;
    let mut esc = false;
    let mut comment = false;
    for c in text.chars() {
        if comment {
            if c == '\n' {
                comment = false;
            }
            continue;
        }
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            ';' => comment = true,
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0 && !in_str
}

/// C's `%.<p>g` of a finite float (SKILL formats through C `printf`): `p` significant
/// digits, exponent form when the exponent is below -4 or at least `p`, trailing zeros cut.
fn fmt_c_g(f: f64, p: usize) -> String {
    if !f.is_finite() {
        return if f.is_nan() {
            "nan".into()
        } else if f > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let p = p.max(1);
    let e = format!("{:.*e}", p - 1, f);
    let (mant, exp) = e.split_once('e').expect("exponent form");
    let x: i64 = exp.parse().expect("exponent");
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s.to_owned()
        }
    };
    if x < -4 || x >= p as i64 {
        let sign = if x < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mant), x.abs())
    } else {
        trim(&format!("{:.*}", (p as i64 - 1 - x) as usize, f))
    }
}

/// SKILL `sprintf` directives: `%d` (integers only — a float is an error, as SKILL
/// silently returns nil there), `%s` (string or symbol), `%L` (print form), `%c`, `%%`,
/// `%g` (floats only, C semantics, with `.precision`), with optional `-` and width.
fn format(who: &str, fmt: &str, args: &[V]) -> Result<String, Flow> {
    let mut out = String::new();
    let mut args = args.iter();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut left = false;
        let mut width = 0usize;
        let mut precision: Option<usize> = None;
        if chars.peek() == Some(&'-') {
            left = true;
            chars.next();
        }
        while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
            width = width * 10 + d as usize;
            chars.next();
        }
        if chars.peek() == Some(&'.') {
            chars.next();
            let mut p = 0usize;
            while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                p = p * 10 + d as usize;
                chars.next();
            }
            precision = Some(p);
        }
        let Some(code) = chars.next() else {
            return Err(Flow::Err(format!("{who}: format ends in %")));
        };
        if code == '%' {
            out.push('%');
            continue;
        }
        let Some(v) = args.next() else {
            return Err(Flow::Err(format!("{who}: too few arguments for {fmt:?}")));
        };
        let piece = match code {
            'd' => match v {
                V::Int(i) => i.to_string(),
                _ => {
                    return Err(Flow::Err(format!(
                        "{who}: %d of a non-integer (SKILL returns nil)"
                    )));
                }
            },
            's' => match v {
                V::Str(s) | V::Sym(s) => s.to_string(),
                _ => return Err(Flow::Err(format!("{who}: %s of a non-string"))),
            },
            'c' => match v {
                V::Str(s) => s.chars().next().map(String::from).unwrap_or_default(),
                _ => return Err(Flow::Err(format!("{who}: %c of a non-string"))),
            },
            'L' => {
                let mut s = String::new();
                print(v, &mut s);
                s
            }
            'g' => match v {
                V::Float(f) => fmt_c_g(*f, precision.unwrap_or(6)),
                _ => return Err(Flow::Err(format!("{who}: %g of a non-float"))),
            },
            other => {
                return Err(Flow::Err(format!(
                    "{who}: directive %{other} is outside the subset"
                )));
            }
        };
        let pad = width.saturating_sub(piece.chars().count());
        if left {
            out.push_str(&piece);
            out.extend(std::iter::repeat_n(' ', pad));
        } else {
            out.extend(std::iter::repeat_n(' ', pad));
            out.push_str(&piece);
        }
    }
    if args.next().is_some() {
        return Err(Flow::Err(format!("{who}: too many arguments for {fmt:?}")));
    }
    Ok(out)
}

/// Every symbol in call position in `forms` (heads of lists, `funcall`/`mapcar`/`sort`
/// targets given as `'name`), skipping data under `quote`. The static half of the lint:
/// each must be a special form, a builtin, a procedure of the script set or a Cadence
/// function the mock knows.
pub fn called_names(forms: &[V]) -> Vec<String> {
    fn walk(x: &V, out: &mut Vec<String>, in_bindings: bool) {
        let V::List(items) = x else { return };
        let Some(V::Sym(head)) = items.first() else {
            for i in items.iter() {
                walk(i, out, false);
            }
            return;
        };
        if &**head == "quote" {
            if let Some(V::Sym(s)) = items.get(1) {
                out.push(format!("'{s}"));
            }
            return;
        }
        if !in_bindings {
            out.push(head.to_string());
        }
        let rest = &items[1..];
        match &**head {
            "let" | "prog" => {
                if let Some(V::List(b)) = rest.first() {
                    for bind in b.iter() {
                        if let V::List(p) = bind
                            && let Some(init) = p.get(1)
                        {
                            walk(init, out, false);
                        }
                    }
                }
                for f in &rest[1.min(rest.len())..] {
                    walk(f, out, false);
                }
            }
            "procedure" | "lambda" => {
                for f in &rest[1.min(rest.len())..] {
                    walk(f, out, false);
                }
            }
            "cond" | "case" => {
                let start = usize::from(&**head == "case");
                if start == 1
                    && let Some(k) = rest.first()
                {
                    walk(k, out, false);
                }
                for clause in &rest[start.min(rest.len())..] {
                    if let V::List(c) = clause {
                        let skip = usize::from(&**head == "case");
                        for f in &c[skip..] {
                            walk(f, out, false);
                        }
                    }
                }
            }
            _ => {
                for f in rest {
                    walk(f, out, false);
                }
            }
        }
    }
    let mut out = Vec::new();
    for f in forms {
        walk(f, &mut out, false);
    }
    out
}
