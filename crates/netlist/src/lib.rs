//! Netlist emitters (D22): ngspice, xyce, cdl, spectre and structural verilog, plus the
//! [`NameMap`] between hierarchical paths and emitted names.
//!
//! Reads [`schemify_elab::Elab`] only, so every dialect sees the same resolved values.
//! Pure (no I/O, no clock, no environment) and deterministic: one elaboration gives the
//! same bytes on every platform. Normative spec: `docs/spec/netlist.md` §7, decisions
//! in §7.6.
//!
//! # Spine
//!
//! `elab::elaborate` → gate on `violations` (the caller's) → [`emit`]: plan the
//! templates → header → globals → bench params → corner lines → top block → code
//! blocks → definitions by name → once-only text → end. A block is its instance lines
//! sorted by emitted name, then its cell's own `code` lines (a component's netlist
//! text, D17 as amended).
//!
//! ```
//! use schemify_doc::Project;
//! use schemify_elab::{Dialect, Request, elaborate, no_funcs};
//!
//! let mut p = Project::new(None, None, 1).unwrap();
//! let res = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n";
//! let bench = "schemify 1 shbench\n\ninst 0aaaaa ./res\n  at 0 0 R0\n  name 1\n  R 2k\n\nlabel 0bbbbb 0 0 R0 in\nlabel 0ccccc 0 20 R0 0\n";
//! p.index_library("", "", &[("res.shprim", res.as_bytes()), ("tb.shbench", bench.as_bytes())]).unwrap();
//! p.open("", "res.shprim", res.as_bytes(), None).unwrap();
//! let top = p.open("", "tb.shbench", bench.as_bytes(), None).unwrap().handle;
//! let req = Request { config: None, corner: None, overrides: &[], env: &[], dialect: Dialect::Ngspice };
//! let e = elaborate(&p, top, &req, &mut no_funcs);
//! assert!(!e.violations.blocking());
//! let n = schemify_netlist::emit(&e, Dialect::Ngspice);
//! assert_eq!(n.text, "* tb (schemify ngspice)\nR1 in 0 2000\n.end\n");
//! assert_eq!(n.name_map.inst_name("/1").as_deref(), Some("R1"));
//! assert_eq!(n.name_map.inst_path("r1").as_deref(), Some("/1"));
//! ```

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
// Tables are SoA: loops index several parallel columns by one row.
#![allow(clippy::needless_range_loop)]

// Data design (the six questions):
// 1. In: one `Elab` (definitions, instance rows, leaf impls, bench lines). Out: one
//    netlist `String` and a `NameMap`.
// 2. Many: one line per emitted instance row; rows are per definition, not flattened, so
//    a design of 10^6 leaves in 5 definitions writes ~10^2 lines, and a flat bench of
//    10^5 instances writes 10^5.
// 3. Widths: rows, definitions and names are `u32` (elab's capacities: 2^24 rows, 2^26
//    names); text offsets are `usize` while writing and `u32` in the name map (asserted
//    below 4 GiB).
// 4. Access: rows of one definition are read in order (name, target, conns, params
//    together), lines are appended to one buffer and sorted only when a template
//    reorders names.
// 5. Lifetime: everything is per `emit` call; scratch buffers are reused per line.
// 6. Parallel: a large block's rows render in chunks on scoped threads (`Writer::rows`),
//    appended in row order; the name map and line sort stay sequential.

mod inst;
mod name_map;

pub use name_map::NameMap;

use inst::{Plans, Tmp, Uses};
use schemify_elab::PinDir;
use schemify_elab::{CornerLine, Dialect, Elab, Ignore, Name, Target};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// One emitted netlist.
#[derive(Clone, Debug, Default)]
pub struct Netlist {
    /// The netlist text, `\n` line ends, ending in a newline.
    pub text: String,
    /// Hierarchical path ↔ emitted name, for instances and nets.
    pub name_map: NameMap,
}

/// "No row / no definition" in `u32` columns.
pub(crate) const NONE: u32 = u32::MAX;

/// Emits `elab` in `dialect` (netlist spec §7).
///
/// Precondition: the caller gated on `elab.violations` (spec §8); `emit` never fails,
/// so a design with errors still yields text: a template that fails to render becomes a
/// comment naming the error, and an unconnected pin bit is written `?`.
///
/// Cost: O(emitted rows + their connections) plus a sort of a definition's lines when
/// a template does not keep the rows' name order; the hierarchy is not flattened.
///
/// # Panics
///
/// Panics if `elab` violates its own table invariants (CSR columns of mismatched
/// lengths), i.e. it was not produced by `schemify_elab::elaborate`.
pub fn emit(elab: &Elab, dialect: Dialect) -> Netlist {
    assert_eq!(elab.defs.inst_start.len(), elab.defs.cell.len() + 1);
    assert_eq!(elab.defs.code_start.len(), elab.defs.cell.len() + 1);
    assert_eq!(elab.insts.conn_start.len(), elab.insts.name.len() + 1);
    let plans = Plans::new(elab, dialect);
    let order = defs_by_name(elab);
    let mut w = Writer::new(elab, dialect, &plans, &order);

    header(&mut w.out, elab, dialect);
    globals(&mut w.out, elab, dialect);
    bench_params(&mut w.out, elab, dialect);
    corners(&mut w.out, elab, dialect);
    if elab.top != NONE {
        w.def_block(elab.top, true);
    }
    code(&mut w.out, elab);
    for &d in &order {
        w.def_block(d, false);
    }
    once_only(&mut w.out, elab, dialect, &w.tmp);
    end(&mut w.out, dialect);

    assert!(w.out.ends_with('\n'));
    Netlist {
        text: w.out,
        name_map: w.map.finish(),
    }
}

/// Emits `elab` with the hierarchy flattened into top-level lines: leaf `name`s become
/// `<local>__<instance path, `__`-joined>` and internal nets `<path>.<net>`. Test oracle
/// only (hierarchical vs flattened emission must be equivalent); SPICE dialects only.
/// A component's own `code` lines are left out: their text names nets and devices in
/// the scope of its subckt, which flattening cannot rewrite.
///
/// # Panics
///
/// Panics for spectre and verilog, and as [`emit`].
#[doc(hidden)]
pub fn emit_flat(elab: &Elab, dialect: Dialect) -> String {
    assert!(
        is_spice(dialect),
        "flat emission is a SPICE-dialect test oracle"
    );
    assert_eq!(elab.defs.inst_start.len(), elab.defs.cell.len() + 1);
    let plans = Plans::new(elab, dialect);
    let mut out = String::new();
    let mut tmp = Tmp::new(elab);
    header(&mut out, elab, dialect);
    globals(&mut out, elab, dialect);
    bench_params(&mut out, elab, dialect);
    corners(&mut out, elab, dialect);
    let mut lines: Vec<String> = Vec::new();
    if elab.top != NONE {
        let top_ports: Vec<String> = elab
            .ports(elab.top)
            .iter()
            .map(|n| net_text(elab, *n).to_owned())
            .collect();
        flat_def(
            elab,
            dialect,
            &plans,
            &mut tmp,
            elab.top,
            ("", ""),
            &top_ports,
            &mut lines,
        );
    }
    lines.sort();
    if dialect == Dialect::Cdl {
        out.push_str(".SUBCKT ");
        out.push_str(elab.names.get(elab.defs.name[elab.top as usize]));
        for p in elab.ports(elab.top) {
            out.push(' ');
            out.push_str(net_text(elab, *p));
        }
        out.push('\n');
    }
    lines.iter().for_each(|l| out.push_str(l));
    if dialect == Dialect::Cdl {
        out.push_str(".ENDS\n");
    }
    code(&mut out, elab);
    once_only(&mut out, elab, dialect, &tmp);
    end(&mut out, dialect);
    out
}

/// Whether `d` is one of the SPICE-syntax dialects (ngspice, xyce, cdl): verbatim
/// names, `*` comments, `.subckt` blocks, case-insensitive names.
pub(crate) fn is_spice(d: Dialect) -> bool {
    matches!(d, Dialect::Ngspice | Dialect::Xyce | Dialect::Cdl)
}

/// Appends `name` as the dialect spells an identifier (spec §7.6): SPICE dialects
/// verbatim; spectre escapes every character outside `[A-Za-z0-9_]` with `\`; verilog
/// writes a non-simple identifier as an escaped identifier (`\d[3] `).
#[inline]
pub(crate) fn push_name(out: &mut String, name: &str, d: Dialect) {
    match d {
        Dialect::Ngspice | Dialect::Xyce | Dialect::Cdl => out.push_str(name),
        Dialect::Spectre => {
            for c in name.chars() {
                if !(c.is_ascii_alphanumeric() || c == '_') {
                    out.push('\\');
                }
                out.push(c);
            }
        }
        Dialect::Verilog => {
            let b = name.as_bytes();
            let simple = !b.is_empty()
                && (b[0].is_ascii_alphabetic() || b[0] == b'_')
                && b.iter()
                    .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$');
            if simple {
                out.push_str(name);
            } else {
                out.push('\\');
                out.push_str(name);
                out.push(' ');
            }
        }
    }
}

#[cfg(test)]
#[test]
fn push_name_spells_identifiers_per_dialect() {
    for (name, d, want) in [
        ("a[0]", Dialect::Cdl, "a[0]"),
        ("a[0]", Dialect::Spectre, r"a\[0\]"),
        ("a_b9", Dialect::Spectre, "a_b9"),
        ("_x$1", Dialect::Verilog, "_x$1"),
        ("x", Dialect::Verilog, "x"),
        ("1a", Dialect::Verilog, r"\1a "),
        ("$a", Dialect::Verilog, r"\$a "),
        ("a-b", Dialect::Verilog, r"\a-b "),
        ("", Dialect::Verilog, r"\ "),
    ] {
        let mut out = String::from(">");
        push_name(&mut out, name, d);
        assert_eq!(out, format!(">{want}"), "{name} {d:?}");
    }
}

/// The text of net `n`; `?` for an unconnected bit ([`Name::NONE`]).
pub(crate) fn net_text(e: &Elab, n: Name) -> &str {
    if n == Name::NONE { "?" } else { e.names.get(n) }
}

/// Respelled net names, sorted by [`Name`]: ngspice's tokenizer splits `d[0]` inside
/// an XSPICE port vector `[ … ]`, so every net some emitted row writes there is spelled
/// with `[k]` → `_k` (`d[0]` → `d_0`) throughout the netlist (spec §7.2); a respelling
/// that would equal another net's name gets `_` appended until it is unique. Empty for
/// other dialects. Cost: one pass over the emitted rows' connections.
fn xspice_spellings(e: &Elab, plans: &Plans, d: Dialect) -> Vec<(Name, String)> {
    if d != Dialect::Ngspice {
        return Vec::new();
    }
    let mut hits: Vec<Name> = Vec::new();
    // The emitted definitions: the used ones (elab marks the top used).
    for c in (0..e.defs.cell.len() as u32).filter(|&c| e.defs.used[c as usize]) {
        for r in e.insts_of(c).filter(|&r| e.insts.ignore[r] == Ignore::No) {
            let plan = plans.of_row(e, r);
            let conns = e.conns(r);
            for p in (0..plan.vector.len()).filter(|&p| plan.vector[p]) {
                let (a, w) = plan.slot(p);
                let bits = conns.get(a as usize..(a + w) as usize).unwrap_or(&[]);
                hits.extend(
                    bits.iter()
                        .filter(|n| **n != Name::NONE && e.names.get(**n).contains('[')),
                );
            }
        }
    }
    e.names.sort(&mut hits);
    hits.dedup();
    if hits.is_empty() {
        return Vec::new();
    }
    // Every net text of the netlist: a respelling must not merge two nets.
    let mut taken: std::collections::HashSet<String> = e
        .insts
        .conn
        .iter()
        .chain(&e.defs.port_net)
        .filter(|n| **n != Name::NONE)
        .map(|n| e.names.get(*n).to_owned())
        .collect();
    let mut out: Vec<(Name, String)> = Vec::with_capacity(hits.len());
    for n in hits {
        let mut s: String = e.names.get(n).replace('[', "_").replace(']', "");
        while taken.contains(&s) {
            s.push('_');
        }
        taken.insert(s.clone());
        out.push((n, s));
    }
    out.sort_unstable_by_key(|p| p.0.0);
    assert!(out.windows(2).all(|w| w[0].0 != w[1].0));
    out
}

/// The emitted text of net `n` under the respellings `rs` (sorted by [`Name`]).
pub(crate) fn spelled<'a>(e: &'a Elab, rs: &'a [(Name, String)], n: Name) -> &'a str {
    match rs.binary_search_by_key(&n.0, |p| p.0.0) {
        Ok(i) => &rs[i].1,
        Err(_) => net_text(e, n),
    }
}

/// The comment leader of `d`.
fn comment(d: Dialect) -> &'static str {
    if is_spice(d) { "*" } else { "//" }
}

/// Used definitions other than the top, sorted by subckt name (spec §7.1).
fn defs_by_name(e: &Elab) -> Vec<u32> {
    let mut order: Vec<(Name, u32)> = (0..e.defs.cell.len() as u32)
        .filter(|&d| e.defs.used[d as usize] && d != e.top)
        .map(|d| (e.defs.name[d as usize], d))
        .collect();
    e.names.sort_by_text(&mut order);
    assert!(order.len() < e.defs.cell.len().max(1));
    order.into_iter().map(|k| k.1).collect()
}

// ------------------------------------------------------------------ sections

fn header(out: &mut String, e: &Elab, d: Dialect) {
    assert!(out.is_empty(), "the header is the first line");
    let top = e.cells.first().map_or("top", |c| e.names.get(c.cell));
    out.push_str(comment(d));
    out.push(' ');
    out.push_str(top);
    out.push_str(" (schemify ");
    out.push_str(d.as_str());
    out.push_str(")\n");
    if d == Dialect::Spectre {
        out.push_str("simulator lang=spectre\n");
    }
}

/// Spec §7.2: one `.global` line of every global but `0` (spectre lists `0` too);
/// verilog has no global nets (§7.6).
fn globals(out: &mut String, e: &Elab, d: Dialect) {
    let word = match d {
        Dialect::Ngspice | Dialect::Xyce => ".global",
        Dialect::Cdl => ".GLOBAL",
        Dialect::Spectre => "global",
        Dialect::Verilog => return,
    };
    let names: Vec<&str> = e
        .globals
        .iter()
        .map(|g| e.names.get(*g))
        .filter(|g| d == Dialect::Spectre || *g != "0")
        .collect();
    if names.is_empty() {
        return;
    }
    out.push_str(word);
    for g in names {
        out.push(' ');
        push_name(out, g, d);
    }
    out.push('\n');
}

/// Bench vars in name order: `.param k=v` (ngspice, xyce), one `parameters` line
/// (spectre); none for cdl and verilog.
fn bench_params(out: &mut String, e: &Elab, d: Dialect) {
    let b = &e.bench;
    assert_eq!(b.var_name.len(), b.var_text.len());
    for v in 0..b.var_name.len() {
        match d {
            Dialect::Ngspice | Dialect::Xyce => out.push_str(".param "),
            Dialect::Spectre if v == 0 => out.push_str("parameters "),
            Dialect::Spectre => out.push(' '),
            Dialect::Cdl | Dialect::Verilog => return,
        }
        out.push_str(e.names.get(b.var_name[v]));
        out.push('=');
        out.push_str(e.names.get(b.var_text[v]));
        if d != Dialect::Spectre {
            out.push('\n');
        }
    }
    if d == Dialect::Spectre && !b.var_name.is_empty() {
        out.push('\n');
    }
}

/// Corner lines in file order: `.lib path section`, `.include path`, `.temp t`
/// (xyce: `.options device temp=t`; spectre: `include` / `options`); none for cdl and
/// verilog.
fn corners(out: &mut String, e: &Elab, d: Dialect) {
    let b = &e.bench;
    if matches!(d, Dialect::Cdl | Dialect::Verilog) || b.corner_kind.is_empty() {
        return; // a non-bench top has empty bench tables (no CSR seed either)
    }
    assert_eq!(b.corner_start.len(), b.corner_kind.len() + 1);
    for k in 0..b.corner_kind.len() {
        let args = &b.corner_args[b.corner_start[k] as usize..b.corner_start[k + 1] as usize];
        let arg = |i: usize| args.get(i).map_or("", |n| e.names.get(*n));
        match (b.corner_kind[k], d) {
            (CornerLine::Lib, Dialect::Spectre) => {
                out.push_str("include \"");
                out.push_str(arg(0));
                out.push_str("\" section=");
                out.push_str(arg(1));
            }
            (CornerLine::Include, Dialect::Spectre) => {
                out.push_str("include \"");
                out.push_str(arg(0));
                out.push('"');
            }
            (CornerLine::Temp, Dialect::Spectre) => {
                out.push_str("corner_temp options temp=");
                out.push_str(arg(0));
            }
            (CornerLine::Lib, _) => {
                out.push_str(".lib ");
                out.push_str(arg(0));
                out.push(' ');
                out.push_str(arg(1));
            }
            (CornerLine::Include, _) => {
                out.push_str(".include ");
                out.push_str(arg(0));
            }
            (CornerLine::Temp, Dialect::Xyce) => {
                out.push_str(".options device temp=");
                out.push_str(arg(0));
            }
            (CornerLine::Temp, _) => {
                out.push_str(".temp ");
                out.push_str(arg(0));
            }
        }
        out.push('\n');
    }
}

/// Bench code blocks for the dialect, already selected (none for cdl, spec §7.4) and
/// sorted by elab, verbatim.
fn code(out: &mut String, e: &Elab) {
    let b = &e.bench;
    assert!(b.code_lines.is_empty() || b.code_start.len() == b.code_name.len() + 1);
    for &l in &b.code_lines {
        out.push_str(e.names.get(l));
        out.push('\n');
    }
}

/// Returns whether leaf impl row `l` of `e` is source text the runner compiles (spec
/// §7.6.8, run spec §5): a `veriloga` or `verilog` impl with a `file`, a `verilog` impl
/// with a `body`, or a `veriloga` impl whose `body` has an `endmodule` line. A `veriloga`
/// body without one is precompiled SPICE text (model card, OSDI load) and is emitted as is.
///
/// # Panics
///
/// Panics if `l` is not a row of `e.impls`.
pub fn compiled_source(e: &Elab, l: usize) -> bool {
    assert!(
        l < e.impls.cell.len(),
        "impl row {l} of {}",
        e.impls.cell.len()
    );
    let f = &e.cells[e.impls.cell[l] as usize];
    let k = e.impls.index[l] as usize;
    let body = &e.impls.body[e.impls.body_start[l] as usize..e.impls.body_start[l + 1] as usize];
    match e.names.get(f.impl_lang[k]) {
        "veriloga" => {
            f.impl_file[k] != Name::NONE
                || body.iter().any(|&n| {
                    let w = e.names.get(n).split_whitespace().next();
                    w.is_some_and(|w| w.starts_with("endmodule"))
                })
        }
        "verilog" => f.impl_file[k] != Name::NONE || !body.is_empty(),
        _ => false,
    }
}

/// Once-only text (spec §7.1, §7.2): OSDI loads, then generated `.model` lines, then
/// impl bodies by (cell, impl). Only impls some emitted row uses.
fn once_only(out: &mut String, e: &Elab, d: Dialect, tmp: &Tmp) {
    let uses = &tmp.uses;
    assert_eq!(uses.impl_used.len(), e.impls.cell.len());
    let stem_of = |l: usize| -> String {
        let f = &e.cells[e.impls.cell[l] as usize];
        let imp = e.impls.index[l] as usize;
        match f.impl_file[imp] {
            Name::NONE => e.names.get(f.stem).to_owned(),
            file => {
                let base = e.names.get(file).rsplit(['/', '\\']).next().unwrap_or("");
                base.rsplit_once('.').map_or(base, |(s, _)| s).to_owned()
            }
        }
    };
    // OSDI loads and models (ngspice only: elab rejects veriloga/verilog elsewhere). A
    // Verilog-A source (a `file`, or a `body` that is source) is compiled by the runner:
    // one OSDI load per stem and a `.model` per model name (the impl's `model` attr, and
    // each default-template row's model). Any other `veriloga` impl is precompiled: its
    // body carries that text (§7.6).
    let mut osdi: Vec<String> = Vec::new();
    let mut models: Vec<String> = Vec::new();
    let lang_of = |l: usize| {
        let f = &e.cells[e.impls.cell[l] as usize];
        e.names.get(f.impl_lang[e.impls.index[l] as usize])
    };
    let model_line = |l: usize, m: &str, models: &mut Vec<String>, osdi: &mut Vec<String>| {
        let stem = stem_of(l);
        match lang_of(l) {
            "veriloga" if compiled_source(e, l) => {
                models.push(format!(".model {m} {stem}"));
                osdi.push(stem);
            }
            "verilog" => models.push(format!(".model {m} d_cosim simulation=\"{stem}.vvp\"")),
            _ => {}
        }
    };
    if d == Dialect::Ngspice {
        for l in (0..e.impls.cell.len()).filter(|&l| uses.impl_used[l]) {
            let f = &e.cells[e.impls.cell[l] as usize];
            match f.impl_model[e.impls.index[l] as usize] {
                Name::NONE if lang_of(l) == "veriloga" && compiled_source(e, l) => {
                    osdi.push(stem_of(l));
                }
                Name::NONE => {}
                m => model_line(l, e.names.get(m), &mut models, &mut osdi),
            }
        }
        for &(model, l) in &uses.models {
            model_line(l as usize, e.names.get(model), &mut models, &mut osdi);
        }
    }
    osdi.sort();
    osdi.dedup();
    models.sort();
    models.dedup();
    for s in &osdi {
        out.push_str(".control\npre_osdi ");
        out.push_str(s);
        out.push_str(".osdi\n.endc\n");
    }
    for m in &models {
        out.push_str(m);
        out.push('\n');
    }
    // Bodies: SPICE text for the SPICE dialects (spectre wraps it in `lang=spice`):
    // `spice` impls and, except for cdl, precompiled Verilog-A ones and the schematic
    // impls of emitted definitions (model cards); Verilog source for verilog. A Verilog-A
    // source (`file` or source `body`) is the runner's.
    let mut used = uses.impl_used.clone();
    for (def, &l) in e.defs.body.iter().enumerate() {
        if l != u32::MAX && e.defs.used[def] {
            used[l as usize] = true;
        }
    }
    let mut bodies: Vec<usize> = (0..e.impls.cell.len())
        .filter(|&l| {
            let wanted = match (lang_of(l), d) {
                ("verilog", Dialect::Verilog) => true,
                (_, Dialect::Verilog) => false,
                ("spice", _) => true,
                // Simulation-only text (OSDI load, model card): not for LVS.
                ("veriloga", _) => !compiled_source(e, l) && d != Dialect::Cdl,
                ("schematic", _) => d != Dialect::Cdl,
                _ => false,
            };
            used[l] && wanted && e.impls.body_start[l] < e.impls.body_start[l + 1]
        })
        .collect();
    let key = |l: usize| {
        let f = &e.cells[e.impls.cell[l] as usize];
        (
            e.names.get(f.cell),
            e.names.get(f.impl_name[e.impls.index[l] as usize]),
        )
    };
    bodies.sort_by(|a, b| key(*a).cmp(&key(*b)));
    for l in bodies {
        if d == Dialect::Spectre {
            out.push_str("simulator lang=spice\n");
        }
        for &line in
            &e.impls.body[e.impls.body_start[l] as usize..e.impls.body_start[l + 1] as usize]
        {
            out.push_str(e.names.get(line));
            out.push('\n');
        }
        if d == Dialect::Spectre {
            out.push_str("simulator lang=spectre\n");
        }
    }
}

fn end(out: &mut String, d: Dialect) {
    match d {
        Dialect::Ngspice | Dialect::Xyce => out.push_str(".end\n"),
        Dialect::Cdl => out.push_str(".END\n"),
        Dialect::Spectre | Dialect::Verilog => {}
    }
}

/// Direction letter of each port of definition `d` in port order (CDL `*.PININFO`,
/// verilog declarations): ordinary pins bit by bit, then declared inherited pins, then
/// inherited pins propagated from the subtree (`inout`). Tolerates a port list that
/// disagrees with the pins (`E012` designs): missing directions are `inout`.
fn port_dirs(e: &Elab, d: u32) -> Vec<PinDir> {
    let f = &e.cells[e.defs.cell[d as usize] as usize];
    let n = e.ports(d).len();
    let mut dirs = Vec::with_capacity(n);
    for inherited in [false, true] {
        for p in 0..f.pin_name.len() {
            if (f.pin_inherit[p] != Name::NONE) == inherited && f.pin_alias[p] == u32::MAX {
                dirs.extend(std::iter::repeat_n(f.pin_dir[p], f.bits(p).len()));
            }
        }
    }
    dirs.resize(n, PinDir::Inout);
    assert_eq!(dirs.len(), n);
    dirs
}

// ------------------------------------------------------------------ definitions

/// The emission state: output buffer, line index of the definition being written,
/// per-line scratch and the name map under construction.
struct Writer<'e> {
    e: &'e Elab,
    d: Dialect,
    plans: &'e Plans,
    out: String,
    tmp: Tmp,
    /// Lines of the current definition: (start, end, name start, name end) in `out`.
    lines: Vec<(usize, usize, usize, usize)>,
    /// Elab definition → name-map definition (`NONE` when not emitted).
    def_map: Vec<u32>,
    map: name_map::Builder,
    /// Verilog: the definition's nets, sorted, for `wire` declarations.
    wires: Vec<Name>,
    /// Net respellings ([`xspice_spellings`]), sorted by [`Name`].
    respell: Vec<(Name, String)>,
}

impl<'e> Writer<'e> {
    fn new(e: &'e Elab, d: Dialect, plans: &'e Plans, order: &[u32]) -> Writer<'e> {
        let mut def_map = vec![NONE; e.defs.cell.len()];
        let mut rows = 0;
        if e.top != NONE {
            def_map[e.top as usize] = 0;
            rows += e.insts_of(e.top).len();
        }
        for (k, &d) in order.iter().enumerate() {
            def_map[d as usize] = k as u32 + 1;
            rows += e.insts_of(d).len();
        }
        let body_lines = e.impls.body.len() + e.bench.code_lines.len() + e.defs.code.len();
        let respell = xspice_spellings(e, plans, d);
        let mut map = name_map::Builder::new(e, d, order.len() + usize::from(e.top != NONE), rows);
        map.respell = respell.clone();
        Writer {
            e,
            d,
            plans,
            out: String::with_capacity(4096 + 48 * rows + 64 * body_lines),
            tmp: Tmp::new(e),
            lines: Vec::with_capacity(rows),
            def_map,
            map,
            wires: Vec::new(),
            respell,
        }
    }

    /// Writes definition `d`: its opening line(s), its instance lines sorted by emitted
    /// name, its closing line. The top is unwrapped except for cdl (a `.SUBCKT`, spec
    /// §7.4) and verilog (a `module`).
    fn def_block(&mut self, d: u32, top: bool) {
        let (e, dialect) = (self.e, self.d);
        assert!(
            self.def_map[d as usize] != NONE,
            "definition {d} is emitted"
        );
        let name = e.names.get(e.defs.name[d as usize]);
        let wrap = !top || matches!(dialect, Dialect::Cdl | Dialect::Verilog);
        if wrap {
            self.open_def(d, name);
        }
        self.map.def(e, d, &self.def_map);
        self.rows(d);
        // D17 as amended (spec §7.6.15): the cell's netlist text, in every definition.
        let code =
            e.defs.code_start[d as usize] as usize..e.defs.code_start[d as usize + 1] as usize;
        for &line in &e.defs.code[code] {
            self.out.push_str(e.names.get(line));
            self.out.push('\n');
        }
        if wrap {
            match dialect {
                Dialect::Ngspice | Dialect::Xyce => self.out.push_str(".ends "),
                Dialect::Cdl => self.out.push_str(".ENDS "),
                Dialect::Spectre => self.out.push_str("ends "),
                Dialect::Verilog => self.out.push_str("endmodule"),
            }
            if dialect != Dialect::Verilog {
                push_name(&mut self.out, name, dialect);
            }
            self.out.push('\n');
        }
    }

    fn open_def(&mut self, d: u32, name: &str) {
        let (e, dialect, out) = (self.e, self.d, &mut self.out);
        let ports = e.ports(d);
        let (open, sep, close) = match dialect {
            Dialect::Ngspice | Dialect::Xyce => (".subckt ", " ", ""),
            Dialect::Cdl => (".SUBCKT ", " ", ""),
            Dialect::Spectre => ("subckt ", " ", ")"),
            Dialect::Verilog => ("module ", ", ", ");"),
        };
        out.push_str(open);
        push_name(out, name, dialect);
        if !close.is_empty() {
            out.push_str(" (");
        }
        for (i, p) in ports.iter().enumerate() {
            if i > 0 || close.is_empty() {
                out.push_str(sep);
            }
            push_name(out, spelled(e, &self.respell, *p), dialect);
        }
        out.push_str(close);
        out.push('\n');
        match dialect {
            Dialect::Cdl if !ports.is_empty() => {
                out.push_str("*.PININFO");
                for (p, dir) in ports.iter().zip(port_dirs(e, d)) {
                    out.push(' ');
                    out.push_str(net_text(e, *p));
                    out.push_str(match dir {
                        PinDir::In => ":I",
                        PinDir::Out => ":O",
                        PinDir::Inout => ":B",
                    });
                }
                out.push('\n');
            }
            Dialect::Verilog => self.verilog_decls(d),
            _ => {}
        }
    }

    /// Verilog port directions and one `wire` per other net of the definition's
    /// emitted rows, sorted by name (spec §7.5).
    fn verilog_decls(&mut self, d: u32) {
        let (e, out) = (self.e, &mut self.out);
        let ports = e.ports(d);
        for (p, dir) in ports.iter().zip(port_dirs(e, d)) {
            out.push_str(match dir {
                PinDir::In => "  input ",
                PinDir::Out => "  output ",
                PinDir::Inout => "  inout ",
            });
            push_name(out, net_text(e, *p), Dialect::Verilog);
            out.push_str(";\n");
        }
        self.wires.clear();
        for r in e.insts_of(d) {
            if e.insts.ignore[r] == Ignore::No {
                self.wires
                    .extend(e.conns(r).iter().filter(|n| **n != Name::NONE));
            }
        }
        self.wires.sort_unstable();
        self.wires.dedup();
        self.wires.retain(|n| !ports.contains(n));
        e.names.sort(&mut self.wires);
        for n in &self.wires {
            out.push_str("  wire ");
            push_name(out, e.names.get(*n), Dialect::Verilog);
            out.push_str(";\n");
        }
    }

    /// Instance lines of definition `d`, sorted by emitted name (ties keep row order),
    /// and their name-map rows.
    fn rows(&mut self, d: u32) {
        let e = self.e;
        let seg = self.out.len();
        self.lines.clear();
        let rows = e.insts_of(d);
        // Rows render independently: a large block renders in chunks on scoped threads and
        // the chunks are appended in row order, so the text is the serial text (D37).
        let chunks = if rows.len() >= PARALLEL_ROWS && !SERIAL.load(Relaxed) {
            std::thread::available_parallelism().map_or(1, |n| n.get().min(4))
        } else {
            1
        };
        let step = rows.len().div_ceil(chunks.max(1)).max(1);
        let parts: Vec<std::ops::Range<usize>> = (rows.start..rows.end)
            .step_by(step)
            .map(|a| a..(a + step).min(rows.end))
            .collect();
        let (dl, plans, respell) = (self.d, self.plans, &self.respell);
        let rendered: Vec<Chunk> = if parts.len() > 1 {
            std::thread::scope(|sc| {
                let workers: Vec<_> = parts
                    .iter()
                    .skip(1)
                    .map(|p| {
                        sc.spawn(|| {
                            let mut tmp = Tmp::new(e);
                            let mut c = render(e, dl, plans, respell, p.clone(), &mut tmp);
                            c.uses = tmp.uses;
                            c
                        })
                    })
                    .collect();
                let first = render(e, dl, plans, respell, parts[0].clone(), &mut self.tmp);
                let mut all = vec![first];
                all.extend(workers.into_iter().map(|w| w.join().expect("rows render")));
                all
            })
        } else {
            parts
                .iter()
                .map(|p| render(e, dl, plans, respell, p.clone(), &mut self.tmp))
                .collect()
        };
        let mut sorted = true;
        for (k, c) in rendered.into_iter().enumerate() {
            if k > 0 {
                self.tmp.uses.merge(c.uses);
            }
            let base = self.out.len();
            self.out.push_str(&c.text);
            for (r, a, b, ok) in c.lines {
                let (start, end) = (base + a, base + b);
                let name = emitted_name(&self.out, start, self.d);
                if let Some(prev) = self.lines.last() {
                    sorted &=
                        self.out.as_bytes()[prev.2..prev.3] <= self.out.as_bytes()[name.0..name.1];
                }
                if ok {
                    let child = match e.insts.target[r] {
                        Target::Def(c) => self.def_map[c as usize],
                        Target::Leaf(_) => NONE,
                    };
                    self.map.row(e, r, &self.out[name.0..name.1], child);
                }
                self.lines.push((start, end, name.0, name.1));
            }
        }
        if !sorted {
            sort_lines(&mut self.out, seg, &self.lines);
        }
    }
}

/// Blocks with at least this many rows render on several threads.
const PARALLEL_ROWS: usize = 1 << 15;

/// Forces serial rendering (the D37 jobs = 1 vs N oracle).
static SERIAL: AtomicBool = AtomicBool::new(false);

/// Renders every block on one thread (`true`) or lets large blocks use several (`false`,
/// the default). The text is identical either way; tests compare the two.
#[doc(hidden)]
pub fn set_serial(serial: bool) {
    SERIAL.store(serial, Relaxed);
}

/// Instance lines of a run of rows, rendered on their own: the text, then per kept line
/// `(row, start, end, rendered without error)` with offsets into the text, and the impls
/// and models the lines use.
struct Chunk {
    text: String,
    lines: Vec<(usize, usize, usize, bool)>,
    uses: Uses,
}

/// Renders rows `rows` of one block (spec §7.6.1: a failed template becomes an error
/// comment, an empty rendering no line). The impls and models used accumulate in `tmp`;
/// the chunk's `uses` is left empty for the caller to fill.
fn render(
    e: &Elab,
    d: Dialect,
    plans: &Plans,
    respell: &[(Name, String)],
    rows: std::ops::Range<usize>,
    tmp: &mut Tmp,
) -> Chunk {
    let indent = if d == Dialect::Verilog { "  " } else { "" };
    let mut out = String::with_capacity(48 * rows.len());
    let mut lines = Vec::with_capacity(rows.len());
    for r in rows {
        if e.insts.ignore[r] != Ignore::No {
            continue;
        }
        let start = out.len();
        out.push_str(indent);
        let local = e.names.get(e.insts.name[r]);
        let rendered = inst::line(
            e,
            d,
            plans,
            r,
            local,
            |n| spelled(e, respell, n),
            &mut out,
            tmp,
        );
        if let Err(msg) = &rendered {
            out.truncate(start);
            out.push_str(indent);
            out.push_str(comment(d));
            out.push_str(" error in ");
            out.push_str(local);
            out.push_str(": ");
            out.push_str(&msg.replace('\n', " "));
        }
        if out.len() == start + indent.len() {
            out.truncate(start);
            continue; // a template that renders nothing emits no line
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
        lines.push((r, start, out.len(), rendered.is_ok()));
    }
    assert!(lines.last().is_none_or(|l| l.2 == out.len()));
    Chunk {
        text: out,
        lines,
        uses: Uses::default(),
    }
}

/// The byte range of the emitted instance name of the line starting at `start`: its
/// first token, the second for verilog (`<module> <instance> (…);`). Lines sort by it.
fn emitted_name(out: &str, start: usize, d: Dialect) -> (usize, usize) {
    let rest = &out[start..];
    let skip = usize::from(d == Dialect::Verilog);
    let Some(tok) = rest.split_ascii_whitespace().nth(skip) else {
        return (out.len(), out.len());
    };
    // `tok` borrows `rest`, so its address gives its offset.
    let a = start + (tok.as_ptr() as usize - rest.as_ptr() as usize);
    assert!(out[a..].starts_with(tok) && !tok.is_empty());
    (a, a + tok.len())
}

/// Reorders the lines written since `seg` by emitted name (stable). `lines` are
/// `(start, end, name start, name end)` offsets into `out`, contiguous from `seg` to
/// the end.
fn sort_lines(out: &mut String, seg: usize, lines: &[(usize, usize, usize, usize)]) {
    assert!(lines.first().is_some_and(|l| l.0 == seg));
    assert!(lines.last().is_some_and(|l| l.1 == out.len()));
    // ponytail: the out-of-order case copies the definition's text once; fine for
    // templates that reorder names (rare), revisit if a PDK makes it common.
    let text = out[seg..].to_owned();
    let mut order: Vec<(&str, u32)> = (0..lines.len())
        .map(|i| (&text[lines[i].2 - seg..lines[i].3 - seg], i as u32))
        .collect();
    order.sort_unstable();
    out.truncate(seg);
    for i in order.into_iter().map(|o| o.1 as usize) {
        out.push_str(&text[lines[i].0 - seg..lines[i].1 - seg]);
    }
    assert_eq!(out.len(), seg + text.len());
}

/// Flattens definition `d` at instance path `path` (`__`-joined local names, empty at
/// the top; `dotted` is the same path `.`-joined) whose ports are bound to `ports`
/// (flat net names), appending its leaf lines.
#[allow(clippy::too_many_arguments)]
fn flat_def(
    e: &Elab,
    dialect: Dialect,
    plans: &Plans,
    tmp: &mut Tmp,
    d: u32,
    (path, dotted): (&str, &str),
    ports: &[String],
    lines: &mut Vec<String>,
) {
    assert_eq!(ports.len(), e.ports(d).len());
    let flat = |n: Name| -> String {
        if n == Name::NONE {
            return "?".to_owned();
        }
        if let Some(j) = e.ports(d).iter().position(|p| *p == n) {
            return ports[j].clone();
        }
        let text = e.names.get(n);
        if dotted.is_empty() || e.globals.contains(&n) {
            text.to_owned()
        } else {
            format!("{dotted}.{text}")
        }
    };
    for r in e.insts_of(d) {
        if e.insts.ignore[r] != Ignore::No {
            continue;
        }
        let local = e.names.get(e.insts.name[r]);
        match e.insts.target[r] {
            Target::Def(c) => {
                let child: Vec<String> = e.conns(r).iter().map(|n| flat(*n)).collect();
                let p = if path.is_empty() {
                    local.to_owned()
                } else {
                    format!("{path}__{local}")
                };
                let dt = if dotted.is_empty() {
                    local.to_owned()
                } else {
                    format!("{dotted}.{local}")
                };
                flat_def(e, dialect, plans, tmp, c, (&p, &dt), &child, lines);
            }
            Target::Leaf(_) => {
                let names: Vec<(Name, String)> =
                    e.conns(r).iter().map(|n| (*n, flat(*n))).collect();
                let name = if path.is_empty() {
                    local.to_owned()
                } else {
                    format!("{local}__{path}")
                };
                let mut out = String::new();
                let lookup = |n: Name| -> &str {
                    names
                        .iter()
                        .find(|x| x.0 == n)
                        .map_or("?", |x| x.1.as_str())
                };
                if inst::line(e, dialect, plans, r, &name, lookup, &mut out, tmp).is_ok()
                    && !out.is_empty()
                {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    lines.push(out);
                }
            }
        }
    }
}
