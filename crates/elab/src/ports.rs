//! Port lists of leaf impl bodies (format rank 5 `body <impl>`): the ordered `(name, dir)`
//! a Verilog / Verilog-A `module` header or a SPICE `.subckt` line declares. The cell
//! settings panel syncs pins from it; the D11 `E002` check ("every impl matches the cell's
//! pins") can read bodies through it later.
//!
//! Data design: in, one body's text (a cell's definition, kilobytes); out, ≤ [`PORTS_MAX`]
//! ports in declaration order. One pass to tokens (`O(text)`), one over the tokens; nothing
//! outlives the call.

use schemify_doc::PinDir;

/// Most ports a body may declare: the `pins` table capacity (format rank 1, 2^12).
pub const PORTS_MAX: usize = 1 << 12;

/// Why a body has no readable port list, at 1-based `line` of the body text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortError {
    /// 1-based line in the body text (1 when the whole text is at fault).
    pub line: u32,
    /// What is wrong, in words.
    pub why: String,
}

impl std::fmt::Display for PortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.why)
    }
}

fn err(line: u32, why: impl Into<String>) -> PortError {
    PortError {
        line,
        why: why.into(),
    }
}

/// Reads the ports an impl body of language `lang` declares, in port order.
///
/// `verilog` and `veriloga`: the first `module` (ANSI `module m (input a, output [3:0] d)`,
/// a bus becoming `d[3:0]`; or non-ANSI `module m (a, d); input a; ...`, where a port with
/// no `input`/`output`/`inout` declaration, e.g. only `electrical`, is `inout`). Comments,
/// `(* attributes *)`, a `#(...)` parameter list and `` ` `` directive lines are skipped.
/// `spice`: the first `.subckt` (any case, `+` continuation lines); every node is `inout`
/// and the list ends at `params:` or the first `name=value`.
///
/// Errors (nothing partial is returned): another language, no `module`/`.subckt`, an
/// unterminated port or parameter list, a duplicate port, more than [`PORTS_MAX`] ports.
pub fn impl_ports(lang: &str, text: &str) -> Result<Vec<(String, PinDir)>, PortError> {
    let ports = match lang {
        "verilog" | "veriloga" => verilog_ports(text)?,
        "spice" => spice_ports(text)?,
        _ => return Err(err(1, format!("a {lang} impl declares no ports"))),
    };
    assert!(ports.len() <= PORTS_MAX);
    debug_assert!(
        ports
            .iter()
            .enumerate()
            .all(|(i, p)| ports[..i].iter().all(|q| q.0 != p.0))
    );
    Ok(ports)
}

fn push(
    ports: &mut Vec<(String, PinDir)>,
    name: String,
    dir: PinDir,
    line: u32,
) -> Result<(), PortError> {
    if ports.iter().any(|p| p.0 == name) {
        return Err(err(line, format!("duplicate port {name}")));
    }
    if ports.len() == PORTS_MAX {
        return Err(err(line, format!("more than {PORTS_MAX} ports")));
    }
    ports.push((name, dir));
    Ok(())
}

// ------------------------------------------------------------------------ SPICE

fn spice_ports(text: &str) -> Result<Vec<(String, PinDir)>, PortError> {
    // Logical lines: a `+` line continues the one before.
    let mut logical: Vec<(u32, String)> = Vec::new();
    for (k, raw) in text.lines().enumerate() {
        let line = raw.trim_start();
        let line = line[..line.find([';', '$']).unwrap_or(line.len())].trim_end();
        if line.starts_with('*') || line.is_empty() {
            continue;
        }
        match (line.strip_prefix('+'), logical.last_mut()) {
            (Some(rest), Some((_, prev))) => {
                prev.push(' ');
                prev.push_str(rest);
            }
            _ => logical.push((k as u32 + 1, line.to_owned())),
        }
    }
    let (line, sub) = logical
        .iter()
        .find(|(_, l)| {
            l.split_whitespace()
                .next()
                .is_some_and(|w| w.eq_ignore_ascii_case(".subckt"))
        })
        .ok_or_else(|| err(1, "no .subckt line"))?;
    let mut words = sub.split_whitespace().skip(1);
    words
        .next()
        .ok_or_else(|| err(*line, ".subckt without a name"))?;
    let mut ports = Vec::new();
    for w in words {
        if w.eq_ignore_ascii_case("params:") || w.contains('=') {
            break;
        }
        push(&mut ports, w.to_owned(), PinDir::Inout, *line)?;
    }
    Ok(ports)
}

// ---------------------------------------------------------------------- Verilog

#[derive(Clone, Copy)]
struct Tok<'a> {
    text: &'a str,
    line: u32,
}

/// Identifiers, numbers and one-character punctuation; comments, strings, attributes and
/// directive lines dropped.
fn tokens(text: &str) -> Vec<Tok<'_>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1u32);
    let skip_to = |i: usize, end: &str| text[i..].find(end).map_or(b.len(), |k| i + k + end.len());
    // To the end of the line, leaving its `\n` to be counted.
    let eol = |i: usize| text[i..].find('\n').map_or(b.len(), |k| i + k);
    while i < b.len() {
        let c = b[i];
        let start = i;
        i = match c {
            _ if c.is_ascii_whitespace() => i + 1,
            b'/' if b.get(i + 1) == Some(&b'/') => eol(i),
            b'/' if b.get(i + 1) == Some(&b'*') => skip_to(i + 2, "*/"),
            b'(' if b.get(i + 1) == Some(&b'*') && b.get(i + 2) != Some(&b')') => {
                skip_to(i + 2, "*)")
            }
            b'`' => eol(i),
            b'"' => skip_to(i + 1, "\""),
            b'\\' => i + text[i..].find(char::is_whitespace).unwrap_or(b.len() - i),
            _ if c.is_ascii_alphanumeric() || c == b'_' || c == b'\'' => {
                let n = b[i..]
                    .iter()
                    .position(|&d| !(d.is_ascii_alphanumeric() || b"_$'.".contains(&d)))
                    .unwrap_or(b.len() - i);
                // A number may carry `.`; an identifier stops at it.
                let n = if c.is_ascii_digit() || c == b'\'' {
                    n
                } else {
                    b[i..i + n].iter().position(|&d| d == b'.').unwrap_or(n)
                };
                i + n
            }
            // One (possibly multi-byte) character of punctuation.
            _ => i + text[i..].chars().next().map_or(1, char::len_utf8),
        };
        let t = &text[start..i];
        let keep = !t.trim().is_empty()
            && !t.starts_with("//")
            && !t.starts_with("/*")
            && !t.starts_with("(*")
            && !t.starts_with('`')
            && !t.starts_with('"');
        if keep {
            out.push(Tok { text: t, line });
        } else {
            line += t.bytes().filter(|&d| d == b'\n').count() as u32;
        }
    }
    out
}

fn dir_of(word: &str) -> Option<PinDir> {
    match word {
        "input" => Some(PinDir::In),
        "output" => Some(PinDir::Out),
        "inout" => Some(PinDir::Inout),
        _ => None,
    }
}

fn is_ident(t: &str) -> bool {
    t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '\\')
}

/// Index one past the bracket closing the one at `open`, or `None` when the tokens end.
fn close(toks: &[Tok<'_>], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (k, t) in toks.iter().enumerate().skip(open) {
        match t.text {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth -= 1;
                if depth == 0 {
                    return Some(k + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits `toks` at depth-0 commas.
fn items<'t, 'a>(toks: &'t [Tok<'a>]) -> Vec<&'t [Tok<'a>]> {
    let (mut out, mut depth, mut from) = (Vec::new(), 0i32, 0);
    for (k, t) in toks.iter().enumerate() {
        match t.text {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            "," if depth == 0 => {
                out.push(&toks[from..k]);
                from = k + 1;
            }
            _ => {}
        }
    }
    out.push(&toks[from..]);
    out
}

/// The first `[...]` of `toks` as text (`[3:0]`), and the last identifier outside brackets.
fn range_and_name<'a>(toks: &[Tok<'a>]) -> (String, Option<Tok<'a>>) {
    let (mut range, mut name, mut depth) = (String::new(), None, 0i32);
    let mut first_done = false;
    for t in toks {
        match t.text {
            "[" => depth += 1,
            "]" => {
                depth -= 1;
                if depth == 0 && !first_done {
                    range.push(']');
                    first_done = true;
                }
            }
            "=" if depth == 0 => break,
            _ => {}
        }
        if depth > 0 && !first_done {
            range.push_str(t.text);
        } else if depth == 0 && t.text != "]" && is_ident(t.text) {
            name = Some(*t);
        }
    }
    (range, name)
}

fn verilog_ports(text: &str) -> Result<Vec<(String, PinDir)>, PortError> {
    let toks = tokens(text);
    let m = toks
        .iter()
        .position(|t| t.text == "module" || t.text == "macromodule")
        .ok_or_else(|| err(1, "no module"))?;
    let line0 = toks[m].line;
    let mut i = m + 2;
    if toks.get(m + 1).is_none_or(|t| !is_ident(t.text)) {
        return Err(err(line0, "module without a name"));
    }
    if toks.get(i).is_some_and(|t| t.text == "#") {
        if toks.get(i + 1).is_none_or(|t| t.text != "(") {
            return Err(err(line0, "# without a parameter list"));
        }
        i = close(&toks, i + 1).ok_or_else(|| err(line0, "unterminated parameter list"))?;
    }
    // (port, dir, declared in the header)
    let mut ports: Vec<(String, PinDir)> = Vec::new();
    let mut ansi: Vec<bool> = Vec::new();
    if toks.get(i).is_some_and(|t| t.text == "(") {
        let j = close(&toks, i).ok_or_else(|| err(line0, "unterminated port list"))?;
        let mut carry: Option<(PinDir, String)> = None;
        for item in items(&toks[i + 1..j - 1]) {
            let Some(first) = item.first() else { continue };
            let dir = item.iter().find_map(|t| dir_of(t.text));
            let (range, name) = range_and_name(item);
            let Some(name) = name.filter(|n| dir_of(n.text).is_none()) else {
                return Err(err(first.line, "port without a name"));
            };
            let (dir, range, declared) = match (dir, &carry) {
                (Some(d), _) => {
                    carry = Some((d, range.clone()));
                    (d, range, true)
                }
                (None, Some((d, r))) if item.len() == 1 => (*d, r.clone(), true),
                (None, _) => (PinDir::Inout, range, false),
            };
            push(&mut ports, format!("{}{range}", name.text), dir, name.line)?;
            ansi.push(declared);
        }
        i = j;
    }
    // Non-ANSI declarations after the header, up to `endmodule`.
    let rest = &toks[i.min(toks.len())..];
    let end = rest
        .iter()
        .position(|t| t.text == "endmodule")
        .unwrap_or(rest.len());
    for stmt in rest[..end].split(|t| t.text == ";") {
        let Some(dir) = stmt.first().and_then(|t| dir_of(t.text)) else {
            continue;
        };
        let (range, _) = range_and_name(stmt);
        for piece in items(&stmt[1..]) {
            let Some(name) = range_and_name(piece).1 else {
                continue;
            };
            let k = ports.iter().position(|p| p.0 == name.text);
            if let Some(k) = k.filter(|&k| !ansi[k]) {
                ports[k] = (format!("{}{range}", name.text), dir);
                ansi[k] = true;
            }
        }
    }
    assert_eq!(ports.len(), ansi.len());
    Ok(ports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use PinDir::{In, Inout, Out};

    fn ok(lang: &str, text: &str) -> Vec<(String, PinDir)> {
        impl_ports(lang, text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn ports(list: &[(&str, PinDir)]) -> Vec<(String, PinDir)> {
        list.iter().map(|(n, d)| ((*n).to_owned(), *d)).collect()
    }

    /// (lang, body, expected ports)
    type Case = (
        &'static str,
        &'static str,
        &'static [(&'static str, PinDir)],
    );

    #[test]
    fn verilog_headers() {
        let cases: &[Case] = &[
            (
                "verilog",
                "module m (input a, output [3:0] d, inout c);\nendmodule",
                &[("a", In), ("d[3:0]", Out), ("c", Inout)],
            ),
            (
                "verilog",
                "(* keep *) module m #(parameter W = 4, parameter string S = \"x)\")\n\
                 (input wire [W-1:0] a, b, output reg y = 0);\nendmodule",
                &[("a[W-1:0]", In), ("b[W-1:0]", In), ("y", Out)],
            ),
            (
                "verilog",
                "// header\n`include \"disciplines.vams\"\n`define X 1\n/* module fake(q); */\n\
                 module m(a, b, c);\n  input a;\n  output [1:0] b;\nendmodule\nmodule n(z); endmodule",
                &[("a", In), ("b[1:0]", Out), ("c", Inout)],
            ),
            (
                "veriloga",
                "`include \"disciplines.vams\"\nmodule res(p, n);\n  electrical p, n;\n  \
                 parameter real r = 1k;\n  analog begin\n    V(p, n) <+ r * I(p, n);\n  end\nendmodule",
                &[("p", Inout), ("n", Inout)],
            ),
            (
                "veriloga",
                "module amp(inout electrical vdd, input electrical in, output electrical out);\nendmodule",
                &[("vdd", Inout), ("in", In), ("out", Out)],
            ),
            ("verilog", "module empty;\nendmodule", &[]),
        ];
        for (lang, text, want) in cases {
            assert_eq!(ok(lang, text), ports(want), "{text}");
        }
    }

    #[test]
    fn spice_subckts() {
        let cases: &[(&str, &[(&str, PinDir)])] = &[
            (
                "* comment\n.SUBCKT inv in out\n+ vdd vss params: w=1u\nM1 out in vss vss n\n.ends",
                &[
                    ("in", Inout),
                    ("out", Inout),
                    ("vdd", Inout),
                    ("vss", Inout),
                ],
            ),
            (
                ".subckt r a b r=1k ; trailing\n.ends",
                &[("a", Inout), ("b", Inout)],
            ),
            (".subckt lone\n.ends", &[]),
        ];
        for (text, want) in cases {
            assert_eq!(ok("spice", text), ports(want), "{text}");
        }
    }

    #[test]
    fn errors_name_the_line() {
        let cases: &[(&str, &str, u32, &str)] = &[
            ("verilog", "// nothing\n", 1, "no module"),
            ("verilog", "\nmodule m (a, b\n", 2, "unterminated port list"),
            (
                "verilog",
                "module m #(parameter p = 1\n",
                1,
                "unterminated parameter list",
            ),
            (
                "verilog",
                "module m (a,\n b,\n a);\nendmodule",
                3,
                "duplicate port a",
            ),
            (
                "veriloga",
                "module m (input [1:0]);\nendmodule",
                1,
                "port without a name",
            ),
            ("verilog", "module", 1, "module without a name"),
            ("spice", "R1 a b 1k\n", 1, "no .subckt line"),
            ("spice", "\n.subckt x a\n+ a\n", 2, "duplicate port a"),
            ("spice", ".subckt\n", 1, ".subckt without a name"),
            ("schematic", "", 1, "a schematic impl declares no ports"),
        ];
        for (lang, text, line, why) in cases {
            let e = impl_ports(lang, text).expect_err(text);
            assert_eq!((e.line, e.why.as_str()), (*line, *why), "{text}");
        }
    }

    #[test]
    fn port_count_is_bounded() {
        let mut text = String::from(".subckt big");
        for k in 0..=PORTS_MAX {
            text.push_str(&format!(" p{k}"));
        }
        let e = impl_ports("spice", &text).expect_err("too many");
        assert!(e.why.starts_with("more than"));
        text.truncate(text.rfind(' ').unwrap());
        assert_eq!(impl_ports("spice", &text).unwrap().len(), PORTS_MAX);
    }
}
