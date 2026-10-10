//! SPICE/CDL text → statement tables (no evaluation, no flattening).
//!
//! Rules (the practical subset ngspice and xschem/CDL output use):
//! - `*` lines are comments; inline comments start at `;` or at a `$` with
//!   whitespace on both sides (CDL `$SUB=`/`$[model]` are not comments).
//! - `+` lines continue the previous statement; comments may sit in between.
//! - `.control` … `.endc` is kept as opaque text (blank and `*` lines
//!   dropped, whitespace collapsed); `.end` stops parsing.
//! - Case is folded everywhere except inside `"…"` strings, `.include`/`.lib`
//!   paths and control text.
//! - The first line is *not* a title: xschem and Schemify both start with a comment.

/// Statement tables of one netlist. Bodies own their instances; subckts are
/// referenced by name (resolution happens in `flat`).
#[derive(Default)]
pub(crate) struct Netlist {
    pub subckts: Vec<Subckt>,
    pub top: Body,
    /// `include <path>` / `lib <path> <section>`, paths case-preserved.
    pub includes: Vec<String>,
    pub models: Vec<Model>,
    /// Other dot-cards and unclassifiable lines, normalized text.
    pub directives: Vec<String>,
    /// One entry per `.control` block: trimmed lines joined by `\n`.
    pub controls: Vec<String>,
    pub globals: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) struct Subckt {
    pub name: String,
    pub ports: Vec<String>,
    pub defaults: Vec<(String, String)>,
    pub body: Body,
}

#[derive(Default)]
pub(crate) struct Body {
    pub params: Vec<(String, String)>,
    pub insts: Vec<Inst>,
}

pub(crate) struct Model {
    /// Enclosing subckt name, empty at top level.
    pub scope: String,
    pub name: String,
    pub kind: String,
    pub values: Vec<String>,
    pub params: Vec<(String, String)>,
}

/// One device or subckt call line.
pub(crate) struct Inst {
    pub name: String,
    /// Lowercase first letter of `name`.
    pub kind: u8,
    pub nodes: Vec<String>,
    /// Model or subckt name; empty for value-only devices (`R1 a b 1k`).
    pub model: String,
    /// Positional, non-node tokens after the model (`area`, `R` value).
    pub values: Vec<String>,
    pub params: Vec<(String, String)>,
    /// Source/behavioural spec tokens (V I E G F H B K W); evaluated in `flat`.
    pub spec: Vec<String>,
    /// XSPICE port grouping (`[n][nn]`), empty for other kinds.
    pub shape: String,
}

/// Parses `text`; never fails: lines it cannot classify become directives
/// plus a warning, so nothing is silently dropped.
pub(crate) fn parse(text: &str) -> Netlist {
    let mut nl = Netlist::default();
    let mut open: Vec<usize> = Vec::new(); // stack of subckts being defined
    let mut control: Option<Vec<String>> = None;
    let mut pending: Option<(usize, String)> = None;

    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();
        if let Some(block) = control.as_mut() {
            let word = first_word_lower(line);
            if word == ".endc" {
                nl.controls.push(block.join("\n"));
                control = None;
            } else if word == ".end" {
                break; // `.end` ends the netlist even inside an open block
            } else if !line.is_empty() && !line.starts_with('*') {
                block.push(line.split_whitespace().collect::<Vec<_>>().join(" "));
            }
            continue;
        }
        if line.is_empty() || line.starts_with('*') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            match pending.as_mut() {
                Some((_, text)) => {
                    text.push(' ');
                    text.push_str(strip_inline_comment(rest));
                }
                None => nl
                    .warnings
                    .push(format!("line {line_no}: continuation without a statement")),
            }
            continue;
        }
        if let Some((n, text)) = pending.take() {
            statement(&mut nl, &mut open, n, &text);
        }
        match first_word_lower(line).as_str() {
            ".control" => control = Some(Vec::new()),
            ".end" => break,
            _ => pending = Some((line_no, strip_inline_comment(line).to_string())),
        }
    }
    if let Some((n, text)) = pending.take() {
        statement(&mut nl, &mut open, n, &text);
    }
    if let Some(block) = control {
        nl.warnings.push("unterminated .control block".to_string());
        nl.controls.push(block.join("\n"));
    }
    if !open.is_empty() {
        nl.warnings
            .push(format!("{} unterminated .subckt", open.len()));
    }
    nl
}

fn first_word_lower(line: &str) -> String {
    line.split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Cuts at `;` or at a whitespace-delimited `$`, outside quotes.
fn strip_inline_comment(s: &str) -> &str {
    let b = s.as_bytes();
    let mut quote = 0u8;
    for i in 0..b.len() {
        let c = b[i];
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
            continue;
        }
        match c {
            b'\'' | b'"' => quote = c,
            b';' => return s[..i].trim_end(),
            b'$' => {
                let before = i == 0 || b[i - 1].is_ascii_whitespace();
                let after = i + 1 == b.len() || b[i + 1].is_ascii_whitespace();
                if before && after {
                    return s[..i].trim_end();
                }
            }
            _ => {}
        }
    }
    s.trim_end()
}

/// Splits on whitespace outside `'…'`, `"…"`, `{…}` and `(…)`, then glues
/// `k = v`, `k= v` and `k =v` into one `k=v` token. With `parens_are_space`,
/// `(` `)` outside quotes separate tokens instead (`.model` cards).
fn tokenize(s: &str, parens_are_space: bool) -> Vec<String> {
    let mut raw: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut brace = 0u32;
    let mut paren = 0u32;
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        if brace > 0 {
            cur.push(c);
            if c == '{' {
                brace += 1;
            } else if c == '}' {
                brace -= 1;
            }
            continue;
        }
        let separator = if parens_are_space {
            c.is_whitespace() || c == '(' || c == ')'
        } else {
            c.is_whitespace() && paren == 0
        };
        if separator {
            if !cur.is_empty() {
                raw.push(std::mem::take(&mut cur));
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '{' => brace = 1,
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            _ => {}
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        raw.push(cur);
    }

    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let t = &raw[i];
        let next = raw.get(i + 1);
        match (out.last_mut(), next) {
            (Some(last), _) if t == "=" => {
                last.push('=');
                last.push_str(next.map_or("", String::as_str));
                i += 2;
            }
            (Some(last), _) if t.starts_with('=') => {
                last.push_str(t);
                i += 1;
            }
            (_, Some(n)) if t.ends_with('=') && key_of(t).is_some() => {
                out.push(format!("{t}{n}"));
                i += 2;
            }
            _ => {
                out.push(t.clone());
                i += 1;
            }
        }
    }
    out
}

/// The `k` of a `k=v` token: a non-empty key with no quote, brace or paren.
fn key_of(t: &str) -> Option<&str> {
    let eq = t.find('=')?;
    let key = &t[..eq];
    let plain = !key.is_empty() && !key.contains(['\'', '"', '{', '(', ')', '[']);
    plain.then_some(key)
}

fn split_kv(t: &str) -> Option<(String, String)> {
    let key = key_of(t)?;
    Some((
        key.to_ascii_lowercase(),
        lower_outside_dquotes(&t[key.len() + 1..]),
    ))
}

/// Lowercases everything except `"…"` string contents (file names in XSPICE
/// / d_cosim parameters are case-sensitive).
pub(crate) fn lower_outside_dquotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    for c in s.chars() {
        if c == '"' {
            in_str = !in_str;
        }
        if in_str {
            out.push(c);
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

fn body_mut<'a>(nl: &'a mut Netlist, open: &[usize]) -> &'a mut Body {
    match open.last() {
        Some(&i) => &mut nl.subckts[i].body,
        None => &mut nl.top,
    }
}

fn scope_name(nl: &Netlist, open: &[usize]) -> String {
    open.last()
        .map(|&i| nl.subckts[i].name.clone())
        .unwrap_or_default()
}

fn statement(nl: &mut Netlist, open: &mut Vec<usize>, line_no: usize, text: &str) {
    let tokens = tokenize(text, false);
    let Some(first) = tokens.first() else { return };
    let head = first.to_ascii_lowercase();
    if head.starts_with('.') {
        dot_card(nl, open, line_no, &head, text, &tokens);
        return;
    }
    let kind = head.as_bytes()[0];
    if !kind.is_ascii_alphabetic() {
        nl.warnings.push(format!(
            "line {line_no}: unrecognized line kept as text: {text}"
        ));
        let scope = scope_name(nl, open);
        nl.directives.push(format!(
            "{scope}? {}",
            lower_outside_dquotes(&tokens.join(" "))
        ));
        return;
    }
    let inst = instance(head, kind, &tokens[1..]);
    body_mut(nl, open).insts.push(inst);
}

/// A token a card cannot place: warned about and kept as a directive so it
/// still takes part in the comparison.
fn stray_token(nl: &mut Netlist, line_no: usize, scope: &str, card: &str, t: &str) {
    nl.warnings
        .push(format!("line {line_no}: stray token {t} in {card}"));
    nl.directives.push(format!(
        "{scope}: stray {card} {}",
        lower_outside_dquotes(t)
    ));
}

fn dot_card(
    nl: &mut Netlist,
    open: &mut Vec<usize>,
    line_no: usize,
    head: &str,
    text: &str,
    tokens: &[String],
) {
    match head {
        ".subckt" => {
            let Some(name) = tokens.get(1) else {
                nl.warnings
                    .push(format!("line {line_no}: .subckt without a name"));
                return;
            };
            let mut ports = Vec::new();
            let mut defaults = Vec::new();
            let mut stray = Vec::new();
            for t in &tokens[2..] {
                if t.eq_ignore_ascii_case("params:") {
                    continue;
                }
                match split_kv(t) {
                    Some(kv) => defaults.push(kv),
                    None if defaults.is_empty() => ports.push(t.to_ascii_lowercase()),
                    None => stray.push(t),
                }
            }
            let name = name.to_ascii_lowercase();
            for t in stray {
                stray_token(nl, line_no, &name, ".subckt", t);
            }
            nl.subckts.push(Subckt {
                name,
                ports,
                defaults,
                body: Body::default(),
            });
            open.push(nl.subckts.len() - 1);
        }
        ".ends" => {
            if open.pop().is_none() {
                nl.warnings
                    .push(format!("line {line_no}: .ends without .subckt"));
            }
        }
        ".param" | ".params" => {
            for t in &tokens[1..] {
                match split_kv(t) {
                    Some(kv) => body_mut(nl, open).params.push(kv),
                    None => stray_token(nl, line_no, &scope_name(nl, open), ".param", t),
                }
            }
        }
        ".global" => nl
            .globals
            .extend(tokens[1..].iter().map(|t| t.to_ascii_lowercase())),
        ".include" | ".inc" => {
            let rest = text
                .trim()
                .split_once(char::is_whitespace)
                .map_or("", |(_, r)| r);
            let path = rest.trim().trim_matches(['"', '\'']);
            nl.includes.push(format!("include {path}"));
        }
        ".lib" if tokens.len() >= 3 => {
            let path = tokens[1].trim_matches(['"', '\'']);
            let section = tokens[2..].join(" ").to_ascii_lowercase();
            nl.includes.push(format!("lib {path} {section}"));
        }
        ".model" => {
            let toks = tokenize(text, true);
            if toks.len() < 3 {
                nl.warnings
                    .push(format!("line {line_no}: short .model card"));
            }
            let mut model = Model {
                scope: scope_name(nl, open),
                name: toks
                    .get(1)
                    .map(|t| t.to_ascii_lowercase())
                    .unwrap_or_default(),
                kind: toks
                    .get(2)
                    .map(|t| t.to_ascii_lowercase())
                    .unwrap_or_default(),
                values: Vec::new(),
                params: Vec::new(),
            };
            for t in toks.iter().skip(3) {
                match split_kv(t) {
                    Some(kv) => model.params.push(kv),
                    None => model.values.push(lower_outside_dquotes(t)),
                }
            }
            nl.models.push(model);
        }
        _ => {
            let scope = scope_name(nl, open);
            let text = lower_outside_dquotes(&tokens.join(" "));
            nl.directives.push(if scope.is_empty() {
                text
            } else {
                format!("{scope}: {text}")
            });
        }
    }
}

/// Node counts by device letter; `None` means "nodes are the bare tokens
/// before the model name, which is the last non-numeric bare token".
fn fixed_nodes(kind: u8) -> Option<usize> {
    match kind {
        b'r' | b'c' | b'l' | b'd' | b'v' | b'i' | b'f' | b'h' | b'b' | b'w' => Some(2),
        b's' | b't' => Some(4),
        b'k' => Some(0),
        _ => None,
    }
}

fn instance(name: String, kind: u8, rest: &[String]) -> Inst {
    let mut inst = Inst {
        name,
        kind,
        nodes: Vec::new(),
        model: String::new(),
        values: Vec::new(),
        params: Vec::new(),
        spec: Vec::new(),
        shape: String::new(),
    };
    let lowered: Vec<String> = rest.iter().map(|t| lower_outside_dquotes(t)).collect();

    // Sources and behavioural devices: nodes, then an opaque spec.
    let spec_kind = matches!(
        kind,
        b'v' | b'i' | b'e' | b'g' | b'f' | b'h' | b'b' | b'k' | b'w'
    );
    if spec_kind {
        let mut n = fixed_nodes(kind).unwrap_or(2);
        if matches!(kind, b'e' | b'g') && !is_behavioural(lowered.get(2)) {
            n = 4;
        }
        let n = n.min(lowered.len());
        inst.nodes = lowered[..n].to_vec();
        if kind == b'b' {
            inst.spec.push(lowered[n..].concat());
            return inst;
        }
        for t in &lowered[n..] {
            if t.starts_with(['\'', '{']) || key_of(t).is_some() {
                inst.spec.push(t.clone()); // expression or k=v: one unit
            } else {
                let parts =
                    t.split(|c: char| c == '(' || c == ')' || c == ',' || c.is_whitespace());
                inst.spec
                    .extend(parts.filter(|s| !s.is_empty()).map(str::to_string));
            }
        }
        return inst;
    }

    if kind == b'a' {
        xspice(&mut inst, &lowered);
        return inst;
    }

    let mut bare: Vec<String> = Vec::new();
    let mut sub: Vec<String> = Vec::new();
    for t in &lowered {
        if t == "/" || t == "params:" {
            continue; // CDL `X1 a b / cell`, HSPICE-style `params:`
        }
        if let Some(model) = t.strip_prefix("$[").and_then(|m| m.strip_suffix(']')) {
            inst.model = model.to_string(); // CDL `$[model]`
            continue;
        }
        match split_kv(t) {
            Some((k, v)) if k == "$sub" => sub.push(v), // CDL substrate is a connection
            Some(kv) => inst.params.push(kv),
            None => bare.push(t.clone()),
        }
    }

    match fixed_nodes(kind) {
        Some(n) => {
            let n = n.min(bare.len());
            inst.nodes = bare[..n].to_vec();
            for t in &bare[n..] {
                if inst.model.is_empty()
                    && crate::eval::parse_number(t).is_none()
                    && !t.starts_with(['\'', '{'])
                {
                    inst.model = t.clone();
                } else {
                    inst.values.push(t.clone());
                }
            }
        }
        None => {
            let model_at = bare
                .iter()
                .rposition(|t| crate::eval::parse_number(t).is_none());
            match model_at {
                Some(m) if inst.model.is_empty() => {
                    inst.nodes = bare[..m].to_vec();
                    inst.model = bare[m].clone();
                    inst.values = bare[m + 1..].to_vec();
                }
                _ => inst.nodes = bare,
            }
        }
    }
    inst.nodes.extend(sub);
    inst
}

fn is_behavioural(third: Option<&String>) -> bool {
    let Some(t) = third else { return true };
    key_of(t).is_some()
        || [
            "poly", "value", "vol", "cur", "table", "laplace", "freq", "{",
        ]
        .iter()
        .any(|p| t.starts_with(p))
}

/// `A1 [a b] %v c model k=v`: brackets and `%` modifiers become the shape,
/// the last bare token is the model.
fn xspice(inst: &mut Inst, lowered: &[String]) {
    let mut toks: Vec<String> = Vec::new();
    for t in lowered {
        if let Some(kv) = split_kv(t) {
            inst.params.push(kv);
            continue;
        }
        let mut cur = String::new();
        for c in t.chars() {
            if c == '[' || c == ']' {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
                toks.push(c.to_string());
            } else {
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            toks.push(cur);
        }
    }
    let Some(model_at) = toks
        .iter()
        .rposition(|t| t != "[" && t != "]" && !t.starts_with('%'))
    else {
        inst.shape = toks.concat();
        return;
    };
    inst.model = toks[model_at].clone();
    for t in &toks[..model_at] {
        if t == "[" || t == "]" || t.starts_with('%') {
            inst.shape.push_str(t);
        } else {
            inst.shape.push('n');
            inst.nodes.push(t.clone());
        }
    }
    inst.values = toks[model_at + 1..].to_vec();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_glues_assignments_and_respects_quotes() {
        let t = tokenize(
            "M1 d g s b nch W = 1u L= 2u ad='a * b' x ={1 + 2} s=\"A B\"",
            false,
        );
        assert_eq!(
            t,
            [
                "M1",
                "d",
                "g",
                "s",
                "b",
                "nch",
                "W=1u",
                "L=2u",
                "ad='a * b'",
                "x={1 + 2}",
                "s=\"A B\""
            ]
        );
        let m = tokenize(".model n1 nmos(level=1 vto = 0.7)", true);
        assert_eq!(m, [".model", "n1", "nmos", "level=1", "vto=0.7"]);
    }

    #[test]
    fn inline_comments_but_not_cdl_dollar() {
        assert_eq!(strip_inline_comment("R1 a b 1k ; note"), "R1 a b 1k");
        assert_eq!(strip_inline_comment("R1 a b 1k $ note"), "R1 a b 1k");
        assert_eq!(
            strip_inline_comment("R1 a b 1 $SUB=sub! $[rppd] w=1"),
            "R1 a b 1 $SUB=sub! $[rppd] w=1"
        );
        assert_eq!(strip_inline_comment("V1 a 0 'x;y'"), "V1 a 0 'x;y'");
    }

    #[test]
    fn instance_kinds_split_nodes_from_model() {
        let nl = parse(
            "* c\nXM1 d g s b sky130_fd_pr__nfet_01v8 L=0.15 W=1\n+ nf=1\nR1 a b 10 m=1\nQ1 c b e s npn 2\n\
             E1 o 0 i 0 2\nE2 o 0 vol='v(a)*2'\nA1 [ clk ] [q0 q1] counter\nXI0 a y / inv\n\
             RR1 a b 123 $SUB=sub! $[rppd] w=1u\n",
        );
        let i = &nl.top.insts;
        assert_eq!(
            (i[0].nodes.len(), i[0].model.as_str(), i[0].params.len()),
            (4, "sky130_fd_pr__nfet_01v8", 3)
        );
        assert_eq!(
            (
                i[1].nodes.len(),
                i[1].model.as_str(),
                i[1].values.as_slice()
            ),
            (2, "", &["10".to_string()][..])
        );
        assert_eq!((i[2].nodes.len(), i[2].model.as_str()), (4, "npn"));
        assert_eq!(i[3].nodes.len(), 4);
        assert_eq!(i[4].nodes.len(), 2);
        assert_eq!(
            (
                i[5].nodes.as_slice(),
                i[5].shape.as_str(),
                i[5].model.as_str()
            ),
            (
                &["clk".to_string(), "q0".into(), "q1".into()][..],
                "[n][nn]",
                "counter"
            )
        );
        assert_eq!((i[6].nodes.len(), i[6].model.as_str()), (2, "inv"));
        assert_eq!(
            (i[7].nodes.as_slice(), i[7].model.as_str()),
            (&["a".to_string(), "b".into(), "sub!".into()][..], "rppd")
        );
        assert!(nl.warnings.is_empty(), "{:?}", nl.warnings);
    }

    #[test]
    fn blocks_includes_and_end() {
        let nl = parse(
            ".subckt inv a y vdd params: w=1\nM1 y a vdd vdd p w=w\n.ends\n.include /A/B.lib\n.lib \"/X/Y.lib\" TT\n\
             .control\n  save   all\nrun\n.endc\n.end\nR9 x y 1\n",
        );
        assert_eq!(nl.subckts.len(), 1);
        assert_eq!(nl.subckts[0].ports, ["a", "y", "vdd"]);
        assert_eq!(nl.subckts[0].defaults, [("w".to_string(), "1".to_string())]);
        assert_eq!(nl.includes, ["include /A/B.lib", "lib /X/Y.lib tt"]);
        assert_eq!(nl.controls, ["save all\nrun"]);
        assert!(nl.top.insts.is_empty(), "nothing after .end");
    }
}
