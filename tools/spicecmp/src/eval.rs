//! Numeric evaluation of SPICE parameter values: SI-suffixed literals and
//! arithmetic over parameters in scope (ngspice semantics: `log` is natural log).
//! Anything else is left to text comparison by the caller.

/// Nesting limit for parentheses and unary operators; deeper input is rejected
/// instead of risking the stack on hostile text.
const DEPTH_MAX: u32 = 64;

/// Parses a SPICE number with an optional scale suffix (`1.5k`, `2meg`, `10uF`,
/// `1mil`); trailing unit letters after the scale are ignored, as ngspice does.
pub(crate) fn parse_number(s: &str) -> Option<f64> {
    let mut lexer = Lexer {
        s: s.as_bytes(),
        pos: 0,
    };
    let value = lexer.number()?;
    if lexer.pos != s.len() {
        return None;
    }
    Some(value)
}

/// Evaluates `expr` (outer `'…'` / `{…}` already stripped or not) with
/// identifiers resolved through `lookup`; `None` when the text is not pure
/// arithmetic over known names or the result is not finite.
pub(crate) fn eval(expr: &str, lookup: &impl Fn(&str) -> Option<f64>) -> Option<f64> {
    let inner = strip_quotes(expr.trim());
    let mut parser = Parser {
        lex: Lexer {
            s: inner.as_bytes(),
            pos: 0,
        },
        lookup,
        depth: 0,
    };
    let value = parser.expr()?;
    parser.lex.skip_ws();
    if parser.lex.pos != inner.len() || !value.is_finite() {
        return None;
    }
    Some(value)
}

/// Canonical text for a number: 10 significant digits, so printing differences
/// below that (xschem's `%.15g` vs ours) compare equal.
// ponytail: fixed 10-digit rounding; two values straddling a rounding boundary
// compare unequal. Upgrade path: tolerance-aware matching instead of hashing.
pub(crate) fn canon_number(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    // Round to 10 digits, then print the shortest text for that value.
    let rounded: f64 = format!("{x:.9e}").parse().expect("Rust float text parses");
    format!("{rounded:e}")
}

/// Removes one level of `'…'` or `{…}` quoting.
pub(crate) fn strip_quotes(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'\'' && b[b.len() - 1] == b'\'') || (b[0] == b'{' && b[b.len() - 1] == b'}'))
    {
        return &s[1..s.len() - 1];
    }
    s
}

struct Lexer<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Lexer<'_> {
    fn skip_ws(&mut self) {
        while self.pos < self.s.len() && self.s[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            return true;
        }
        false
    }

    /// Mantissa, optional exponent, optional scale suffix and unit letters.
    fn number(&mut self) -> Option<f64> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if self.pos == start || (self.pos == start + 1 && self.s[start] == b'.') {
            self.pos = start;
            return None;
        }
        // Exponent only when digits follow, so `1e` stays "1" + unit letter.
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        let mantissa: f64 = std::str::from_utf8(&self.s[start..self.pos])
            .ok()?
            .parse()
            .ok()?;
        let suffix_start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        let suffix = self.s[suffix_start..self.pos].to_ascii_lowercase();
        let scale = if suffix.starts_with(b"meg") {
            1e6
        } else if suffix.starts_with(b"mil") {
            25.4e-6
        } else {
            match suffix.first() {
                Some(b't') => 1e12,
                Some(b'g') => 1e9,
                Some(b'k') => 1e3,
                Some(b'm') => 1e-3,
                Some(b'u') => 1e-6,
                Some(b'n') => 1e-9,
                Some(b'p') => 1e-12,
                Some(b'f') => 1e-15,
                Some(b'a') => 1e-18,
                _ => 1.0,
            }
        };
        Some(mantissa * scale)
    }

    fn ident(&mut self) -> Option<&str> {
        let start = self.pos;
        if !self
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        {
            return None;
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
        {
            self.pos += 1;
        }
        std::str::from_utf8(&self.s[start..self.pos]).ok()
    }
}

struct Parser<'a, F> {
    lex: Lexer<'a>,
    lookup: &'a F,
    depth: u32,
}

impl<F: Fn(&str) -> Option<f64>> Parser<'_, F> {
    fn expr(&mut self) -> Option<f64> {
        let mut acc = self.term()?;
        loop {
            if self.lex.eat(b'+') {
                acc += self.term()?;
            } else if self.lex.eat(b'-') {
                acc -= self.term()?;
            } else {
                return Some(acc);
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut acc = self.unary()?;
        loop {
            self.lex.skip_ws();
            let at_pow = self.lex.s.get(self.lex.pos..self.lex.pos + 2) == Some(b"**");
            if !at_pow && self.lex.eat(b'*') {
                acc *= self.unary()?;
            } else if self.lex.eat(b'/') {
                acc /= self.unary()?;
            } else {
                return Some(acc);
            }
        }
    }

    /// Unary sign binds looser than `^`: `-2^2` is -4.
    fn unary(&mut self) -> Option<f64> {
        self.depth += 1;
        let v = if self.depth > DEPTH_MAX {
            None
        } else if self.lex.eat(b'-') {
            self.unary().map(|x| -x)
        } else if self.lex.eat(b'+') {
            self.unary()
        } else {
            self.power()
        };
        self.depth -= 1;
        v
    }

    /// Right-associative `^` / `**`; the exponent may carry a sign (`2^-1`).
    fn power(&mut self) -> Option<f64> {
        let base = self.primary()?;
        self.lex.skip_ws();
        if self.lex.eat(b'^') {
            return Some(base.powf(self.unary()?));
        }
        if self.lex.s.get(self.lex.pos..self.lex.pos + 2) == Some(b"**") {
            self.lex.pos += 2;
            return Some(base.powf(self.unary()?));
        }
        Some(base)
    }

    fn primary(&mut self) -> Option<f64> {
        self.lex.skip_ws();
        if self.lex.eat(b'(') {
            let v = self.expr()?;
            return self.lex.eat(b')').then_some(v);
        }
        if let Some(v) = self.lex.number() {
            return Some(v);
        }
        let name = self.lex.ident()?.to_ascii_lowercase();
        if !self.lex.eat(b'(') {
            if name == "pi" {
                return Some(std::f64::consts::PI);
            }
            return (self.lookup)(&name);
        }
        let mut args: [f64; 2] = [0.0; 2];
        let mut n = 0usize;
        if !self.lex.eat(b')') {
            loop {
                let v = self.expr()?;
                if n == args.len() {
                    return None;
                }
                args[n] = v;
                n += 1;
                if self.lex.eat(b')') {
                    break;
                }
                if !self.lex.eat(b',') {
                    return None;
                }
            }
        }
        call(&name, &args[..n])
    }
}

fn call(name: &str, args: &[f64]) -> Option<f64> {
    let v = match (name, args) {
        ("int", [x]) => x.trunc(),
        ("nint" | "round", [x]) => x.round(),
        ("floor", [x]) => x.floor(),
        ("ceil", [x]) => x.ceil(),
        ("abs", [x]) => x.abs(),
        ("sqrt", [x]) => x.sqrt(),
        ("exp", [x]) => x.exp(),
        ("ln" | "log", [x]) => x.ln(),
        ("log10", [x]) => x.log10(),
        ("sin", [x]) => x.sin(),
        ("cos", [x]) => x.cos(),
        ("tan", [x]) => x.tan(),
        ("atan", [x]) => x.atan(),
        ("min", [a, b]) => a.min(*b),
        ("max", [a, b]) => a.max(*b),
        ("pow" | "pwr", [a, b]) => a.powf(*b),
        _ => return None,
    };
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<f64> {
        None
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * a.abs().max(b.abs()).max(1e-30)
    }

    #[test]
    fn numbers_with_suffixes() {
        let cases = [
            ("1k", 1e3),
            ("1K", 1e3),
            ("2meg", 2e6),
            ("2MEG", 2e6),
            ("1mil", 25.4e-6),
            ("1m", 1e-3),
            ("10uF", 10e-6),
            ("0.15u", 0.15e-6),
            ("1e-3", 1e-3),
            ("1.5e3k", 1.5e6),
            ("3", 3.0),
            (".5", 0.5),
            ("5.", 5.0),
            ("1f", 1e-15),
            ("4v", 4.0),
        ];
        for (text, want) in cases {
            let got = parse_number(text).unwrap_or_else(|| panic!("{text}"));
            assert!(close(got, want), "{text}: {got} != {want}");
        }
        for bad in ["", ".", "k", "1 k", "1+2", "e3"] {
            assert_eq!(parse_number(bad), None, "{bad}");
        }
    }

    #[test]
    fn arithmetic_and_scope() {
        let scope = |n: &str| match n {
            "w" => Some(2e-6),
            "nf" => Some(2.0),
            _ => None,
        };
        let cases = [
            ("'int((nf+1)/2) * W/nf * 0.18u'", 1.0 * 2e-6 / 2.0 * 0.18e-6),
            ("{2*w}", 4e-6),
            ("-2^2", -4.0),
            ("2**3**2", 512.0),
            ("max(1, 3) + min(2,5)", 5.0),
            ("int(-1.5)", -1.0),
            ("log(exp(2))", 2.0),
            ("1/(1/9.8e-10)", 9.8e-10),
        ];
        for (text, want) in cases {
            let got = eval(text, &scope).unwrap_or_else(|| panic!("{text}"));
            assert!(close(got, want), "{text}: {got} != {want}");
        }
        for bad in ["w_n", "1/0", "2*", "(1", "foo(1)", "1 2", "'1'2'"] {
            assert_eq!(eval(bad, &none), None, "{bad}");
        }
    }

    #[test]
    fn deep_nesting_is_rejected_not_overflowed() {
        let deep = format!("{}1{}", "(".repeat(10_000), ")".repeat(10_000));
        assert_eq!(eval(&deep, &none), None);
        let unary = format!("{}1", "-".repeat(10_000));
        assert_eq!(eval(&unary, &none), None);
        assert_eq!(eval("((1))", &none), Some(1.0));
    }

    #[test]
    fn canonical_text_absorbs_print_precision() {
        assert_eq!(canon_number(1e-6), canon_number(0.000001000000000000001));
        assert_eq!(canon_number(0.0), "0");
        assert_eq!(canon_number(-0.0), "0");
        assert_ne!(canon_number(1.0), canon_number(1.001));
    }
}
