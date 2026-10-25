//! Netlists (D22): elab's tables lowered to SpiceRack's circuit IR, written by SpiceRack's
//! xschem-layout codegen in ngspice, espice, xyce, cdl, spectre or structural verilog,
//! plus the [`NameMap`] between hierarchical paths and emitted names.
//!
//! Reads [`schemify_elab::Elab`] only, so every dialect sees the same resolved values.
//! Pure (no I/O, no clock, no environment) and deterministic: one elaboration gives the
//! same bytes on every platform. Normative spec: `docs/spec/netlist.md` §7, decisions
//! in §7.6.
//!
//! # Spine
//!
//! `elab::elaborate` → gate on `violations` (the caller's) → [`emit`]: plan the
//! templates → [`lower`] (title, globals, bench params, corner lines, bench code, the
//! top block, definitions by name, once-only text) → `spicerack::codegen::xschem::write`.
//! A block is its instance rows as packed element lines (template text; a default
//! template's subcircuit instance as SpiceRack's `push_instance` spells it), then its
//! cell's own `code` lines (a component's netlist text, D17 as amended); SpiceRack sorts
//! the rows by element name and spells the dialect.
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
//    `Lowered` (SpiceRack's `CircuitIR` and what its xschem deck carries beside it,
//    written to text by SpiceRack) and a `NameMap`.
// 2. Many: one element per emitted instance row; rows are per definition, not
//    flattened, so a design of 10^6 leaves in 5 definitions has ~10^2 elements, and a
//    flat bench of 10^5 instances 10^5. Every row is a line of packed `Lines` (one text
//    buffer and a `u32` end per row, no allocation per row), default-template
//    subcircuit rows included (an IR `Instance` would own a `String` per connection).
// 3. Widths: rows, definitions and names are `u32` (elab's capacities: 2^24 rows, 2^26
//    names); line ends and name-map offsets are `u32` (asserted below 4 GiB).
// 4. Access: rows of one definition are read in order (name, target, conns, params
//    together). The name-map pass reads every element name, so it also marks runs
//    already in name order (`Lines::sorted`): SpiceRack then copies them without a
//    second scan, and sorts a block's lines only when they are out of name order.
// 5. Lifetime: everything is per `emit` call; the IR is dropped once written.
// 6. Parallel: a large block's rows render in chunks on scoped threads (`Writer::rows`),
//    appended in row order; the name map stays sequential.

mod inst;
mod name_map;

pub use name_map::NameMap;
pub use spicerack::codegen::xschem::{Lines, SetupLine};
pub use spicerack::ir::CircuitIR;

use inst::{Plans, Tmp};
use schemify_elab::PinDir;
use schemify_elab::{CornerLine, Dialect, Elab, Ignore, Name, Target};
use spicerack::codegen::Dialect as SrDialect;
use spicerack::codegen::xschem;
use spicerack::ir::{ModelDef, ParamDef, Port, PortDirection, Subcircuit, Testbench};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// One emitted netlist.
#[derive(Clone, Debug, Default)]
pub struct Netlist {
    /// The netlist text, `\n` line ends, ending in a newline.
    pub text: String,
    /// Hierarchical path ↔ emitted name, for instances and nets.
    pub name_map: NameMap,
}

/// An elaboration lowered for one dialect: SpiceRack's IR and what its xschem deck
/// carries beside it (`spicerack::codegen::xschem::Deck`), and the name map.
#[derive(Clone, Debug, Default)]
pub struct Lowered {
    /// The circuit: the top, the definitions by name, the bench vars (CDL: the top's
    /// tuple), the bench code, the Verilog-A loads and models.
    pub ir: CircuitIR,
    /// `<top cell> (schemify <dialect>)`.
    pub title: String,
    /// The global nets.
    pub globals: Vec<String>,
    /// The corner lines in file order.
    pub setup: Vec<SetupLine>,
    /// Impl bodies written once.
    pub bodies: Vec<String>,
    /// Per block (the top, then each definition): its template rows, one run per part
    /// rendered in parallel.
    pub lines: Vec<Vec<Lines>>,
    /// Per block: its Verilog `wire`s (empty for other dialects).
    pub nets: Vec<Vec<String>>,
    /// Path ↔ emitted name, keyed by the element names the deck writes.
    pub name_map: NameMap,
}

/// "No row / no definition" in `u32` columns.
pub(crate) const NONE: u32 = u32::MAX;

/// Emits `elab` in `dialect` (netlist spec §7): [`lower`], then SpiceRack's
/// xschem-layout codegen writes the deck.
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
    let mut l = lower(elab, dialect);
    let text = write(&mut l, dialect);
    Netlist {
        text,
        name_map: l.name_map,
    }
}

/// SpiceRack writes `l` (its lines and bodies are moved into the deck).
fn write(l: &mut Lowered, dialect: Dialect) -> String {
    let parts = xschem::Deck {
        ir: &l.ir,
        title: Some(std::mem::take(&mut l.title)),
        globals: std::mem::take(&mut l.globals),
        setup: std::mem::take(&mut l.setup),
        bodies: std::mem::take(&mut l.bodies),
        lines: std::mem::take(&mut l.lines),
        nets: std::mem::take(&mut l.nets),
    };
    let text = xschem::write(&parts, deck(dialect)).expect("every dialect has an xschem layout");
    assert!(text.ends_with('\n'));
    text
}

/// Lowers `elab` to SpiceRack's IR for `dialect` (spec §7): every row rendered as a
/// packed element line, with the name map built from the element names the deck sorts
/// and writes.
///
/// # Panics
///
/// As [`emit`].
pub fn lower(elab: &Elab, dialect: Dialect) -> Lowered {
    assert_eq!(elab.defs.inst_start.len(), elab.defs.cell.len() + 1);
    assert_eq!(elab.defs.code_start.len(), elab.defs.cell.len() + 1);
    assert_eq!(elab.insts.conn_start.len(), elab.insts.name.len() + 1);
    let plans = Plans::new(elab, dialect);
    let order = defs_by_name(elab);
    let mut w = Writer::new(elab, dialect, &plans, &order);
    let mut l = frame(elab, dialect);
    l.lines = Vec::with_capacity(order.len() + 1);
    l.nets = Vec::with_capacity(order.len() + 1);
    if elab.top != NONE {
        let params = std::mem::take(&mut l.ir.top.parameters);
        let (top, lines, nets) = w.def_block(elab.top);
        l.ir.top = top;
        if dialect != Dialect::Cdl {
            l.ir.top.parameters = params; // a bench's vars; CDL's top keeps its tuple
        }
        l.lines.push(lines);
        l.nets.push(nets);
    } else {
        l.lines.push(Vec::new());
        l.nets.push(Vec::new());
    }
    for &d in &order {
        let (sc, lines, nets) = w.def_block(d);
        l.ir.subcircuit_defs.push(sc);
        l.lines.push(lines);
        l.nets.push(nets);
    }
    once_only(&mut l, elab, dialect, &w.tmp);
    assert_eq!(l.lines.len(), order.len() + 1);
    l.name_map = w.map.finish();
    l
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
    let mut tmp = Tmp::new(elab);
    let mut l = frame(elab, dialect);
    let mut top = Lines::default();
    if elab.top != NONE {
        let top_ports: Vec<String> = elab
            .ports(elab.top)
            .iter()
            .map(|n| net_text(elab, *n).to_owned())
            .collect();
        let mut lines = Vec::new();
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
        lines.sort();
        lines.iter().for_each(|line| top.push(line));
        l.ir.top.name = elab.names.get(elab.defs.name[elab.top as usize]).to_owned();
        l.ir.top.ports = ports(elab, elab.top, &[]);
        if dialect == Dialect::Cdl {
            l.ir.top.parameters = cdl_params(elab, elab.top);
        }
    }
    l.lines = vec![vec![top]];
    once_only(&mut l, elab, dialect, &tmp);
    write(&mut l, dialect)
}

/// SpiceRack's dialect of `d`.
pub(crate) fn deck(d: Dialect) -> SrDialect {
    match d {
        Dialect::Ngspice => SrDialect::Ngspice,
        Dialect::Espice => SrDialect::Espice,
        Dialect::Xyce => SrDialect::Xyce,
        Dialect::Cdl => SrDialect::Cdl,
        Dialect::Spectre => SrDialect::Spectre,
        Dialect::Verilog => SrDialect::Verilog,
        Dialect::Hspice => SrDialect::Hspice,
        Dialect::Ltspice => SrDialect::Ltspice,
    }
}

/// Whether `d` is one of the SPICE-syntax dialects (ngspice, espice, xyce, cdl): verbatim
/// names, `*` comments, `.subckt` blocks, case-insensitive names.
pub(crate) fn is_spice(d: Dialect) -> bool {
    xschem::is_spice(deck(d))
}

/// Appends `name` as the dialect spells an identifier (SpiceRack's `push_ident`: SPICE
/// verbatim, spectre `\`-escaped, verilog escaped identifiers).
#[inline]
pub(crate) fn push_name(out: &mut String, name: &str, d: Dialect) {
    xschem::push_ident(out, name, deck(d));
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
    xschem::comment(deck(d))
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

/// The deck around the blocks (spec §7.6.2–3): the title `<top cell> (schemify
/// <dialect>)`, the globals, the bench vars in name order (the top's parameters), the
/// corner lines in file order and the bench code (elab selected the code for the
/// dialect: none for CDL). SpiceRack leaves out what a dialect has no line for.
fn frame(e: &Elab, d: Dialect) -> Lowered {
    let b = &e.bench;
    assert_eq!(b.var_name.len(), b.var_text.len());
    let top = e.cells.first().map_or("top", |c| e.names.get(c.cell));
    let mut setup = Vec::with_capacity(b.corner_kind.len());
    // A non-bench top has empty bench tables (no CSR seed either).
    if !b.corner_kind.is_empty() {
        assert_eq!(b.corner_start.len(), b.corner_kind.len() + 1);
    }
    for k in 0..b.corner_kind.len() {
        let args = &b.corner_args[b.corner_start[k] as usize..b.corner_start[k + 1] as usize];
        let arg = |i: usize| args.get(i).map_or("", |n| e.names.get(*n)).to_owned();
        setup.push(match b.corner_kind[k] {
            CornerLine::Lib => SetupLine::Lib {
                path: arg(0),
                section: arg(1),
            },
            CornerLine::Include => SetupLine::Include { path: arg(0) },
            CornerLine::Temp => SetupLine::Temp { value: arg(0) },
        });
    }
    assert!(b.code_lines.is_empty() || b.code_start.len() == b.code_name.len() + 1);
    let code: Vec<String> = b
        .code_lines
        .iter()
        .map(|l| e.names.get(*l).to_owned())
        .collect();
    let parameters = (0..b.var_name.len())
        .map(|v| ParamDef {
            name: e.names.get(b.var_name[v]).to_owned(),
            default: Some(e.names.get(b.var_text[v]).to_owned()),
        })
        .collect();
    Lowered {
        ir: CircuitIR {
            testbench: (!code.is_empty()).then(|| Testbench {
                extra_lines: code,
                ..Testbench::default()
            }),
            top: Subcircuit {
                parameters,
                ..Subcircuit::default()
            },
            ..CircuitIR::default()
        },
        title: format!("{top} (schemify {})", d.as_str()),
        globals: e
            .globals
            .iter()
            .map(|g| e.names.get(*g).to_owned())
            .collect(),
        setup,
        ..Lowered::default()
    }
}

/// Definition `d`'s parameter tuple (spec §7.4): CDL has no `.param`, so instance text
/// that names a cell parameter (`R1 in out R`) resolves only from the `.SUBCKT` line, as
/// Virtuoso's auCdl writes it (`.SUBCKT rcfilter gnd in out R=1K C=1u`).
fn cdl_params(e: &Elab, d: u32) -> Vec<ParamDef> {
    let (a, b) = (
        e.defs.param_start[d as usize] as usize,
        e.defs.param_start[d as usize + 1] as usize,
    );
    assert!(a <= b && b <= e.defs.param_name.len());
    (a..b)
        .map(|k| ParamDef {
            name: e.names.get(e.defs.param_name[k]).to_owned(),
            default: Some(e.names.get(e.defs.param_text[k]).to_owned()),
        })
        .collect()
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

/// Once-only text (spec §7.1, §7.2): Verilog-A loads (`<stem>.va`; SpiceRack writes
/// ngspice's OSDI load or espice's `.hdl`), the generated models, the impl bodies by
/// (cell, impl). Only impls some emitted row uses.
fn once_only(l: &mut Lowered, e: &Elab, d: Dialect, tmp: &Tmp) {
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
    // OSDI (`.hdl` for espice) loads and models (ngspice and espice only: elab rejects
    // veriloga elsewhere, and verilog outside ngspice and the verilog dialect). A
    // Verilog-A source (a `file`, or a `body` that is source) is compiled by the runner:
    // one OSDI load per stem and a `.model` per model name (the impl's `model` attr, and
    // each default-template row's model). Any other `veriloga` impl is precompiled: its
    // body carries that text (§7.6).
    let mut osdi: Vec<String> = Vec::new();
    // (name, kind, d_cosim simulation file): `.model <name> <kind> [simulation="…"]`.
    let mut models: Vec<(String, String, Option<String>)> = Vec::new();
    let lang_of = |l: usize| {
        let f = &e.cells[e.impls.cell[l] as usize];
        e.names.get(f.impl_lang[e.impls.index[l] as usize])
    };
    let model_line = |l: usize, m: &str, models: &mut Vec<_>, osdi: &mut Vec<String>| {
        let stem = stem_of(l);
        match lang_of(l) {
            "veriloga" if compiled_source(e, l) => {
                models.push((m.to_owned(), stem.clone(), None));
                osdi.push(stem);
            }
            "verilog" if d == Dialect::Ngspice => models.push((
                m.to_owned(),
                "d_cosim".into(),
                Some(format!("\"{stem}.vvp\"")),
            )),
            _ => {}
        }
    };
    if matches!(d, Dialect::Ngspice | Dialect::Espice) {
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
    l.ir.top.osdi_loads = osdi.into_iter().map(|s| s + ".va").collect();
    l.ir.top.models = models
        .into_iter()
        .map(|(name, kind, sim)| ModelDef {
            name,
            kind,
            parameters: sim
                .map(|s| ("simulation".to_owned(), s))
                .into_iter()
                .collect(),
        })
        .collect();
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
    l.bodies = bodies
        .into_iter()
        .map(|l| {
            let lines =
                &e.impls.body[e.impls.body_start[l] as usize..e.impls.body_start[l + 1] as usize];
            let mut text = String::new();
            for &line in lines {
                text.push_str(e.names.get(line));
                text.push('\n');
            }
            text
        })
        .collect();
}

/// Direction of each port of definition `d` in port order (CDL `*.PININFO`, verilog
/// declarations): ordinary pins bit by bit, then declared inherited pins, then inherited
/// pins propagated from the subtree (`inout`). Tolerates a port list that disagrees with
/// the pins (`E012` designs): missing directions are `inout`.
fn port_dirs(e: &Elab, d: u32) -> Vec<PortDirection> {
    let f = &e.cells[e.defs.cell[d as usize] as usize];
    let n = e.ports(d).len();
    let mut dirs = Vec::with_capacity(n);
    for inherited in [false, true] {
        for p in 0..f.pin_name.len() {
            if (f.pin_inherit[p] != Name::NONE) == inherited && f.pin_alias[p] == u32::MAX {
                let dir = match f.pin_dir[p] {
                    PinDir::In => PortDirection::Input,
                    PinDir::Out => PortDirection::Output,
                    PinDir::Inout => PortDirection::InOut,
                };
                dirs.extend(std::iter::repeat_n(dir, f.bits(p).len()));
            }
        }
    }
    dirs.resize(n, PortDirection::InOut);
    assert_eq!(dirs.len(), n);
    dirs
}

/// Definition `d`'s ports: their nets under the respellings `rs`, with directions.
fn ports(e: &Elab, d: u32, rs: &[(Name, String)]) -> Vec<Port> {
    e.ports(d)
        .iter()
        .zip(port_dirs(e, d))
        .map(|(p, direction)| Port {
            name: spelled(e, rs, *p).to_owned(),
            direction,
        })
        .collect()
}

// ------------------------------------------------------------------ definitions

/// The lowering state: the plans, per-row scratch and the name map under construction.
struct Writer<'e> {
    e: &'e Elab,
    d: Dialect,
    plans: &'e Plans,
    tmp: Tmp,
    /// Elab definition → name-map definition (`NONE` when not emitted).
    def_map: Vec<u32>,
    map: name_map::Builder,
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
        let mut map = name_map::Builder::new(e, d, order.len() + usize::from(e.top != NONE), rows);
        map.respell = xspice_spellings(e, plans, d);
        Writer {
            e,
            d,
            plans,
            tmp: Tmp::new(e),
            def_map,
            map,
        }
    }

    /// Definition `d`: the subcircuit (its ports, its cell's own code, D17 as amended,
    /// spec §7.6.15, and for CDL its parameter tuple), its rows as lines and its verilog
    /// `wire`s. The deck sorts the rows by element name.
    fn def_block(&mut self, d: u32) -> (Subcircuit, Vec<Lines>, Vec<String>) {
        let e = self.e;
        assert!(
            self.def_map[d as usize] != NONE,
            "definition {d} is emitted"
        );
        self.map.def(e, d, &self.def_map);
        let lines = self.rows(d);
        let code =
            e.defs.code_start[d as usize] as usize..e.defs.code_start[d as usize + 1] as usize;
        let sc = Subcircuit {
            name: e.names.get(e.defs.name[d as usize]).to_owned(),
            ports: ports(e, d, &self.map.respell),
            parameters: if self.d == Dialect::Cdl {
                cdl_params(e, d)
            } else {
                Vec::new()
            },
            raw_spice: e.defs.code[code]
                .iter()
                .map(|l| e.names.get(*l).to_owned())
                .collect(),
            ..Subcircuit::default()
        };
        let nets = if self.d == Dialect::Verilog {
            self.wires(d)
        } else {
            Vec::new()
        };
        (sc, lines, nets)
    }

    /// Verilog: every net of the definition's emitted rows that is not a port, sorted by
    /// name (spec §7.5).
    fn wires(&self, d: u32) -> Vec<String> {
        let e = self.e;
        let ports = e.ports(d);
        let mut wires: Vec<Name> = Vec::new();
        for r in e.insts_of(d) {
            if e.insts.ignore[r] == Ignore::No {
                wires.extend(e.conns(r).iter().filter(|n| **n != Name::NONE));
            }
        }
        wires.sort_unstable();
        wires.dedup();
        wires.retain(|n| !ports.contains(n));
        e.names.sort(&mut wires);
        wires.iter().map(|n| e.names.get(*n).to_owned()).collect()
    }

    /// Instance rows of definition `d` in row order as packed lines, each run marked
    /// sorted when the whole block is in element-name order, and their name-map rows.
    fn rows(&mut self, d: u32) -> Vec<Lines> {
        let e = self.e;
        let rows = e.insts_of(d);
        // Rows render independently: a large block renders in chunks on scoped threads and
        // the chunks are appended in row order, so the IR is the serial IR (D37).
        let chunks = if rows.len() >= PARALLEL_ROWS && !SERIAL.load(Relaxed) {
            std::thread::available_parallelism().map_or(1, |n| n.get().min(4))
        } else {
            1
        };
        let step = rows.len().div_ceil(chunks).max(1);
        // At least one part, so an empty block renders one empty chunk.
        let parts: Vec<std::ops::Range<usize>> = (rows.start..rows.end.max(rows.start + 1))
            .step_by(step)
            .map(|a| a..(a + step).min(rows.end))
            .collect();
        let (dl, plans, respell) = (self.d, self.plans, &self.map.respell);
        // The first part renders here into `self.tmp`; every other part on its own thread
        // with its own scratch, whose uses are merged after.
        let mut block = std::thread::scope(|sc| {
            let workers: Vec<_> = parts[1..]
                .iter()
                .map(|p| {
                    sc.spawn(|| {
                        let mut tmp = Tmp::new(e);
                        (render(e, dl, plans, respell, p.clone(), &mut tmp), tmp.uses)
                    })
                })
                .collect();
            let mut all = render(e, dl, plans, respell, parts[0].clone(), &mut self.tmp);
            for w in workers {
                let (c, uses) = w.join().expect("rows render");
                self.tmp.uses.merge(uses);
                all.runs.extend(c.runs);
                all.kept.extend(c.kept);
            }
            all
        });
        let xd = deck(dl);
        let (mut run, mut line) = (0, 0);
        let (mut sorted, mut prev) = (true, "");
        for &(r, rendered) in &block.kept {
            while line == block.runs[run].len() {
                (run, line) = (run + 1, 0); // the next part's run
            }
            let name = xschem::element_name(block.runs[run].get(line), xd);
            sorted &= prev <= name;
            prev = name;
            if rendered {
                let r = r as usize;
                let child = match e.insts.target[r] {
                    Target::Def(c) => self.def_map[c as usize],
                    Target::Leaf(_) => NONE,
                };
                self.map.row(e, r, name, child);
            }
            line += 1;
        }
        assert_eq!(
            block.kept.len(),
            block.runs.iter().map(Lines::len).sum::<usize>()
        );
        // The deck then copies the runs without reading their names again.
        block.runs.iter_mut().for_each(|l| l.sorted = sorted);
        block.runs
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

/// The rows of one block in row order as packed lines, and per kept row whether it
/// rendered (a failed row is an error comment line with no name-map row).
struct Block {
    /// One run per rendered part, in row order.
    runs: Vec<Lines>,
    kept: Vec<(u32, bool)>,
}

/// Renders rows `rows` of one block straight into its packed lines (spec §7.6.1: a
/// failed template becomes an error comment, an empty rendering no element). The impls
/// and models used accumulate in `tmp`.
fn render(
    e: &Elab,
    d: Dialect,
    plans: &Plans,
    respell: &[(Name, String)],
    rows: std::ops::Range<usize>,
    tmp: &mut Tmp,
) -> Block {
    let mut lines = Lines {
        text: String::with_capacity(48 * rows.len()),
        ends: Vec::with_capacity(rows.len()),
        sorted: false,
    };
    let mut kept = Vec::with_capacity(rows.len());
    for r in rows {
        if e.insts.ignore[r] != Ignore::No {
            continue;
        }
        let local = e.names.get(e.insts.name[r]);
        let start = lines.text.len();
        let rendered = inst::line(
            e,
            d,
            plans,
            r,
            local,
            |n| spelled(e, respell, n),
            &mut lines.text,
            tmp,
        );
        match &rendered {
            Ok(()) if lines.text.len() == start => continue, // renders nothing
            Ok(()) => end_line(&mut lines),
            Err(msg) => {
                lines.text.truncate(start);
                push_error(&mut lines.text, d, local, msg);
                end_line(&mut lines);
            }
        }
        kept.push((r as u32, rendered.is_ok()));
    }
    assert_eq!(kept.len(), lines.len());
    Block {
        runs: vec![lines],
        kept,
    }
}

/// Ends a rendered line with its newline (the deck then copies a part's lines whole).
fn end_line(lines: &mut Lines) {
    if !lines.text.ends_with('\n') {
        lines.text.push('\n');
    }
    lines.end_line();
}

/// Appends the comment a failed row is written as: `* error in <name>: <message>`.
fn push_error(out: &mut String, d: Dialect, local: &str, msg: &str) {
    out.push_str(comment(d));
    out.push_str(" error in ");
    out.push_str(local);
    out.push_str(": ");
    out.push_str(&msg.replace('\n', " "));
}

/// Flattens definition `d` at instance path `path` (`__`-joined local names, empty at
/// the top; `dotted` is the same path `.`-joined) whose ports are bound to `ports`
/// (flat net names), appending its leaf lines (failed and empty rows are left out).
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
                let ok = inst::line(e, dialect, plans, r, &name, lookup, &mut out, tmp).is_ok();
                if ok && !out.is_empty() {
                    lines.push(out);
                }
            }
        }
    }
}
