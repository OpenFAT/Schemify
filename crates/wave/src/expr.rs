//! Math expression engine for derived traces.
//!
//! `db(V(out)/V(in))`, `V(out) - V(in)`, `diff(V(out))/diff(time)`,
//! `fft(V(out))`, `mag(...)`, `ph(...)`, with SPICE SI-suffix literals
//! (`5n`, `2.5meg`).
//!
//! Pipeline: lexer → Pratt parser → `Expr` AST → `eval` over columns.
//! Evaluation is complex-domain throughout (AC data); functions operate
//! per sweep step so `diff`/`fft` never leak across step boundaries.

use crate::data::RawPlot;
use crate::si::parse_si;
use core::ops::Range;
use rustfft::{FftPlanner, num_complex::Complex64};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum ExprError {
    #[error("unexpected character `{0}`")]
    Lex(char),
    #[error("unexpected token `{0}`")]
    Parse(String),
    #[error("unexpected end of expression")]
    Eof,
    #[error("unknown signal `{0}`")]
    UnknownSignal(String),
    #[error("function `{0}` requires a signal argument")]
    Arity(&'static str),
    #[error("length mismatch between operands")]
    LengthMismatch,
    #[error("fft requires at least 2 points per step")]
    FftTooShort,
}

// ════════════════════════════════════════════════════════════
// AST
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    Db,
    Mag, // also `abs`
    Ph,  // also `phase`; degrees
    Re,
    Im,
    Diff,
    Fft,
    Sqrt,
    Log10,
    Ln,
    Exp,
    Sin,
    Cos,
    Tan,
}

impl Func {
    fn from_name(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "db" => Func::Db,
            "mag" | "abs" => Func::Mag,
            "ph" | "phase" => Func::Ph,
            "re" | "real" => Func::Re,
            "im" | "imag" => Func::Im,
            "diff" | "d" => Func::Diff,
            "fft" => Func::Fft,
            "sqrt" => Func::Sqrt,
            "log" | "log10" => Func::Log10,
            "ln" => Func::Ln,
            "exp" => Func::Exp,
            "sin" => Func::Sin,
            "cos" => Func::Cos,
            "tan" => Func::Tan,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Func::Db => "db",
            Func::Mag => "mag",
            Func::Ph => "ph",
            Func::Re => "re",
            Func::Im => "im",
            Func::Diff => "diff",
            Func::Fft => "fft",
            Func::Sqrt => "sqrt",
            Func::Log10 => "log10",
            Func::Ln => "ln",
            Func::Exp => "exp",
            Func::Sin => "sin",
            Func::Cos => "cos",
            Func::Tan => "tan",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(f64),
    /// Signal reference, e.g. `v(out)` or `time` — name kept verbatim.
    Signal(String),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Call(Func, Box<Expr>),
}

// ════════════════════════════════════════════════════════════
// Lexer
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
}

fn lex(src: &str) -> Result<Vec<Tok>, ExprError> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        match c {
            ' ' | '\t' | '\r' | '\n' => i += 1,
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            '*' => {
                out.push(Tok::Star);
                i += 1;
            }
            '/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            '^' => {
                out.push(Tok::Caret);
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '0'..='9' | '.' => {
                // Number with optional exponent and SI suffix: `2.5meg`,
                // `1e-9`, `10n`. Greedy: digits/dot, exponent, alpha tail.
                let start = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                if i < b.len()
                    && (b[i] == b'e' || b[i] == b'E')
                    && i + 1 < b.len()
                    && (b[i + 1].is_ascii_digit() || b[i + 1] == b'+' || b[i + 1] == b'-')
                {
                    i += 2;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                while i < b.len() && (b[i] as char).is_ascii_alphabetic() {
                    i += 1;
                }
                let txt = &src[start..i];
                out.push(Tok::Num(parse_si(txt).ok_or(ExprError::Lex(c))?));
            }
            _ if c.is_ascii_alphabetic() || c == '_' || c == '@' => {
                // Identifier; SPICE signal names allow `.`, `:`, `#`, `[`,
                // `]`, digits — everything except operators and parens.
                let start = i;
                while i < b.len() {
                    let ch = b[i] as char;
                    if ch.is_ascii_alphanumeric() || "_.:#[]@".contains(ch) {
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(Tok::Ident(src[start..i].to_string()));
            }
            _ => return Err(ExprError::Lex(c)),
        }
    }
    Ok(out)
}

// ════════════════════════════════════════════════════════════
// Parser — Pratt, standard precedence (+- < */ < ^ < unary)
// ════════════════════════════════════════════════════════════

pub fn parse_expr(src: &str) -> Result<Expr, ExprError> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.expr(0)?;
    if p.pos != p.toks.len() {
        return Err(ExprError::Parse(format!("{:?}", p.toks[p.pos])));
    }
    Ok(e)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Result<Tok, ExprError> {
        let t = self.toks.get(self.pos).cloned().ok_or(ExprError::Eof)?;
        self.pos += 1;
        Ok(t)
    }

    fn expect(&mut self, t: Tok) -> Result<(), ExprError> {
        let got = self.next()?;
        if got == t {
            Ok(())
        } else {
            Err(ExprError::Parse(format!("{got:?}")))
        }
    }

    fn expr(&mut self, min_bp: u8) -> Result<Expr, ExprError> {
        let mut lhs = self.atom()?;
        loop {
            let (op, bp) = match self.peek() {
                Some(Tok::Plus) => (BinOp::Add, 1),
                Some(Tok::Minus) => (BinOp::Sub, 1),
                Some(Tok::Star) => (BinOp::Mul, 3),
                Some(Tok::Slash) => (BinOp::Div, 3),
                Some(Tok::Caret) => (BinOp::Pow, 5),
                _ => break,
            };
            if bp < min_bp {
                break;
            }
            self.pos += 1;
            // Left-assoc: rhs binds at bp+1. (^ right-assoc: rhs at bp.)
            let rhs_bp = if op == BinOp::Pow { bp } else { bp + 1 };
            let rhs = self.expr(rhs_bp)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn atom(&mut self) -> Result<Expr, ExprError> {
        match self.next()? {
            Tok::Num(v) => Ok(Expr::Num(v)),
            Tok::Minus => Ok(Expr::Neg(Box::new(self.expr(7)?))),
            Tok::Plus => self.expr(7),
            Tok::LParen => {
                let e = self.expr(0)?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Tok::Ident(name) => {
                if self.peek() == Some(&Tok::LParen) {
                    // `name(...)`: a known function, or a signal reference
                    // like `v(out)` / `i(R1)` whose name includes the parens.
                    if let Some(f) = Func::from_name(&name) {
                        self.pos += 1; // consume (
                        let arg = self.expr(0)?;
                        self.expect(Tok::RParen)?;
                        return Ok(Expr::Call(f, Box::new(arg)));
                    }
                    // Signal form: ident ( ident ) — reconstruct verbatim.
                    self.pos += 1; // consume (
                    let inner = match self.next()? {
                        Tok::Ident(s) => s,
                        t => return Err(ExprError::Parse(format!("{t:?}"))),
                    };
                    self.expect(Tok::RParen)?;
                    Ok(Expr::Signal(format!("{name}({inner})")))
                } else {
                    // Bare signal: `time`, `frequency`, node name.
                    Ok(Expr::Signal(name))
                }
            }
            t => Err(ExprError::Parse(format!("{t:?}"))),
        }
    }
}

// ════════════════════════════════════════════════════════════
// Evaluation
// ════════════════════════════════════════════════════════════

/// Evaluated column. `domain` is `Some` only when the function changed the
/// domain (currently just `fft`): new x column plus remapped step ranges.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalResult {
    pub re: Vec<f64>,
    /// Empty when the result is purely real.
    pub im: Vec<f64>,
    pub domain: Option<(Vec<f64>, Vec<Range<u32>>)>,
}

impl EvalResult {
    fn is_complex(&self) -> bool {
        !self.im.is_empty()
    }
}

/// Internal value: scalar or column. Scalars broadcast over columns.
enum Value {
    Scalar(f64, f64),
    Col(EvalResult),
}

pub fn eval(expr: &Expr, src: &RawPlot) -> Result<EvalResult, ExprError> {
    match eval_value(expr, src)? {
        Value::Col(c) => Ok(c),
        // A constant expression: single-point column.
        Value::Scalar(re, im) => Ok(EvalResult {
            re: vec![re],
            im: if im != 0.0 { vec![im] } else { vec![] },
            domain: None,
        }),
    }
}

fn eval_value(expr: &Expr, src: &RawPlot) -> Result<Value, ExprError> {
    match expr {
        Expr::Num(v) => Ok(Value::Scalar(*v, 0.0)),
        Expr::Signal(name) => {
            let v = src
                .find_var(name)
                .ok_or_else(|| ExprError::UnknownSignal(name.clone()))?;
            Ok(Value::Col(EvalResult {
                re: src.col(v).to_vec(),
                im: if src.complex {
                    src.col_im(v).to_vec()
                } else {
                    vec![]
                },
                domain: None,
            }))
        }
        Expr::Neg(e) => {
            let v = eval_value(e, src)?;
            Ok(match v {
                Value::Scalar(re, im) => Value::Scalar(-re, -im),
                Value::Col(mut c) => {
                    for x in &mut c.re {
                        *x = -*x;
                    }
                    for x in &mut c.im {
                        *x = -*x;
                    }
                    Value::Col(c)
                }
            })
        }
        Expr::Bin(op, a, b) => {
            let a = eval_value(a, src)?;
            let b = eval_value(b, src)?;
            eval_bin(*op, a, b)
        }
        Expr::Call(f, arg) => eval_call(*f, arg, src),
    }
}

#[inline]
fn c_bin(op: BinOp, ar: f64, ai: f64, br: f64, bi: f64) -> (f64, f64) {
    match op {
        BinOp::Add => (ar + br, ai + bi),
        BinOp::Sub => (ar - br, ai - bi),
        BinOp::Mul => (ar * br - ai * bi, ar * bi + ai * br),
        BinOp::Div => {
            let d = br * br + bi * bi;
            ((ar * br + ai * bi) / d, (ai * br - ar * bi) / d)
        }
        BinOp::Pow => {
            if ai == 0.0 && bi == 0.0 && (ar >= 0.0 || br.fract() == 0.0) {
                (ar.powf(br), 0.0)
            } else {
                let z = Complex64::new(ar, ai).powc(Complex64::new(br, bi));
                (z.re, z.im)
            }
        }
    }
}

fn eval_bin(op: BinOp, a: Value, b: Value) -> Result<Value, ExprError> {
    use Value::*;
    Ok(match (a, b) {
        (Scalar(ar, ai), Scalar(br, bi)) => {
            let (re, im) = c_bin(op, ar, ai, br, bi);
            Scalar(re, im)
        }
        (Col(c), Scalar(br, bi)) => Col(map_col(op, c, br, bi, false)),
        (Scalar(ar, ai), Col(c)) => Col(map_col(op, c, ar, ai, true)),
        (Col(a), Col(b)) => {
            if a.re.len() != b.re.len() {
                return Err(ExprError::LengthMismatch);
            }
            let n = a.re.len();
            let complex = a.is_complex() || b.is_complex();
            let mut re = vec![0.0; n];
            let mut im = if complex { vec![0.0; n] } else { vec![] };
            for i in 0..n {
                let ai = a.im.get(i).copied().unwrap_or(0.0);
                let bi = b.im.get(i).copied().unwrap_or(0.0);
                let (r, m) = c_bin(op, a.re[i], ai, b.re[i], bi);
                re[i] = r;
                if complex {
                    im[i] = m;
                }
            }
            // Domain metadata survives if either side carries it (fft(a)/n).
            Col(EvalResult {
                re,
                im,
                domain: a.domain.or(b.domain),
            })
        }
    })
}

/// Column ⊕ scalar (or scalar ⊕ column when `flip`).
fn map_col(op: BinOp, mut c: EvalResult, sr: f64, si: f64, flip: bool) -> EvalResult {
    let n = c.re.len();
    let complex = c.is_complex() || si != 0.0;
    if complex && c.im.is_empty() {
        c.im = vec![0.0; n];
    }
    for i in 0..n {
        let (ar, ai) = (c.re[i], c.im.get(i).copied().unwrap_or(0.0));
        let (r, m) = if flip {
            c_bin(op, sr, si, ar, ai)
        } else {
            c_bin(op, ar, ai, sr, si)
        };
        c.re[i] = r;
        if complex {
            c.im[i] = m;
        }
    }
    c
}

fn eval_call(f: Func, arg: &Expr, src: &RawPlot) -> Result<Value, ExprError> {
    let v = eval_value(arg, src)?;
    // Scalars: only the pointwise functions make sense.
    let mut c = match v {
        Value::Scalar(re, im) => {
            return Ok(match f {
                Func::Db => Value::Scalar(20.0 * (re * re + im * im).sqrt().log10(), 0.0),
                Func::Mag => Value::Scalar((re * re + im * im).sqrt(), 0.0),
                Func::Ph => Value::Scalar(im.atan2(re).to_degrees(), 0.0),
                Func::Re => Value::Scalar(re, 0.0),
                Func::Im => Value::Scalar(im, 0.0),
                Func::Sqrt => Value::Scalar(re.sqrt(), 0.0),
                Func::Log10 => Value::Scalar(re.log10(), 0.0),
                Func::Ln => Value::Scalar(re.ln(), 0.0),
                Func::Exp => Value::Scalar(re.exp(), 0.0),
                Func::Sin => Value::Scalar(re.sin(), 0.0),
                Func::Cos => Value::Scalar(re.cos(), 0.0),
                Func::Tan => Value::Scalar(re.tan(), 0.0),
                Func::Diff | Func::Fft => return Err(ExprError::Arity(f.name())),
            });
        }
        Value::Col(c) => c,
    };

    match f {
        Func::Db => {
            for i in 0..c.re.len() {
                let im = c.im.get(i).copied().unwrap_or(0.0);
                c.re[i] = 20.0 * (c.re[i] * c.re[i] + im * im).sqrt().log10();
            }
            c.im.clear();
        }
        Func::Mag => {
            for i in 0..c.re.len() {
                let im = c.im.get(i).copied().unwrap_or(0.0);
                c.re[i] = (c.re[i] * c.re[i] + im * im).sqrt();
            }
            c.im.clear();
        }
        Func::Ph => {
            for i in 0..c.re.len() {
                let im = c.im.get(i).copied().unwrap_or(0.0);
                c.re[i] = im.atan2(c.re[i]).to_degrees();
            }
            c.im.clear();
        }
        Func::Re => c.im.clear(),
        Func::Im => {
            if c.im.is_empty() {
                c.re.iter_mut().for_each(|x| *x = 0.0);
            } else {
                core::mem::swap(&mut c.re, &mut c.im);
                c.im.clear();
            }
        }
        Func::Sqrt | Func::Log10 | Func::Ln | Func::Exp | Func::Sin | Func::Cos | Func::Tan => {
            let g: fn(f64) -> f64 = match f {
                Func::Sqrt => f64::sqrt,
                Func::Log10 => f64::log10,
                Func::Ln => f64::ln,
                Func::Exp => f64::exp,
                Func::Sin => f64::sin,
                Func::Cos => f64::cos,
                _ => f64::tan,
            };
            c.re.iter_mut().for_each(|x| *x = g(*x));
            c.im.clear(); // real-only functions drop the imaginary part
        }
        Func::Diff => {
            // Successive difference per sweep step, same length:
            // d[i] = y[i] - y[i-1], d[first] = d[first+1] (so
            // diff(V)/diff(time) yields a sane derivative everywhere).
            let steps: Vec<Range<u32>> = match &c.domain {
                Some((_, s)) => s.clone(),
                None => src.steps.clone(),
            };
            diff_in_place(&mut c.re, &steps);
            if !c.im.is_empty() {
                diff_in_place(&mut c.im, &steps);
            }
        }
        Func::Fft => {
            return fft_col(&c, src).map(Value::Col);
        }
    }
    Ok(Value::Col(c))
}

fn diff_in_place(y: &mut [f64], steps: &[Range<u32>]) {
    for s in steps {
        let (a, b) = (s.start as usize, (s.end as usize).min(y.len()));
        if b - a < 2 {
            if b > a {
                y[a] = 0.0;
            }
            continue;
        }
        let mut prev = y[a];
        for v in y[a + 1..b].iter_mut() {
            let cur = *v;
            *v = cur - prev;
            prev = cur;
        }
        y[a] = y[a + 1];
    }
}

/// FFT per sweep step: linear-resample onto a uniform grid (next pow2 of the
/// step length), transform, keep the positive half as magnitude+phase
/// (complex output). X becomes frequency bins. Steps are remapped to the
/// concatenated output ranges.
fn fft_col(c: &EvalResult, src: &RawPlot) -> Result<EvalResult, ExprError> {
    let x = src.scale();
    let steps: Vec<Range<u32>> = match &c.domain {
        Some((_, s)) => s.clone(),
        None => src.steps.clone(),
    };
    if c.re.len() != x.len() {
        return Err(ExprError::LengthMismatch);
    }

    let mut planner = FftPlanner::new();
    let mut out_re = Vec::new();
    let mut out_im = Vec::new();
    let mut out_x = Vec::new();
    let mut out_steps = Vec::new();

    for s in &steps {
        let (a, b) = (s.start as usize, (s.end as usize).min(c.re.len()));
        let n = b - a;
        if n < 2 {
            return Err(ExprError::FftTooShort);
        }
        let (xs, ys) = (&x[a..b], &c.re[a..b]);
        let span = xs[n - 1] - xs[0];
        if span <= 0.0 {
            return Err(ExprError::FftTooShort);
        }
        let m = n.next_power_of_two();
        // Uniform resample (transient timesteps are non-uniform).
        let mut buf: Vec<Complex64> = (0..m)
            .map(|i| {
                let t = xs[0] + span * i as f64 / (m - 1) as f64;
                Complex64::new(lerp_at(xs, ys, t), 0.0)
            })
            .collect();
        planner.plan_fft_forward(m).process(&mut buf);

        let half = m / 2;
        let start = out_re.len() as u32;
        let df = 1.0 / span;
        let scale = 2.0 / m as f64; // single-sided amplitude
        for (k, v) in buf.iter().take(half).enumerate() {
            out_x.push(k as f64 * df);
            out_re.push(v.re * scale);
            out_im.push(v.im * scale);
        }
        out_steps.push(start..out_re.len() as u32);
    }

    Ok(EvalResult {
        re: out_re,
        im: out_im,
        domain: Some((out_x, out_steps)),
    })
}

/// Linear interpolation of (xs, ys) at t. xs must be non-decreasing.
fn lerp_at(xs: &[f64], ys: &[f64], t: f64) -> f64 {
    match xs.binary_search_by(|v| v.partial_cmp(&t).unwrap_or(core::cmp::Ordering::Less)) {
        Ok(i) => ys[i],
        Err(0) => ys[0],
        Err(i) if i >= xs.len() => ys[ys.len() - 1],
        Err(i) => {
            let (x0, x1) = (xs[i - 1], xs[i]);
            let f = if x1 > x0 { (t - x0) / (x1 - x0) } else { 0.0 };
            ys[i - 1] + (ys[i] - ys[i - 1]) * f
        }
    }
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    use crate::data::{VarKind, Variable};

    /// Real plot with columns time, v(out), v(in).
    fn plot(time: Vec<f64>, vout: Vec<f64>, vin: Vec<f64>, steps: Vec<Range<u32>>) -> RawPlot {
        let n = time.len() as u32;
        RawPlot {
            plotname: String::new(),
            complex: false,
            variables: ["time", "v(out)", "v(in)"]
                .map(|name| Variable {
                    name: name.into(),
                    kind: VarKind::Other,
                })
                .into(),
            n_points: n,
            re: [time, vout, vin].concat(),
            im: vec![],
            steps,
        }
    }

    fn src() -> RawPlot {
        plot(
            vec![0.0, 1.0, 2.0, 3.0],
            vec![2.0, 4.0, 6.0, 8.0],
            vec![1.0, 2.0, 3.0, 4.0],
            vec![0..4],
        )
    }

    #[test]
    fn arith_and_signals() {
        let e = parse_expr("V(out) - V(in)").unwrap();
        let r = eval(&e, &src()).unwrap();
        assert_eq!(r.re, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn precedence_and_si_literals() {
        let e = parse_expr("v(in) * 2 + 1k").unwrap();
        let r = eval(&e, &src()).unwrap();
        assert_eq!(r.re, vec![1002.0, 1004.0, 1006.0, 1008.0]);

        let e = parse_expr("2 + 3 * 4 ^ 2").unwrap();
        let r = eval(&e, &src()).unwrap();
        assert_eq!(r.re, vec![50.0]);
    }

    #[test]
    fn db_of_ratio() {
        let e = parse_expr("db(V(out) / V(in))").unwrap();
        let r = eval(&e, &src()).unwrap();
        // ratio = 2 everywhere → 20*log10(2) ≈ 6.0206
        for v in r.re {
            assert!((v - 6.020599913279624).abs() < 1e-12);
        }
    }

    #[test]
    fn diff_derivative() {
        let e = parse_expr("diff(V(out)) / diff(time)").unwrap();
        let r = eval(&e, &src()).unwrap();
        assert_eq!(r.re, vec![2.0, 2.0, 2.0, 2.0]); // slope 2 everywhere
    }

    #[test]
    fn diff_respects_steps() {
        let s = plot(
            vec![0.0, 1.0, 0.0, 1.0],
            vec![0.0, 10.0, 100.0, 110.0],
            vec![0.0; 4],
            vec![0..2, 2..4],
        );
        let e = parse_expr("diff(v(out))").unwrap();
        let r = eval(&e, &s).unwrap();
        // No 90-unit spike across the step boundary.
        assert_eq!(r.re, vec![10.0, 10.0, 10.0, 10.0]);
    }

    #[test]
    fn unary_neg_and_unknown_signal() {
        let e = parse_expr("-v(in)").unwrap();
        let r = eval(&e, &src()).unwrap();
        assert_eq!(r.re, vec![-1.0, -2.0, -3.0, -4.0]);

        let e = parse_expr("v(nope)").unwrap();
        assert_eq!(
            eval(&e, &src()),
            Err(ExprError::UnknownSignal("v(nope)".into()))
        );
    }

    #[test]
    fn fft_finds_tone() {
        // 64 samples of sin(2π·8t) over 1s → peak at bin 8.
        let n = 64;
        let time: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64).collect();
        let vout: Vec<f64> = time
            .iter()
            .map(|t| (2.0 * std::f64::consts::PI * 8.0 * t).sin())
            .collect();
        let s = plot(time, vout, vec![0.0; n], vec![0..n as u32]);
        let e = parse_expr("mag(fft(v(out)))").unwrap();
        let r = eval(&e, &s).unwrap();
        let (x, _) = r.domain.unwrap();
        let peak =
            r.re.iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .unwrap()
                .0;
        assert!((x[peak] - 8.0).abs() < 1.5, "peak at {} Hz", x[peak]);
        assert!(r.re[peak] > 0.5);
    }

    #[test]
    fn complex_ac_db_phase() {
        // Columns: frequency, v(out); im has a (zero) column per variable.
        let s = RawPlot {
            plotname: String::new(),
            complex: true,
            variables: ["frequency", "v(out)"]
                .map(|name| Variable {
                    name: name.into(),
                    kind: VarKind::Other,
                })
                .into(),
            n_points: 2,
            re: vec![1.0, 10.0, 0.0, 1.0],
            im: vec![0.0, 0.0, 1.0, -1.0],
            steps: vec![0..2],
        };
        let r = eval(&parse_expr("db(v(out))").unwrap(), &s).unwrap();
        assert!((r.re[0] - 0.0).abs() < 1e-12); // |j| = 1 → 0 dB
        let r = eval(&parse_expr("ph(v(out))").unwrap(), &s).unwrap();
        assert!((r.re[0] - 90.0).abs() < 1e-12);
        assert!((r.re[1] + 45.0).abs() < 1e-12);
    }

    #[test]
    fn parse_errors() {
        assert!(parse_expr("v(out").is_err());
        assert!(parse_expr("1 +").is_err());
        assert!(parse_expr("$bad").is_err());
        assert!(parse_expr("").is_err());
    }
}
