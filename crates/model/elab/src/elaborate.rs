//! Hierarchy elaboration (netlist spec §1, §2, §5, §6, D11a): a top document, a bench
//! config, a corner and overrides in, the tables a netlister walks ([`Elab`]) out.
//!
//! The output stays hierarchical: one [`Defs`] row per distinct specialization of a
//! schematic impl, its instances in [`Insts`], leaf impls in [`Impls`]. A design that
//! flattens to 10^6 leaves costs only its distinct definitions.
//!
//! Spine of [`elaborate`]: request → config and corner → top cell → discover (resolve
//! every reachable cell, collect global labels) → `build_def` (depth first, children
//! before parents: prepare the cell (connect, graph, evaluate in context, checks) →
//! select impls → recurse → share or append the definition) → name definitions → mark
//! used → globals → bench lines → sort violations.
//!
//! Files: `tables` (the output), `prepare` (cells), `impls` (selection, leaf rows),
//! `rows` (one definition's instance rows), `defs` (sharing, naming), `bench` (header).
//!
//! Contents:
//! - `pub struct Request`
//! - `pub const DEPTH_MAX`
//! - `pub fn elaborate<F: Funcs>(project, top, req, funcs) -> Elab`
//! - re-exported from `tables`: `Dialect`, `Target`, `COMPUTED`, `DEFAULT`, `OVERRIDDEN`,
//!   `Defs`, `Insts`, `Impls`, `CornerLine`, `Bench`, `Elab`

mod bench;
mod defs;
mod impls;
mod prepare;
mod rows;
mod tables;

pub use tables::{
    Bench, COMPUTED, CornerLine, DEFAULT, Defs, Dialect, Elab, Impls, Insts, OVERRIDDEN, Target,
};

use crate::cells::{self, CellName};
use crate::eval::Work;
use crate::graph::Graph;
use crate::iface::{Held, Iface};
use crate::names::{Name, Names};
use crate::netexpr;
use crate::nets::{CellNets, Ignore};
use crate::values::{Funcs, NONE, Val};
use crate::violations::{Violation, Violations};
use schemify_doc::{DocHandle, Document, ObjId, Project, Records};
use schemify_format::FileKind;
use std::collections::HashMap;
use std::mem;
use std::ops::Range;

/// What to elaborate besides the top document.
#[derive(Clone, Copy, Debug)]
pub struct Request<'req> {
    /// Bench config; `None` takes the bench's `config` header attribute, if any.
    pub config: Option<&'req str>,
    /// Corner; `None` takes the bench's `corner` header attribute, if any.
    pub corner: Option<&'req str>,
    /// `(var, expression)` assignments applied after the corner's `set` lines, in
    /// order (sweep/MC/optimizer values, then CLI overrides); later entries win.
    pub overrides: &'req [(&'req str, &'req str)],
    /// Environment for `${NAME}` expansion after the project's `set` entries (spec
    /// §7.1). The app passes what it allows (the process environment natively, nothing
    /// on the web); elab itself reads no environment.
    pub env: &'req [(&'req str, &'req str)],
    /// Target dialect.
    pub dialect: Dialect,
}

/// Deepest hierarchy elaborated before `E014` is reported.
pub const DEPTH_MAX: u32 = 1024;

/// The selected bench config's rules.
#[derive(Default)]
struct Config {
    /// `inst <path> <impl>` rules.
    inst: Vec<InstRule>,
    /// Parallel to `inst`: some instance path matched the rule (`W037` otherwise).
    inst_hit: Vec<bool>,
    /// `cell <cell row> <impl>` rules.
    cell: Vec<CellRule>,
    /// `order` impl names, first match wins.
    order: Vec<String>,
}

/// A config `inst <path> <impl>` rule.
struct InstRule {
    path: String,
    imp: String,
}

/// A config `cell <cell> <impl>` rule, its cell resolved to a project row.
struct CellRule {
    row: usize,
    imp: String,
}

/// Per reached cell: its document and what preparing it derived (once per cell).
struct CellData<'project> {
    doc: &'project Document,
    resolved: bool,
    /// Why each unresolved instance row did not resolve (`None`: resolved).
    unresolved: Vec<Option<String>>,
    prepared: bool,
    /// Cell (index into `Elaborator::ifaces`) of each instance row, `NONE` unresolved.
    inst_iface: Vec<u32>,
    graph: Graph,
    waivers: Vec<(ObjId, Name)>,
    /// Own params' default values and their tuple text (spec §6 naming).
    defaults: Option<Vec<Val>>,
    default_key: Option<String>,
}

/// One instance row of a definition being built. Its connections and params are ranges
/// of the definition's [`DefRows`] (elements of one instance share the params range).
struct RowTmp {
    name: Name,
    /// The name's first 8 bytes, big-endian, zero-padded: orders rows like their text
    /// wherever those bytes differ (the sort compares text only on a tie).
    prefix: u64,
    id: ObjId,
    target: Target,
    ignore: Ignore,
    conn: Range<u32>,
    params: Range<u32>,
}

/// The rows of one definition being built (flat, appended per instance, so a row costs
/// no allocation of its own).
struct DefRows {
    rows: Vec<RowTmp>,
    conn: Vec<Name>,
    params: Vec<ParamEntry>,
    /// Inherited pins the subtree reaches this definition with.
    propagated: Vec<InheritedPin>,
}

/// One param of one instance row being built.
struct ParamEntry {
    name: Name,
    /// Canonical value text (spec §6).
    text: Name,
    value: Val,
    /// [`COMPUTED`], [`DEFAULT`], [`OVERRIDDEN`].
    flags: u8,
}

/// An inherited pin (D29) and the net it defaults to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InheritedPin {
    pin: Name,
    default: Name,
}

/// One elaboration's working state; its tables move into [`Elab`] at the end.
struct Elaborator<'project, 'request, F> {
    project: &'project Project,
    top: DocHandle,
    req: &'request Request<'request>,
    funcs: &'request mut F,
    names: Names,
    ifaces: Vec<Iface>,
    cells: Vec<CellData<'project>>,
    nets: Vec<Option<CellNets>>,
    /// Cell row → index into `ifaces`.
    row_cell: HashMap<u32, u32>,
    config: Config,
    /// The corner's `set` lines, then the request's overrides (top cell only).
    subst: Vec<(String, String)>,
    viol: Vec<Violation>,
    /// (cell, tuple text) → definition, for paths no config `inst` rule reaches below.
    memo: HashMap<(u32, String), u32>,
    /// (cell, tuple text, children signature) → definition.
    by_key: HashMap<(u32, String, String), u32>,
    /// (cell, impl) → [`Impls`] row, `NONE` when unsupported for the dialect.
    impl_ids: HashMap<(u32, u32), u32>,
    /// Cells on the current instantiation path (recursion check).
    stack: Vec<u32>,
    needs_open: Vec<u32>,
    /// Names made global by `global true` labels (sorted).
    globals: Vec<Name>,
    defs: Defs,
    insts: Insts,
    impls: Impls,
    ws: Work,
}

/// Elaborates `top` (a bench or any cell) for `req` (netlist spec §1–§6, §8).
///
/// Every cell the hierarchy reaches must be indexed; cells whose schematic or body is
/// needed must be open (else they are listed in [`Elab::needs_open`] and treated as
/// interface-only). Never fails: problems are [`Elab::violations`].
///
/// Cost: per distinct definition, its cell's graph evaluation plus its instance rows;
/// connectivity and graphs are built once per reached cell.
///
/// # Panics
///
/// Panics if `top` is a stale handle.
pub fn elaborate<F: Funcs>(
    project: &Project,
    top: DocHandle,
    req: &Request<'_>,
    funcs: &mut F,
) -> Elab {
    let doc = project
        .doc(top)
        .expect("elaborate needs an open top document");
    let mut e = Elaborator {
        project,
        top,
        req,
        funcs,
        names: Names::default(),
        ifaces: Vec::new(),
        cells: Vec::new(),
        nets: Vec::new(),
        row_cell: HashMap::new(),
        config: Config::default(),
        subst: Vec::new(),
        viol: Vec::new(),
        memo: HashMap::new(),
        by_key: HashMap::new(),
        impl_ids: HashMap::new(),
        stack: Vec::new(),
        needs_open: Vec::new(),
        globals: Vec::new(),
        defs: Defs::default(),
        insts: Insts::default(),
        impls: Impls::default(),
        ws: Work::default(),
    };
    for start in [
        &mut e.defs.param_start,
        &mut e.defs.port_start,
        &mut e.defs.inh_start,
        &mut e.defs.inst_start,
        &mut e.defs.code_start,
        &mut e.insts.conn_start,
        &mut e.insts.param_start,
        &mut e.impls.body_start,
    ] {
        start.push(0);
    }

    // ---- request → config and corner
    let bench = doc.kind() == FileKind::Shbench;
    let header = |key| if bench { doc.header_value(key) } else { None };
    let (config, corner) = (
        req.config.or(header("config")),
        req.corner.or(header("corner")),
    );
    let top_row = cells::row_of(project, doc);
    let CellName {
        cell: top_name,
        stem: top_stem,
    } = top_row.map_or_else(CellName::top, |r| cells::display(project, r));
    let top_cell = e.names.intern(&top_name);
    if let Some(c) = config {
        e.read_config(doc, c, top_cell);
    }
    let corner_row = corner.and_then(|c| record(doc.corners(), doc, c));
    if let Some(c) = corner.filter(|_| corner_row.is_none()) {
        e.request_finding(top_cell, &format!("corner `{c}` does not exist"));
    }
    if let Some(r) = corner_row {
        let s = doc.strings();
        for (k, vals) in doc.pool().attrs(doc.corners().attrs[r]) {
            if s.get(k) == "set" && vals.len() == 2 {
                e.subst
                    .push((s.get(vals[0]).to_owned(), s.get(vals[1]).to_owned()));
            }
        }
    }
    let overrides = req.overrides.iter();
    e.subst
        .extend(overrides.map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));

    // ---- top cell and the hierarchy
    let row = top_row.map_or(NONE, |r| r as u32);
    let iface = Iface::from_doc(doc, &top_name, &top_stem, row, Held::Open, &mut e.names);
    e.push_cell(doc, iface);
    if let Some(r) = top_row {
        e.row_cell.insert(r as u32, 0);
    }
    e.discover();
    let top_tuple = e.defaults(0);
    let top_def = e.build_def(0, top_tuple, String::new(), 0);
    assert!(e.stack.is_empty(), "build_def pops every level it pushes");
    e.unmatched_inst_rules(top_cell);

    // ---- names, used flags, globals, bench lines
    e.name_defs();
    let mut used = vec![false; e.defs.cell.len()];
    if let Some(t) = top_def {
        mark(&e.defs, &e.insts, t, &mut used);
    }
    e.defs.used = used;
    let globals = used_globals(&e);
    let bench = if bench {
        e.bench_lines(doc, corner_row)
    } else {
        Bench::default()
    };
    e.needs_open.sort_unstable();
    e.needs_open.dedup();
    let out = Elab {
        violations: Violations::from_rows(mem::take(&mut e.viol), &e.names),
        top: top_def.unwrap_or(NONE),
        globals,
        bench,
        needs_open: e.needs_open,
        cells: e.ifaces,
        nets: e.nets,
        defs: e.defs,
        insts: e.insts,
        impls: e.impls,
        names: e.names,
    };
    assert_eq!(out.defs.inst_start.len(), out.defs.cell.len() + 1);
    assert_eq!(out.insts.conn_start.len(), out.insts.name.len() + 1);
    assert!(
        out.top == NONE || (out.top as usize) < out.defs.cell.len(),
        "the top is a definition"
    );
    assert_eq!(
        out.defs.used.len(),
        out.defs.cell.len(),
        "one used flag per definition"
    );
    out
}

// ---- private ----

/// Marks definition `d` and every definition reachable from it through rows that are
/// not ignored. Recursion depth ≤ [`DEPTH_MAX`] (definitions are built at most that deep).
fn mark(defs: &Defs, insts: &Insts, d: u32, used: &mut [bool]) {
    assert!((d as usize) < used.len(), "definition {d} out of range");
    if used[d as usize] {
        return;
    }
    used[d as usize] = true;
    for r in defs.inst_start[d as usize] as usize..defs.inst_start[d as usize + 1] as usize {
        if let (Target::Def(c), Ignore::No) = (insts.target[r], insts.ignore[r]) {
            mark(defs, insts, c, used);
        }
    }
}

/// Global net names used anywhere (connections and ports), sorted by text: the `is_global`
/// spellings and the names of `global true` labels.
fn used_globals<F>(e: &Elaborator<'_, '_, F>) -> Vec<Name> {
    // Each distinct connection name is tested once (a bitset over the arena): a flat
    // design repeats few names among 10^6 connections.
    let mut seen = vec![0u64; (e.names.len() as usize).div_ceil(64)];
    let mut globals: Vec<Name> = Vec::new();
    let connected = e.insts.conn.iter().chain(&e.defs.port_net);
    for &n in connected {
        if n == Name::NONE {
            continue;
        }
        let (w, bit) = (n.0 as usize / 64, 1u64 << (n.0 % 64));
        if seen[w] & bit != 0 {
            continue;
        }
        seen[w] |= bit;
        if netexpr::is_global(e.names.get(n)) || e.globals.binary_search(&n).is_ok() {
            globals.push(n);
        }
    }
    e.names.sort(&mut globals);
    assert!(globals.len() <= e.names.len() as usize);
    debug_assert!(
        globals.windows(2).all(|w| w[0] != w[1]),
        "each name tested once"
    );
    globals
}

/// Returns the row of the record named `name` in a named bench table.
fn record(t: &Records, doc: &Document, name: &str) -> Option<usize> {
    assert_eq!(t.fields.len(), t.kind.len(), "record columns in step");
    let s = doc.strings();
    (0..t.kind.len()).find(|&r| s.get(t.fields[r][0]) == name)
}
