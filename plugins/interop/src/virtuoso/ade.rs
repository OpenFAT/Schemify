//! Maestro (ADE Explorer/Assembler) ⇄ bench tables (spec `interop.md` §9).
//!
//! Spine of [`export`]: the bench's analysis block (`analysis_source`) → its lines
//! (analyses, test model files, `.temp`, `.param`) → the other code blocks (spectre text as
//! definition files) → vars and their sweeps → corners → outputs and specs → reports for
//! the rest → `Test::records` (§9 order). Spine of [`import`]: `parse` (every record
//! checked, with its line) → vars and sweeps → corners → the analysis block (SPICE when
//! every analysis has a SPICE form, else Spectre) → outputs and specs → definition files.
//!
//! Both directions go through one table, `Test`: the export fills it from a bench and
//! writes it; the import parses records into it and writes bench records. Names chosen on
//! import are those the export can carry, so an unchanged maestro re-imports to the same
//! record identities and the stash (§7.3) restores the bench byte for byte: the analysis
//! block is named after the test (the export names the test after that block), a sweep after
//! its var (the export maps only sweeps so named), a definition file `<name>.scs` after its
//! spectre block.

// Data design (six questions): in = one bench document (≤ 2^22 records) or one maestro
// test's records; out = records or bench records. Rows per kind live in one `Vec` each
// (existence-based: no row for an absent kind), at most `ROWS_MAX` per kind, strings owned
// (values are short text). Lifetime: one call. Access: each kind is written once, in §9
// order, after sorting. Benches are independent (parallel by bench; one bench here).

use schemify_expr::{Mode, Program, Scratch, Value as XValue, write_num};
use schemify_format::{Document, FileKind, RecordKind};

use super::ael::{self, AelName, Name};
use crate::design::{Cell, Design};
use crate::notes::Notes;
use crate::sfyx::{Error, Record, Value, i, pairs, s, text};

/// Rows per kind (vars, analyses, corners, models, outputs, specs, definition files) of one
/// test; more is refused on import (a bench, at most 2^22 records, is asserted below it on
/// export).
pub const ROWS_MAX: usize = 1 << 16;

/// Points per sweep (run.md §2's bound).
pub const SWEEP_POINTS_MAX: u32 = 65_536;

/// One analysis row: `(name, enabled, options)`.
type Analysis = (String, bool, Vec<(String, String)>);

/// One maestro test and the setup it sees: the table both directions go through.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Test {
    name: String,
    lib: String,
    cell: String,
    view: String,
    sim: String,
    /// `(name, value, scope)`: scope `""` global, else a corner name.
    vars: Vec<(String, String, String)>,
    /// `(name, enabled, options)`.
    analyses: Vec<Analysis>,
    /// `(name, enabled)`.
    corners: Vec<(String, bool)>,
    /// `(corner, file, section)`: corner `""` = the test's model files.
    models: Vec<(String, String, String)>,
    /// `(name, kind, expr)`: kind `point` or `net`.
    outputs: Vec<(String, String, String)>,
    /// `(output, op, value)`: op `gt` or `lt`.
    specs: Vec<(String, String, String)>,
    /// `(file, text)`.
    defns: Vec<(String, String)>,
}

impl Test {
    /// The records of the test in §9 order: `maestro`; global `var`s by name; `analysis`
    /// by name; the test's `model`s in order; per corner (by name) `corner`, its `model`s in
    /// order and its `var`s by name; `output`s by name; `spec`s by (output, op); `defn`s by
    /// file.
    fn records(&self) -> Vec<Record> {
        let by_name = |a: &(String, String, String), b: &(String, String, String)| {
            (&a.0, &a.2).cmp(&(&b.0, &b.2))
        };
        let mut vars = self.vars.clone();
        vars.sort_by(by_name);
        let mut analyses = self.analyses.clone();
        analyses.sort_by(|a, b| a.0.cmp(&b.0));
        let mut corners = self.corners.clone();
        corners.sort();
        let mut outputs = self.outputs.clone();
        outputs.sort();
        let mut specs = self.specs.clone();
        specs.sort();
        let mut defns = self.defns.clone();
        defns.sort();
        let var =
            |v: &(String, String, String)| Record::new("var", vec![s(&v.0), s(&v.1), s(&v.2)]);
        let model =
            |m: &(String, String, String)| Record::new("model", vec![s(&m.0), s(&m.1), s(&m.2)]);
        let mut out = vec![Record::new(
            "maestro",
            vec![
                s(&self.name),
                s(&self.lib),
                s(&self.cell),
                s(&self.view),
                s(&self.sim),
            ],
        )];
        out.extend(vars.iter().filter(|v| v.2.is_empty()).map(var));
        for (n, en, opts) in &analyses {
            let kv = pairs(opts.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            out.push(Record::new("analysis", vec![s(n), i(i32::from(*en)), kv]));
        }
        out.extend(self.models.iter().filter(|m| m.0.is_empty()).map(model));
        for (c, en) in &corners {
            out.push(Record::new("corner", vec![s(c), i(i32::from(*en))]));
            out.extend(self.models.iter().filter(|m| &m.0 == c).map(model));
            out.extend(vars.iter().filter(|v| &v.2 == c).map(var));
        }
        for (n, k, e) in &outputs {
            out.push(Record::new("output", vec![s(n), s(k), s(e)]));
        }
        for (o, op, v) in &specs {
            out.push(Record::new("spec", vec![s(o), s(op), s(v)]));
        }
        for (f, t) in &defns {
            out.push(Record::new("defn", vec![s(f), text(t)]));
        }
        let n = self.vars.len()
            + self.analyses.len()
            + self.corners.len()
            + self.models.len()
            + self.outputs.len()
            + self.specs.len()
            + self.defns.len();
        assert_eq!(out.len(), n + 1, "every row is one record");
        out
    }
}

// ---------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------

/// Code languages whose text is SPICE (format.md rank 6).
const SPICE_LANGS: [&str; 3] = ["spice", "ngspice", "xyce"];

/// The maestro records of bench cell `ci` (§9.1), in §9 order, the `maestro` record naming
/// Virtuoso library `vlib` (the plan's library of the cell) and the cell's name. Bench
/// content §9.1 does not map is reported in `notes` (`needs_review:` `code sweep output
/// spec expression design-variable montecarlo optimize config`); the caller stashes it
/// (§7.3).
///
/// # Panics
///
/// When `ci` is not a `.shbench` of `d`.
pub fn export(d: &Design, ci: usize, vlib: &str, notes: &mut Notes) -> Vec<Record> {
    let c = &d.cells[ci];
    assert_eq!(
        c.kind(),
        FileKind::Shbench,
        "{}: maestro is a bench's",
        c.path
    );
    // A bench with no simulation setup (a schematic with no symbol, as a Virtuoso
    // testbench without maestro imports) gets no maestro view: none is created in Virtuoso.
    let setup = [
        RecordKind::Var,
        RecordKind::Corner,
        RecordKind::Sweep,
        RecordKind::Output,
        RecordKind::Spec,
        RecordKind::Code,
        RecordKind::Montecarlo,
        RecordKind::Optimize,
        RecordKind::Config,
    ];
    if !setup.iter().any(|k| c.records(*k).next().is_some()) {
        return Vec::new();
    }
    let cell = c.path.as_str();
    let vars: Vec<String> = c
        .records(RecordKind::Var)
        .map(|r| c.field(r, 0).to_owned())
        .collect();
    let src = analysis_source(c);
    let mut t = Test {
        name: safe_name(src.map_or(c.name.as_str(), |r| c.field(r, 0))),
        lib: vlib.to_owned(),
        cell: c.name.clone(),
        view: "schematic".into(),
        sim: "spectre".into(),
        ..Test::default()
    };
    // Code blocks: the analysis block's lines, spectre text as definition files.
    let mut codes: Vec<u32> = c.records(RecordKind::Code).collect();
    codes.sort_by(|a, b| c.field(*a, 0).cmp(c.field(*b, 0)));
    for r in codes {
        let (name, lang) = (c.field(r, 0), c.field(r, 1));
        let lines: Vec<&str> = c.doc.verbatim(r).collect();
        if Some(r) == src {
            let spice = SPICE_LANGS.contains(&lang);
            for (k, line) in lines.iter().enumerate() {
                if !source_line(line, spice, &vars, &mut t, cell, notes) && !line.trim().is_empty()
                {
                    notes.review(
                        "code",
                        cell,
                        format!("code {name} line {}: `{}` has no maestro form", k + 1, line),
                    );
                }
            }
        } else if lang == "spectre" && !name.contains(['/', '\\']) {
            let mut body = lines.join("\n");
            body.push('\n');
            t.defns.push((format!("{name}.scs"), body));
        } else {
            let why = if src.is_some() && SPICE_LANGS.contains(&lang) {
                "only the analyses of the first analysis block map"
            } else {
                "no maestro form"
            };
            notes.review("code", cell, format!("code {name} {lang}: {why}"));
        }
    }
    // Vars and their sweeps.
    let mut swept: Vec<(String, String)> = Vec::new();
    let mut sweeps: Vec<u32> = c.records(RecordKind::Sweep).collect();
    sweeps.sort_by(|a, b| c.field(*a, 0).cmp(c.field(*b, 0)));
    for r in sweeps {
        let name = c.field(r, 0);
        let var = c.attr(r, "var").unwrap_or("");
        let points = if name != var || !vars.iter().any(|v| v == var) {
            Err("a sweep maps only when named after its bench var".to_owned())
        } else if swept.iter().any(|x| x.0 == var) {
            Err("a second sweep of the var".to_owned())
        } else {
            sweep_list(c, r, &vars)
        };
        match points {
            Ok(list) => swept.push((var.to_owned(), list)),
            Err(why) => notes.review("sweep", cell, format!("sweep {name}: {why}")),
        }
    }
    for r in c.records(RecordKind::Var) {
        let name = c.field(r, 0);
        if t.vars.iter().any(|v| v.0 == name && v.2.is_empty()) {
            notes.review(
                "code",
                cell,
                format!("var {name}: also set by the analysis block"),
            );
            continue;
        }
        let value = match swept.iter().find(|x| x.0 == name) {
            Some((_, list)) => Some(list.clone()),
            None => to_ael(c.field(r, 1), &vars, cell, notes),
        };
        if let Some(v) = value {
            t.vars.push((name.to_owned(), v, String::new()));
        }
    }
    // Corners.
    for r in c.records(RecordKind::Corner) {
        let corner = c.field(r, 0).to_owned();
        t.corners.push((corner.clone(), true));
        for a in c.doc.attrs(r) {
            let vals: Vec<&str> = c.doc.attr_values(a).collect();
            match (c.doc.attr_key(a), vals.as_slice()) {
                ("lib", [p, sec]) => t
                    .models
                    .push((corner.clone(), path_to_ade(p), (*sec).into())),
                ("include", [p]) => t
                    .models
                    .push((corner.clone(), path_to_ade(p), String::new())),
                ("set", [var, e]) => {
                    if let Some(v) = to_ael(e, &vars, cell, notes) {
                        t.vars.push(((*var).into(), v, corner.clone()));
                    }
                }
                ("temp", [e]) => {
                    if let Some(v) = to_ael(e, &vars, cell, notes) {
                        t.vars.push(("temperature".into(), v, corner.clone()));
                    }
                }
                (k, _) => notes.review(
                    "code",
                    cell,
                    format!("corner {corner}: `{k}` has no maestro form"),
                ),
            }
        }
    }
    // Outputs and specs.
    let letter = signal_letter(&t.analyses);
    for r in c.records(RecordKind::Output) {
        let (name, e) = (c.field(r, 0), c.field(r, 1));
        match out_to_ade(e, &vars, letter) {
            Some(x) => t.outputs.push((name.to_owned(), "point".into(), x)),
            None => notes.review(
                "output",
                cell,
                format!("output {name}: `{e}` has no ADE form"),
            ),
        }
    }
    for r in c.records(RecordKind::Spec) {
        let out = c.field(r, 0);
        if !t.outputs.iter().any(|o| o.0 == out) {
            notes.review("spec", cell, format!("spec {out}: its output does not map"));
            continue;
        }
        for (key, op) in [("min", "gt"), ("max", "lt")] {
            if let Some(e) = c.attr(r, key)
                && let Some(v) = to_ael(e, &vars, cell, notes)
            {
                t.specs.push((out.to_owned(), op.into(), v));
            }
        }
    }
    for (k, reason) in [
        (RecordKind::Montecarlo, "montecarlo"),
        (RecordKind::Optimize, "optimize"),
        (RecordKind::Config, "config"),
    ] {
        for r in c.records(k) {
            notes.review(
                reason,
                cell,
                format!("{reason} {}: not mapped (§9.3)", c.field(r, 0)),
            );
        }
    }
    for rows in [
        t.vars.len(),
        t.models.len(),
        t.outputs.len(),
        t.specs.len(),
        t.defns.len(),
    ] {
        assert!(
            rows <= ROWS_MAX,
            "{cell}: a bench holds at most 2^22 records"
        );
    }
    t.records()
}

/// The code block whose analyses map: the first (by name) SPICE block with a line that is
/// an analysis, or spectre block whose every line but comments has a maestro form and one
/// is an analysis (the import's own Spectre form, §9.2).
fn analysis_source(c: &Cell) -> Option<u32> {
    let mut codes: Vec<u32> = c.records(RecordKind::Code).collect();
    codes.sort_by(|a, b| c.field(*a, 0).cmp(c.field(*b, 0)));
    codes.into_iter().find(|&r| {
        let lines: Vec<&str> = c.doc.verbatim(r).collect();
        match c.field(r, 1) {
            l if SPICE_LANGS.contains(&l) => lines
                .iter()
                .any(|x| matches!(spice_line(x), Some(SpiceLine::Analysis(..)))),
            "spectre" => {
                lines
                    .iter()
                    .any(|x| matches!(spectre_line(x), Some(SpectreLine::Analysis(..))))
                    && lines.iter().all(|x| {
                        let t = x.trim();
                        t.is_empty() || t.starts_with("//") || spectre_line(x).is_some()
                    })
            }
            _ => false,
        }
    })
}

/// A Virtuoso-safe test name: `[A-Za-z0-9_]`, others `_`.
fn safe_name(n: &str) -> String {
    let out: String = n
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() { "test".into() } else { out }
}

/// Adds the maestro form of one line of the analysis block to `t`; false when it has none.
fn source_line(
    line: &str,
    spice: bool,
    vars: &[String],
    t: &mut Test,
    cell: &str,
    notes: &mut Notes,
) -> bool {
    let parsed = if spice {
        spice_line(line)
    } else {
        spectre_line(line).map(|l| match l {
            SpectreLine::Analysis(n, o) => SpiceLine::Analysis(n, o),
            SpectreLine::Model(f, sec) => SpiceLine::Model(f, sec),
            SpectreLine::Temp(v) => SpiceLine::Temp(v),
        })
    };
    let has_global = |t: &Test, n: &str| t.vars.iter().any(|v| v.0 == n && v.2.is_empty());
    match parsed {
        None => false,
        Some(SpiceLine::Analysis(name, opts)) => {
            let op_only = |o: &[(String, String)]| o.len() == 1 && o[0].0 == "saveOppoint";
            match t.analyses.iter_mut().find(|a| a.0 == name) {
                None => {
                    t.analyses.push((name, true, opts));
                    true
                }
                // `.op` and `.dc` share ADE's one dc analysis.
                Some(a) if name == "dc" && (op_only(&a.2) != op_only(&opts)) => {
                    if op_only(&a.2) {
                        let keep = std::mem::take(&mut a.2);
                        a.2 = opts;
                        a.2.extend(keep);
                    } else {
                        a.2.extend(opts);
                    }
                    true
                }
                Some(_) => false,
            }
        }
        Some(SpiceLine::Model(f, sec)) => {
            t.models.push((String::new(), path_to_ade(&f), sec));
            true
        }
        Some(SpiceLine::Temp(v))
            if !has_global(t, "temperature") && !vars.iter().any(|x| x == "temperature") =>
        {
            let v = if spice {
                to_ael(&v, vars, cell, notes)
            } else {
                Some(v)
            };
            v.map(|v| t.vars.push(("temperature".into(), v, String::new())))
                .is_some()
        }
        Some(SpiceLine::Temp(_)) => false,
        Some(SpiceLine::Param(kvs)) => {
            if kvs
                .iter()
                .any(|(k, _)| has_global(t, k) || vars.iter().any(|x| x == k))
            {
                return false;
            }
            let mut rows = Vec::new();
            for (k, e) in kvs {
                match to_ael(&e, vars, cell, notes) {
                    Some(v) => rows.push((k, v, String::new())),
                    None => return false,
                }
            }
            t.vars.extend(rows);
            true
        }
    }
}

/// What one line of an analysis block means.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SpiceLine {
    /// An ADE analysis and its options (values already AEL).
    Analysis(String, Vec<(String, String)>),
    /// A model file and section (`""` for `.include`).
    Model(String, String),
    /// The temperature (Schemify expression for SPICE, AEL for Spectre lines).
    Temp(String),
    /// `.param k=v …` (Schemify expressions).
    Param(Vec<(String, String)>),
}

fn kv(k: &str, v: &str) -> (String, String) {
    (k.to_owned(), v.to_owned())
}

/// AC/noise sweep options of `dec|lin n f1 f2` (values SPICE numbers).
fn freq_opts(ty: &str, n: &str, f1: &str, f2: &str) -> Option<Vec<(String, String)>> {
    let (f1, f2, n) = (
        ael::number_to_ael(f1)?,
        ael::number_to_ael(f2)?,
        ael::number_to_ael(n)?,
    );
    let mut o = vec![kv("start", &f1), kv("stop", &f2)];
    match ty.to_ascii_lowercase().as_str() {
        "dec" => o.extend([
            kv("incrType", "Logarithmic"),
            kv("stepTypeLog", "Points Per Decade"),
            kv("dec", &n),
        ]),
        "lin" => o.extend([
            kv("incrType", "Linear"),
            kv("stepTypeLin", "Number of Steps"),
            kv("lin", &n),
        ]),
        _ => return None,
    }
    Some(o)
}

/// The maestro meaning of one SPICE line, `None` for lines §9.1 does not map (anything
/// that does not parse exactly: extra words such as `uic`, expressions, two-source `.dc`).
fn spice_line(line: &str) -> Option<SpiceLine> {
    let w: Vec<&str> = line.split_whitespace().collect();
    let num = |x: &str| ael::number_to_ael(x);
    let dir = w.first()?.to_ascii_lowercase();
    Some(match (dir.as_str(), &w[1..]) {
        (".tran", [_, stop, rest @ ..]) if rest.len() <= 2 => {
            let mut o = vec![kv("stop", &num(stop)?)];
            if let Some(st) = rest.first() {
                o.push(kv("start", &num(st)?));
            }
            if let Some(m) = rest.get(1) {
                o.push(kv("maxstep", &num(m)?));
            }
            num(w[1])?;
            SpiceLine::Analysis("tran".into(), o)
        }
        (".ac", [ty, n, f1, f2]) => SpiceLine::Analysis("ac".into(), freq_opts(ty, n, f1, f2)?),
        (".dc", [src, a, b, st]) if is_ident(src) => SpiceLine::Analysis(
            "dc".into(),
            vec![
                kv("sweep", "Component Parameter"),
                kv("dev", &format!("/{src}")),
                kv("param", "dc"),
                kv("start", &num(a)?),
                kv("stop", &num(b)?),
                kv("step", &num(st)?),
            ],
        ),
        (".op", []) => SpiceLine::Analysis("dc".into(), vec![kv("saveOppoint", "t")]),
        (".noise", [out, src, ty, n, f1, f2]) if is_ident(src) => {
            let node = out
                .strip_prefix("v(")
                .or_else(|| out.strip_prefix("V("))
                .and_then(|x| x.strip_suffix(')'))
                .filter(|x| is_ident(x))?;
            let mut o = freq_opts(ty, n, f1, f2)?;
            o.push(kv("oprobe", &format!("/{node}")));
            o.push(kv("iprobe", &format!("/{src}")));
            SpiceLine::Analysis("noise".into(), o)
        }
        (".temp", [t]) => SpiceLine::Temp((*t).to_owned()),
        (".param", kvs) if !kvs.is_empty() => {
            let mut out = Vec::new();
            for p in kvs {
                let (k, v) = p.split_once('=')?;
                if !is_ident(k) || v.is_empty() || Program::compile(v, Mode::Scalar).is_err() {
                    return None;
                }
                out.push((k.to_owned(), v.to_owned()));
            }
            SpiceLine::Param(out)
        }
        (".include" | ".inc", [f]) => SpiceLine::Model(unquote_path(f), String::new()),
        (".lib", [f, sec]) => SpiceLine::Model(unquote_path(f), (*sec).to_owned()),
        _ => return None,
    })
}

fn unquote_path(f: &str) -> String {
    f.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(f)
        .to_owned()
}

fn is_ident(x: &str) -> bool {
    let mut ch = x.chars();
    ch.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && ch.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// What one line of a Spectre analysis block (the import's own form, §9.2) means.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SpectreLine {
    /// `<kind> <kind> k=v …`.
    Analysis(String, Vec<(String, String)>),
    /// `include "<file>" [section=<s>]`.
    Model(String, String),
    /// `temp options temp=<t>`.
    Temp(String),
}

/// Splits a Spectre line into words; a `"…"` (with `\"` and `\\`) may sit inside a word.
fn spectre_words(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut it = line.chars();
    while let Some(ch) = it.next() {
        match ch {
            '"' => {
                started = true;
                loop {
                    match it.next()? {
                        '"' => break,
                        '\\' => cur.push(it.next()?),
                        o => cur.push(o),
                    }
                }
            }
            ch if ch.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            ch => {
                started = true;
                cur.push(ch);
            }
        }
    }
    if started {
        out.push(cur);
    }
    Some(out)
}

fn spectre_line(line: &str) -> Option<SpectreLine> {
    let w = spectre_words(line)?;
    let opts = |rest: &[String]| -> Option<Vec<(String, String)>> {
        rest.iter()
            .map(|p| {
                p.split_once('=')
                    .map(|(k, v)| kv(k, v))
                    .filter(|(k, _)| is_ident(k))
            })
            .collect()
    };
    match w.as_slice() {
        [inc, f, rest @ ..] if inc == "include" && rest.len() <= 1 => {
            let sec = match rest.first() {
                Some(x) => x.strip_prefix("section=")?.to_owned(),
                None => String::new(),
            };
            Some(SpectreLine::Model(f.clone(), sec))
        }
        [a, b, t] if a == "temp" && b == "options" => {
            Some(SpectreLine::Temp(t.strip_prefix("temp=")?.to_owned()))
        }
        [a, b, rest @ ..] if a == b && is_ident(a) => {
            Some(SpectreLine::Analysis(a.clone(), opts(rest)?))
        }
        _ => None,
    }
}

/// A Spectre word for `v`: quoted when empty or holding white space, `"`, `\` or `=`.
fn spectre_word(v: &str) -> String {
    if !v.is_empty()
        && !v
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\\' | '='))
    {
        return v.to_owned();
    }
    let mut out = String::from("\"");
    for ch in v.chars() {
        if matches!(ch, '"' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// `${NAME}` → `$NAME` (§9.1).
fn path_to_ade(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut rest = p;
    while let Some(k) = rest.find("${") {
        out.push_str(&rest[..k]);
        match rest[k + 2..].find('}') {
            Some(e) => {
                out.push('$');
                out.push_str(&rest[k + 2..k + 2 + e]);
                rest = &rest[k + 3 + e..];
            }
            None => {
                out.push_str(&rest[k..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// `$NAME` → `${NAME}` (the inverse of [`path_to_ade`]).
fn path_from_ade(p: &str) -> String {
    let mut out = String::with_capacity(p.len() + 4);
    let b = p.as_bytes();
    let mut k = 0;
    while k < b.len() {
        if b[k] == b'$' && k + 1 < b.len() && (b[k + 1].is_ascii_alphabetic() || b[k + 1] == b'_') {
            let mut e = k + 1;
            while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'_') {
                e += 1;
            }
            out.push_str("${");
            out.push_str(&p[k + 1..e]);
            out.push('}');
            k = e;
        } else {
            let ch = p[k..].chars().next().expect("in bounds");
            out.push(ch);
            k += ch.len_utf8();
        }
    }
    out
}

/// A bench value as AEL (§3.4: bench vars bare), its review reason noted; `None` (noted
/// `expression`) when it has no AEL form.
fn to_ael(e: &str, vars: &[String], cell: &str, notes: &mut Notes) -> Option<String> {
    let r = ael::to_ael(e, |n| {
        if vars.iter().any(|v| v == n) {
            Name::BenchVar
        } else {
            Name::Unknown
        }
    });
    match r {
        Ok(t) => {
            if let Some(why) = t.review {
                notes.review(why, cell, format!("`{e}` → `{}`", t.text));
            }
            Some(t.text)
        }
        Err(why) => {
            notes.review(why, cell, format!("`{e}` has no AEL form"));
            None
        }
    }
}

/// The comma list of a sweep's points (run.md §2): `values` entries through §3.4, `lin`/
/// `log` points as canonical number text.
fn sweep_list(c: &Cell, r: u32, vars: &[String]) -> Result<String, String> {
    let shape = |k: &str| {
        c.doc
            .find_attr(r, k)
            .map(|a| c.doc.attr_values(a).collect::<Vec<_>>())
    };
    let points: Vec<String> = match (shape("values"), shape("lin"), shape("log")) {
        (Some(v), None, None) => {
            let mut out = Vec::new();
            for e in v {
                let t = ael::to_ael(e, |n| {
                    if vars.iter().any(|x| x == n) {
                        Name::BenchVar
                    } else {
                        Name::Unknown
                    }
                })
                .map_err(|_| format!("`{e}` has no AEL form"))?;
                if t.review.is_some() || top_level_split(&t.text).len() != 1 {
                    return Err(format!("`{e}` has no comma-list form"));
                }
                out.push(t.text);
            }
            out
        }
        (None, Some(a), None) | (None, None, Some(a)) if a.len() == 3 => {
            let bound = |x: &str| {
                ael::fold_numeric(x)
                    .and_then(|t| t.parse::<f64>().ok())
                    .ok_or(format!("bound `{x}` is not a constant"))
            };
            let n: u32 = a[2]
                .parse()
                .ok()
                .filter(|n| (1..=SWEEP_POINTS_MAX).contains(n))
                .ok_or("count out of range")?;
            grid(bound(a[0])?, bound(a[1])?, n, shape("log").is_some())
                .ok_or("log bounds must be nonzero with one sign")?
        }
        _ => return Err("needs exactly one of values, lin, log".into()),
    };
    if points.len() < 2 {
        return Err("one point is the var's value, not a sweep".into());
    }
    Ok(points.join(","))
}

/// `n` points from `a` to `b` (run.md §2): linear, or geometric through the expression
/// language (bit-identical on every target), the last exactly `b`; canonical number text.
fn grid(a: f64, b: f64, n: u32, log: bool) -> Option<Vec<String>> {
    assert!((1..=SWEEP_POINTS_MAX).contains(&n));
    if log && (a == 0.0 || b == 0.0 || (a < 0.0) != (b < 0.0)) {
        return None;
    }
    let geo = Program::compile("a * (b / a) ^ t", Mode::Scalar).expect("constant source");
    let mut scratch = Scratch::default();
    let mut out = Vec::with_capacity(n as usize);
    for k in 0..n {
        let t = if n == 1 {
            0.0
        } else {
            f64::from(k) / f64::from(n - 1)
        };
        let x = match (k + 1 == n && n > 1, log) {
            (true, _) => b,
            (false, false) => a + (b - a) * t,
            (false, true) => {
                let ins = [XValue::Num(a), XValue::Num(b), XValue::Num(t)];
                match geo.eval(&ins, &mut scratch) {
                    Ok(XValue::Num(x)) => x,
                    _ => return None,
                }
            }
        };
        let mut s = String::new();
        write_num(&mut s, x);
        out.push(s);
    }
    assert_eq!(out.len(), n as usize);
    Some(out)
}

/// `s` split at commas outside parentheses and strings, each piece trimmed.
fn top_level_split(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut q, mut start) = (0i32, false, 0);
    let mut esc = false;
    for (k, ch) in s.char_indices() {
        match ch {
            _ if esc => esc = false,
            '\\' if q => esc = true,
            '"' => q = !q,
            '(' if !q => depth += 1,
            ')' if !q => depth -= 1,
            ',' if !q && depth == 0 => {
                out.push(s[start..k].trim());
                start = k + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

// ---------------------------------------------------------------------------------------
// Outputs (calculator ⇄ ADE expressions)
// ---------------------------------------------------------------------------------------

/// Calculator functions with an ADE spelling: (Schemify, ADE).
const OUT_FNS: [(&str, &str); 9] = [
    ("ymax", "ymax"),
    ("ymin", "ymin"),
    ("avg", "average"),
    ("rms", "rms"),
    ("integ", "integ"),
    ("deriv", "deriv"),
    ("abs", "abs"),
    ("sqrt", "sqrt"),
    ("exp", "exp"),
];

/// The ADE signal letter for `v()`/`i()` (§9.1): the first of tran (`T`), ac (`F`), a dc
/// sweep (`S`), an operating point (`DC`) the test runs; `T` without analyses.
fn signal_letter(analyses: &[Analysis]) -> &'static str {
    let has = |n: &str| analyses.iter().find(|a| a.0 == n);
    if has("tran").is_some() {
        "T"
    } else if has("ac").is_some() {
        "F"
    } else if let Some(dc) = has("dc") {
        if dc.2.iter().any(|(k, _)| k == "sweep") {
            "S"
        } else {
            "DC"
        }
    } else {
        "T"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tk {
    Ws,
    Num,
    Ident,
    Str,
    Op,
}

/// Tokens `(kind, text)` of an expression in either syntax; `None` on a stray character or
/// an unterminated string.
fn out_tokens(s: &str) -> Option<Vec<(Tk, &str)>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut k = 0;
    while k < b.len() {
        let st = k;
        let c = b[k];
        let kind = if c.is_ascii_whitespace() {
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
            Tk::Ws
        } else if c.is_ascii_digit() || (c == b'.' && b.get(k + 1).is_some_and(u8::is_ascii_digit))
        {
            while k < b.len() && (b[k].is_ascii_digit() || b[k] == b'.') {
                k += 1;
            }
            if k < b.len() && (b[k] == b'e' || b[k] == b'E') {
                let mut j = k + 1;
                if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                    j += 1;
                }
                if j < b.len() && b[j].is_ascii_digit() {
                    k = j;
                    while k < b.len() && b[k].is_ascii_digit() {
                        k += 1;
                    }
                }
            }
            while k < b.len() && b[k].is_ascii_alphabetic() {
                k += 1;
            }
            Tk::Num
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while k < b.len()
                && (b[k].is_ascii_alphanumeric()
                    || b[k] == b'_'
                    || (b[k] == b'.'
                        && b.get(k + 1)
                            .is_some_and(|n| n.is_ascii_alphabetic() || *n == b'_')))
            {
                k += 1;
            }
            Tk::Ident
        } else if c == b'"' {
            k += 1;
            loop {
                match b.get(k)? {
                    b'"' => break,
                    b'\\' => k += 2,
                    _ => k += 1,
                }
            }
            k += 1;
            Tk::Str
        } else if s[k..].starts_with("**") {
            k += 2;
            Tk::Op
        } else if b"+-*/()^,%<>=!?:&|".contains(&c) {
            k += 1;
            Tk::Op
        } else {
            return None;
        };
        out.push((kind, &s[st..k]));
    }
    Some(out)
}

/// Index of the next non-space token at or after `k`.
fn next_tok(t: &[(Tk, &str)], mut k: usize) -> usize {
    while k < t.len() && t[k].0 == Tk::Ws {
        k += 1;
    }
    k
}

/// The single argument of the call whose `(` is at `k`: (argument token, index after `)`).
fn one_arg<'a>(t: &[(Tk, &'a str)], k: usize) -> Option<((Tk, &'a str), usize)> {
    if t.get(k)?.1 != "(" {
        return None;
    }
    let a = next_tok(t, k + 1);
    let close = next_tok(t, a + 1);
    (t.get(close)?.1 == ")").then(|| (t[a], close + 1))
}

/// A calculator output expression as an ADE expression (§9.1); `None` when it uses
/// anything the table lacks.
fn out_to_ade(e: &str, vars: &[String], letter: &str) -> Option<String> {
    let t = out_tokens(e)?;
    let mut out = String::with_capacity(e.len() + 16);
    let mut k = 0;
    while k < t.len() {
        let (kind, x) = t[k];
        match kind {
            Tk::Ws => out.push_str(x),
            Tk::Num => out.push_str(&ael::number_to_ael(x)?),
            Tk::Str => return None,
            Tk::Op => match x {
                "+" | "-" | "*" | "/" | "(" | ")" => out.push_str(x),
                "^" => out.push_str("**"),
                _ => return None,
            },
            Tk::Ident => {
                let open = next_tok(&t, k + 1);
                let is_call = t.get(open).is_some_and(|n| n.1 == "(");
                if is_call && matches!(x, "v" | "V" | "i" | "I") {
                    let ((ak, a), after) = one_arg(&t, open)?;
                    let name = match ak {
                        Tk::Ident => a.to_owned(),
                        Tk::Str if !a.contains('\\') => a[1..a.len() - 1].to_owned(),
                        _ => return None,
                    };
                    let path = name.replace('.', "/").replace('[', "<").replace(']', ">");
                    if x.eq_ignore_ascii_case("v") {
                        out.push_str(&format!("V{letter}(\"/{path}\")"));
                    } else {
                        out.push_str(&format!("I{letter}(\"/{path}/PLUS\")"));
                    }
                    k = after;
                    continue;
                } else if is_call {
                    out.push_str(OUT_FNS.iter().find(|f| f.0 == x)?.1);
                } else if vars.iter().any(|v| v == x) {
                    out.push_str(&format!("VAR(\"{x}\")"));
                } else {
                    return None;
                }
            }
        }
        k += 1;
    }
    Some(out)
}

/// An ADE output expression as a calculator expression (the inverse of [`out_to_ade`]);
/// `None` when it uses anything else or does not compile.
fn out_from_ade(e: &str) -> Option<String> {
    let t = out_tokens(e)?;
    let mut out = String::with_capacity(e.len());
    let mut k = 0;
    while k < t.len() {
        let (kind, x) = t[k];
        match kind {
            Tk::Ws => out.push_str(x),
            Tk::Num => out.push_str(&ael::from_ael(x, |_| AelName::Unknown, |_| None)?.text),
            Tk::Str => return None,
            Tk::Op => match x {
                "+" | "-" | "*" | "/" | "(" | ")" => out.push_str(x),
                "**" => out.push('^'),
                _ => return None,
            },
            Tk::Ident => {
                let open = next_tok(&t, k + 1);
                let sig = matches!(x, "VT" | "VF" | "VS" | "VDC" | "IT" | "IF" | "IS" | "IDC");
                if sig || x == "VAR" {
                    let ((ak, a), after) = one_arg(&t, open)?;
                    if ak != Tk::Str || a.contains('\\') {
                        return None;
                    }
                    let a = &a[1..a.len() - 1];
                    if x == "VAR" {
                        if !is_ident(a) {
                            return None;
                        }
                        out.push_str(a);
                    } else {
                        let p = a.strip_prefix('/')?;
                        let p = if x.starts_with('I') {
                            p.strip_suffix("/PLUS")?
                        } else {
                            p
                        };
                        let name = p.replace('/', ".").replace('<', "[").replace('>', "]");
                        let bare = !name.is_empty() && name.split('.').all(is_ident);
                        let f = if x.starts_with('V') { 'v' } else { 'i' };
                        if bare {
                            out.push_str(&format!("{f}({name})"));
                        } else {
                            out.push_str(&format!("{f}(\"{name}\")"));
                        }
                    }
                    k = after;
                    continue;
                }
                if t.get(open).is_some_and(|n| n.1 == "(") {
                    out.push_str(OUT_FNS.iter().find(|f| f.1 == x)?.0);
                } else {
                    return None;
                }
            }
        }
        k += 1;
    }
    Program::compile(&out, Mode::Calc).ok().map(|_| out)
}

// ---------------------------------------------------------------------------------------
// Parse (records → Test), the import's boundary check
// ---------------------------------------------------------------------------------------

fn bad(r: &Record, msg: impl Into<String>) -> Error {
    Error {
        line: r.line,
        msg: format!("{}: {}", r.tag, msg.into()),
    }
}

fn arity(r: &Record, n: usize) -> Result<(), Error> {
    if r.fields.len() == n {
        Ok(())
    } else {
        Err(bad(r, format!("{} fields, expected {n}", r.fields.len())))
    }
}

fn flag(r: &Record, k: usize) -> Result<bool, Error> {
    match r.int(k)? {
        0 => Ok(false),
        1 => Ok(true),
        v => Err(bad(r, format!("enable {v} (0 or 1)"))),
    }
}

/// Parses one maestro test (its `maestro` record first): every record's arity and element
/// types, `enable` 0/1, options as `(key value)` strings, a `var`/`model` scope naming a
/// corner declared before it, a `spec` naming an output before it with op `lt`/`gt`, output
/// kinds `point`/`net`, no name twice per kind, at most [`ROWS_MAX`] rows per kind.
fn parse(records: &[Record]) -> Result<Test, Error> {
    let m = records.first().ok_or(Error {
        line: 0,
        msg: "maestro: an empty test".into(),
    })?;
    if m.tag != "maestro" {
        return Err(bad(m, "a test starts with its maestro record"));
    }
    arity(m, 5)?;
    let mut t = Test {
        name: m.str(0)?.to_owned(),
        lib: m.str(1)?.to_owned(),
        cell: m.str(2)?.to_owned(),
        view: m.str(3)?.to_owned(),
        sim: m.str(4)?.to_owned(),
        ..Test::default()
    };
    if t.name.is_empty() || t.cell.is_empty() {
        return Err(bad(m, "empty test or cell name"));
    }
    for r in &records[1..] {
        let scope_ok = |t: &Test, sc: &str| sc.is_empty() || t.corners.iter().any(|c| c.0 == sc);
        match r.tag.as_str() {
            "var" => {
                arity(r, 3)?;
                let (n, v, sc) = (r.str(0)?, r.str(1)?, r.str(2)?);
                if n.is_empty() || !scope_ok(&t, sc) {
                    return Err(bad(
                        r,
                        format!("var `{n}` in scope `{sc}`: no such corner before it"),
                    ));
                }
                if t.vars.iter().any(|x| x.0 == n && x.2 == sc) {
                    return Err(bad(r, format!("var {n} twice in scope `{sc}`")));
                }
                t.vars.push((n.into(), v.into(), sc.into()));
            }
            "analysis" => {
                arity(r, 3)?;
                let n = r.str(0)?;
                let en = flag(r, 1)?;
                let mut opts = Vec::new();
                for o in r.list(2)? {
                    match o.items() {
                        [Value::Str(k), Value::Str(v)] => opts.push((k.clone(), v.clone())),
                        _ => {
                            return Err(bad(
                                r,
                                format!("analysis {n}: an option is not (key value) strings"),
                            ));
                        }
                    }
                }
                if n.is_empty() || t.analyses.iter().any(|a| a.0 == n) {
                    return Err(bad(r, format!("analysis `{n}` empty or twice")));
                }
                t.analyses.push((n.into(), en, opts));
            }
            "corner" => {
                arity(r, 2)?;
                let n = r.str(0)?;
                let en = flag(r, 1)?;
                if n.is_empty() || t.corners.iter().any(|c| c.0 == n) {
                    return Err(bad(r, format!("corner `{n}` empty or twice")));
                }
                t.corners.push((n.into(), en));
            }
            "model" => {
                arity(r, 3)?;
                let (sc, f, sec) = (r.str(0)?, r.str(1)?, r.str(2)?);
                if f.is_empty() || !scope_ok(&t, sc) {
                    return Err(bad(
                        r,
                        format!("model `{f}` of `{sc}`: empty file or no such corner before it"),
                    ));
                }
                t.models.push((sc.into(), f.into(), sec.into()));
            }
            "output" => {
                arity(r, 3)?;
                let (n, k, e) = (r.str(0)?, r.str(1)?, r.str(2)?);
                if !matches!(k, "point" | "net") {
                    return Err(bad(r, format!("output {n}: kind `{k}` (point or net)")));
                }
                if n.is_empty() || t.outputs.iter().any(|o| o.0 == n) {
                    return Err(bad(r, format!("output `{n}` empty or twice")));
                }
                t.outputs.push((n.into(), k.into(), e.into()));
            }
            "spec" => {
                arity(r, 3)?;
                let (o, op, v) = (r.str(0)?, r.str(1)?, r.str(2)?);
                if !matches!(op, "lt" | "gt") {
                    return Err(bad(r, format!("spec {o}: op `{op}` (lt or gt)")));
                }
                if !t.outputs.iter().any(|x| x.0 == o) {
                    return Err(bad(
                        r,
                        format!("spec names output `{o}`, not declared before it"),
                    ));
                }
                if t.specs.iter().any(|x| x.0 == o && x.1 == op) {
                    return Err(bad(r, format!("spec {o} {op} twice")));
                }
                t.specs.push((o.into(), op.into(), v.into()));
            }
            "defn" => {
                arity(r, 2)?;
                let f = r.str(0)?;
                let body = r.text(1)?;
                if f.is_empty() || f.contains(['/', '\\']) || t.defns.iter().any(|d| d.0 == f) {
                    return Err(bad(r, format!("defn `{f}`: empty, a path, or twice")));
                }
                t.defns.push((f.into(), body));
            }
            "maestro" => return Err(bad(r, "a second test in one test's records")),
            o => return Err(bad(r, format!("`{o}` is not a maestro record"))),
        }
        let rows = [
            t.vars.len(),
            t.analyses.len(),
            t.corners.len(),
            t.models.len(),
        ];
        if rows
            .iter()
            .chain(&[t.outputs.len(), t.specs.len(), t.defns.len()])
            .any(|&n| n > ROWS_MAX)
        {
            return Err(bad(r, "more than 2^16 rows of one kind"));
        }
    }
    Ok(t)
}

/// Checks one maestro test's records (its `maestro` record first) as [`import`] reads them.
///
/// # Errors
///
/// The first malformed record, with its line: wrong arity or element types, `enable` other
/// than 0/1, an option that is not `(key value)` strings, a var/model naming a corner not
/// declared before it, a spec naming an output not declared before it or an op other than
/// `lt`/`gt`, an output kind other than `point`/`net`, a name twice in its kind, a
/// definition file that is a path.
pub fn check(records: &[Record]) -> Result<(), Error> {
    parse(records).map(|_| ())
}

// ---------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------

/// Kinds that mean a test was already imported into the bench being built.
const BENCH_KINDS: [RecordKind; 6] = [
    RecordKind::Var,
    RecordKind::Corner,
    RecordKind::Sweep,
    RecordKind::Output,
    RecordKind::Spec,
    RecordKind::Code,
];

/// Appends the bench records (`var`, `sweep`, `corner`, `output`, `spec`, `code`) that
/// `records` (one maestro test and its records, in file order) describe to `bench` (§9.2).
/// What cannot come back is reported in `notes` under `cell`: an analysis without a SPICE
/// form (the block is then Spectre), an output whose ADE expression does not translate (a
/// comment in the analysis block), a disabled corner or analysis, a value without a
/// Schemify form, a second test of the same view (only the first maps).
///
/// Records [`check`] refuses are reported (`needs_review:code`) and nothing is added; the
/// reader checks them first, so this happens only for records built in memory.
pub fn import(records: &[Record], bench: &mut Document<'static>, cell: &str, notes: &mut Notes) {
    let t = match parse(records) {
        Ok(t) => t,
        Err(e) => {
            notes.review("code", cell, format!("maestro refused: {e}"));
            return;
        }
    };
    if (1..bench.record_count()).any(|r| BENCH_KINDS.contains(&bench.record_kind(r))) {
        notes.review(
            "config",
            cell,
            format!(
                "maestro test {}: only the first test of a view maps",
                t.name
            ),
        );
        return;
    }
    let before = bench.record_count();
    let globals: Vec<String> = t
        .vars
        .iter()
        .filter(|v| v.2.is_empty() && v.0 != "temperature")
        .map(|v| v.0.clone())
        .collect();
    let from = |v: &str, notes: &mut Notes| -> Option<String> {
        let r = ael::from_ael(
            v,
            |n| {
                if globals.iter().any(|g| g == n) {
                    AelName::BenchVar
                } else {
                    AelName::Unknown
                }
            },
            |_| None,
        );
        match r {
            Some(tr) if Program::compile(&tr.text, Mode::Scalar).is_ok() => {
                if let Some(why) = tr.review {
                    notes.review(why, cell, format!("`{v}` → `{}`", tr.text));
                }
                Some(tr.text)
            }
            _ => {
                notes.review("expression", cell, format!("`{v}` has no Schemify form"));
                None
            }
        }
    };
    // Vars and sweeps.
    for (n, v, sc) in t
        .vars
        .iter()
        .filter(|v| v.2.is_empty() && v.0 != "temperature")
    {
        debug_assert!(sc.is_empty());
        let parts = top_level_split(v);
        let mut pts = Vec::new();
        for p in &parts {
            match from(p, notes) {
                Some(x) => pts.push(x),
                None => break,
            }
        }
        if pts.len() != parts.len() {
            continue;
        }
        begin(bench, RecordKind::Var, &[n, &pts[0]]);
        if pts.len() > 1 {
            begin(bench, RecordKind::Sweep, &[n]);
            attr(bench, "var", &[n]);
            let (key, vals) = sweep_shape(&pts);
            attr(
                bench,
                key,
                &vals.iter().map(String::as_str).collect::<Vec<_>>(),
            );
        }
    }
    // Corners.
    for (c, en) in &t.corners {
        if !en {
            notes.review("config", cell, format!("corner {c} is disabled in maestro"));
        }
        begin(bench, RecordKind::Corner, &[c]);
        for (_, f, sec) in t.models.iter().filter(|m| &m.0 == c) {
            let sec = sec
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(sec);
            if sec.is_empty() {
                attr(bench, "include", &[&path_from_ade(f)]);
            } else {
                attr(bench, "lib", &[&path_from_ade(f), sec]);
            }
        }
        let mut temp = None;
        for (n, v, _) in t.vars.iter().filter(|v| &v.2 == c) {
            if top_level_split(v).len() > 1 {
                notes.review(
                    "expression",
                    cell,
                    format!("corner {c} var {n}: a sweep list"),
                );
                continue;
            }
            let Some(x) = from(v, notes) else { continue };
            if n == "temperature" {
                temp = Some(x);
            } else {
                attr(bench, "set", &[n, &x]);
            }
        }
        if let Some(x) = temp {
            attr(bench, "temp", &[&x]);
        }
    }
    // The analysis block: SPICE when every analysis has a SPICE form, else Spectre.
    let temp = t
        .vars
        .iter()
        .find(|v| v.0 == "temperature" && v.2.is_empty())
        .map(|v| &v.1);
    let enabled: Vec<&Analysis> = t
        .analyses
        .iter()
        .filter(|a| {
            if !a.1 {
                notes.review(
                    "code",
                    cell,
                    format!("analysis {} is disabled in maestro: dropped", a.0),
                );
            }
            a.1
        })
        .collect();
    let spice: Option<Vec<String>> = enabled
        .iter()
        .map(|a| spice_form(&a.0, &a.2))
        .collect::<Option<Vec<Vec<String>>>>()
        .map(|v| v.concat());
    let spice_temp = match temp {
        Some(v) => from(v, notes).map(|x| format!(".temp {x}")),
        None => Some(String::new()),
    };
    let mut lines: Vec<String> = Vec::new();
    let as_spice = spice.is_some() && spice_temp.is_some();
    if as_spice {
        for (_, f, sec) in t.models.iter().filter(|m| m.0.is_empty()) {
            let f = path_from_ade(f);
            let f = if f.contains(char::is_whitespace) {
                format!("\"{f}\"")
            } else {
                f
            };
            lines.push(if sec.is_empty() {
                format!(".include {f}")
            } else {
                format!(".lib {f} {sec}")
            });
        }
        lines.extend(spice_temp.filter(|x| !x.is_empty()));
        lines.extend(spice.unwrap_or_default());
    } else {
        for a in &enabled {
            if spice_form(&a.0, &a.2).is_none() {
                notes.review(
                    "code",
                    cell,
                    format!(
                        "analysis {} has no SPICE form: kept in the spectre block",
                        a.0
                    ),
                );
            }
        }
        for (_, f, sec) in t.models.iter().filter(|m| m.0.is_empty()) {
            let mut l = format!("include {}", spectre_word(f));
            if !sec.is_empty() {
                l.push_str(&format!(" section={}", spectre_word(sec)));
            }
            lines.push(l);
        }
        if let Some(v) = temp {
            lines.push(format!("temp options temp={}", spectre_word(v)));
        }
        for (n, _, opts) in &enabled {
            let mut l = format!("{n} {n}");
            for (k, v) in opts {
                l.push_str(&format!(" {k}={}", spectre_word(v)));
            }
            lines.push(l);
        }
    }
    // Outputs and specs.
    let comment = if as_spice { "*" } else { "//" };
    let mut kept: Vec<&str> = Vec::new();
    for (n, k, e) in &t.outputs {
        match (k.as_str(), out_from_ade(e)) {
            ("point", Some(x)) => {
                begin(bench, RecordKind::Output, &[n, &x]);
                kept.push(n);
            }
            _ => {
                notes.review(
                    "output",
                    cell,
                    format!("output {n} ({k}) `{e}` has no calculator form"),
                );
                lines.push(format!("{comment} ade output {n} {k}: {e}").replace(['\n', '\r'], " "));
            }
        }
    }
    for o in &kept {
        let mut bounds: Vec<(&str, String)> = Vec::new();
        for (key, op) in [("min", "gt"), ("max", "lt")] {
            if let Some((_, _, v)) = t.specs.iter().find(|x| x.0 == *o && x.1 == op)
                && let Some(x) = from(v, notes)
            {
                bounds.push((key, x));
            }
        }
        if !bounds.is_empty() {
            begin(bench, RecordKind::Spec, &[o]);
            for (key, x) in bounds {
                attr(bench, key, &[&x]);
            }
        }
    }
    for (o, op, _) in &t.specs {
        if !kept.contains(&o.as_str()) {
            notes.review(
                "spec",
                cell,
                format!("spec {o} {op}: its output did not come back"),
            );
        }
    }
    if !lines.is_empty() {
        begin(
            bench,
            RecordKind::Code,
            &[&t.name, if as_spice { "spice" } else { "spectre" }],
        );
        for l in &lines {
            bench.push_verbatim(l);
        }
    }
    // Definition files.
    for (f, body) in &t.defns {
        let stem = f.strip_suffix(".scs").unwrap_or(f);
        if stem == t.name {
            notes.review(
                "code",
                cell,
                format!("defn {f}: its name is the analysis block's"),
            );
            continue;
        }
        begin(bench, RecordKind::Code, &[stem, "spectre"]);
        for l in body.lines() {
            bench.push_verbatim(&l.replace('\r', ""));
        }
    }
    assert!(bench.record_count() >= before, "the import only appends");
}

fn begin(doc: &mut Document<'static>, k: RecordKind, fields: &[&str]) {
    doc.begin_record(k);
    for f in fields {
        doc.push_field(f);
    }
}

fn attr(doc: &mut Document<'static>, key: &str, vals: &[&str]) {
    doc.push_attr(key);
    for v in vals {
        doc.push_value(v);
    }
}

/// The sweep attribute that reproduces `pts` exactly: `lin a b n`, else `log a b n`, else
/// `values …` (lin/log only when every point is canonical number text).
fn sweep_shape(pts: &[String]) -> (&'static str, Vec<String>) {
    assert!(pts.len() >= 2);
    let canon = |p: &String| {
        p.parse::<f64>().ok().filter(|x| {
            let mut s = String::new();
            write_num(&mut s, *x);
            &s == p
        })
    };
    let n = pts.len() as u32;
    if let (Some(a), Some(b), true) = (
        canon(&pts[0]),
        canon(&pts[pts.len() - 1]),
        n <= SWEEP_POINTS_MAX,
    ) {
        for (key, log) in [("lin", false), ("log", true)] {
            if grid(a, b, n, log).as_deref() == Some(pts) {
                return (
                    key,
                    vec![pts[0].clone(), pts[pts.len() - 1].clone(), n.to_string()],
                );
            }
        }
    }
    ("values", pts.to_vec())
}

/// The SPICE lines of one ADE analysis (§9.2), `None` when it has none: options exactly
/// those §9.1 writes for it, numbers only.
fn spice_form(name: &str, opts: &[(String, String)]) -> Option<Vec<String>> {
    let get = |k: &str| opts.iter().find(|o| o.0 == k).map(|o| o.1.as_str());
    let num = |k: &str| -> Option<String> {
        let t = ael::from_ael(get(k)?, |_| AelName::Unknown, |_| None)?.text;
        ael::number_to_ael(&t).map(|_| t)
    };
    let only = |keys: &[&str]| opts.iter().all(|o| keys.contains(&o.0.as_str()));
    let freq = || -> Option<String> {
        let (f1, f2) = (num("start")?, num("stop")?);
        match (get("incrType")?, get("stepTypeLog"), get("stepTypeLin")) {
            ("Logarithmic", Some("Points Per Decade"), None) => {
                Some(format!("dec {} {f1} {f2}", num("dec")?))
            }
            ("Linear", None, Some("Number of Steps")) => {
                Some(format!("lin {} {f1} {f2}", num("lin")?))
            }
            _ => None,
        }
    };
    const FREQ: [&str; 7] = [
        "start",
        "stop",
        "incrType",
        "stepTypeLog",
        "stepTypeLin",
        "dec",
        "lin",
    ];
    match name {
        "tran" if only(&["stop", "start", "maxstep"]) => {
            let stop = num("stop")?;
            let x: f64 = ael::fold_numeric(&stop)?.parse().ok()?;
            let mut step = String::new();
            write_num(&mut step, x / 1000.0);
            let mut l = format!(".tran {step} {stop}");
            match (
                get("start").map(|_| num("start")),
                get("maxstep").map(|_| num("maxstep")),
            ) {
                (None, None) => {}
                (Some(st), None) => l.push_str(&format!(" {}", st?)),
                (st, Some(m)) => {
                    l.push_str(&format!(" {} {}", st.unwrap_or(Some("0".into()))?, m?))
                }
            }
            Some(vec![l])
        }
        "ac" if only(&FREQ) => Some(vec![format!(".ac {}", freq()?)]),
        "dc" => {
            let op = get("saveOppoint");
            let sweep = ["sweep", "dev", "param", "start", "stop", "step"];
            if !only(&[&sweep[..], &["saveOppoint"]].concat()) || op.is_some_and(|v| v != "t") {
                return None;
            }
            let mut out = Vec::new();
            if op.is_some() {
                out.push(".op".to_owned());
            }
            if opts.len() > usize::from(op.is_some()) {
                let dev = get("dev")?.strip_prefix('/').filter(|d| is_ident(d))?;
                if get("sweep")? != "Component Parameter" || get("param")? != "dc" {
                    return None;
                }
                out.push(format!(
                    ".dc {dev} {} {} {}",
                    num("start")?,
                    num("stop")?,
                    num("step")?
                ));
            }
            (!out.is_empty()).then_some(out)
        }
        "noise" if only(&[&FREQ[..], &["oprobe", "iprobe"]].concat()) => {
            let o = get("oprobe")?.strip_prefix('/').filter(|d| is_ident(d))?;
            let src = get("iprobe")?.strip_prefix('/').filter(|d| is_ident(d))?;
            Some(vec![format!(".noise v({o}) {src} {}", freq()?)])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_translate_both_ways() {
        let vars = vec!["vdd".to_owned()];
        let a = out_to_ade(
            "ymax(v(out)) - avg(v(x1.n)) * 2^2 / vdd + i(V1)",
            &vars,
            "T",
        )
        .unwrap();
        assert_eq!(
            a,
            "ymax(VT(\"/out\")) - average(VT(\"/x1/n\")) * 2**2 / VAR(\"vdd\") + IT(\"/V1/PLUS\")"
        );
        assert_eq!(
            out_from_ade(&a).unwrap(),
            "ymax(v(out)) - avg(v(x1.n)) * 2^2 / vdd + i(V1)"
        );
        assert_eq!(
            out_from_ade(&out_to_ade("v(\"d[3]\")", &[], "F").unwrap()).unwrap(),
            "v(\"d[3]\")"
        );
        assert!(out_to_ade("cross(v(out), 0.5)", &vars, "T").is_none());
        assert!(out_to_ade("x > 1", &vars, "T").is_none());
        assert!(out_from_ade("bandwidth(mag(VF(\"/OUT\")) 3 \"low\")").is_none());
        assert!(out_from_ade("IT(\"/M1/D\")").is_none());
    }

    #[test]
    fn paths_and_words_round_trip() {
        assert_eq!(path_to_ade("${PDK}/m.lib"), "$PDK/m.lib");
        assert_eq!(path_from_ade("$PDK/m.lib"), "${PDK}/m.lib");
        assert_eq!(path_from_ade("a$/b"), "a$/b");
        for v in ["", "a b", "x=\"1\"", "plain", "\\"] {
            let w = spectre_words(&format!("k={}", spectre_word(v))).unwrap();
            assert_eq!(w, vec![format!("k={v}")]);
        }
        assert_eq!(top_level_split("pow(a,b), 2,3"), vec!["pow(a,b)", "2", "3"]);
    }

    #[test]
    fn sweeps_recover_their_shape() {
        let lin = grid(1.0, 3.0, 3, false).unwrap();
        assert_eq!(lin, ["1", "2", "3"]);
        assert_eq!(sweep_shape(&lin).0, "lin");
        let log = grid(1.0, 100.0, 3, true).unwrap();
        assert_eq!(
            sweep_shape(&log),
            ("log", vec!["1".into(), "100".into(), "3".into()])
        );
        let v: Vec<String> = ["1k", "2"].map(String::from).to_vec();
        assert_eq!(sweep_shape(&v).0, "values");
        assert!(grid(0.0, 1.0, 2, true).is_none());
    }
}
