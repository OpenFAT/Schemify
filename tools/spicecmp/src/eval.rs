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

/// One lexeme of an expression that does not evaluate as a whole.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Lex<'a> {
    /// A number literal (SI suffix applied).
    Num(f64),
    /// An identifier that can name a parameter: not a function name and not a
    /// node or device name inside `v(…)` / `i(…)`.
    Name(&'a str),
    /// Anything else, verbatim: punctuation, `"…"` strings, function names,
    /// the arguments of `v(…)` / `i(…)`, and one `" "` between two words.
    Text(&'a str),
}

/// Splits `s` into lexemes, dropping whitespace except one `" "` between two
/// adjacent words (identifiers or numbers), so spacing never matters but
/// `a b` and `ab` stay different.
pub(crate) fn lex(s: &str) -> Vec<Lex<'_>> {
    let b = s.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'.';
    let mut out: Vec<Lex> = Vec::new();
    // Paren depth inside `v(…)` / `i(…)`: names there are nodes, never params.
    let mut node_depth = 0u32;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let after_word = matches!(out.last(), Some(Lex::Num(_) | Lex::Name(_)))
                || matches!(out.last(), Some(Lex::Text(t)) if t.bytes().last().is_some_and(is_word));
            if after_word && i < b.len() && is_word(b[i]) {
                out.push(Lex::Text(" "));
            }
            continue;
        }
        if c == b'"' {
            let end = s[i + 1..].find('"').map_or(b.len(), |k| i + k + 2);
            out.push(Lex::Text(&s[i..end]));
            i = end;
            continue;
        }
        if node_depth > 0 {
            match c {
                b'(' => node_depth += 1,
                b')' => node_depth -= 1,
                _ => {}
            }
            let start = i;
            i += 1;
            while i < b.len() && is_word(b[i]) && is_word(b[start]) {
                i += 1;
            }
            out.push(Lex::Text(&s[start..i]));
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let mut lx = Lexer { s: b, pos: i };
            if let Some(v) = lx.number() {
                out.push(Lex::Num(v));
                i = lx.pos;
                continue;
            }
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && is_word(b[i]) {
                i += 1;
            }
            let name = &s[start..i];
            let mut j = i;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if b.get(j) == Some(&b'(') {
                if name.eq_ignore_ascii_case("v") || name.eq_ignore_ascii_case("i") {
                    out.push(Lex::Text(name));
                    out.push(Lex::Text("("));
                    node_depth = 1;
                    i = j + 1;
                } else {
                    out.push(Lex::Text(name));
                }
            } else {
                out.push(Lex::Name(name));
            }
            continue;
        }
        let len = s[i..].chars().next().map_or(1, char::len_utf8);
        out.push(Lex::Text(&s[i..i + len]));
        i += len;
    }
    out
}

/// Canonical text of an expression that does not evaluate as a whole (B/E/G
/// source specs, `pwl` points, text-valued params): one level of outer quotes
/// removed, numbers in [`canon_number`] form, parameter names that `lookup`
/// resolves replaced by their value, whitespace dropped (see [`lex`]).
///
/// Why: SPICE evaluates a parameter name inside such an expression in the
/// instance's scope, so `v(a)/ron` with `ron=0.1` in scope *is* `v(a)/0.1`;
/// a netlist that passes parameters (xschem) and one that specializes them
/// (Schemify) then compare equal, while any different value still differs.
pub(crate) fn canon_text<'s>(raw: &str, lookup: &impl Fn(&str) -> Option<Sub<'s>>) -> String {
    let mut out = String::with_capacity(raw.len());
    let l = lex(strip_quotes(raw.trim()));
    for (i, x) in l.iter().enumerate() {
        match *x {
            Lex::Num(x) => out.push_str(&canon_number(x)),
            // `k=` names a key (`m=1` after a B expression), never a value.
            Lex::Name(n) if is_key(&l, i) => out.push_str(n),
            Lex::Name(n) => match lookup(n) {
                Some(Sub::Num(x)) => out.push_str(&canon_number(x)),
                // A param whose value is itself text (`VTH=VTH` passed down from an
                // undefined name): its text, so the name it stands for shows.
                Some(Sub::Text(t)) if matches!(lex(t)[..], [Lex::Name(_)]) => out.push_str(t),
                Some(Sub::Text(t)) => {
                    out.push('(');
                    out.push_str(t);
                    out.push(')');
                }
                None => out.push_str(n),
            },
            Lex::Text(t) => out.push_str(t),
        }
    }
    out
}

/// What a parameter name stands for in [`canon_text`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum Sub<'s> {
    /// An evaluated value.
    Num(f64),
    /// Canonical text of a value that does not evaluate.
    Text(&'s str),
}

/// Whether lexeme `i` is followed by a single `=` (an assignment, not `==`).
pub(crate) fn is_key(l: &[Lex], i: usize) -> bool {
    l.get(i + 1) == Some(&Lex::Text("=")) && l.get(i + 2) != Some(&Lex::Text("="))
}

/// Replaces the node list of every `v(…)` in `s` by `#` per node, appending
/// the node names (trimmed) to `nodes` in order of appearance. `"…"` strings
/// are copied verbatim; `i(…)` (device names) is left alone.
pub(crate) fn take_v_nodes(s: &str, nodes: &mut Vec<String>) -> String {
    let b = s.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'.';
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            let end = s[i + 1..].find('"').map_or(b.len(), |k| i + k + 2);
            out.push_str(&s[i..end]);
            i = end;
            continue;
        }
        if (c == b'v' || c == b'V') && (i == 0 || !is_word(b[i - 1])) {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let close = (b.get(j) == Some(&b'('))
                .then(|| s[j + 1..].find(')'))
                .flatten();
            if let Some(k) = close {
                let args = &s[j + 1..j + 1 + k];
                out.push_str("v(");
                for (n, a) in args.split(',').enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    nodes.push(a.trim().to_string());
                    out.push('#');
                }
                out.push(')');
                i = j + k + 2;
                continue;
            }
        }
        let len = s[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&s[i..i + len]);
        i += len;
    }
    out
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
