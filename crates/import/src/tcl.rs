//! Tcl without a Tcl runtime (D14, PDK doc §5.5): pattern recognisers for the few Tcl
//! forms that carry netlist or gating data. Nothing here evaluates Tcl.
//!
//! - [`pdk_vars`]: `set NAME ${PDK_ROOT}/…` lines of a PDK `xschemrc` → project `set`
//!   entries (`${NAME}` is expanded by the netlister from the project, then the
//!   environment).
//! - [`subst`]: the body of a `tcleval(…)` netlist text (code blocks, `device_model`)
//!   with every `$VAR` rewritten to `${VAR}`; any command is refused.
//! - [`drc_procs`]: the `switch -regexp $model { … if { $w < 0.42 } … }` procs of
//!   sky130/IHP → per-model `min`/`max` rules; everything else is listed as unknown.

/// Extracts PDK path variables from `xschemrc` text: `set NAME value` lines whose value
/// references another variable (`${PDK_ROOT}`, `$PDK`, `$::X`) and holds no command
/// substitution. `$X`/`$::X` become `${X}`; the first definition of a name wins.
/// xschem's own settings (`XSCHEM_*`) and environment reads (`$env(…)`) are skipped.
///
/// # Examples
///
/// ```
/// use schemify_import::tcl::pdk_vars;
/// let v = pdk_vars("  set PDK_ROOT $env(PDK_ROOT)\n  set SKYWATER_MODELS ${PDK_ROOT}/$PDK/libs.tech/combined\n");
/// assert_eq!(v, [("SKYWATER_MODELS".to_string(), "${PDK_ROOT}/${PDK}/libs.tech/combined".to_string())]);
/// ```
pub fn pdk_vars(xschemrc: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in xschemrc.lines() {
        let mut w = line.split_whitespace();
        let (Some("set"), Some(name), Some(value), None) = (w.next(), w.next(), w.next(), w.next())
        else {
            continue;
        };
        let name_ok = name
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
            && !name.starts_with("XSCHEM_");
        if !name_ok
            || !value.contains('$')
            || value.contains(['[', ']', '"'])
            || value.contains("$env(")
        {
            continue;
        }
        let Ok(value) = braced_vars(value, |_| true) else {
            continue;
        };
        if !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_owned(), value));
        }
    }
    assert!(out.iter().all(|(_, v)| !v.contains("$env")));
    out
}

/// Rewrites `$X`, `$::X`, `${X}` to `${X}`, refusing names `known` rejects.
fn braced_vars(text: &str, known: impl Fn(&str) -> bool) -> Result<String, String> {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len() + 8);
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'$' {
            let ch = text[i..].chars().next().expect("in bounds");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let mut j = i + 1;
        let braced = b.get(j) == Some(&b'{');
        if braced {
            j += 1;
        } else if text[j..].starts_with("::") {
            j += 2;
        }
        let start = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
        let name = &text[start..j];
        if name.is_empty() {
            out.push_str("$$"); // a literal `$` (netlist spec §7.1)
            i += 1;
            continue;
        }
        if braced {
            if b.get(j) != Some(&b'}') {
                return Err(format!("unterminated ${{ in {text}"));
            }
            j += 1;
        }
        if !known(name) {
            return Err(format!("${name}"));
        }
        out.push_str("${");
        out.push_str(name);
        out.push('}');
        i = j;
    }
    Ok(out)
}

/// Translates the body of a `tcleval(…)` netlist text: `$VAR`/`$::VAR` → `${VAR}` for
/// names in `vars`, a literal `$` → `$$` (netlist spec §7.1 expansion syntax), `\c` →
/// `c`. Braces stay literal (Tcl `subst` does not group).
///
/// # Errors
///
/// The offending construct when the text uses a variable not in `vars` or any
/// `[command]` (those would need a Tcl interpreter; spec §5 sends them to review).
pub fn subst(text: &str, vars: &[(String, String)]) -> Result<String, String> {
    if let Some(open) = unescaped(text, b'[') {
        let end = text[open..].find(']').map_or(text.len(), |e| open + e + 1);
        return Err(text[open..end].to_owned());
    }
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('$') => plain.push('\u{1}'), // escaped dollar survives as literal
                Some(n) => plain.push(n),
                None => {}
            }
        } else {
            plain.push(c);
        }
    }
    // `PDK_ROOT` and `PDK` are the PDK's own Tcl globals (set from the environment by
    // its xschemrc); the netlister resolves them from the project, then the environment.
    let out = braced_vars(&plain, |n| {
        matches!(n, "PDK_ROOT" | "PDK" | "USER_CONF_DIR") || vars.iter().any(|(v, _)| v == n)
    })?;
    // xschem's user configuration directory is `~/.xschem` (golden tb_diff_amp:
    // `pre_osdi <HOME>/.xschem/xschem_library/diff_amp.osdi`).
    Ok(out
        .replace("${USER_CONF_DIR}", "${HOME}/.xschem")
        .replace('\u{1}', "$$"))
}

fn unescaped(text: &str, needle: u8) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == needle => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// A comparison in a `drc` proc arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    /// `if { $v < N }` flags small values: N is a minimum.
    Min,
    /// `if { $v > N }` flags large values: N is a maximum.
    Max,
}

/// One recognised rule: `var` compared against `value` (as written in the proc).
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    /// The proc variable compared.
    pub var: String,
    /// Which bound.
    pub bound: Bound,
    /// The threshold as written.
    pub value: f64,
}

/// One `switch -regexp $model` arm.
#[derive(Clone, Debug, PartialEq)]
pub struct Arm {
    /// The arm's regular expression (Tcl ARE subset).
    pub pattern: String,
    /// Recognised rules.
    pub rules: Vec<Rule>,
    /// Lines inside the arm that are not recognised rules (reported).
    pub unknown: Vec<String>,
}

/// `set v [expr { double($v) / double($d) }]`: the proc compares `v / d`.
#[derive(Clone, Debug, PartialEq)]
pub struct Division {
    /// Variable divided.
    pub var: String,
    /// Divisor variable.
    pub by: String,
    /// The division applies only when the symbol file name does *not* match this.
    pub unless_symbol: Option<String>,
}

/// A recognised `drc` proc.
#[derive(Clone, Debug, PartialEq)]
pub struct DrcProc {
    /// Proc name.
    pub name: String,
    /// Formal arguments in order.
    pub args: Vec<String>,
    /// Variables whose `u` suffix the proc strips (`regsub {u$} $w {} w`): their
    /// thresholds are micrometres.
    pub micro: Vec<String>,
    /// Divisions applied before the switch.
    pub divisions: Vec<Division>,
    /// Switch arms in order.
    pub arms: Vec<Arm>,
    /// Lines outside the arms that are not recognised (reported).
    pub unknown: Vec<String>,
}

/// Collects every `proc` of `text`. Rules are recognised line by line inside a
/// `switch -regexp $model`; every other line is kept in the proc's `unknown` lists so
/// the caller can route the symbols that call it to review (a proc with no switch has
/// no arms).
///
/// # Examples
///
/// ```
/// use schemify_import::tcl::{drc_procs, Bound};
/// let p = drc_procs("proc d {instance symbol model w l} {\n set res {}\n switch -regexp $model {\n  {nfet$} {\n   if { $w < 0.42 } {\n    append res x\n   }\n  }\n }\n return $res\n}\n");
/// assert_eq!(p[0].arms[0].rules[0].bound, Bound::Min);
/// assert!(p[0].unknown.is_empty());
/// ```
pub fn drc_procs(text: &str) -> Vec<DrcProc> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim();
        let Some(rest) = l.strip_prefix("proc ") else {
            i += 1;
            continue;
        };
        let Some((name, rest)) = rest.trim().split_once(char::is_whitespace) else {
            i += 1;
            continue;
        };
        let args = proc_args(rest.trim());
        let end = proc_end(&lines, i);
        out.push(parse_proc(name, args, &lines[i + 1..end]));
        i = end + 1;
    }
    out
}

/// Argument names of a Tcl proc argument list `{a b {c 1}}`: each element's first
/// word (`{c 1}` is `c` with default `1`).
fn proc_args(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0u32;
    let mut word = String::new();
    let mut first_in_group = false;
    for c in list.chars() {
        let keep = depth == 1 || (depth == 2 && first_in_group);
        match c {
            '{' => {
                depth += 1;
                first_in_group = depth == 2;
            }
            '}' => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            c if c.is_whitespace() => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
                if depth == 2 {
                    first_in_group = false;
                }
            }
            c if keep => word.push(c),
            _ => {}
        }
    }
    out
}

/// Line index of the `}` closing the proc opened on line `start` (brace count).
fn proc_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i32;
    for (k, l) in lines.iter().enumerate().skip(start) {
        let code = l.split(" ;#").next().unwrap_or(l);
        for c in code.bytes() {
            match c {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
        }
        if depth <= 0 && k > start {
            return k;
        }
    }
    lines.len().saturating_sub(1)
}

fn parse_proc(name: &str, args: Vec<String>, body: &[&str]) -> DrcProc {
    let mut p = DrcProc {
        name: name.to_owned(),
        args,
        micro: Vec::new(),
        divisions: Vec::new(),
        arms: Vec::new(),
        unknown: Vec::new(),
    };
    let mut in_switch = false;
    let mut symbol_cond: Option<String> = None;
    let mut arm: Option<Arm> = None;
    for raw in body {
        let l = raw.split(" ;#").next().unwrap_or(raw).trim();
        if l.is_empty() || l.starts_with('#') || l == "}" || l.starts_with("append res") {
            if l == "}" && symbol_cond.is_some() && !in_switch {
                symbol_cond = None;
            }
            continue;
        }
        if matches!(l, "set res {}" | "return $res" | "set res \"\"") || l.starts_with("puts ") {
            continue;
        }
        if let Some(v) = l.strip_prefix("regsub {u$} $")
            && let Some(var) = v.split_whitespace().next()
        {
            p.micro.push(var.to_owned());
            continue;
        }
        if l.starts_with("if { [string is") || l.starts_with("if {[string is") {
            continue; // numeric guard: the rules only fire for numeric values
        }
        if let Some(re) = l
            .strip_prefix("if {![regexp {")
            .and_then(|r| r.split_once("} $symbol]} {"))
        {
            symbol_cond = Some(re.0.to_owned());
            continue;
        }
        if let Some(d) = division(l) {
            p.divisions.push(Division {
                unless_symbol: symbol_cond.clone(),
                ..d
            });
            continue;
        }
        if l.starts_with("switch -regexp $model {") || l.starts_with("switch -regexp -- $model {") {
            in_switch = true;
            continue;
        }
        if in_switch {
            if let Some(pat) = l.strip_prefix('{').and_then(|r| r.strip_suffix("} {")) {
                if let Some(a) = arm.take() {
                    p.arms.push(a);
                }
                arm = Some(Arm {
                    pattern: pat.to_owned(),
                    rules: Vec::new(),
                    unknown: Vec::new(),
                });
                continue;
            }
            if let Some(a) = arm.as_mut() {
                match rule(l) {
                    Some(r) if p.args.contains(&r.var) => a.rules.push(r),
                    _ if l.starts_with("if ") => a.unknown.push(l.to_owned()),
                    _ => a.unknown.push(l.to_owned()),
                }
                continue;
            }
        }
        p.unknown.push(l.to_owned());
    }
    if let Some(a) = arm {
        p.arms.push(a);
    }
    p
}

/// `if { $w < 0.42 } {` → rule.
fn rule(l: &str) -> Option<Rule> {
    let inner = l.strip_prefix("if {")?.strip_suffix("} {")?.trim();
    let mut w = inner.split_whitespace();
    let (var, op, value, None) = (w.next()?, w.next()?, w.next()?, w.next()) else {
        return None;
    };
    let var = var.strip_prefix('$')?;
    let bound = match op {
        "<" | "<=" => Bound::Min,
        ">" | ">=" => Bound::Max,
        _ => return None,
    };
    let value: f64 = value.parse().ok()?;
    value.is_finite().then(|| Rule {
        var: var.to_owned(),
        bound,
        value,
    })
}

/// `set w [expr { double($w) / double($nf)}]` → division.
fn division(l: &str) -> Option<Division> {
    let rest = l.strip_prefix("set ")?;
    let (var, rest) = rest.split_once(' ')?;
    let e = rest
        .trim()
        .strip_prefix("[expr {")?
        .strip_suffix("}]")?
        .trim();
    let (a, b) = e.split_once('/')?;
    let strip = |s: &str| {
        let s = s.trim();
        let s = s
            .strip_prefix("double(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(s);
        s.strip_prefix('$').map(str::to_owned)
    };
    let (num, den) = (strip(a)?, strip(b)?);
    (num == var).then_some(Division {
        var: num,
        by: den,
        unless_symbol: None,
    })
}

/// Unanchored match of a Tcl `switch -regexp` pattern against `text`. Supports the
/// subset PDK procs use: literals, `\x`, `.`, `[set]` with ranges, `* + ?`, `^ $`, `|`.
///
/// # Examples
///
/// ```
/// use schemify_import::tcl::regex_match;
/// assert!(regex_match("[np]fet_01v8$", "sky130_fd_pr__nfet_01v8"));
/// assert!(!regex_match("[np]fet_01v8$", "nfet_01v8_lvt"));
/// assert!(regex_match("rsil$|rppd$", "rppd"));
/// assert!(regex_match("fet.*_nf\\.sym$", "nfet_01v8_nf.sym"));
/// ```
pub fn regex_match(pattern: &str, text: &str) -> bool {
    let t = text.as_bytes();
    split_top(pattern).iter().any(|alt| {
        let p = alt.as_bytes();
        if let Some(rest) = p.strip_prefix(b"^") {
            matches_here(rest, t, 0)
        } else {
            (0..=t.len()).any(|s| matches_here(p, t, s))
        }
    })
}

fn split_top(p: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut in_set, mut esc) = (0, false, false);
    for (i, c) in p.bytes().enumerate() {
        match (esc, c) {
            (true, _) => esc = false,
            (_, b'\\') => esc = true,
            (_, b'[') => in_set = true,
            (_, b']') => in_set = false,
            (_, b'|') if !in_set => {
                parts.push(&p[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&p[start..]);
    parts
}

/// One pattern atom: its byte length and whether it matches `c`.
fn atom(p: &[u8], c: u8) -> (usize, bool) {
    match p[0] {
        b'.' => (1, true),
        b'\\' if p.len() > 1 => (2, p[1] == c),
        b'[' => {
            let end = p.iter().position(|&x| x == b']').unwrap_or(p.len() - 1);
            let set = &p[1..end];
            let (neg, set) = match set.first() {
                Some(b'^') => (true, &set[1..]),
                _ => (false, set),
            };
            let mut hit = false;
            let mut k = 0;
            while k < set.len() {
                if k + 2 < set.len() && set[k + 1] == b'-' {
                    hit |= (set[k]..=set[k + 2]).contains(&c);
                    k += 3;
                } else {
                    hit |= set[k] == c;
                    k += 1;
                }
            }
            (end + 1, hit != neg)
        }
        x => (1, x == c),
    }
}

fn matches_here(p: &[u8], t: &[u8], at: usize) -> bool {
    if p.is_empty() {
        return true;
    }
    if p == b"$" {
        return at == t.len();
    }
    let probe = t.get(at).copied().unwrap_or(0);
    let (len, _) = atom(p, probe);
    let quant = p.get(len).copied();
    let one = |k: usize| t.get(k).is_some_and(|&c| atom(p, c).1);
    match quant {
        Some(b'*') | Some(b'+') | Some(b'?') => {
            let rest = &p[len + 1..];
            let min = usize::from(quant == Some(b'+'));
            let max = if quant == Some(b'?') {
                1
            } else {
                t.len() - at.min(t.len())
            };
            let mut n = 0;
            while n < max && one(at + n) {
                n += 1;
            }
            (min..=n).rev().any(|k| matches_here(rest, t, at + k))
        }
        _ => one(at) && matches_here(&p[len..], t, at + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_argument_lists() {
        assert_eq!(
            proc_args("{instance symbol {nf 1}} {"),
            ["instance", "symbol", "nf"]
        );
        assert_eq!(proc_args("{a {b 2} c}"), ["a", "b", "c"]);
        assert_eq!(proc_args("{}"), Vec::<String>::new());
    }

    #[test]
    fn subst_rewrites_vars_and_refuses_commands() {
        let vars = vec![("SKYWATER_MODELS".to_string(), "x".to_string())];
        assert_eq!(
            subst(
                ".lib $::SKYWATER_MODELS/sky130.lib.spice tt\n\\$keep",
                &vars
            )
            .as_deref(),
            Ok(".lib ${SKYWATER_MODELS}/sky130.lib.spice tt\n$$keep")
        );
        assert!(subst("$::OTHER/x", &vars).is_err());
        assert_eq!(
            subst("pre_osdi $USER_CONF_DIR/lib/a.osdi", &vars).as_deref(),
            Ok("pre_osdi ${HOME}/.xschem/lib/a.osdi")
        );
        assert!(subst(".include [abs_sym_path a.cir]", &vars).is_err());
        assert_eq!(subst("a \\[b\\]", &vars).as_deref(), Ok("a [b]"));
    }

    #[test]
    fn sky130_style_proc() {
        let text = "proc sky130_fet_drc {instance symbol model w l {nf 1}} {\n  set res {}\n  if { [string is double $w] && [string is double $l] && [string is integer $nf]} {\n    if {![regexp {fet.*_nf\\.sym$} $symbol]} {\n      set w [expr { double($w) / double($nf)}]\n    }\n    switch -regexp $model {\n      {[np]fet_01v8$} {\n        if { $w < 0.42 } {\n          append res \"x\" \\n\n        }\n        if { $l < 0.15 } {\n          append res \"y\" \\n\n        }\n      }\n    } ;# switch\n  }\n  return $res\n}\n";
        let p = &drc_procs(text)[0];
        assert_eq!(p.args, ["instance", "symbol", "model", "w", "l", "nf"]);
        assert_eq!(
            p.divisions[0].unless_symbol.as_deref(),
            Some("fet.*_nf\\.sym$")
        );
        assert_eq!(p.arms[0].rules.len(), 2);
        assert!(
            p.unknown.is_empty() && p.arms[0].unknown.is_empty(),
            "{p:?}"
        );
    }

    #[test]
    fn regex_subset() {
        assert!(regex_match("^ab?c+$", "acc") && !regex_match("^ab?c+$", "abbc"));
        assert!(regex_match("[^a]x", "bx") && !regex_match("^[^a]x", "ax"));
        assert!(regex_match("a|b", "zb") && !regex_match("a|b", "zc"));
    }
}
