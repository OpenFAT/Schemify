//! xschem netlist text → Schemify expression-language text (spec `import.md` §4, §5).
//!
//! Three translations, all pure string → string:
//! - [`convert_format`]: a symbol's `format`/`lvs_format`/… string → an expr template
//!   (`@name` → `{name}`, `@@P` → `{P}`, `expr(…)` → `{…}`, literal braces doubled).
//! - [`translate_expr`]: an `expr()` body, a SPICE-quoted value or a Tcl `[ev …]` body
//!   → an expr-language expression, checked by compiling it.
//! - [`string_value`]: a free-text attribute value → a template string whose parent-cell
//!   parameter references become holes.
//!
//! Nothing here guesses: anything outside the recognised idioms comes back as a
//! [`Review`] for the caller's `needs_review` table.

use schemify_expr::{Mode, Program};

/// Why a construct could not be translated; the caller turns it into a `needs_review`
/// report item and leaves the affected field at its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    /// `tcleval(…)` with a variable or command outside the known idioms (§5).
    Tcl,
    /// An `expr()` body or a parameter value that is not an expression we can translate.
    Expression,
    /// `@@pin` / `@#n` naming a pin that does not exist or is not an identifier.
    PinToken,
    /// An xschem built-in token with no Schemify equivalent (`@path`, `@schname`, …).
    BuiltinToken,
    /// A `drc=` rule outside the recognised `min`/`max` patterns (D14).
    Drc,
    /// Per-instance netlist text: instance `format`, `device_model`, `spice_sym_def`.
    InstanceText,
    /// A symbol reference that resolves to no input file.
    MissingSymbol,
    /// A symbol construct with no v1 translation (bus taps, generators, LCC, embedded).
    Unsupported,
    /// A pin or parameter name that cannot be represented (empty, duplicate, reserved).
    Name,
    /// An `ipin`/`opin`/`iopin` whose label is not a pin of the cell's symbol.
    Port,
}

impl Reason {
    /// Returns the report word.
    pub fn word(self) -> &'static str {
        match self {
            Reason::Tcl => "tcl",
            Reason::Expression => "expression",
            Reason::PinToken => "pin-token",
            Reason::BuiltinToken => "builtin-token",
            Reason::Drc => "drc",
            Reason::InstanceText => "instance-text",
            Reason::MissingSymbol => "missing-symbol",
            Reason::Unsupported => "unsupported",
            Reason::Name => "name",
            Reason::Port => "port",
        }
    }
}

/// A translation failure: the reason and the offending text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    /// Why.
    pub reason: Reason,
    /// The construct, quoted from the source.
    pub detail: String,
}

pub(crate) fn review(reason: Reason, detail: impl Into<String>) -> Review {
    Review {
        reason,
        detail: detail.into(),
    }
}

/// What a format string may reference, for [`convert_format`].
#[derive(Clone, Copy, Debug)]
pub struct FormatCtx<'a> {
    /// The cell's parameter names (identifiers).
    pub params: &'a [String],
    /// Ordinary pins in netlist order.
    pub pins: &'a [String],
    /// Inherited (net-parameter) pins: `@x` means the net on pin `x`.
    pub implicit: &'a [String],
    /// `spiceprefix` from the symbol template, folded in as literal text (`X`). `None`
    /// when the template has none: then `@spiceprefix` refers to the parameter of that
    /// name (or is empty, like xschem, when there is none).
    pub spiceprefix: Option<&'a str>,
    /// A schematic impl: `k=@k` pairs passing the cell's own parameters are dropped
    /// (the netlister specialises the subcircuit instead, netlist spec §6).
    pub schematic: bool,
    /// `(xschem pin, Schemify pin)` renames: a pin named like a parameter is renamed,
    /// because template pin names shadow parameters (expr spec §6) while xschem keeps
    /// pins and attributes apart.
    pub rename: &'a [(String, String)],
}

fn pin_out<'a>(ctx: &'a FormatCtx, p: &'a str) -> &'a str {
    ctx.rename
        .iter()
        .find(|(x, _)| x == p)
        .map_or(p, |(_, s)| s.as_str())
}

/// A converted template.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Converted {
    /// The expr template.
    pub text: String,
    /// `@x` tokens that name no parameter, pin or built-in. The caller declares each as
    /// a string parameter defaulting to `""`, which renders empty exactly as xschem
    /// renders an unresolved token, while still letting instances set it.
    pub unknown: Vec<String>,
    /// Unresolved tokens that cannot become parameters (not identifiers): rendered
    /// empty, as xschem does, and counted in the report.
    pub dropped: Vec<String>,
}

/// Template identifiers the netlister provides (expr spec §6).
pub const BUILTINS: &[&str] = &["name", "cell", "model", "pins", "params", "file"];

/// Builtins a parameter may not be named after (`model` may: the builtin falls back to
/// the `model` parameter).
pub const RESERVED: &[&str] = &["name", "cell", "pins", "params", "file"];

/// Returns whether `s` is an expr identifier (`[A-Za-z_][A-Za-z0-9_]*`, not reserved).
pub fn is_ident(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
        && !matches!(s, "true" | "false" | "pi")
}

/// Converts one xschem format string to an expr template (spec §4).
///
/// # Errors
///
/// A [`Review`] for every construct with no faithful translation: unknown Tcl, a pin
/// token naming no pin, xschem built-ins such as `@path`.
///
/// # Examples
///
/// ```
/// use interop::template::{convert_format, FormatCtx};
/// let params = ["model".to_string(), "W".to_string()];
/// let pins = ["D".to_string(), "G".to_string()];
/// let ctx = FormatCtx { params: &params, pins: &pins, implicit: &[], spiceprefix: Some("X"), schematic: false, rename: &[] };
/// let c = convert_format("@spiceprefix@name @pinlist sky130_fd_pr__@model W=@W {x}", &ctx).unwrap();
/// assert_eq!(c.text, "X{name} {pins} sky130_fd_pr__{model} W={W} {{x}}");
/// ```
pub fn convert_format(src: &str, ctx: &FormatCtx) -> Result<Converted, Review> {
    assert!(ctx.params.iter().all(|p| is_ident(p)));
    let trimmed = src.trim();
    let (body, tcl) = match trimmed
        .strip_prefix("tcleval(")
        .and_then(|s| s.strip_suffix(')'))
    {
        Some(inner) => (inner, true),
        None => (src, false),
    };
    let unmarked = name_prefix_marks(body);
    let body = unmarked.as_str();
    let dropped_pairs;
    let body = if ctx.schematic {
        dropped_pairs = drop_param_pairs(body, ctx.params);
        dropped_pairs.as_str()
    } else {
        body
    };
    let mut out = Converted::default();
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\\' if body[i + 1..].starts_with('\n') || body[i + 1..].starts_with("\r\n") => {
                // An escaped newline (`\\` + newline in the file) joins lines (golden
                // sky130 test_stdcells: the `sqwsource` format is one `V… pulse …` line).
                i += if b[i + 1] == b'\r' { 3 } else { 2 };
            }
            b'\\' => {
                // `\x`: the backslash is a token terminator or an escape; emit `x`.
                i += 1;
                if let Some(ch) = body[i..].chars().next() {
                    push_literal(&mut out.text, ch);
                    i += ch.len_utf8();
                }
            }
            b'@' | b'%' => i = convert_token(body, i, ctx, &mut out)?,
            b'[' if tcl => i = convert_tcl_command(body, i, ctx, &mut out)?,
            b'$' if tcl => {
                let rest = &body[i + 1..];
                if rest.starts_with(|ch: char| ch.is_ascii_alphanumeric() || "_:{".contains(ch)) {
                    return Err(review(
                        Reason::Tcl,
                        format!("variable in a netlist template: {src}"),
                    ));
                }
                out.text.push('$');
                i += 1;
            }
            b'e' if word_start(b, i) && call_at(body, i).is_some() => {
                let (name_len, eng) = call_at(body, i).expect("checked");
                let open = i + name_len;
                let close = matching_paren(body, open).ok_or_else(|| {
                    review(Reason::Expression, format!("unbalanced expr(: {src}"))
                })?;
                let inner = body[open + 1..close].trim();
                let inner = inner
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                    .unwrap_or(inner);
                let e = translate_expr(inner, ExprMode::Xschem, ctx.params)
                    .ok_or_else(|| review(Reason::Expression, format!("expr({inner})")))?;
                note_unknown_idents(&e, ctx, &mut out);
                hole(&mut out.text, &e, eng);
                i = close + 1;
            }
            _ => {
                let ch = body[i..].chars().next().expect("in bounds");
                push_literal(&mut out.text, ch);
                i += ch.len_utf8();
            }
        }
    }
    assert!(i >= b.len());
    Ok(out)
}

/// Formatting of a hole holding a translated `expr`/`ev` body.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Eng {
    /// `expr(…)`: canonical number text.
    Plain,
    /// `expr_eng(…)` / `[ev …]` / `[ev7 …]`: engineering notation, this many digits.
    Digits(u8),
}

fn hole(out: &mut String, e: &str, eng: Eng) {
    match eng {
        Eng::Plain => {
            out.push('{');
            out.push_str(e);
            out.push('}');
        }
        Eng::Digits(d) => {
            out.push_str("{eng(");
            out.push_str(e);
            out.push_str(", ");
            out.push_str(&d.to_string());
            out.push_str(")}");
        }
    }
}

/// `expr(` / `expr_eng(` / `expr_eng4(` at `i`: (name length incl. `(` minus 1, format).
fn call_at(s: &str, i: usize) -> Option<(usize, Eng)> {
    let rest = &s[i..];
    if rest.starts_with("expr(") {
        Some((4, Eng::Plain))
    } else if rest.starts_with("expr_eng4(") {
        Some((9, Eng::Digits(4)))
    } else if rest.starts_with("expr_eng(") {
        // ponytail: xschem prints expr_eng at its `ev_precision` setting; 5 digits is the
        // documented default. Add a project knob if a PDK depends on another precision.
        Some((8, Eng::Digits(5)))
    } else {
        None
    }
}

fn word_start(b: &[u8], i: usize) -> bool {
    i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')
}

/// Returns the index of the `)` matching the `(` at `open`, skipping `'…'` quotes.
fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    assert_eq!(b.get(open), Some(&b'('));
    let mut depth = 0u32;
    let mut quoted = false;
    for (k, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

fn push_literal(out: &mut String, ch: char) {
    match ch {
        '{' => out.push_str("{{"),
        '}' => out.push_str("}}"),
        _ => out.push(ch),
    }
}

/// Drops the `#` marks of xschem's `#pfx#@name` (`pfx` prefixed to every bit of the
/// instance name, research: feature inventory, format tokens): elab names each bit of
/// an iterated instance, so `pfx{name}` is that prefix on every bit. A `#` that opens
/// no such word stays literal.
fn name_prefix_marks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(k) = rest.find("#@name") {
        let word = rest[..k].rfind(char::is_whitespace).map_or(0, |w| w + 1);
        match rest[word..k].strip_prefix('#') {
            Some(pfx) if !pfx.contains('#') => {
                out.push_str(&rest[..word]);
                out.push_str(pfx);
                out.push_str("@name");
            }
            _ => out.push_str(&rest[..k + "#@name".len()]),
        }
        rest = &rest[k + "#@name".len()..];
    }
    out.push_str(rest);
    out
}

/// Removes whitespace-separated `k=@k` words where `k` is a parameter.
fn drop_param_pairs(s: &str, params: &[String]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_end = 0;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let word = &s[start..i];
        let pair = word
            .split_once("=@")
            .is_some_and(|(k, v)| k == v && params.iter().any(|p| p == k));
        if pair {
            // Drop the word and the whitespace before it.
            let ws_start = s[..start].trim_end().len().max(last_end);
            out.push_str(&s[last_end..ws_start]);
            last_end = i;
        }
    }
    out.push_str(&s[last_end..]);
    out
}

/// Converts the `@`/`%` token at `i`; returns the index after it.
fn convert_token(s: &str, i: usize, ctx: &FormatCtx, out: &mut Converted) -> Result<usize, Review> {
    let b = s.as_bytes();
    let sigil = b[i];
    let mut j = i + 1;
    let kind = match (sigil, b.get(j)) {
        (b'%', _) => b'%',
        (_, Some(b'@')) => {
            j += 1;
            b'P'
        }
        (_, Some(b'#')) => {
            j += 1;
            b'#'
        }
        _ => b'@',
    };
    let name_start = j;
    while j < b.len() && !matches!(b[j], b' ' | b'\t' | b'\n' | b'\r' | b'@' | b'%' | b'\\') {
        j += 1;
    }
    let name = &s[name_start..j];
    if name.is_empty() {
        // A lone sigil is literal text (`@@` before a terminator re-scans the second `@`).
        out.text.push(sigil as char);
        return Ok(i + 1);
    }
    let is_pin = |n: &str| ctx.pins.iter().any(|p| p == n) || ctx.implicit.iter().any(|p| p == n);
    match kind {
        b'P' => {
            if is_pin(name) && is_ident(pin_out(ctx, name)) {
                hole(&mut out.text, pin_out(ctx, name), Eng::Plain);
            } else if is_pin(name) {
                return Err(review(Reason::PinToken, format!("@@{name}")));
            } else {
                // A pin the symbol does not have renders empty: golden sky130
                // `nand4b_1` (`@@A` on a symbol whose pin is `A_N`) lists no net there.
                out.dropped.push(format!("@@{name}"));
            }
        }
        b'#' => {
            let (pin, attr) = name.split_once(':').unwrap_or((name, "net_name"));
            let pin_name = match pin.parse::<usize>() {
                Ok(n) => ctx.pins.get(n).map(String::as_str),
                Err(_) => is_pin(pin).then_some(pin),
            };
            match (pin_name, attr) {
                (Some(p), "net_name" | "resolved_net") if is_ident(pin_out(ctx, p)) => {
                    hole(&mut out.text, pin_out(ctx, p), Eng::Plain)
                }
                _ => return Err(review(Reason::PinToken, format!("@#{name}"))),
            }
        }
        b'%' => {
            if ctx.params.iter().any(|p| p == name) {
                hole(&mut out.text, name, Eng::Plain);
            } else {
                name.chars().for_each(|ch| push_literal(&mut out.text, ch));
            }
        }
        _ => convert_attr_token(name, ctx, out)?,
    }
    Ok(j)
}

fn convert_attr_token(name: &str, ctx: &FormatCtx, out: &mut Converted) -> Result<(), Review> {
    let has_param = |n: &str| ctx.params.iter().any(|p| p == n);
    match name {
        "name" => hole(&mut out.text, "name", Eng::Plain),
        "symname" => hole(&mut out.text, "cell", Eng::Plain),
        "pinlist" => {
            if ctx.implicit.is_empty() {
                hole(&mut out.text, "pins", Eng::Plain);
            } else {
                // `{pins}` would include inherited pins; spell the ordinary ones out.
                for (k, p) in ctx.pins.iter().enumerate() {
                    let p = pin_out(ctx, p);
                    if !is_ident(p) {
                        return Err(review(Reason::PinToken, format!("@pinlist with pin {p}")));
                    }
                    if k > 0 {
                        out.text.push(' ');
                    }
                    hole(&mut out.text, p, Eng::Plain);
                }
            }
        }
        "spiceprefix" => match ctx.spiceprefix {
            Some(v) => v.chars().for_each(|ch| push_literal(&mut out.text, ch)),
            None if has_param("spiceprefix") => hole(&mut out.text, "spiceprefix", Eng::Plain),
            None => {}
        },
        "savecurrent" if has_param("savecurrent") => {
            out.text
                .push_str("{savecurrent ? \"\\n.save i(\" + name + \")\" : \"\"}");
        }
        n if has_param(n) || ctx.implicit.iter().any(|p| p == n) => {
            hole(&mut out.text, n, Eng::Plain)
        }
        n if ctx.pins.iter().any(|p| p == n) => {
            // xschem looks attributes up by name; a pin name is not an attribute, so the
            // token renders empty (a parameter of that name would be shadowed by the pin).
            out.dropped.push(n.to_owned());
        }
        "path" | "schname" | "schname_ext" | "topschname" | "symname_ext" | "symref"
        | "prop_ptr" | "schprop" | "schvhdlprop" | "schverilogprop" | "schtedaxprop"
        | "schsymbolprop" | "sch_last_modified" | "sym_last_modified" | "time_last_modified" => {
            return Err(review(Reason::BuiltinToken, format!("@{name}")));
        }
        n if n.starts_with("spice_get_") || n.starts_with("tcleval") => {
            return Err(review(Reason::BuiltinToken, format!("@{n}")));
        }
        n if is_ident(n) && !RESERVED.contains(&n) => {
            if !out.unknown.iter().any(|u| u == n) {
                out.unknown.push(n.to_owned());
            }
            hole(&mut out.text, n, Eng::Plain);
        }
        n => out.dropped.push(n.to_owned()),
    }
    Ok(())
}

/// Converts the Tcl `[cmd …]` at `i` inside a `tcleval(…)` format; returns the index
/// after its `]`. Only `ev`/`ev7` (arithmetic printed with 4/7 digits) are known.
fn convert_tcl_command(
    s: &str,
    i: usize,
    ctx: &FormatCtx,
    out: &mut Converted,
) -> Result<usize, Review> {
    let close = bracket_end(s, i).ok_or_else(|| review(Reason::Tcl, s.to_owned()))?;
    let inner = s[i + 1..close].trim();
    let (cmd, arg) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
    let digits = match cmd {
        "ev" => 4,
        "ev7" => 7,
        _ => return Err(review(Reason::Tcl, format!("[{inner}]"))),
    };
    let arg = arg.trim();
    let arg = arg
        .strip_prefix('{')
        .and_then(|a| a.strip_suffix('}'))
        .unwrap_or(arg);
    let e = translate_expr(arg, ExprMode::Tcl, ctx.params)
        .ok_or_else(|| review(Reason::Expression, format!("[{inner}]")))?;
    note_unknown_idents(&e, ctx, out);
    hole(&mut out.text, &e, Eng::Digits(digits));
    Ok(close + 1)
}

/// Index of the `]` closing the `[` at `open` (nested brackets and braces counted).
fn bracket_end(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut k = open;
    while k < b.len() {
        match b[k] {
            b'\\' => k += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
        k += 1;
    }
    None
}

/// Records identifiers of a translated expression that are not parameters or pins as
/// unknown tokens (they become `""` string parameters, matching xschem's empty value).
fn note_unknown_idents(e: &str, ctx: &FormatCtx, out: &mut Converted) {
    let Ok(p) = Program::compile(e, Mode::Scalar) else {
        return;
    };
    for id in p.idents() {
        let known = ctx.params.iter().any(|x| x == id)
            || ctx.pins.iter().any(|x| x == id)
            || ctx.implicit.iter().any(|x| x == id)
            || BUILTINS.contains(&id);
        if !known && is_ident(id) && !out.unknown.iter().any(|u| u == id) {
            out.unknown.push(id.to_owned());
        }
    }
}

/// Which expression dialect a body is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExprMode {
    /// xschem's `expr()` evaluator: `@x` parameters, `log` = log10, named constants
    /// `e k h echarge abszero c`.
    Xschem,
    /// A SPICE value (`.param`, `'…'`, `{…}`): `log` = ln, every name is a parameter.
    Spice,
    /// A Tcl `expr` body (`[ev …]`, `drc` procs): `$x`/`@x` variables, `double()`.
    Tcl,
}

/// Translates an expression to the expr language and checks it compiles.
///
/// Identifiers matching a name in `known` case-insensitively are rewritten to that
/// spelling (SPICE is case-insensitive, the expr language is not); others stay as
/// written. SI suffixes stay; trailing unit letters are stripped (`1.6V` → `1.6`).
/// Returns `None` for anything outside the shared subset: unknown functions, strings,
/// SPICE-only syntax, or text that does not compile.
///
/// # Examples
///
/// ```
/// use interop::template::{translate_expr, ExprMode};
/// let k = ["W".to_string(), "nf".to_string()];
/// assert_eq!(translate_expr("int((@nf + 1)/2) * @W / @nf * 0.29", ExprMode::Xschem, &k).as_deref(),
///            Some("int((nf + 1)/2) * W / nf * 0.29"));
/// assert_eq!(translate_expr("log(w)*2.5uF", ExprMode::Spice, &k).as_deref(), Some("ln(W)*2.5u"));
/// assert_eq!(translate_expr("pwl(0 0 1n 1)", ExprMode::Spice, &k), None);
/// ```
pub fn translate_expr(src: &str, mode: ExprMode, known: &[String]) -> Option<String> {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 8);
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            i += 1;
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            let (lit, len) = number_token(&src[i..])?;
            out.push_str(&lit);
            i += len;
        } else if c == b'@' || (c == b'$' && mode == ExprMode::Tcl) {
            let mut j = i + 1;
            if mode == ExprMode::Tcl && src[j..].starts_with("::") {
                j += 2;
            }
            let start = j;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            let name = &src[start..j];
            if name.is_empty() {
                return None;
            }
            out.push_str(canonical_name(name, known));
            i = j;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &src[start..i];
            let call = src[i..].trim_start().starts_with('(');
            out.push_str(&map_word(word, call, mode, known)?);
        } else {
            let two = src.get(i..i + 2).unwrap_or("");
            let (tok, len) = match two {
                "**" => ("^", 2),
                "<=" | ">=" | "==" | "!=" | "&&" | "||" => (two, 2),
                _ => match c {
                    b'+' | b'-' | b'*' | b'/' | b'%' | b'^' | b'(' | b')' | b',' | b'?' | b':'
                    | b'<' | b'>' | b'!' => (&src[i..i + 1], 1),
                    _ => return None,
                },
            };
            out.push_str(tok);
            i += len;
        }
    }
    let out = out.trim().to_owned();
    if out.is_empty() {
        return None;
    }
    Program::compile(&out, Mode::Scalar).ok()?;
    Some(out)
}

fn canonical_name<'a>(name: &'a str, known: &'a [String]) -> &'a str {
    known
        .iter()
        .find(|k| k.eq_ignore_ascii_case(name))
        .map_or(name, String::as_str)
}

fn map_word(word: &str, call: bool, mode: ExprMode, known: &[String]) -> Option<String> {
    if call {
        let lower = word.to_ascii_lowercase();
        let f = match (lower.as_str(), mode) {
            ("log", ExprMode::Xschem) => "log10",
            ("log", _) => "ln",
            ("double", ExprMode::Tcl) => "",
            (
                "ln" | "log10" | "exp" | "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan"
                | "atan2" | "sinh" | "cosh" | "tanh" | "abs" | "int" | "round" | "floor" | "ceil"
                | "min" | "max" | "pow",
                _,
            ) => return Some(lower),
            _ => return None,
        };
        return Some(f.to_owned());
    }
    if mode == ExprMode::Xschem {
        let constant = match word {
            "pi" => Some("pi"),
            "e" => Some("exp(1)"),
            "k" => Some("phys.k"),
            "h" => Some("phys.h"),
            "echarge" => Some("phys.q"),
            "abszero" => Some("(-phys.kelvin0)"),
            "c" => Some("phys.c"),
            _ => None,
        };
        if let Some(k) = constant {
            return Some(k.to_owned());
        }
    }
    if word.eq_ignore_ascii_case("pi") {
        return Some("pi".to_owned());
    }
    Some(canonical_name(word, known).to_owned())
}

/// Lexes a SPICE number at the start of `s`: returns the expr literal and the bytes
/// consumed. `None` when a digit run is followed by something no literal allows.
fn number_token(s: &str) -> Option<(String, usize)> {
    let [mantissa, number, letters] = crate::spice::lex_number(s)?;
    let mut lit = s[..mantissa].trim_end_matches('.').to_owned();
    if lit.starts_with('.') {
        lit.insert(0, '0');
    }
    lit.push_str(&s[mantissa..number]);
    let unit = s[number..letters].to_ascii_lowercase();
    if unit.starts_with("meg") {
        lit.push_str("meg");
    } else if unit.starts_with("mil") {
        lit.push_str("mil");
    } else if let Some(f) = unit.chars().next() {
        match f {
            'f' | 'p' | 'n' | 'u' | 'm' | 'k' | 'g' | 't' => lit.push(f),
            'a' => lit.push_str("e-18"),
            _ => {} // unit letters only (`V`, `Hz`, `ohm`): stripped
        }
    }
    if s.as_bytes()
        .get(letters)
        .is_some_and(|c| c.is_ascii_digit() || *c == b'_' || *c == b'.')
    {
        return None;
    }
    assert!(!lit.is_empty());
    Some((lit, letters))
}

/// Returns the expr literal for `text` when the whole text is one SPICE number
/// (`0.15u`, `10Meg`, `1.6V`, `-2e-3`), else `None`.
///
/// # Examples
///
/// ```
/// use interop::template::spice_number;
/// assert_eq!(spice_number("10Meg").as_deref(), Some("10meg"));
/// assert_eq!(spice_number(" -1.5e-3 ").as_deref(), Some("-1.5e-3"));
/// assert_eq!(spice_number("W_N"), None);
/// ```
pub fn spice_number(text: &str) -> Option<String> {
    let t = text.trim();
    let (sign, rest) = match t.as_bytes().first() {
        Some(b'-') => ("-", &t[1..]),
        Some(b'+') => ("", &t[1..]),
        _ => ("", t),
    };
    if !rest.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let (lit, len) = number_token(rest)?;
    let out = format!("{sign}{lit}");
    // Out-of-range literals (`1e999`) are not numbers the expression language accepts.
    (len == rest.len() && Program::compile(&out, Mode::Scalar).is_ok()).then_some(out)
}

/// Converts free text to a template string: literal braces doubled, and every word
/// that names a parameter of `parents` (case-insensitively) turned into a hole, so a
/// value like `w=WP` inside a subcircuit renders the parent's evaluated `WP`. An
/// xschem `@name` token naming a parent parameter is the same hole without its `@`
/// (`model=@modeln` → `{modeln}`; goldens render the parameter's value there). A word
/// followed by `=` is a SPICE parameter being assigned and stays (`c='C'` → `c='{C}'`,
/// golden ccap).
///
/// # Examples
///
/// ```
/// use interop::template::string_value;
/// let p = ["WP".to_string(), "C".to_string()];
/// assert_eq!(string_value("'wp*2' {x}", &p), "'{WP}*2' {{x}}");
/// assert_eq!(string_value("c='C' m == c", &p), "c='{C}' m == {C}");
/// ```
pub fn string_value(text: &str, parents: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if (c.is_ascii_alphabetic() || c == b'_') && word_start(b, i) {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            // A word being assigned (`c='C'`) names a SPICE parameter, never the parent's.
            let next = text[i..].trim_start_matches([' ', '\t']).as_bytes();
            let assigned = next.first() == Some(&b'=') && next.get(1) != Some(&b'=');
            match parents
                .iter()
                .find(|p| !assigned && p.eq_ignore_ascii_case(word))
            {
                Some(p) => {
                    if start > 0 && b[start - 1] == b'@' {
                        out.pop();
                    }
                    hole(&mut out, p, Eng::Plain);
                }
                None => out.push_str(word),
            }
        } else {
            let ch = text[i..].chars().next().expect("in bounds");
            push_literal(&mut out, ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Escapes literal braces so `text` renders verbatim as a template.
pub fn literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    text.chars().for_each(|c| push_literal(&mut out, c));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// Spec §4 table, one row per case.
    #[test]
    fn template_conversion_table() {
        let params = s(&[
            "model",
            "W",
            "L",
            "nf",
            "value",
            "prefix",
            "m",
            "El",
            "savecurrent",
        ]);
        let pins = s(&["D", "G", "S", "B"]);
        let ctx = FormatCtx {
            params: &params,
            pins: &pins,
            implicit: &[],
            spiceprefix: Some("X"),
            schematic: false,
            rename: &[],
        };
        let rows: &[(&str, &str)] = &[
            ("@name", "{name}"),
            ("@spiceprefix@name", "X{name}"),
            ("@model L=@L", "{model} L={L}"),
            ("@pinlist", "{pins}"),
            ("@@D @@G", "{D} {G}"),
            ("@symname", "{cell}"),
            ("@name @@D @prefix\\nand2_1", "{name} {D} {prefix}nand2_1"),
            ("R\\$ \\$SUB=@value", "R$ $SUB={value}"),
            ("{lit} }", "{{lit}} }}"),
            ("W='@W * @nf '", "W='{W} * {nf} '"),
            (
                "ad=expr('int((@nf + 1)/2) * @W / @nf * 0.29')",
                "ad={int((nf + 1)/2) * W / nf * 0.29}",
            ),
            ("v=expr_eng(@W*2)", "v={eng(W*2, 5)}"),
            (
                "@value@savecurrent",
                "{value}{savecurrent ? \"\\n.save i(\" + name + \")\" : \"\"}",
            ),
            ("%m %zz", "{m} zz"),
            ("@#0:net_name @#1", "{D} {G}"),
            (
                "tcleval(Q@name le=[ev { @El * 1.0e-6 } ])",
                "Q{name} le={eng(El * 1.0e-6, 4)}",
            ),
            ("[ @@D ]", "[ {D} ]"),
            // An escaped newline joins lines (sqwsource).
            ("@name \\\npulse 0", "{name} pulse 0"),
        ];
        for (src, want) in rows {
            let got = convert_format(src, &ctx).unwrap_or_else(|e| panic!("{src}: {e:?}"));
            assert_eq!(&got.text, want, "{src}");
            schemify_expr::Template::parse(&got.text).unwrap_or_else(|e| panic!("{want}: {e:?}"));
        }
        // Unknown tokens become empty-default params; non-identifiers are dropped.
        let c = convert_format("@extra m=@m @gnd)", &ctx).unwrap();
        assert_eq!(c.text, "{extra} m={m} ");
        assert_eq!((c.unknown, c.dropped), (s(&["extra"]), s(&["gnd)"])));
        // A pin token naming no pin renders empty, as in the goldens (nand4b_1).
        let c = convert_format("@name @@Q @@D", &ctx).unwrap();
        assert_eq!((c.text.as_str(), c.dropped), ("{name}  {D}", s(&["@@Q"])));
        // Negative space: constructs that must be reviewed, never guessed.
        for bad in [
            "@path",
            "tcleval([my_proc 1])",
            "tcleval($::X/f)",
            "@#1:pinnumber",
            "@spice_get_voltage",
        ] {
            assert!(convert_format(bad, &ctx).is_err(), "{bad}");
        }
    }

    #[test]
    fn schematic_impl_drops_param_pairs_and_inherited_pins_spell_out() {
        let params = s(&["WN", "WP"]);
        let pins = s(&["A", "Z"]);
        let ctx = FormatCtx {
            params: &params,
            pins: &pins,
            implicit: &s(&["VDD"]),
            spiceprefix: None,
            schematic: true,
            rename: &[],
        };
        let c = convert_format("@name @pinlist @VDD @symname WN=@WN WP=@WP m=@m", &ctx).unwrap();
        assert_eq!(c.text, "{name} {A} {Z} {VDD} {cell} m={m}");
        assert_eq!(c.unknown, s(&["m"]));
    }

    /// Spec §5 expression rules.
    #[test]
    fn expression_translation_table() {
        let k = s(&["W", "nf", "VCC"]);
        let rows: &[(&str, ExprMode, Option<&str>)] = &[
            ("@W*2", ExprMode::Xschem, Some("W*2")),
            ("log(10)", ExprMode::Xschem, Some("log10(10)")),
            ("log(10)", ExprMode::Spice, Some("ln(10)")),
            ("ln(2) + sqrt(4)", ExprMode::Xschem, Some("ln(2) + sqrt(4)")),
            ("2^3", ExprMode::Xschem, Some("2^3")),
            ("2**3", ExprMode::Spice, Some("2^3")),
            (
                "0.15u + 10Meg + 1.6V + 5nH",
                ExprMode::Spice,
                Some("0.15u + 10meg + 1.6 + 5n"),
            ),
            ("1.", ExprMode::Spice, Some("1")),
            (
                "k*abszero",
                ExprMode::Xschem,
                Some("phys.k*(-phys.kelvin0)"),
            ),
            ("vcc/2", ExprMode::Spice, Some("VCC/2")),
            (
                "double($W) / double($nf)",
                ExprMode::Tcl,
                Some("(W) / (nf)"),
            ),
            ("a > b ? 1 : 2", ExprMode::Spice, Some("a > b ? 1 : 2")),
            ("pwl(0 0 1n 1)", ExprMode::Spice, None),
            ("agauss(1,2,3)", ExprMode::Spice, None),
            ("v(a)", ExprMode::Spice, None),
            ("'x'", ExprMode::Spice, None),
            ("", ExprMode::Spice, None),
            ("1 2", ExprMode::Spice, None),
        ];
        for (src, mode, want) in rows {
            assert_eq!(
                translate_expr(src, *mode, &k).as_deref(),
                *want,
                "{src} {mode:?}"
            );
        }
        assert_eq!(spice_number("1a").as_deref(), Some("1e-18"));
        assert_eq!(spice_number("1k5"), None);
        assert_eq!(spice_number(".5"), Some("0.5".into()));
    }

    #[test]
    fn name_prefix_marks_are_dropped() {
        assert_eq!(
            name_prefix_marks("#@spiceprefix\\MA#@name @#0"),
            "@spiceprefix\\MA@name @#0"
        );
        assert_eq!(
            name_prefix_marks("#MB#@name x #y#@name"),
            "MB@name x y@name"
        );
        // Not a prefix mark: no opening `#` in the word, or a stray one.
        assert_eq!(name_prefix_marks("a#@name"), "a#@name");
        assert_eq!(name_prefix_marks("# c @name"), "# c @name");
    }

    #[test]
    fn string_values_escape_and_bind_parent_params() {
        let p = s(&["W_N"]);
        assert_eq!(string_value("dc 0 ac 1", &p), "dc 0 ac 1");
        assert_eq!(string_value("pulse 0 {VCC}", &p), "pulse 0 {{VCC}}");
        assert_eq!(string_value("w_n*2 xw_n", &p), "{W_N}*2 xw_n");
        assert_eq!(string_value("@w_n @other", &p), "{W_N} @other");
        assert_eq!(literal("{a}"), "{{a}}");
    }
}
