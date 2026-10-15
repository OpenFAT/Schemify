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
use crate::design::{Cell, Design, push_attr, push_record};
use crate::notes::Notes;
use crate::retarget::Target;
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

/// One bench's export: its maestro records and its OCEAN script (§9.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BenchExport {
    /// The maestro records in §9 order; empty for a bench with no simulation setup.
    pub records: Vec<Record>,
    /// The bench cell's name: the script's name and results directory.
    pub cell: String,
    /// The OCEAN script text; empty when `records` is.
    pub ocean: String,
    /// Definition files the script loads (`definitionFile`), `(file, text)`: the same
    /// files as the `defn` records, written beside the script under `<cell>/`.
    pub defns: Vec<(String, String)>,
}

/// The maestro records and the OCEAN script of bench cell `ci` (§9.1, §9.6), the
/// `maestro` record naming Virtuoso library `vlib` (the plan's library of the cell) and the
/// cell's name. `target` (a `.retarget` file given with `--target`) replaces the models of
/// every corner it names; `inst` answers the output translator's instance questions
/// ([`InstInfo`]). Bench content §9.1 does not map is reported in `notes`
/// (`needs_review:` `code sweep output spec plot expression design-variable montecarlo
/// optimize config corner`); the caller stashes it (§7.3).
///
/// # Panics
///
/// When `ci` is not a `.shbench` of `d`.
pub fn export<F: Fn(&str) -> Option<InstInfo>>(
    d: &Design,
    ci: usize,
    vlib: &str,
    target: Option<&Target>,
    inst: F,
    notes: &mut Notes,
) -> BenchExport {
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
        return BenchExport::default();
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
            // `.options errpreset=…` sets an option of the tran analysis: after the analyses.
            let mut order: Vec<usize> = (0..lines.len()).collect();
            order.sort_by_key(|&k| {
                spice && matches!(spice_line(lines[k]), Some(SpiceLine::ErrPreset(_)))
            });
            for k in order {
                let line = &lines[k];
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
        // The target machine's model files for this corner (`--target`, §10.2) win.
        let bound = target.and_then(|tg| tg.corners.iter().find(|x| x.name == corner));
        if let Some(b) = bound {
            for (f, sec) in &b.models {
                t.models.push((corner.clone(), path_to_ade(f), sec.clone()));
            }
        }
        for a in c.doc.attrs(r) {
            let vals: Vec<&str> = c.doc.attr_values(a).collect();
            match (c.doc.attr_key(a), vals.as_slice()) {
                ("lib", [_, _]) | ("include", [_]) if bound.is_some() => {}
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
    // Outputs (maestro and OCEAN spellings) and their plots.
    let letter = signal_letter(&t.analyses);
    let mut scope = OutScope {
        vars: &vars,
        letter,
        inst,
    };
    let mut ocean_outs: Vec<(String, String)> = Vec::new();
    for r in c.records(RecordKind::Output) {
        let (name, e) = (c.field(r, 0), c.field(r, 1));
        // `analysis <a>` picks the dataset (format.md rank 6), else the test's default.
        scope.letter = match c.attr(r, "analysis") {
            None => letter,
            Some(a) => match analysis_letter(a, &t.analyses) {
                Some(l) => l,
                None => {
                    notes.review(
                        "output",
                        cell,
                        format!("output {name}: analysis `{a}` is not one the test runs"),
                    );
                    continue;
                }
            },
        };
        // One spelling serves both: OCEAN evaluates ADE's expression functions.
        match out_expr(e, &scope) {
            Some(x) => {
                t.outputs.push((name.to_owned(), "point".into(), x.clone()));
                ocean_outs.push((name.to_owned(), x));
            }
            _ => notes.review(
                "output",
                cell,
                format!("output {name}: `{e}` has no ADE form"),
            ),
        }
    }
    let mut plots: Vec<(String, String)> = Vec::new();
    for r in c.records(RecordKind::Output) {
        let (name, Some(x)) = (c.field(r, 0), c.attr(r, "vs")) else {
            continue;
        };
        if !ocean_outs.iter().any(|o| o.0 == name) || !ocean_outs.iter().any(|o| o.0 == x) {
            notes.review(
                "plot",
                cell,
                format!("output {name} vs {x}: an output of the plot does not map"),
            );
            continue;
        }
        plots.push((name.to_owned(), x.to_owned()));
        notes.review(
            "plot",
            cell,
            format!(
                "output {name} vs {x}: plotted by the OCEAN script only (a maestro output is \
                 one value per point; the plot spans the sweep)"
            ),
        );
    }
    for r in c.records(RecordKind::Spec) {
        let out = c.field(r, 0);
        if !t.outputs.iter().any(|o| o.0 == out) {
            notes.review("spec", cell, format!("spec {out}: its output does not map"));
            continue;
        }
        for (key, op) in [("min", "gt"), ("max", "lt")] {
            let Some(e) = c.attr(r, key) else { continue };
            // A bound over bench vars (`min A0_spec/(1+A0_spec)`) is folded to its number
            // (an ADE spec target is a value); one over a swept var varies per point and has
            // no single target.
            let idents: Vec<String> = Program::compile(e, Mode::Scalar)
                .map(|p| p.idents().map(str::to_owned).collect())
                .unwrap_or_default();
            if idents.is_empty() {
                if let Some(v) = to_ael(e, &vars, cell, notes) {
                    t.specs.push((out.to_owned(), op.into(), v));
                }
            } else if let Some(s) = idents.iter().find(|n| swept.iter().any(|x| &x.0 == *n)) {
                notes.review(
                    "spec",
                    cell,
                    format!("spec {out} {key} `{e}`: depends on swept var {s}; not sent"),
                );
            } else if let Some(x) = crate::retarget::cell_value(d, ci, e) {
                let mut v = String::new();
                write_num(&mut v, x);
                t.specs.push((out.to_owned(), op.into(), v));
            } else {
                notes.review(
                    "spec",
                    cell,
                    format!("spec {out} {key} `{e}`: does not evaluate over the bench vars"),
                );
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
    let default_corner = c
        .header("corner")
        .map(str::to_owned)
        .or_else(|| t.corners.first().map(|x| x.0.clone()));
    if t.corners.len() > 1 {
        notes.review(
            "corner",
            cell,
            format!(
                "the OCEAN script runs corner {} only (maestro has all {})",
                default_corner.as_deref().unwrap_or(""),
                t.corners.len()
            ),
        );
    }
    let ocean = ocean_script(
        &t,
        cell,
        &swept,
        &ocean_outs,
        &plots,
        default_corner.as_deref(),
        notes,
    );
    BenchExport {
        records: t.records(),
        cell: t.cell.clone(),
        ocean,
        defns: t.defns.clone(),
    }
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
        Some(SpiceLine::ErrPreset(p)) => match t.analyses.iter_mut().find(|a| a.0 == "tran") {
            Some(a) if !a.2.iter().any(|o| o.0 == "errpreset") => {
                a.2.push(kv("errpreset", &p));
                true
            }
            _ => false,
        },
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

/// Spectre's transient accuracy presets (`tran` parameter `errpreset`).
const ERRPRESETS: [&str; 3] = ["liberal", "moderate", "conservative"];

/// What one line of an analysis block means.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SpiceLine {
    /// `.options errpreset=<preset>`: the transient analysis's accuracy preset.
    ErrPreset(String),
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
        (".options" | ".option", [kv]) => {
            let p = kv.strip_prefix("errpreset=")?;
            SpiceLine::ErrPreset(
                ERRPRESETS
                    .iter()
                    .find(|x| x.eq_ignore_ascii_case(p))?
                    .to_string(),
            )
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

/// `v` as a quoted Spectre string, `"` and `\` escaped.
fn spectre_quoted(v: &str) -> String {
    let w = spectre_word(v);
    if w.starts_with('"') {
        w
    } else {
        format!("\"{w}\"")
    }
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
/// language (bit-identical on every target), the last exactly `b`; each point rounded to
/// 15 significant digits, then canonical number text (`0.01`, never the binary noise
/// `0.010000000000000002` of `0.8 · 1/80`). The rounding moves a point by at most 5e-16
/// relative, far below any simulator's resolution, and makes the list the decimal grid the
/// bench means.
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
        let x: f64 = format!("{x:.14e}").parse().expect("Rust float text parses");
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
// OCEAN script (§9.6)
// ---------------------------------------------------------------------------------------

/// What `maeGetAnalysis` answers for the fields of a new analysis nobody set (ICADVM20.1,
/// read by the probe on a live run, 2026-10-15): (analysis, field, value). A dump drops them
/// like `anaName`, so a setup reads as what was set; a field set to another value is kept.
const FORM_DEFAULTS: &[(&str, &str, &str)] = &[
    ("tran", "trannoisemethod", "default"),
    ("tran", "noiseseed", "1"),
    ("tran", "binnum", "16"),
    ("tran", "noiseruns", "100"),
    ("tran", "dynamicParameter", "Parameter"),
    ("tran", "dynParamChoice", "User-defined"),
    ("tran", "dynParamSetChoice", "User-defined"),
    ("tran", "dynchoice", "Parameter vector"),
    ("tran", "dynamicChoice", "Vector"),
    ("tran", "write", "spectre.ic"),
    ("tran", "writefinal", "spectre.fc"),
    ("tran", "annotate", "status"),
    ("tran", "annotatedigits", "4"),
    ("tran", "progress_t", "60"),
    ("tran", "progress_p", "10"),
    ("tran", "finalTimeOp", "t"),
    ("tran", "maxiters", "5"),
    ("tran", "threshold", "0.0"),
    ("tran", "detail", "node"),
    ("tran", "sort", "name"),
    ("tran", "thermalmethod", "Steady State"),
    ("tran", "triseLimit", "300"),
    ("tran", "resTriseRpt", "No"),
    ("tran", "iterNum", "2"),
    ("tran", "thermalTimeStep", "0"),
    ("tran", "sortMethod", "Temperature Rise"),
    ("tran", "triseTemp", "0.0"),
    ("tran", "saveChannelOnly", "No"),
    ("dc", "rangeType", "Start-Stop"),
    ("dc", "incrType", "Automatic"),
    ("dc", "stepTypeLin", "Step Size"),
    ("dc", "stepTypeLog", "Points Per Decade"),
    ("dc", "write", "spectre.dc"),
    ("dc", "oppoint", "rawfile"),
    ("dc", "maxiters", "150"),
    ("dc", "maxsteps", "10000"),
    ("dc", "annotate", "status"),
    ("dc", "threshold", "0.0"),
    ("dc", "detail", "node"),
    ("dc", "sort", "name"),
    ("ac", "rangeType", "Start-Stop"),
    ("ac", "stepTypeLin", "Step Size"),
    ("ac", "outType", "Voltage"),
    ("ac", "srcType", "port"),
    ("ac", "perturbation", "linear"),
    ("ac", "special", "None"),
    ("ac", "annotate", "status"),
];

fn is_form_default(analysis: &str, k: &str, v: &str) -> bool {
    FORM_DEFAULTS
        .iter()
        .any(|&(a, f, d)| a == analysis && f == k && d == v)
}

/// Maestro analysis options that are ADE form fields (the sweep and step choosers) with no
/// OCEAN `analysis` keyword or Spectre parameter: OCEAN and Spectre infer them from the
/// keywords present.
const FORM_ONLY: [&str; 4] = ["sweep", "incrType", "stepTypeLin", "stepTypeLog"];

/// `s` as a SKILL string literal (`\` and `"` escaped).
fn skill_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether AEL text `v` is one number SKILL reads as such (`20n`, `-0.35`, `1e-6`).
fn skill_number(v: &str) -> bool {
    let body = v.strip_prefix('-').unwrap_or(v);
    ael::from_ael(body, |_| AelName::Unknown, |_| None)
        .and_then(|t| ael::number_to_ael(&t.text))
        .is_some()
}

/// The signals an OCEAN expression reads, `(kind, path)` with kind `'v` or `'i`, in first
/// appearance order without repeats: what the script `save`s.
fn signals_of(expr: &str, out: &mut Vec<(char, String)>) {
    const FNS: [&str; 8] = [
        "VT(\"", "VF(\"", "VS(\"", "VDC(\"", "IT(\"", "IF(\"", "IS(\"", "IDC(\"",
    ];
    let b = expr.as_bytes();
    for start in 0..b.len() {
        let rest = &expr[start..];
        let Some(f) = FNS.iter().find(|f| rest.starts_with(**f)) else {
            continue;
        };
        if start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
            continue; // `myVT("…`: another function
        }
        let tail = &rest[f.len()..];
        let Some(end) = tail.find('"') else { continue };
        let row = (
            if f.starts_with('V') { 'v' } else { 'i' },
            tail[..end].to_owned(),
        );
        if !out.contains(&row) {
            out.push(row);
        }
    }
}

/// The OCEAN script of test `t` (§9.6): runnable headless (`ocean -nograph < <cell>.ocn`)
/// after `SfyImport` (and `SfyRetarget`) created the bench schematic. Corner `corner` (the
/// bench's default, else its first) supplies the model files, var overrides and
/// temperature; every swept var is a nested `paramAnalysis`; each output is one
/// `ocnPrint` table `<cell>/<output>.txt`, each plot one `<cell>/<y>_vs_<x>.txt` of
/// `waveVsWave`. A swept point that is not a number literal is reported (`sweep`) and the
/// sweep runs without it.
fn ocean_script(
    t: &Test,
    cell: &str,
    swept: &[(String, String)],
    outs: &[(String, String)],
    plots: &[(String, String)],
    corner: Option<&str>,
    notes: &mut Notes,
) -> String {
    use std::fmt::Write as _;
    assert!(!t.cell.is_empty(), "a test names its bench cell");
    // Named after the bench cell: tests of different benches may share a name.
    let dir = &t.cell;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "; {dir}.ocn: OCEAN script of bench {cell}, written by schemify-interop (interop.md §9.6).\n\
         ; Run after SfyImport (and SfyRetarget) of the same .sfyx, in a directory whose cds.lib\n\
         ; defines library {lib} (OCEAN finds libraries through ./cds.lib) and where {dir}/ may\n\
         ; be created; if SfyImport had ?lib, put that library in design() below:\n\
         ;   ocean -nograph < {dir}.ocn\n\
         ; Writes {dir}/psf (results) and {dir}/<output>.txt (one ocnPrint table per output).",
        lib = t.lib
    );
    let _ = writeln!(o, "createDir({})", skill_str(dir));
    let _ = writeln!(o, "simulator('spectre)");
    let _ = writeln!(
        o,
        "design({} {} {})",
        skill_str(&t.lib),
        skill_str(&t.cell),
        skill_str(&t.view)
    );
    let _ = writeln!(o, "resultsDir({})", skill_str(&format!("{dir}/psf")));
    let in_scope = |sc: &str| sc.is_empty() || Some(sc) == corner;
    let models: Vec<String> = t
        .models
        .iter()
        .filter(|m| in_scope(&m.0))
        .map(|m| format!("'({} {})", skill_str(&m.1), skill_str(&m.2)))
        .collect();
    if !models.is_empty() {
        let _ = writeln!(o, "modelFile({})", models.join(" "));
    }
    let mut defns = t.defns.clone();
    defns.sort();
    for (f, _) in &defns {
        let _ = writeln!(o, "definitionFile({})", skill_str(&format!("{dir}/{f}")));
    }
    // Design variables: globals, then the corner's overrides; a swept var starts at its
    // first point (paramAnalysis sets the rest).
    let mut vars: Vec<(String, String)> = Vec::new();
    for (n, v, sc) in &t.vars {
        if !in_scope(sc) {
            continue;
        }
        let v = match swept.iter().find(|s| &s.0 == n) {
            Some((_, list)) => top_level_split(list)[0].to_owned(),
            None => v.clone(),
        };
        match vars.iter_mut().find(|x| &x.0 == n) {
            Some(x) if !sc.is_empty() => x.1 = v,
            Some(_) => {}
            None => vars.push((n.clone(), v)),
        }
    }
    vars.sort();
    for (n, v) in &vars {
        if n == "temperature" {
            let _ = writeln!(o, "temp({v})");
        } else if skill_number(v) {
            let _ = writeln!(o, "desVar({} {v})", skill_str(n));
        } else {
            let _ = writeln!(o, "desVar({} {})", skill_str(n), skill_str(v));
        }
    }
    let mut analyses = t.analyses.clone();
    analyses.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, en, opts) in &analyses {
        if !en {
            continue;
        }
        let mut line = format!("analysis('{name}");
        for (k, v) in opts {
            if FORM_ONLY.contains(&k.as_str()) {
                continue;
            }
            if k == "saveOppoint" {
                line.push_str(" ?saveOppoint t");
            } else {
                let _ = write!(line, " ?{k} {}", skill_str(v));
            }
        }
        line.push(')');
        let _ = writeln!(o, "{line}");
    }
    let mut sigs = Vec::new();
    for (_, e) in outs {
        signals_of(e, &mut sigs);
    }
    for kind in ['v', 'i'] {
        let paths: Vec<String> = sigs
            .iter()
            .filter(|s| s.0 == kind)
            .map(|s| skill_str(&s.1))
            .collect();
        if !paths.is_empty() {
            let _ = writeln!(o, "save('{kind} {})", paths.join(" "));
        }
    }
    // Sweeps: one nested paramAnalysis per swept var, by var name.
    let mut sweeps: Vec<(String, Vec<String>)> = Vec::new();
    for (var, list) in swept {
        let pts: Vec<String> = top_level_split(list)
            .into_iter()
            .filter(|p| {
                let ok = skill_number(p);
                if !ok {
                    notes.review(
                        "sweep",
                        cell,
                        format!("sweep {var}: OCEAN point `{p}` is not a number; left out"),
                    );
                }
                ok
            })
            .map(str::to_owned)
            .collect();
        if !pts.is_empty() {
            sweeps.push((var.clone(), pts));
        }
    }
    sweeps.sort();
    if sweeps.is_empty() {
        let _ = writeln!(o, "run()");
    } else {
        let mut call = String::new();
        for (var, pts) in sweeps.iter().rev() {
            let inner = if call.is_empty() {
                String::new()
            } else {
                format!("\n  {call}")
            };
            call = format!(
                "paramAnalysis({} ?values '({}){inner})",
                skill_str(var),
                pts.join(" ")
            );
        }
        let _ = writeln!(o, "{call}");
        let _ = writeln!(o, "paramRun()");
    }
    for (name, e) in outs {
        let _ = writeln!(
            o,
            "ocnPrint(?output {} ?numberNotation 'scientific ?precision 8 {e})",
            skill_str(&format!("{dir}/{name}.txt"))
        );
    }
    for (y, x) in plots {
        let ye = &outs.iter().find(|q| &q.0 == y).expect("plots map").1;
        let xe = &outs.iter().find(|q| &q.0 == x).expect("plots map").1;
        let _ = writeln!(
            o,
            "ocnPrint(?output {} ?numberNotation 'scientific ?precision 8 waveVsWave(?x {xe} ?y {ye}))",
            skill_str(&format!("{dir}/{y}_vs_{x}.txt"))
        );
    }
    let _ = writeln!(o, "exit()");
    debug_assert!(o.ends_with("exit()\n") && o.contains("design("));
    o
}

// ---------------------------------------------------------------------------------------
// Outputs (calculator ⇄ ADE expressions)
// ---------------------------------------------------------------------------------------

/// One-argument calculator functions with an ADE spelling, both ways: (Schemify, ADE). The
/// ADE ones are the waveform calculator's (*ADE Calculator* / *OCEAN Reference*): `real`
/// `imag` for `re` `im`, `phaseDeg` (degrees, as `ph`), `dB20` for `db`, `peakToPeak` for
/// `pp`, `average` for `avg`.
const OUT_FNS: [(&str, &str); 19] = [
    ("ymax", "ymax"),
    ("ymin", "ymin"),
    ("avg", "average"),
    ("rms", "rms"),
    ("integ", "integ"),
    ("deriv", "deriv"),
    ("abs", "abs"),
    ("sqrt", "sqrt"),
    ("exp", "exp"),
    ("ln", "ln"),
    ("log10", "log10"),
    ("re", "real"),
    ("im", "imag"),
    ("mag", "mag"),
    ("ph", "phaseDeg"),
    ("db", "dB20"),
    ("pp", "peakToPeak"),
    ("phase_margin", "phaseMargin"),
    ("gain_margin", "gainMargin"),
];

/// What the output translator knows about one instance path of the bench.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstInfo {
    /// The instance's Virtuoso path, segments joined by `/` (`DUT/M1` for SPICE's
    /// `XDUT.M1`).
    pub path: String,
    /// The Virtuoso terminal `i(dev)` reads: the exported master's first terminal (`p` of a
    /// `schemify_core` source or ammeter, `PLUS` on analogLib).
    pub terminal: String,
    /// Path segments inside the instance that hold the operating point when the placed
    /// device is a subcircuit macro (`["main"]` for a `.retarget` class with `op
    /// M0.main`); empty for a plain device.
    pub op_inner: Vec<String>,
}

/// The bench facts an output translation reads.
pub struct OutScope<'a, F: Fn(&str) -> Option<InstInfo>> {
    /// The bench's vars (written `VAR("x")`).
    pub vars: &'a [String],
    /// The signal letter of `v()`/`i()`: `T` (tran), `F` (ac), `S` (dc sweep) or `DC` (§9.1).
    pub letter: &'a str,
    /// Instance path (segments joined by `/`) → its facts; `None` when the path names no
    /// instance (then `i()` reads terminal `PLUS` and `op()` the path as written).
    pub inst: F,
}

/// The signal letter of an output's `analysis <a>` attribute: `tran` `T`, `ac` `F`, `dc` (a
/// sweep) `S`, `op` (the operating point) `DC`; `None` for another name or one the test
/// does not run (`op` needs a dc analysis with `saveOppoint`, `dc` one with a sweep).
fn analysis_letter(a: &str, analyses: &[Analysis]) -> Option<&'static str> {
    let dc = analyses.iter().find(|x| x.0 == "dc");
    let has = |k: &str| dc.is_some_and(|d| d.2.iter().any(|o| o.0 == k));
    match a {
        "tran" | "ac" if analyses.iter().any(|x| x.0 == a) => {
            Some(if a == "tran" { "T" } else { "F" })
        }
        "dc" if has("sweep") => Some("S"),
        "op" if has("saveOppoint") => Some("DC"),
        _ => None,
    }
}

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

/// Most nested calls an output expression may hold (bounds the recursion of [`emit`]).
const OUT_DEPTH_MAX: u32 = 64;

/// A calculator output expression as a maestro output (§9.1, the old three-argument form
/// kept for the tests): `i()` on terminal `PLUS`, no instance facts.
#[cfg(test)]
fn out_to_ade(e: &str, vars: &[String], letter: &str) -> Option<String> {
    out_expr(
        e,
        &OutScope {
            vars,
            letter,
            inst: |_: &str| None,
        },
    )
}

/// A calculator output expression as an ADE (maestro) or OCEAN expression (§9.1 outputs
/// table): signals `v(n)`/`i(dev)` → `V<L>("/n")`/`I<L>("/dev/<terminal>")`, bench vars →
/// `VAR("x")`, `re im mag ph db` → `real imag mag phaseDeg dB20`, `at(y,x)` → `value(y x)`,
/// `x()` → `xval(<the expression's first signal>)`, `max`/`min` of two or more values →
/// the exact `((a)+(b)±abs((a)-(b)))/2` (the calculator's `max` is not element-wise),
/// `pow(a,b)` and `^` → `**`, `pi` → its value, `cross(y,l[,edge[,n]])` → `cross(y l n
/// "rising|falling|either")`, `settle(y,tol)` → `settlingTime` over the whole data,
/// `phase_margin`/`gain_margin` → `phaseMargin`/`gainMargin`, `op("M1","gm")` →
/// `OP("/M1" "gm")`. The same text serves maestro and OCEAN. The source's spacing is kept
/// outside rewritten calls, so a value needing no rewrite comes back byte for byte.
///
/// `None` when the expression uses anything else (strings outside `v`/`i`/`op`, other
/// functions, comparisons, `?:`, `x()` without a signal, nesting deeper than 64 calls):
/// the caller reports it and never writes a partial output.
pub fn out_expr<F: Fn(&str) -> Option<InstInfo>>(e: &str, s: &OutScope<'_, F>) -> Option<String> {
    let t = out_tokens(e)?;
    // `x()` is the x axis of the expression's own data: the first signal's.
    let first = (0..t.len()).find_map(|k| {
        let is_sig = t[k].0 == Tk::Ident && matches!(t[k].1, "v" | "V" | "i" | "I");
        let open = next_tok(&t, k + 1);
        (is_sig && t.get(open).is_some_and(|n| n.1 == "("))
            .then(|| signal(&t, k, open, s))
            .flatten()
    });
    let out = emit(&t, 0, t.len(), s, first.as_ref().map(|x| x.0.as_str()), 0)?;
    debug_assert!(
        !out.contains(['^', ',']),
        "AEL has no `^`, SKILL lists no commas"
    );
    Some(out)
}

/// The signal call `v(…)`/`i(…)` whose name is at `k` and `(` at `open`: (ADE text, index
/// after its `)`).
fn signal<F: Fn(&str) -> Option<InstInfo>>(
    t: &[(Tk, &str)],
    k: usize,
    open: usize,
    s: &OutScope<'_, F>,
) -> Option<(String, usize)> {
    let ((ak, a), after) = one_arg(t, open)?;
    let name = match ak {
        Tk::Ident => a.to_owned(),
        Tk::Str if !a.contains('\\') => a[1..a.len() - 1].to_owned(),
        _ => return None,
    };
    let path = name.replace('.', "/");
    let letter = s.letter;
    let bus = |p: &str| p.replace('[', "<").replace(']', ">");
    let text = if t[k].1.eq_ignore_ascii_case("v") {
        // A net inside a subcircuit: its instance prefix through the bench's names.
        let p = match path.rsplit_once('/') {
            Some((insts, net)) => match (s.inst)(insts) {
                Some(i) => format!("{}/{net}", i.path),
                None => path.clone(),
            },
            None => path.clone(),
        };
        format!("V{letter}(\"/{}\")", bus(&p))
    } else {
        let (p, term) = (s.inst)(&path).map_or_else(
            || (path.clone(), "PLUS".to_owned()),
            |i| (i.path, i.terminal),
        );
        format!("I{letter}(\"/{}/{term}\")", bus(&p))
    };
    Some((text, after))
}

/// Index of the `)` matching the `(` at `open`.
fn close_of(t: &[(Tk, &str)], open: usize) -> Option<usize> {
    let mut depth = 0u32;
    for (k, tok) in t.iter().enumerate().skip(open) {
        match tok.1 {
            "(" => depth += 1,
            ")" => {
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

/// The argument token ranges of the call whose `(` is at `open` and `)` at `close`, each
/// trimmed of white space; `[]` for `()`. `None` for an empty argument.
fn call_args(t: &[(Tk, &str)], open: usize, close: usize) -> Option<Vec<(usize, usize)>> {
    let trim = |mut a: usize, mut b: usize| {
        while a < b && t[a].0 == Tk::Ws {
            a += 1;
        }
        while b > a && t[b - 1].0 == Tk::Ws {
            b -= 1;
        }
        (a < b).then_some((a, b))
    };
    let mut out = Vec::new();
    let (mut depth, mut start) = (0u32, open + 1);
    for (k, tok) in t.iter().enumerate().take(close).skip(open + 1) {
        match tok.1 {
            "(" => depth += 1,
            ")" => depth -= 1,
            "," if depth == 0 => {
                out.push(trim(start, k)?);
                start = k + 1;
            }
            _ => {}
        }
    }
    match trim(start, close) {
        Some(r) => out.push(r),
        None if out.is_empty() => {}
        None => return None,
    }
    Some(out)
}

/// The text of a string or bare-identifier argument (`"M1"`, `M1`).
fn word_arg<'a>(t: &[(Tk, &'a str)], r: (usize, usize)) -> Option<&'a str> {
    if r.1 != r.0 + 1 {
        return None;
    }
    match t[r.0] {
        (Tk::Ident, x) => Some(x),
        (Tk::Str, x) if !x.contains('\\') => Some(&x[1..x.len() - 1]),
        _ => None,
    }
}

/// Emits tokens `lo..hi` (see [`out_expr`]); `first` is the expression's first signal.
fn emit<F: Fn(&str) -> Option<InstInfo>>(
    t: &[(Tk, &str)],
    lo: usize,
    hi: usize,
    s: &OutScope<'_, F>,
    first: Option<&str>,
    depth: u32,
) -> Option<String> {
    if depth >= OUT_DEPTH_MAX {
        return None;
    }
    assert!(lo <= hi && hi <= t.len());
    let mut out = String::new();
    let mut k = lo;
    while k < hi {
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
            Tk::Ident => {}
        }
        if kind != Tk::Ident {
            k += 1;
            continue;
        }
        let open = next_tok(t, k + 1);
        if !(open < hi && t[open].1 == "(") {
            if s.vars.iter().any(|v| v == x) {
                out.push_str(&format!("VAR(\"{x}\")"));
            } else if x == "pi" {
                write_num(&mut out, std::f64::consts::PI);
            } else {
                return None;
            }
            k += 1;
            continue;
        }
        if matches!(x, "v" | "V" | "i" | "I") {
            let (text, after) = signal(t, k, open, s)?;
            out.push_str(&text);
            k = after;
            continue;
        }
        let close = close_of(t, open).filter(|c| *c < hi)?;
        let args = call_args(t, open, close)?;
        let arg = |r: (usize, usize)| emit(t, r.0, r.1, s, first, depth + 1);
        match (x, args.as_slice()) {
            ("x", []) => out.push_str(&format!("xval({})", first?)),
            ("at", [y, x0]) => {
                let (y, x0) = (arg(*y)?, arg(*x0)?);
                out.push_str(&format!("value({y} {x0})"));
            }
            ("pow", [a, b]) => {
                let (a, b) = (arg(*a)?, arg(*b)?);
                out.push_str(&format!("({a})**({b})"));
            }
            ("max" | "min", [a, rest @ ..]) if !rest.is_empty() => {
                let sign = if x == "max" { '+' } else { '-' };
                let mut acc = arg(*a)?;
                for b in rest {
                    let b = arg(*b)?;
                    acc = format!("(({acc})+({b}){sign}abs(({acc})-({b})))/2");
                }
                out.push_str(&acc);
            }
            ("op", [inst, param]) => {
                let (inst, param) = (word_arg(t, *inst)?, word_arg(t, *param)?);
                let segs: Vec<&str> = inst.strip_prefix('/').unwrap_or(inst).split('/').collect();
                if param.is_empty() || segs.iter().any(|g| g.is_empty()) {
                    return None;
                }
                // The longest prefix naming instances; the rest names devices inside
                // the placed PDK subcircuit, as written. Without such a rest the
                // `.retarget` class's `op` path (`M0.main`) supplies it.
                let (info, rest) = (1..=segs.len())
                    .rev()
                    .find_map(|n| (s.inst)(&segs[..n].join("/")).map(|i| (i, &segs[n..])))
                    .unwrap_or_else(|| {
                        let i = InstInfo {
                            path: segs.join("/"),
                            ..InstInfo::default()
                        };
                        (i, &segs[segs.len()..])
                    });
                let mut path: Vec<&str> = info.path.split('/').collect();
                if rest.is_empty() {
                    path.extend(info.op_inner.iter().map(String::as_str));
                } else {
                    path.extend(rest.iter().copied());
                }
                out.push_str(&format!("OP(\"/{}\" \"{param}\")", path.join("/")));
            }
            ("cross", [y, level, more @ ..]) if more.len() <= 2 => {
                let edge = match more
                    .first()
                    .map(|r| arg(*r))
                    .map(|e| e.ok_or(()))
                    .transpose()
                    .ok()?
                    .as_deref()
                {
                    None | Some("0") => "either",
                    Some("1") => "rising",
                    Some("-1") => "falling",
                    Some(_) => return None,
                };
                let n = match more.get(1) {
                    None => "1".to_owned(),
                    Some(r) => arg(*r).filter(|n| n.parse::<u32>().is_ok_and(|n| n > 0))?,
                };
                let (y, level) = (arg(*y)?, arg(*level)?);
                out.push_str(&format!("cross({y} {level} {n} \"{edge}\")"));
            }
            ("settle", [y, tol]) => {
                // Absolute tolerance → settlingTime's percent of the whole step,
                // from the first to the last point of the data.
                let (y, tol) = (arg(*y)?, arg(*tol)?);
                let (x0, x1) = (format!("ymin(xval({y}))"), format!("ymax(xval({y}))"));
                out.push_str(&format!(
                    "settlingTime({y} {x0} t {x1} t \
                             100*({tol})/abs(value({y} {x1})-value({y} {x0})))"
                ));
            }
            (_, [a]) => {
                let f = OUT_FNS.iter().find(|f| f.0 == x)?.1;
                out.push_str(&format!("{f}({})", arg(*a)?));
            }
            _ => return None,
        }
        k = close + 1;
    }
    Some(out)
}

/// An ADE output expression as a calculator expression (the inverse of [`out_to_ade`]);
/// `None` when it uses anything else or does not compile.
/// A number as ADE stores it, 7 significant digits, in engineering text (`632.1206m`, `1m`);
/// any other text unchanged.
fn ade_number(t: &str) -> String {
    match schemify_expr::parse_num(t.trim()) {
        Ok(x) if x.is_finite() => {
            let r: f64 = format!("{x:.6e}").parse().unwrap_or(x);
            let mut s = String::new();
            schemify_expr::write_num(&mut s, r);
            crate::virtuoso::read::si_text(&s)
        }
        _ => t.to_owned(),
    }
}

/// `value(y x)`, `cross(y l n "edge")`, `xval(y)` or `OP("/path" "param")`, the call of
/// ADE function `f` between tokens `open` and `close` of `e`, as the calculator expression
/// export turned into it.
fn ade_call_back(e: &str, t: &[(Tk, &str)], f: &str, open: usize, close: usize) -> Option<String> {
    // Arguments are separated by white space at depth 0 between two operands (`a - b`
    // stays one argument).
    let offset = |s: &str| s.as_ptr() as usize - e.as_ptr() as usize;
    let mut args: Vec<&str> = Vec::new();
    let mut depth = 0u32;
    let mut start: Option<usize> = None;
    let mut last: Option<(Tk, &str)> = None;
    for k in open + 1..close {
        let (kind, s) = t[k];
        match kind {
            Tk::Ws if depth == 0 => {
                let next = t[next_tok(t, k)..close].first().copied();
                let op = |x: Option<(Tk, &str)>| {
                    x.is_some_and(|(k, s)| k == Tk::Op && !matches!(s, "(" | ")"))
                };
                if let Some(a) = start.filter(|_| !op(last) && !op(next)) {
                    args.push(&e[a..offset(s)]);
                    start = None;
                }
                continue;
            }
            Tk::Op if s == "(" => depth += 1,
            Tk::Op if s == ")" => depth = depth.checked_sub(1)?,
            _ => {}
        }
        start.get_or_insert(offset(s));
        if kind != Tk::Ws {
            last = Some((kind, s));
        }
    }
    if let Some(a) = start {
        args.push(e[a..offset(t[close].1)].trim_end());
    }
    let unquote = |s: &str| {
        s.strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .filter(|s| !s.contains(['"', '\\']))
            .map(str::to_owned)
    };
    Some(match (f, args.as_slice()) {
        ("value", [y, x0]) => format!("at({}, {})", out_from_ade(y)?, out_from_ade(x0)?),
        ("xval", [_]) => "x()".to_owned(),
        ("cross", [y, l, n, edge]) => {
            let e = match unquote(edge)?.as_str() {
                "either" => "0",
                "rising" => "1",
                "falling" => "-1",
                _ => return None,
            };
            let (y, l) = (out_from_ade(y)?, out_from_ade(l)?);
            match (*n, e) {
                ("1", "0") => format!("cross({y}, {l})"),
                ("1", _) => format!("cross({y}, {l}, {e})"),
                (n, _) if n.parse::<u32>().is_ok_and(|n| n > 0) => {
                    format!("cross({y}, {l}, {e}, {n})")
                }
                _ => return None,
            }
        }
        ("OP", [path, param]) => {
            let (path, param) = (unquote(path)?, unquote(param)?);
            format!("op(\"{}\", \"{param}\")", path.strip_prefix('/')?)
        }
        _ => return None,
    })
}

fn out_from_ade(e: &str) -> Option<String> {
    let t = out_tokens(e)?;
    let mut out = String::with_capacity(e.len());
    let mut k = 0;
    while k < t.len() {
        let (kind, x) = t[k];
        match kind {
            Tk::Ws => out.push_str(x),
            // ADE keeps 7 significant digits (live run: 0.6321205588285577 came back
            // 0.6321206, 1m as 0.001); both sides of the stash comparison read numbers so.
            Tk::Num => out.push_str(&ade_number(
                &ael::from_ael(x, |_| AelName::Unknown, |_| None)?.text,
            )),
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
                            p.strip_suffix("/PLUS").or_else(|| p.strip_suffix("/p"))?
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
                // The calls export writes with several space-separated arguments, back to
                // their calculator form (§9.1 outputs table read backwards).
                if matches!(x, "value" | "cross" | "xval" | "OP")
                    && t.get(open).is_some_and(|n| n.1 == "(")
                {
                    let close = close_of(&t, open)?;
                    out.push_str(&ade_call_back(e, &t, x, open, close)?);
                    k = close + 1;
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

fn flag(r: &Record, k: usize) -> Result<bool, Error> {
    match r.int(k)? {
        0 => Ok(false),
        1 => Ok(true),
        v => Err(r.err(format!("enable {v} (0 or 1)"))),
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
        return Err(m.err("a test starts with its maestro record"));
    }
    m.arity(5)?;
    let mut t = Test {
        name: m.str(0)?.to_owned(),
        lib: m.str(1)?.to_owned(),
        cell: m.str(2)?.to_owned(),
        view: m.str(3)?.to_owned(),
        sim: m.str(4)?.to_owned(),
        ..Test::default()
    };
    if t.name.is_empty() || t.cell.is_empty() {
        return Err(m.err("empty test or cell name"));
    }
    for r in &records[1..] {
        let scope_ok = |t: &Test, sc: &str| sc.is_empty() || t.corners.iter().any(|c| c.0 == sc);
        match r.tag.as_str() {
            "var" => {
                r.arity(3)?;
                let (n, v, sc) = (r.str(0)?, r.str(1)?, r.str(2)?);
                if n.is_empty() || !scope_ok(&t, sc) {
                    return Err(r.err(format!(
                        "var `{n}` in scope `{sc}`: no such corner before it"
                    )));
                }
                if t.vars.iter().any(|x| x.0 == n && x.2 == sc) {
                    return Err(r.err(format!("var {n} twice in scope `{sc}`")));
                }
                t.vars.push((n.into(), v.into(), sc.into()));
            }
            "analysis" => {
                r.arity(3)?;
                let n = r.str(0)?;
                let en = flag(r, 1)?;
                let mut opts = Vec::new();
                for o in r.list(2)? {
                    match o.items() {
                        [Value::Str(k), Value::Str(v)] => opts.push((k.clone(), v.clone())),
                        _ => {
                            return Err(r.err(format!(
                                "analysis {n}: an option is not (key value) strings"
                            )));
                        }
                    }
                }
                if n.is_empty() || t.analyses.iter().any(|a| a.0 == n) {
                    return Err(r.err(format!("analysis `{n}` empty or twice")));
                }
                // maeGetAnalysis answers form fields nothing set: `anaName` (the analysis
                // name) and the ac/noise sweep chooser `sweep "Frequency"`. They carry no
                // meaning, so a dump of a setup we wrote reads as what we wrote.
                opts.retain(|(k, v)| {
                    !((k == "anaName" && v == n)
                        || (k == "sweep" && v == "Frequency" && matches!(n, "ac" | "noise"))
                        || is_form_default(n, k, v))
                });
                t.analyses.push((n.into(), en, opts));
            }
            "corner" => {
                r.arity(2)?;
                let n = r.str(0)?;
                let en = flag(r, 1)?;
                if n.is_empty() || t.corners.iter().any(|c| c.0 == n) {
                    return Err(r.err(format!("corner `{n}` empty or twice")));
                }
                t.corners.push((n.into(), en));
            }
            "model" => {
                r.arity(3)?;
                let (sc, f, sec) = (r.str(0)?, r.str(1)?, r.str(2)?);
                if f.is_empty() || !scope_ok(&t, sc) {
                    return Err(r.err(format!(
                        "model `{f}` of `{sc}`: empty file or no such corner before it"
                    )));
                }
                t.models.push((sc.into(), f.into(), sec.into()));
            }
            "output" => {
                r.arity(3)?;
                let (n, k, e) = (r.str(0)?, r.str(1)?, r.str(2)?);
                if !matches!(k, "point" | "net") {
                    return Err(r.err(format!("output {n}: kind `{k}` (point or net)")));
                }
                if n.is_empty() || t.outputs.iter().any(|o| o.0 == n) {
                    return Err(r.err(format!("output `{n}` empty or twice")));
                }
                t.outputs.push((n.into(), k.into(), e.into()));
            }
            "spec" => {
                r.arity(3)?;
                let (o, op, v) = (r.str(0)?, r.str(1)?, r.str(2)?);
                if !matches!(op, "lt" | "gt") {
                    return Err(r.err(format!("spec {o}: op `{op}` (lt or gt)")));
                }
                if !t.outputs.iter().any(|x| x.0 == o) {
                    return Err(r.err(format!("spec names output `{o}`, not declared before it")));
                }
                if t.specs.iter().any(|x| x.0 == o && x.1 == op) {
                    return Err(r.err(format!("spec {o} {op} twice")));
                }
                t.specs.push((o.into(), op.into(), v.into()));
            }
            "defn" => {
                r.arity(2)?;
                let f = r.str(0)?;
                let body = r.text(1)?;
                if f.is_empty() || f.contains(['/', '\\']) || t.defns.iter().any(|d| d.0 == f) {
                    return Err(r.err(format!("defn `{f}`: empty, a path, or twice")));
                }
                t.defns.push((f.into(), body));
            }
            "maestro" => return Err(r.err("a second test in one test's records")),
            o => return Err(r.err(format!("`{o}` is not a maestro record"))),
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
            return Err(r.err("more than 2^16 rows of one kind"));
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
        let colon = colon_points(v);
        let parts: Vec<&str> = match &colon {
            Some(p) => p.iter().map(String::as_str).collect(),
            None => top_level_split(v),
        };
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
        push_record(bench, RecordKind::Var, &[n, &pts[0]]);
        if pts.len() > 1 {
            push_record(bench, RecordKind::Sweep, &[n]);
            push_attr(bench, "var", &[n]);
            let (key, vals) = sweep_shape(&pts);
            push_attr(
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
        push_record(bench, RecordKind::Corner, &[c]);
        for (_, f, sec) in t.models.iter().filter(|m| &m.0 == c) {
            let sec = sec
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(sec);
            if sec.is_empty() {
                push_attr(bench, "include", &[&path_from_ade(f)]);
            } else {
                push_attr(bench, "lib", &[&path_from_ade(f), sec]);
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
                push_attr(bench, "set", &[n, &x]);
            }
        }
        if let Some(x) = temp {
            push_attr(bench, "temp", &[&x]);
        }
    }
    // The analysis block: SPICE when every analysis has a SPICE form, else Spectre.
    // ADE answers the test temperature 27 when nothing set it (live run): no `.temp` line
    // then (a bench that wrote `.temp 27` loses only that spelling of the default).
    let temp = t
        .vars
        .iter()
        .find(|v| v.0 == "temperature" && v.2.is_empty())
        .map(|v| &v.1)
        .filter(|v| v.parse::<f64>().is_ok_and(|x| x != 27.0) || v.parse::<f64>().is_err());
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
            // Always quoted: a bare `/pdk/m.scs` reads as an expression (`/` divides);
            // ADE writes `include "…"`.
            let mut l = format!("include {}", spectre_quoted(f));
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
            for (k, v) in opts.iter().filter(|o| !FORM_ONLY.contains(&o.0.as_str())) {
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
                push_record(bench, RecordKind::Output, &[n, &x]);
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
            push_record(bench, RecordKind::Spec, &[o]);
            for (key, x) in bounds {
                push_attr(bench, key, &[&x]);
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
        push_record(
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
        push_record(bench, RecordKind::Code, &[stem, "spectre"]);
        for l in body.lines() {
            bench.push_verbatim(&l.replace('\r', ""));
        }
    }
    assert!(bench.record_count() >= before, "the import only appends");
}

/// The points of ADE's `start:step:stop` sweep form (`"0.81:0.09:0.99"`), `None` for any
/// other text or a step that does not land on `stop` within 1e-9 of a step. The points are
/// [`grid`]'s, so the sweep reads back as `lin start stop n`.
fn colon_points(v: &str) -> Option<Vec<String>> {
    let f: Vec<f64> = v
        .split(':')
        .map(|x| ael::fold_numeric(x.trim())?.parse().ok())
        .collect::<Option<_>>()?;
    let [a, step, b] = f[..] else { return None };
    if step == 0.0 || !(a.is_finite() && step.is_finite() && b.is_finite()) {
        return None;
    }
    let k = (b - a) / step;
    let n = k.round();
    if (k - n).abs() > 1e-9 || n < 1.0 || n + 1.0 > f64::from(SWEEP_POINTS_MAX) {
        return None;
    }
    let pts = grid(a, b, n as u32 + 1, false)?;
    debug_assert!(pts.len() >= 2);
    Some(pts)
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
        "tran" if only(&["stop", "start", "maxstep", "errpreset"]) => {
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
            match get("errpreset") {
                Some(p) if ERRPRESETS.contains(&p) => {
                    Some(vec![format!(".options errpreset={p}"), l])
                }
                Some(_) => None,
                None => Some(vec![l]),
            }
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

    /// What ICADVM20.1 answered for exported outputs (live run 2026-10-15: numbers come back
    /// reformatted, arguments space-separated) reads back as the calculator form.
    #[test]
    fn multi_argument_outputs_read_back_from_maestro() {
        for (ade, calc) in [
            (
                r#"cross(VT("/out") 0.6321206 1 "either")"#,
                "cross(v(out), 632.1206m)",
            ),
            (
                r#"cross(VT("/out") 0.6321205588285577 1 "either")"#,
                "cross(v(out), 632.1206m)",
            ),
            (
                r#"cross(VT("/out") 0.5 2 "rising")"#,
                "cross(v(out), 500m, 1, 2)",
            ),
            (
                r#"cross(VT("/out") 0.5 1 "falling")"#,
                "cross(v(out), 500m, -1)",
            ),
            (r#"value(VT("/out") 0.001)"#, "at(v(out), 1m)"),
            (r#"value(VDC("/out") 1m)"#, "at(v(out), 1m)"),
            (r#"OP("/X1/M2" "gm")"#, r#"op("X1/M2", "gm")"#),
            (r#"2*value(VT("/a") 1m - 1u)"#, "2*at(v(a), 1m - 1u)"),
        ] {
            assert_eq!(out_from_ade(ade).as_deref(), Some(calc), "{ade}");
        }
        assert_eq!(out_from_ade(r#"cross(VT("/out") 0.5 1 "sideways")"#), None);
    }

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
        assert!(out_to_ade("x > 1", &vars, "T").is_none());
        assert!(out_from_ade("bandwidth(mag(VF(\"/OUT\")) 3 \"low\")").is_none());
        assert!(out_from_ade("IT(\"/M1/D\")").is_none());
        // A `schemify_core` ammeter's terminal `p` reads back as i().
        assert_eq!(out_from_ade("IS(\"/VM/p\")").unwrap(), "i(VM)");
    }

    /// The Exercise 1/2 output forms, ADE text written from the *ADE Calculator* /
    /// *OCEAN Reference* function list (independent of the code's tables).
    #[test]
    fn calculator_outputs_map_to_ade_functions() {
        let vars: Vec<String> = ["VDD", "W", "VDS"].map(String::from).to_vec();
        let info = |p: &str| -> Option<InstInfo> {
            let (path, terminal, inner) = match p {
                "VMDN" => ("VMDN", "p", vec![]),
                "XDUT" | "DUT" => ("DUT", "", vec![]),
                "XDUT/M1" | "DUT/M1" => ("DUT/M1", "D", vec!["main".to_owned()]),
                "M7" => ("M7", "D", vec![]),
                _ => return None,
            };
            Some(InstInfo {
                path: path.into(),
                terminal: terminal.into(),
                op_inner: inner,
            })
        };
        let tr = |e: &str, letter: &str| {
            out_expr(
                e,
                &OutScope {
                    vars: &vars,
                    letter,
                    inst: info,
                },
            )
        };
        for (e, l, want) in [
            (
                "abs(at(i(VMDN),VDD))",
                "S",
                r#"abs(value(IS("/VMDN/p") VAR("VDD")))"#,
            ),
            (
                "1/at(deriv(i(VMDN)),VDS)",
                "S",
                r#"1/value(deriv(IS("/VMDN/p")) VAR("VDS"))"#,
            ),
            (
                "1000*ymin(1/max(deriv(log10(abs(i(VMDN)))),1m))",
                "S",
                r#"1000*ymin(1/((deriv(log10(abs(IS("/VMDN/p")))))+(1m)+abs((deriv(log10(abs(IS("/VMDN/p")))))-(1m)))/2)"#,
            ),
            (
                "abs(im(i(VMDN)))/(2*pi*x())",
                "F",
                r#"abs(imag(IF("/VMDN/p")))/(2*3.141592653589793*xval(IF("/VMDN/p")))"#,
            ),
            ("abs(re(i(VMDN)))", "F", r#"abs(real(IF("/VMDN/p")))"#),
            ("db(at(v(out),10))", "F", r#"dB20(value(VF("/out") 10))"#),
            (
                "cross(db(v(out)),0,-1)",
                "F",
                r#"cross(dB20(VF("/out")) 0 1 "falling")"#,
            ),
            (
                "cross(v(XDUT.n1), 0.5, 1, 2)",
                "T",
                r#"cross(VT("/DUT/n1") 0.5 2 "rising")"#,
            ),
            ("phase_margin(-v(out))", "F", r#"phaseMargin(-VF("/out"))"#),
            (
                "settle(v(out),10u)-20n",
                "T",
                r#"settlingTime(VT("/out") ymin(xval(VT("/out"))) t ymax(xval(VT("/out"))) t 100*(10u)/abs(value(VT("/out") ymax(xval(VT("/out"))))-value(VT("/out") ymin(xval(VT("/out"))))))-20n"#,
            ),
            (
                "min(v(a), v(b), 1)",
                "T",
                r#"((((VT("/a"))+(VT("/b"))-abs((VT("/a"))-(VT("/b"))))/2)+(1)-abs((((VT("/a"))+(VT("/b"))-abs((VT("/a"))-(VT("/b"))))/2)-(1)))/2"#,
            ),
            ("pow(v(a), 2)", "T", r#"(VT("/a"))**(2)"#),
            // op(): the bench path, the class's inner device, or an explicit inner device.
            (r#"op("M7","gm")"#, "F", r#"OP("/M7" "gm")"#),
            (r#"op("/XDUT/M1","id")"#, "F", r#"OP("/DUT/M1/main" "id")"#),
            (
                r#"op("XDUT/M1/mx","gds")"#,
                "F",
                r#"OP("/DUT/M1/mx" "gds")"#,
            ),
            (r#"op("Q9","vth")"#, "F", r#"OP("/Q9" "vth")"#),
        ] {
            assert_eq!(tr(e, l).as_deref(), Some(want), "{e}");
        }
        // Negative space: no ADE form, never a partial output.
        for e in [
            "x()",
            "cross(v(a), 0.5, 2)",
            "cross(v(a), 0.5, 1, 0)",
            "slope(v(a))",
            "if(v(a)>0, 1, 2)",
            "v(a) > 1",
            r#"op("M1")"#,
            r#"op("M1", "")"#,
            r#"op("M1//x", "gm")"#,
            "at(v(a))",
            "delay(v(a), 0.5, v(b), 0.5)",
            "unknown_var * 2",
        ] {
            assert_eq!(tr(e, "T"), None, "{e}");
        }
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
        // Gap: 81 points 0..0.8 were written with binary noise (0.010000000000000002).
        let fine = grid(0.0, 0.8, 81, false).unwrap();
        assert_eq!(&fine[..4], ["0", "0.01", "0.02", "0.03"]);
        assert_eq!(fine[7], "0.07");
        assert!(fine.iter().all(|p| p.len() <= 4), "{fine:?}");
        assert_eq!(
            sweep_shape(&fine),
            ("lin", vec!["0".into(), "0.8".into(), "81".into()])
        );
        assert_eq!(
            grid(1.0, 1000.0, 4, true).unwrap(),
            ["1", "10", "100", "1000"]
        );
    }
}
