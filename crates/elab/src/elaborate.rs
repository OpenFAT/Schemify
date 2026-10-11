//! Hierarchy elaboration (netlist spec §1, §2, §5, §6): from a top document, a bench
//! config, a corner and overrides to the tables a netlister walks.
//!
//! The output stays hierarchical: one [`Defs`] row per distinct specialization of a
//! schematic impl, its instances in [`Insts`], leaf impls in [`Impls`]. A design that
//! flattens to 10^6 leaves costs only its distinct definitions.
//!
//! Spine of [`elaborate`]: request → config and corner → top cell → `build_def`
//! (depth first, children before parents: resolve → connect → graph → evaluate in
//! context → checks → select impls → recurse) → name definitions → mark used →
//! globals → bench lines → sort violations.

use crate::cells::{display, is_open, resolve_in_row, row_of};
use crate::check::{
    CellCtx, Code, Violation, Violations, check_overrides, check_values, waivers_found,
};
use crate::connect::{CellNets, Ignore, NO_NET, connect};
use crate::graph::{Graph, OWN, Work};
use crate::iface::Iface;
use crate::names::{Name, Names, fnv1a64, push_base32};
use crate::netexpr::{self, is_global};
use crate::values::{Funcs, NONE, Val};
use schemify_doc::{DocHandle, Document, ObjId, Project, Sym};
use schemify_expr::{Mode, Program, Scratch, Value};
use schemify_format::FileKind;
use std::collections::HashMap;

/// Netlist dialects (D22). Owned here because impl support (`E013`) and code-block
/// selection depend on it; `netlist` matches on the same enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Dialect {
    /// ngspice.
    Ngspice,
    /// Xyce.
    Xyce,
    /// CDL (LVS).
    Cdl,
    /// Spectre.
    Spectre,
    /// Structural Verilog.
    Verilog,
}

impl Dialect {
    /// Returns the dialect word (`ngspice`, …).
    pub fn as_str(self) -> &'static str {
        ["ngspice", "xyce", "cdl", "spectre", "verilog"][self as usize]
    }

    /// `format <dialect>` words an impl may use for this dialect, most specific first
    /// (format spec §4: `spice` serves ngspice, xyce and cdl).
    pub fn format_words(self) -> &'static [&'static str] {
        match self {
            Dialect::Ngspice => &["spice"],
            Dialect::Xyce => &["xyce", "spice"],
            Dialect::Cdl => &["cdl", "spice"],
            Dialect::Spectre => &["spectre"],
            Dialect::Verilog => &["verilog"],
        }
    }

    /// Returns whether a bench `code` block of `lang` is emitted (spec §7: `spice` for
    /// the SPICE simulators, else the dialect's own word; CDL takes none).
    pub fn takes_code(self, lang: &str) -> bool {
        match self {
            Dialect::Cdl => false,
            Dialect::Ngspice => matches!(lang, "spice" | "ngspice"),
            Dialect::Xyce => matches!(lang, "spice" | "xyce"),
            Dialect::Spectre | Dialect::Verilog => lang == self.as_str(),
        }
    }
}

/// What to elaborate besides the top document.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    /// Bench config; `None` takes the bench's `config` header attribute, if any.
    pub config: Option<&'a str>,
    /// Corner; `None` takes the bench's `corner` header attribute, if any.
    pub corner: Option<&'a str>,
    /// `(var, expression)` assignments applied after the corner's `set` lines, in
    /// order (sweep/MC/optimizer values, then CLI overrides); later entries win.
    pub overrides: &'a [(&'a str, &'a str)],
    /// Environment for `${NAME}` expansion after the project's `set` entries (spec
    /// §7.1). The app passes what it allows (the process environment natively, nothing
    /// on the web); elab itself reads no environment.
    pub env: &'a [(&'a str, &'a str)],
    /// Target dialect.
    pub dialect: Dialect,
}

/// What an instance row instantiates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A definition (specialized schematic impl), by [`Defs`] row.
    Def(u32),
    /// A leaf impl, by [`Impls`] row.
    Leaf(u32),
}

/// Param entry flag: the param is `computed`.
pub const COMPUTED: u8 = 1;
/// Param entry flag: the value equals the instantiated cell's default.
pub const DEFAULT: u8 = 2;
/// Param entry flag: the instance overrides it.
pub const OVERRIDDEN: u8 = 4;

/// Definitions: one per distinct (cell, evaluated param tuple, children). Rows are in
/// creation order, children before parents.
#[derive(Clone, Debug, Default)]
pub struct Defs {
    /// Cell (index into [`Elab::cells`]).
    pub cell: Vec<u32>,
    /// Subckt / module name (spec §6 specialization naming).
    pub name: Vec<Name>,
    /// Reachable from the top through instances that are not ignored.
    pub used: Vec<bool>,
    /// CSR offsets (`defs + 1`) into the tuple columns.
    pub param_start: Vec<u32>,
    /// Non-computed params of the tuple, sorted by name.
    pub param_name: Vec<Name>,
    /// Canonical value text.
    pub param_text: Vec<Name>,
    /// CSR offsets (`defs + 1`) into [`Defs::port_net`].
    pub port_start: Vec<u32>,
    /// Port nets in order: ordinary pin bits, then inherited pin bits (spec §7.2).
    pub port_net: Vec<Name>,
    /// CSR offsets (`defs + 1`) into the inherited-pin columns.
    pub inh_start: Vec<u32>,
    /// Inherited pins this definition takes (declared by the cell, then propagated from
    /// its subtree, sorted by name).
    pub inh_pin: Vec<Name>,
    /// Their default nets.
    pub inh_default: Vec<Name>,
    /// CSR offsets (`defs + 1`) into [`Insts`] rows.
    pub inst_start: Vec<u32>,
    /// The [`Impls`] row holding the `body` of the cell's schematic impl (model cards
    /// an imported `device_model` put there), or `u32::MAX` when it has none. Its
    /// `template` is unused: the definition is emitted as a subckt, the body once.
    pub body: Vec<u32>,
    /// CSR offsets (`defs + 1`) into [`Defs::code`].
    pub code_start: Vec<u32>,
    /// The `code` lines a `.shcomp` cell carries for the dialect (D17 as amended
    /// 2026-10-11: netlist text that belongs to the component), blocks sorted by name,
    /// `${NAME}` expanded; the netlister writes them inside every specialization, after
    /// its instance lines (spec §7.6.15). Empty for a bench top (its code is
    /// [`Bench`]'s) and for CDL.
    pub code: Vec<Name>,
    key: Vec<String>,
    sig: Vec<String>,
}

/// Instance rows of all definitions, grouped by definition (`Defs::inst_start`), each
/// group sorted by local name.
#[derive(Clone, Debug, Default)]
pub struct Insts {
    /// Local element name (`X1[3]`, `M1`).
    pub name: Vec<Name>,
    /// Source instance id in the definition's cell document.
    pub id: Vec<ObjId>,
    /// What it instantiates.
    pub target: Vec<Target>,
    /// How it is left out of netlists.
    pub ignore: Vec<Ignore>,
    /// CSR offsets (`rows + 1`) into [`Insts::conn`].
    pub conn_start: Vec<u32>,
    /// Net name per pin bit: ordinary pins in pin order, then the target's inherited
    /// pins; [`Name::NONE`] for an unconnected bit (`E011`, `ignore open`).
    pub conn: Vec<Name>,
    /// CSR offsets (`rows + 1`) into the param columns.
    pub param_start: Vec<u32>,
    /// Param name, in the instantiated cell's schema order.
    pub param_name: Vec<Name>,
    /// Canonical value text (expr spec §7).
    pub param_text: Vec<Name>,
    /// Typed value (for template holes).
    pub param_val: Vec<Val>,
    /// [`COMPUTED`] | [`DEFAULT`] | [`OVERRIDDEN`].
    pub param_flags: Vec<u8>,
}

/// Leaf impls instances use, one row per (cell, impl).
#[derive(Clone, Debug, Default)]
pub struct Impls {
    /// Cell (index into [`Elab::cells`]).
    pub cell: Vec<u32>,
    /// Impl index within the cell's interface.
    pub index: Vec<u32>,
    /// The `format` template for the dialect, or [`Name::NONE`] for the lang's default
    /// template (spec §7.1).
    pub template: Vec<Name>,
    /// CSR offsets (`impls + 1`) into [`Impls::body`].
    pub body_start: Vec<u32>,
    /// `body` lines, emitted once per netlist (empty when the cell has none or is not
    /// open, see [`Elab::needs_open`]).
    pub body: Vec<Name>,
}

/// A corner line kind (format spec §4 rank 6), in file order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerLine {
    /// `lib <path> <section>`.
    Lib,
    /// `include <path>`.
    Include,
    /// `temp <value>` (evaluated).
    Temp,
}

/// Bench data for the netlist header (spec §7.1). Empty for a non-bench top.
#[derive(Clone, Debug, Default)]
pub struct Bench {
    /// Bench var names, sorted.
    pub var_name: Vec<Name>,
    /// Their canonical value text (after corner `set` and overrides).
    pub var_text: Vec<Name>,
    /// Selected corner lines, file order.
    pub corner_kind: Vec<CornerLine>,
    /// CSR offsets (`lines + 1`) into [`Bench::corner_args`].
    pub corner_start: Vec<u32>,
    /// Paths (with `${NAME}` unexpanded: the app expands with the environment),
    /// sections, temperature text.
    pub corner_args: Vec<Name>,
    /// Code blocks for the dialect, sorted by name.
    pub code_name: Vec<Name>,
    /// Their lang.
    pub code_lang: Vec<Name>,
    /// CSR offsets (`blocks + 1`) into [`Bench::code_lines`].
    pub code_start: Vec<u32>,
    /// Verbatim lines.
    pub code_lines: Vec<Name>,
}

/// One elaboration: everything a netlister needs, no `doc` access required.
#[derive(Clone, Debug, Default)]
pub struct Elab {
    /// Every name below.
    pub names: Names,
    /// Reached cells' interfaces (index 0 is the top).
    pub cells: Vec<Iface>,
    /// Connectivity of cells whose schematic was elaborated.
    pub nets: Vec<Option<CellNets>>,
    /// Definitions.
    pub defs: Defs,
    /// Instance rows.
    pub insts: Insts,
    /// Leaf impls.
    pub impls: Impls,
    /// The top definition.
    pub top: u32,
    /// Global net names used anywhere (`0` included), sorted.
    pub globals: Vec<Name>,
    /// Bench header data.
    pub bench: Bench,
    /// Findings, sorted by key.
    pub violations: Violations,
    /// Cell rows elaboration needed the full document of but found only the indexed
    /// interface: open them and elaborate again.
    pub needs_open: Vec<u32>,
}

impl Elab {
    /// Returns the instance rows of definition `d`.
    pub fn insts_of(&self, d: u32) -> std::ops::Range<usize> {
        self.defs.inst_start[d as usize] as usize..self.defs.inst_start[d as usize + 1] as usize
    }

    /// Returns the connection names of instance row `r`.
    pub fn conns(&self, r: usize) -> &[Name] {
        &self.insts.conn[self.insts.conn_start[r] as usize..self.insts.conn_start[r + 1] as usize]
    }

    /// Returns the port nets of definition `d`.
    pub fn ports(&self, d: u32) -> &[Name] {
        let d = d as usize;
        &self.defs.port_net[self.defs.port_start[d] as usize..self.defs.port_start[d + 1] as usize]
    }

    /// Returns the param entries of instance row `r` as a range into the param columns.
    pub fn params_of(&self, r: usize) -> std::ops::Range<usize> {
        self.insts.param_start[r] as usize..self.insts.param_start[r + 1] as usize
    }

    /// Counts leaf instances of the flattened hierarchy under definition `d` (ignored
    /// instances excluded). Cost: O(definitions + rows) with memoization.
    pub fn flat_leaves(&self, d: u32) -> u64 {
        let mut memo = vec![u64::MAX; self.defs.cell.len()];
        self.leaves(d, &mut memo)
    }

    fn leaves(&self, d: u32, memo: &mut [u64]) -> u64 {
        if memo[d as usize] != u64::MAX {
            return memo[d as usize];
        }
        let mut n = 0u64;
        for r in self.insts_of(d) {
            if self.insts.ignore[r] != Ignore::No {
                continue;
            }
            n += match self.insts.target[r] {
                Target::Leaf(_) => 1,
                Target::Def(c) => self.leaves(c, memo),
            };
        }
        memo[d as usize] = n;
        n
    }
}

/// Deepest hierarchy elaborated before `E014` is reported.
pub const DEPTH_MAX: u32 = 1024;

struct Config {
    inst: Vec<(String, String)>,
    cell: Vec<(usize, String)>,
    order: Vec<String>,
}

struct CellData<'p> {
    doc: &'p Document,
    resolved: bool,
    unresolved: Vec<Option<String>>,
    prepared: bool,
    inst_iface: Vec<u32>,
    graph: Graph,
    waivers: Vec<(ObjId, Name)>,
    defaults: Option<Vec<Val>>,
    default_key: Option<String>,
}

/// One instance row of a definition being built. Its connections and params are ranges
/// of the definition's [`RowBufs`] (elements of one instance share the params range).
struct RowTmp {
    name: Name,
    /// The name's first 8 bytes, big-endian, zero-padded: orders rows like their text
    /// wherever those bytes differ (the sort compares text only on a tie).
    prefix: u64,
    id: ObjId,
    target: Target,
    ignore: Ignore,
    conn: std::ops::Range<u32>,
    params: std::ops::Range<u32>,
}

/// The connection and param rows of one definition being built (flat, appended per
/// instance, so a row costs no allocation of its own).
#[derive(Default)]
struct RowBufs {
    conn: Vec<Name>,
    params: Vec<(Name, Name, Val, u8)>,
}

/// The first 8 bytes of `text` as a big-endian number (zero-padded).
fn name_prefix(text: &str) -> u64 {
    let mut b = [0u8; 8];
    let n = text.len().min(8);
    b[..n].copy_from_slice(&text.as_bytes()[..n]);
    u64::from_be_bytes(b)
}

struct Elaborator<'p, 'r, F> {
    project: &'p Project,
    top: DocHandle,
    req: &'r Request<'r>,
    funcs: &'r mut F,
    names: Names,
    ifaces: Vec<Iface>,
    cells: Vec<CellData<'p>>,
    nets: Vec<Option<CellNets>>,
    row_cell: HashMap<u32, u32>,
    config: Config,
    subst: Vec<(String, String)>,
    viol: Vec<Violation>,
    memo: HashMap<(u32, String), u32>,
    by_key: HashMap<(u32, String, String), u32>,
    impl_ids: HashMap<(u32, u32), u32>,
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
        config: Config {
            inst: Vec::new(),
            cell: Vec::new(),
            order: Vec::new(),
        },
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
    e.defs.param_start.push(0);
    e.defs.port_start.push(0);
    e.defs.inh_start.push(0);
    e.defs.inst_start.push(0);
    e.defs.code_start.push(0);
    e.insts.conn_start.push(0);
    e.insts.param_start.push(0);
    e.impls.body_start.push(0);

    // ---- request → config and corner
    let bench = doc.kind() == FileKind::Shbench;
    let config = req.config.or(if bench {
        doc.header_value("config")
    } else {
        None
    });
    let corner = req.corner.or(if bench {
        doc.header_value("corner")
    } else {
        None
    });
    let top_row = row_of(project, doc);
    let (top_name, top_stem) = match top_row {
        Some(r) => display(project, r),
        None => ("top".to_owned(), "top".to_owned()),
    };
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
    e.subst.extend(
        req.overrides
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
    );

    // ---- top cell and the hierarchy
    let row = top_row.map_or(NONE, |r| r as u32);
    let iface = Iface::from_doc(doc, &top_name, &top_stem, row, true, &mut e.names);
    e.push_cell(doc, iface);
    if let Some(r) = top_row {
        e.row_cell.insert(r as u32, 0);
    }
    e.discover();
    let top_tuple = e.defaults(0);
    let top_def = e.build_def(0, top_tuple, String::new(), 0);

    // ---- names, used flags, globals, bench lines
    e.name_defs();
    let mut out = Elab {
        top: top_def.unwrap_or(NONE),
        ..Elab::default()
    };
    let mut used = vec![false; e.defs.cell.len()];
    if let Some(t) = top_def {
        mark(&e.defs, &e.insts, t, &mut used);
    }
    e.defs.used = used;
    // Each distinct connection name is tested once (a bitset over the arena): a flat
    // design repeats few names among 10^6 connections.
    let mut seen = vec![0u64; (e.names.len() as usize).div_ceil(64)];
    let mut globals: Vec<Name> = Vec::new();
    for &n in e.insts.conn.iter().chain(&e.defs.port_net) {
        if n == Name::NONE {
            continue;
        }
        let (w, bit) = (n.0 as usize / 64, 1u64 << (n.0 % 64));
        if seen[w] & bit != 0 {
            continue;
        }
        seen[w] |= bit;
        if is_global(e.names.get(n)) || e.globals.binary_search(&n).is_ok() {
            globals.push(n);
        }
    }
    drop(seen);
    globals.sort_unstable_by(|a, b| e.names.cmp(*a, *b));
    out.bench = if bench {
        e.bench_lines(doc, corner_row)
    } else {
        Bench::default()
    };
    e.needs_open.sort_unstable();
    e.needs_open.dedup();
    out.violations = Violations::from_rows(std::mem::take(&mut e.viol), &e.names);
    out.globals = globals;
    out.needs_open = e.needs_open;
    out.cells = e.ifaces;
    out.nets = e.nets;
    out.defs = e.defs;
    out.insts = e.insts;
    out.impls = e.impls;
    out.names = e.names;
    assert_eq!(out.defs.inst_start.len(), out.defs.cell.len() + 1);
    assert_eq!(out.insts.conn_start.len(), out.insts.name.len() + 1);
    out
}

fn mark(defs: &Defs, insts: &Insts, d: u32, used: &mut [bool]) {
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

/// Returns the row of the record named `name` in a named bench table.
fn record(t: &schemify_doc::Records, doc: &Document, name: &str) -> Option<usize> {
    let s = doc.strings();
    (0..t.kind.len()).find(|&r| s.get(t.fields[r][0]) == name)
}

impl<'p, F: Funcs> Elaborator<'p, '_, F> {
    /// Expands `${NAME}` in `text` (project `set` entries, then [`Request::env`];
    /// `$$` is a literal `$`) and interns the result. An undefined or unclosed name is
    /// `E034` on `cell`, and the text stays as written.
    fn expand_vars(&mut self, cell: Name, text: &str) -> Name {
        if !text.contains('$') {
            return self.names.intern(text);
        }
        let sets = self.project.settings();
        let lookup = |n: &str| {
            sets.iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.as_str())
                .or_else(|| self.req.env.iter().find(|(k, _)| *k == n).map(|(_, v)| *v))
        };
        match expand_dollars(text, lookup) {
            Ok(out) => self.names.intern(&out),
            Err(name) => {
                let param = self.names.intern(&name);
                self.viol.push(Violation {
                    code: Code::E034,
                    cell,
                    object: ObjId::NONE,
                    path: Name::NONE,
                    param,
                    detail: format!("`${{{name}}}` is not defined or expands cyclically"),
                });
                self.names.intern(text)
            }
        }
    }

    fn request_finding(&mut self, cell: Name, detail: &str) {
        self.viol.push(Violation {
            code: Code::E035,
            cell,
            object: ObjId::NONE,
            path: Name::NONE,
            param: Name::NONE,
            detail: detail.to_owned(),
        });
    }

    fn read_config(&mut self, doc: &Document, name: &str, top_cell: Name) {
        let Some(r) = record(doc.configs(), doc, name) else {
            self.request_finding(top_cell, &format!("config `{name}` does not exist"));
            return;
        };
        let s = doc.strings();
        for (k, vals) in doc.pool().attrs(doc.configs().attrs[r]) {
            let v: Vec<&str> = vals.iter().map(|v| s.get(*v)).collect();
            match (s.get(k), v.as_slice()) {
                ("order", names) => self
                    .config
                    .order
                    .extend(names.iter().map(|n| (*n).to_owned())),
                ("inst", [path, imp]) => self
                    .config
                    .inst
                    .push(((*path).to_owned(), (*imp).to_owned())),
                ("cell", [cellref, imp]) => match self.project.resolve(self.top, cellref) {
                    Ok(row) => self.config.cell.push((row, (*imp).to_owned())),
                    Err(_) => self.viol.push(Violation {
                        code: Code::E001,
                        cell: top_cell,
                        object: ObjId::NONE,
                        path: Name::NONE,
                        param: self.names.intern(cellref),
                        detail: format!("config `{name}` names unknown cell `{cellref}`"),
                    }),
                },
                _ => {}
            }
        }
    }

    fn push_cell(&mut self, doc: &'p Document, iface: Iface) -> u32 {
        let ci = self.ifaces.len() as u32;
        assert!(ci < NONE - 1, "cell capacity");
        self.ifaces.push(iface);
        self.nets.push(None);
        self.cells.push(CellData {
            doc,
            resolved: false,
            unresolved: Vec::new(),
            prepared: false,
            inst_iface: Vec::new(),
            graph: Graph::default(),
            waivers: Vec::new(),
            defaults: None,
            default_key: None,
        });
        ci
    }

    fn cell_of_row(&mut self, row: usize) -> u32 {
        if let Some(&ci) = self.row_cell.get(&(row as u32)) {
            return ci;
        }
        let doc = self
            .project
            .interface(row)
            .expect("resolution checked the interface");
        let (cell, stem) = display(self.project, row);
        let open = is_open(self.project, row);
        let iface = Iface::from_doc(doc, &cell, &stem, row as u32, open, &mut self.names);
        let ci = self.push_cell(doc, iface);
        self.row_cell.insert(row as u32, ci);
        ci
    }

    /// Resolves cell `ci`'s instances to cells (creating their interfaces). Once per cell.
    fn resolve(&mut self, ci: u32) {
        if self.cells[ci as usize].resolved {
            return;
        }
        let doc = self.cells[ci as usize].doc;
        let s = doc.strings();
        let ni = doc.instances().id.len();
        let mut inst_iface = vec![NONE; ni];
        let mut unresolved: Vec<Option<String>> = vec![None; ni];
        let mut seen: HashMap<Sym, Result<u32, String>> = HashMap::new();
        for i in 0..ni {
            let sym = doc.instances().cell[i];
            let r = match seen.get(&sym) {
                Some(r) => r.clone(),
                None => {
                    let text = s.get(sym);
                    let row = if ci == 0 {
                        self.project
                            .resolve(self.top, text)
                            .map_err(|_| format!("cannot resolve `{text}`"))
                    } else {
                        resolve_in_row(self.project, self.ifaces[ci as usize].row as usize, text)
                    };
                    let r = row.map(|row| self.cell_of_row(row));
                    seen.insert(sym, r.clone());
                    r
                }
            };
            match r {
                Ok(c) => inst_iface[i] = c,
                Err(why) => unresolved[i] = Some(why),
            }
        }
        let cell = &mut self.cells[ci as usize];
        cell.inst_iface = inst_iface;
        cell.unresolved = unresolved;
        cell.resolved = true;
    }

    /// Resolves everything reachable from the top and collects the names of `global
    /// true` labels (spec §3: global anywhere in the elaborated hierarchy). Over every
    /// reachable schematic, selected or not: a superset, but path-independent.
    fn discover(&mut self) {
        let mut globals = Vec::new();
        let mut ci = 0;
        while ci < self.cells.len() {
            if self.ifaces[ci].open {
                self.resolve(ci as u32);
                crate::connect::global_labels(self.cells[ci].doc, &mut self.names, &mut globals);
            }
            ci += 1;
        }
        globals.sort_unstable();
        globals.dedup();
        self.globals = globals;
    }

    /// The request's var substitutions for cell `ci` (the top only), names interned.
    fn subst_of(&mut self, ci: u32) -> Vec<(Name, String)> {
        if ci != 0 {
            return Vec::new();
        }
        let subst = self.subst.clone();
        subst
            .into_iter()
            .map(|(a, b)| (self.names.intern(&a), b))
            .collect()
    }

    /// Evaluates cell `ci`'s graph for own-param values `own_vals` and gates the values
    /// (`E008`, D13), building the cell's connectivity and graph first, once per cell, with
    /// its context-free findings. Returns the values and their unverified flags.
    ///
    /// A large cell (`crate::PARALLEL_MIN` instances, native targets) builds its connectivity on a
    /// second thread while this one builds, evaluates and gates the graph (`funcs` stays
    /// here). The two share only the name arena: the graph side interns nothing and reads
    /// a snapshot, and every interning call happens in the serial order, so names, values
    /// and findings are the serial ones (D37: jobs = 1 vs N byte-identical).
    fn prepare_eval(&mut self, ci: u32, own_vals: &[Val]) -> (Vec<Val>, Vec<bool>) {
        let (mut vals, mut unv) = (Vec::new(), Vec::new());
        let mut gated = Vec::new();
        if self.cells[ci as usize].prepared {
            let cell = &self.cells[ci as usize];
            let ifaces = &self.ifaces;
            cell.graph.eval_all(
                Some(own_vals),
                &mut vals,
                &mut unv,
                ifaces,
                self.funcs,
                &mut self.ws,
            );
            let cx = CellCtx {
                doc: cell.doc,
                ifaces,
                own: ci,
                inst_iface: &cell.inst_iface,
                graph: &cell.graph,
                waivers: &cell.waivers,
                path: Name::NONE,
            };
            check_values(
                &cx,
                &vals,
                &self.names,
                self.funcs,
                &mut self.ws,
                &mut gated,
            );
            self.viol.append(&mut gated);
            return (vals, unv);
        }
        self.resolve(ci);
        let doc = self.cells[ci as usize].doc;
        let inst_iface = std::mem::take(&mut self.cells[ci as usize].inst_iface);
        let unresolved = std::mem::take(&mut self.cells[ci as usize].unresolved);
        let subst = self.subst_of(ci);
        let subst: Vec<(Name, &str)> = subst.iter().map(|(n, t)| (*n, t.as_str())).collect();
        let waiv = waivers_found(doc, &self.names);
        let mut found = Vec::new();
        let parallel = crate::parallel_ok(doc.instances().id.len());
        let Elaborator {
            names,
            ifaces,
            globals,
            funcs,
            ws,
            ..
        } = self;
        let (nets, graph) = if parallel {
            let snapshot = names.clone();
            std::thread::scope(|sc| {
                let worker = sc.spawn(|| {
                    let mut found = Vec::new();
                    let own = &ifaces[ci as usize];
                    let nets = connect(doc, own, ifaces, &inst_iface, globals, names, &mut found);
                    (nets, found)
                });
                let graph = Graph::build(doc, ci, ifaces, &inst_iface, &subst, &snapshot);
                graph.eval_all(Some(own_vals), &mut vals, &mut unv, ifaces, *funcs, ws);
                let cx = CellCtx {
                    doc,
                    ifaces,
                    own: ci,
                    inst_iface: &inst_iface,
                    graph: &graph,
                    waivers: &waiv,
                    path: Name::NONE,
                };
                check_values(&cx, &vals, &snapshot, *funcs, ws, &mut gated);
                let (nets, f) = worker.join().expect("connect does not panic");
                found = f;
                (nets, graph)
            })
        } else {
            let own = &ifaces[ci as usize];
            let nets = connect(doc, own, ifaces, &inst_iface, globals, names, &mut found);
            let graph = Graph::build(doc, ci, ifaces, &inst_iface, &subst, names);
            graph.eval_all(Some(own_vals), &mut vals, &mut unv, ifaces, *funcs, ws);
            let cx = CellCtx {
                doc,
                ifaces,
                own: ci,
                inst_iface: &inst_iface,
                graph: &graph,
                waivers: &waiv,
                path: Name::NONE,
            };
            check_values(&cx, &vals, names, *funcs, ws, &mut gated);
            (nets, graph)
        };
        {
            // Override keys are interned here, after connect, in both orders.
            let cx = CellCtx {
                doc,
                ifaces: &self.ifaces,
                own: ci,
                inst_iface: &inst_iface,
                graph: &graph,
                waivers: &waiv,
                path: Name::NONE,
            };
            check_overrides(&cx, &unresolved, &mut self.names, &mut found);
        }
        for p in &self.ifaces[ci as usize].too_wide {
            found.push(Violation {
                code: Code::E011,
                cell: self.ifaces[ci as usize].cell,
                object: ObjId::NONE,
                path: Name::NONE,
                param: *p,
                detail: "pin name wider than 65535 bits".to_owned(),
            });
        }
        // The serial order of findings: connectivity, overrides, wide pins, then gating.
        self.viol.append(&mut found);
        self.viol.append(&mut gated);
        self.nets[ci as usize] = Some(nets);
        let cell = &mut self.cells[ci as usize];
        cell.inst_iface = inst_iface;
        cell.graph = graph;
        cell.waivers = waiv;
        cell.prepared = true;
        assert_eq!(vals.len(), cell.graph.len());
        (vals, unv)
    }

    /// Default values of cell `ci`'s own params (its defaults evaluated in its own
    /// scope) and their tuple text. Evaluated on a params-and-vars-only graph.
    fn defaults(&mut self, ci: u32) -> Vec<Val> {
        if let Some(d) = &self.cells[ci as usize].defaults {
            return d.clone();
        }
        let doc = self.cells[ci as usize].doc;
        let none = vec![NONE; doc.instances().id.len()];
        let subst = self.subst_of(ci);
        let subst: Vec<(Name, &str)> = subst.iter().map(|(n, t)| (*n, t.as_str())).collect();
        // Own params and vars only: instance blocks are left out (`none`).
        let g = Graph::build(doc, ci, &self.ifaces, &none, &subst, &self.names);
        let (mut vals, mut unv) = (Vec::new(), Vec::new());
        g.eval_own(&mut vals, &mut unv, &self.ifaces, self.funcs, &mut self.ws);
        vals.truncate(g.params as usize);
        let (key, _) = tuple(&self.ifaces[ci as usize], &vals, &mut self.names);
        let cell = &mut self.cells[ci as usize];
        cell.defaults = Some(vals.clone());
        cell.default_key = Some(key);
        vals
    }

    /// Impl selection (spec §2) for the instance at `path` of cell `c`. Reports `E031`
    /// for config names the cell lacks and `E013` when nothing is selectable.
    fn select(&mut self, path: &str, c: u32) -> Option<u32> {
        let f = &self.ifaces[c as usize];
        let by_name =
            |name: &str, names: &Names| f.impl_name.iter().position(|n| names.get(*n) == name);
        let mut finding = None;
        let mut chosen = None;
        if let Some((_, imp)) = self.config.inst.iter().find(|(p, _)| p == path) {
            match by_name(imp, &self.names) {
                Some(k) => chosen = Some(k),
                None => finding = Some((imp.clone(), "inst")),
            }
        }
        let cell_rule = self.config.cell.iter().find(|(r, _)| *r as u32 == f.row);
        if let Some((_, imp)) = cell_rule.filter(|_| chosen.is_none()) {
            match by_name(imp, &self.names) {
                Some(k) => chosen = Some(k),
                None => finding = finding.or(Some((imp.clone(), "cell"))),
            }
        }
        if chosen.is_none() {
            chosen = self
                .config
                .order
                .iter()
                .find_map(|imp| by_name(imp, &self.names));
        }
        if chosen.is_none() && f.default_impl != NONE {
            chosen = Some(f.default_impl as usize);
        }
        let cell = f.cell;
        if let Some((imp, rule)) = finding {
            let param = self.names.intern(&imp);
            // A `cell` rule is wrong for every path; an `inst` rule for its own.
            let path = if rule == "inst" {
                self.names.intern(path)
            } else {
                Name::NONE
            };
            self.viol.push(Violation {
                code: Code::E031,
                cell,
                object: ObjId::NONE,
                path,
                param,
                detail: format!("config `{rule}` rule names impl `{imp}`, which the cell lacks"),
            });
        }
        if chosen.is_none() {
            self.viol.push(Violation {
                code: Code::E013,
                cell,
                object: ObjId::NONE,
                path: Name::NONE,
                param: Name::NONE,
                detail: "the cell has no selectable impl".to_owned(),
            });
        }
        chosen.map(|k| k as u32)
    }

    /// The leaf impl row for (cell `c`, impl `k`), checking dialect support (`E013`).
    fn leaf_impl(&mut self, c: u32, k: u32) -> Option<u32> {
        if let Some(&id) = self.impl_ids.get(&(c, k)) {
            return (id != NONE).then_some(id);
        }
        let f = &self.ifaces[c as usize];
        let d = self.req.dialect;
        let lang = self.names.get(f.impl_lang[k as usize]);
        let fmts = f.fmt_start[k as usize] as usize..f.fmt_start[k as usize + 1] as usize;
        let template = d.format_words().iter().find_map(|w| {
            fmts.clone()
                .find(|&j| self.names.get(f.fmt_dialect[j]) == *w)
                .map(|j| f.fmt_template[j])
        });
        // Spec §7.1 default templates: which langs have one for which dialect.
        let default_ok = matches!(
            (lang, d),
            ("spice", Dialect::Ngspice | Dialect::Xyce | Dialect::Cdl)
                | ("veriloga", Dialect::Ngspice)
                | ("verilog", Dialect::Ngspice | Dialect::Verilog)
        );
        if template.is_none() && !default_ok {
            let (cell, imp) = (f.cell, f.impl_name[k as usize]);
            let detail = format!(
                "impl `{}` ({lang}) has no format for {}",
                self.names.get(imp),
                d.as_str()
            );
            self.viol.push(Violation {
                code: Code::E013,
                cell,
                object: ObjId::NONE,
                path: Name::NONE,
                param: imp,
                detail,
            });
            self.impl_ids.insert((c, k), NONE);
            return None;
        }
        if let Some(t) = template
            && let Some(detail) = self.template_error(c, k, self.names.get(t))
        {
            // The netlister would write `* error in …` for every row: gate it here.
            let (cell, imp) = (f.cell, f.impl_name[k as usize]);
            self.viol.push(Violation {
                code: Code::E008,
                cell,
                object: ObjId::NONE,
                path: Name::NONE,
                param: imp,
                detail,
            });
        }
        let id = self.impls.cell.len() as u32;
        self.impls.cell.push(c);
        self.impls.index.push(k);
        self.impls.template.push(template.unwrap_or(Name::NONE));
        self.push_body(c, k);
        self.impl_ids.insert((c, k), id);
        Some(id)
    }

    /// Why impl `k` of cell `c` cannot render the template `src`: its parse error, or
    /// the first identifier that no builtin, pin or param provides, resolved as the
    /// netlister resolves them (spec §7.6.6). `None` when every row can render.
    fn template_error(&self, c: u32, k: u32, src: &str) -> Option<String> {
        let tmpl = match schemify_expr::Template::parse(src) {
            Err(err) => return Some(err.to_string()),
            Ok(t) => t,
        };
        let f = &self.ifaces[c as usize];
        let attr = |col: &[Name]| col.get(k as usize).is_some_and(|n| *n != Name::NONE);
        let param = |id: &str| f.param_name.iter().any(|p| self.names.get(*p) == id);
        let pin = |id: &str| {
            f.pin_name
                .iter()
                .any(|p| self.names.get(*p).split('[').next() == Some(id))
        };
        let missing = tmpl.program().idents().find(|&id| match id {
            "name" | "cell" | "pins" | "in_pins" | "out_pins" | "params" => false,
            "model" => !(attr(&f.impl_model) || param(id) || pin(id)),
            "file" => !attr(&f.impl_file),
            _ => !(pin(id) || param(id)),
        })?;
        Some(format!(
            "X004: unknown identifier `{missing}` in the template"
        ))
    }

    /// Appends impl `k`'s `body` lines of cell `c` (rank 5, so only in an open
    /// document) and closes the row's CSR range.
    fn push_body(&mut self, c: u32, k: u32) {
        assert_eq!(self.impls.body_start.len(), self.impls.cell.len());
        let doc = self.cells[c as usize].doc;
        let s = doc.strings();
        let f = &self.ifaces[c as usize];
        let imp_text = self.names.get(f.impl_name[k as usize]).to_owned();
        let bodies = doc.bodies();
        if let Some(r) = (0..bodies.kind.len()).find(|&r| s.get(bodies.fields[r][0]) == imp_text) {
            let cell = self.ifaces[c as usize].cell;
            for &l in &doc.pool().lines[bodies.verbatim[r].range()] {
                let line = self.expand_vars(cell, s.get(l));
                self.impls.body.push(line);
            }
        }
        self.impls.body_start.push(self.impls.body.len() as u32);
        assert_eq!(self.impls.body_start.len(), self.impls.cell.len() + 1);
    }

    /// The [`Impls`] row carrying the body of cell `c`'s schematic impl, `NONE` when no
    /// schematic impl has one (spec §7.6.8). Memoized like leaf rows.
    fn def_body(&mut self, c: u32) -> u32 {
        let f = &self.ifaces[c as usize];
        let doc = self.cells[c as usize].doc;
        let (s, bodies) = (doc.strings(), doc.bodies());
        let k = (0..f.impl_name.len()).find(|&k| {
            self.names.get(f.impl_lang[k]) == "schematic"
                && (0..bodies.kind.len())
                    .any(|r| s.get(bodies.fields[r][0]) == self.names.get(f.impl_name[k]))
        });
        let Some(k) = k.map(|k| k as u32) else {
            return NONE;
        };
        if let Some(&id) = self.impl_ids.get(&(c, k)) {
            return id;
        }
        let id = self.impls.cell.len() as u32;
        self.impls.cell.push(c);
        self.impls.index.push(k);
        self.impls.template.push(Name::NONE);
        self.push_body(c, k);
        self.impl_ids.insert((c, k), id);
        id
    }

    /// Closes the next [`Defs`] row's range of [`Defs::code`] with the `code` lines of cell
    /// `ci` for the dialect. Only a `.shcomp` contributes (D17 as amended): a bench top's
    /// code is [`Bench`]'s, and a primitive has none.
    fn push_def_code(&mut self, ci: u32) {
        assert_eq!(self.defs.code_start.len(), self.defs.cell.len());
        let doc = self.cells[ci as usize].doc;
        if doc.kind() == FileKind::Shcomp {
            let (s, codes) = (doc.strings(), doc.codes());
            let cell = self.ifaces[ci as usize].cell;
            for r in self.code_rows(doc, cell) {
                for &l in &doc.pool().lines[codes.verbatim[r].range()] {
                    let line = self.expand_vars(cell, s.get(l));
                    self.defs.code.push(line);
                }
            }
        }
        self.defs.code_start.push(self.defs.code.len() as u32);
        assert_eq!(self.defs.code_start.len(), self.defs.cell.len() + 1);
    }

    /// Rows of `doc`'s `code` table the dialect takes (spec §9.13), sorted by name. A
    /// block holding `.control` is left out for xyce and reported as `E033` on `cell`.
    fn code_rows(&mut self, doc: &Document, cell: Name) -> Vec<usize> {
        let (s, codes) = (doc.strings(), doc.codes());
        let mut rows: Vec<usize> = (0..codes.kind.len())
            .filter(|&r| self.req.dialect.takes_code(s.get(codes.fields[r][1])))
            .collect();
        rows.sort_by(|a, c| s.get(codes.fields[*a][0]).cmp(s.get(codes.fields[*c][0])));
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let control = self.req.dialect == Dialect::Xyce
                && doc.pool().lines[codes.verbatim[r].range()].iter().any(|l| {
                    s.get(*l)
                        .trim_start()
                        .to_ascii_lowercase()
                        .starts_with(".control")
                });
            if !control {
                out.push(r);
                continue;
            }
            let name = self.names.intern(s.get(codes.fields[r][0]));
            self.viol.push(Violation {
                code: Code::E033,
                cell,
                object: ObjId::NONE,
                path: Name::NONE,
                param: name,
                detail: "xyce has no .control blocks".to_owned(),
            });
        }
        assert!(out.len() <= codes.kind.len());
        out
    }

    /// Builds (or reuses) the definition of cell `ci` for own-param values `tuple` at
    /// instance path `path`. `None` when the cell cannot be elaborated here.
    fn build_def(&mut self, ci: u32, own_vals: Vec<Val>, path: String, depth: u32) -> Option<u32> {
        assert!(
            self.stack.len() as u32 == depth,
            "one stack entry per level"
        );
        let (key, pairs) = tuple(&self.ifaces[ci as usize], &own_vals, &mut self.names);
        let sensitive = self.config.inst.iter().any(|(p, _)| {
            p.len() > path.len() && p.starts_with(&path) && p.as_bytes()[path.len()] == b'/'
        });
        let known = self.memo.get(&(ci, key.clone())).filter(|_| !sensitive);
        if let Some(&d) = known {
            return Some(d);
        }
        if depth >= DEPTH_MAX {
            let p = self.names.intern(&path);
            self.viol.push(Violation {
                code: Code::E014,
                cell: self.ifaces[ci as usize].cell,
                object: ObjId::NONE,
                path: p,
                param: Name::NONE,
                detail: format!("hierarchy deeper than {DEPTH_MAX}"),
            });
            return None;
        }
        let (vals, _unverified) = self.prepare_eval(ci, &own_vals);
        self.stack.push(ci);
        let doc = self.cells[ci as usize].doc;
        let ni = doc.instances().id.len();
        let is_top = depth == 0;
        let mut rows: Vec<RowTmp> = Vec::with_capacity(ni);
        let mut bufs = RowBufs::default();
        {
            let nets = self.nets[ci as usize].as_ref().expect("prepared");
            bufs.conn.reserve(nets.conns.len());
            bufs.params.reserve(vals.len());
        }
        let mut propagated: Vec<(Name, Name)> = Vec::new();
        let mut text = String::new();
        let mut child_path = String::new();
        // Canonical text of recent scalar values (tag, bits) → name: a flat design repeats
        // a few values a million times; formatting and interning each was a tenth of it.
        let mut texts = [(0u8, 0u64, Name::NONE); 64];
        // Per child cell, memoized for the run of instances of one cell: whether rows need
        // their path, and the last (impl → leaf row) (`leaf_impl` memoizes too, by hash).
        let mut pathed_of = (NONE, false);
        let mut leaf_of = (NONE, NONE, None);
        for i in 0..ni {
            let c = self.cells[ci as usize].inst_iface[i];
            if c == NONE {
                continue;
            }
            let first = self.cells[ci as usize].graph.inst_first[i] as usize;
            let pc = self.ifaces[c as usize].param_name.len();
            if vals[first..first + pc].iter().any(Val::is_err) {
                continue; // E008 reported; nothing sound to instantiate
            }
            if self.cells[c as usize].defaults.is_none() {
                self.defaults(c); // computed once per cell, then borrowed below
            }
            let p0 = bufs.params.len() as u32;
            {
                let f = &self.ifaces[c as usize];
                let defaults = self.cells[c as usize].defaults.as_ref().expect("computed");
                let table = &self.cells[ci as usize].graph.table;
                for k in 0..pc {
                    let v = &vals[first + k];
                    let key = match v {
                        Val::Int(x) => (1, *x as u64),
                        Val::Num(x) => (2, x.to_bits()),
                        Val::Bool(b) => (3, u64::from(*b)),
                        _ => (0, 0),
                    };
                    let slot = (key.1.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 58) as usize;
                    let t = if key.0 != 0 && texts[slot].0 == key.0 && texts[slot].1 == key.1 {
                        texts[slot].2
                    } else {
                        text.clear();
                        v.write_text(&mut text);
                        let t = self.names.intern(&text);
                        if key.0 != 0 {
                            texts[slot] = (key.0, key.1, t);
                        }
                        t
                    };
                    debug_assert!(
                        self.names.get(t) == {
                            text.clear();
                            v.write_text(&mut text);
                            text.as_str()
                        }
                    );
                    let mut flags = 0;
                    if f.param_computed[k] {
                        flags |= COMPUTED;
                    }
                    if defaults[k] == *v {
                        flags |= DEFAULT;
                    }
                    if table[first + k] == OWN {
                        flags |= OVERRIDDEN;
                    }
                    bufs.params.push((f.param_name[k], t, v.clone(), flags));
                }
            }
            let params = p0..bufs.params.len() as u32;
            let nets = self.nets[ci as usize].as_ref().expect("prepared");
            let (e0, e1) = (nets.elem_start[i] as usize, nets.elem_start[i + 1] as usize);
            let ignore = nets.ignore[i];
            // The child path is read by config `inst` rules, by schematic children (their
            // own paths, E014) and by inherited pins (E036); a plain leaf row never needs it.
            let f = &self.ifaces[c as usize];
            if pathed_of.0 != c {
                let pathed = !self.config.inst.is_empty()
                    || f.pin_inherit.iter().any(|p| *p != Name::NONE)
                    || f.impl_lang
                        .iter()
                        .any(|l| self.names.get(*l) == "schematic");
                pathed_of = (c, pathed);
            }
            let pathed = pathed_of.1;
            for k in 0..e1 - e0 {
                let elem = self.nets[ci as usize].as_ref().expect("prepared").elem_name[e0 + k];
                child_path.clear();
                if pathed {
                    child_path.push_str(&path);
                    child_path.push('/');
                    child_path.push_str(self.names.get(elem));
                }
                let Some(imp) = self.select(&child_path, c) else {
                    continue;
                };
                let lang = self
                    .names
                    .get(self.ifaces[c as usize].impl_lang[imp as usize]);
                let target = if lang == "schematic" {
                    if !self.ifaces[c as usize].open {
                        self.needs_open.push(self.ifaces[c as usize].row);
                        continue;
                    }
                    if self.stack.contains(&c) {
                        let p = self.names.intern(&child_path);
                        let detail = format!(
                            "{} instantiates itself",
                            self.names.get(self.ifaces[c as usize].cell)
                        );
                        self.viol.push(Violation {
                            code: Code::E014,
                            cell: self.ifaces[ci as usize].cell,
                            object: doc.instances().id[i],
                            path: p,
                            param: Name::NONE,
                            detail,
                        });
                        continue;
                    }
                    let child_tuple = vals[first..first + pc].to_vec();
                    match self.build_def(c, child_tuple, child_path.clone(), depth + 1) {
                        Some(d) => Target::Def(d),
                        None => continue,
                    }
                } else {
                    if !self.ifaces[c as usize].open && self.ifaces[c as usize].row != NONE {
                        self.needs_open.push(self.ifaces[c as usize].row);
                    }
                    if (leaf_of.0, leaf_of.1) != (c, imp) {
                        leaf_of = (c, imp, self.leaf_impl(c, imp));
                    }
                    match leaf_of.2 {
                        Some(id) => Target::Leaf(id),
                        None => continue,
                    }
                };
                let c0 = bufs.conn.len();
                self.instance_conns(
                    ci,
                    i,
                    k,
                    target,
                    is_top,
                    &mut propagated,
                    &child_path,
                    &mut bufs.conn,
                );
                // The instance's own `ignore` wins; else the selected impl's.
                let ignore = match ignore {
                    Ignore::No => self.ifaces[c as usize].impl_ignore[imp as usize],
                    own => own,
                };
                if ignore == Ignore::Open {
                    bufs.conn[c0..].fill(Name::NONE);
                }
                rows.push(RowTmp {
                    name: elem,
                    prefix: name_prefix(self.names.get(elem)),
                    id: doc.instances().id[i],
                    target,
                    ignore,
                    conn: c0 as u32..bufs.conn.len() as u32,
                    params: params.clone(),
                });
            }
        }
        self.stack.pop();
        let def = self.finish_def(ci, key, pairs, sensitive, rows, bufs, propagated, is_top);
        Some(def)
    }

    /// Appends to `conn` the connection names of element `k` of instance row `i` in cell
    /// `ci`: ordinary pin bits, then the target's inherited pins (spec §5): a local
    /// `inherit` override, else (top) the default net, else the definition's own
    /// inherited port.
    #[allow(clippy::too_many_arguments)]
    fn instance_conns(
        &mut self,
        ci: u32,
        i: usize,
        k: usize,
        target: Target,
        is_top: bool,
        propagated: &mut Vec<(Name, Name)>,
        path: &str,
        conn: &mut Vec<Name>,
    ) {
        let c = self.cells[ci as usize].inst_iface[i];
        let nets = self.nets[ci as usize].as_ref().expect("prepared");
        let bits = self.ifaces[c as usize].bits_total as usize;
        let start = nets.conn_start[i] as usize + k * bits;
        conn.extend(nets.conns[start..start + bits].iter().map(|&b| {
            if b == NO_NET {
                Name::NONE
            } else {
                nets.bit_name[b as usize]
            }
        }));
        let none = match target {
            Target::Leaf(_) => self.ifaces[c as usize]
                .pin_inherit
                .iter()
                .all(|p| *p == Name::NONE),
            Target::Def(d) => {
                self.defs.inh_start[d as usize] == self.defs.inh_start[d as usize + 1]
            }
        };
        if none {
            return; // no inherited pins (the common case): nothing more to resolve
        }
        let inherited: Vec<(Name, Name)> = match target {
            Target::Leaf(_) => {
                let f = &self.ifaces[c as usize];
                (0..f.pin_name.len())
                    .filter(|&p| f.pin_inherit[p] != Name::NONE)
                    .map(|p| (f.pin_name[p], f.pin_inherit[p]))
                    .collect()
            }
            Target::Def(d) => {
                let r = self.defs.inh_start[d as usize] as usize
                    ..self.defs.inh_start[d as usize + 1] as usize;
                r.map(|j| (self.defs.inh_pin[j], self.defs.inh_default[j]))
                    .collect()
            }
        };
        let mut buf = String::new();
        let mut tmp = Vec::new();
        for (pin, default) in inherited {
            let nets = self.nets[ci as usize].as_ref().expect("prepared");
            let local = (0..nets.inh_inst.len())
                .find(|&j| nets.inh_inst[j] == i as u32 && nets.inh_pin[j] == pin);
            if let Some(j) = local {
                let r = nets.inh_bit_start[j] as usize..nets.inh_bit_start[j + 1] as usize;
                let bits: Vec<Name> = nets.inh_bits[r]
                    .iter()
                    .map(|&b| nets.bit_name[b as usize])
                    .collect();
                tmp.clear();
                let text = self.names.get(pin).to_owned();
                let _ = netexpr::expand(&text, &mut self.names, &mut tmp, &mut buf);
                if tmp.len() != bits.len() {
                    let (cell, id) = (
                        self.ifaces[ci as usize].cell,
                        self.cells[ci as usize].doc.instances().id[i],
                    );
                    let detail = format!(
                        "inherit `{text}` ({} bits) set to {} bits",
                        tmp.len(),
                        bits.len()
                    );
                    self.viol.push(Violation {
                        code: Code::E011,
                        cell,
                        object: id,
                        path: Name::NONE,
                        param: pin,
                        detail,
                    });
                }
                conn.extend(bits);
                continue;
            }
            let via = if is_top {
                default
            } else {
                match propagated.iter().find(|(p, _)| *p == pin) {
                    Some(&(_, d)) if d != default => {
                        let (cell, p) = (self.ifaces[ci as usize].cell, self.names.intern(path));
                        let detail = format!(
                            "inherited pin `{}` defaults to `{}` and `{}`",
                            self.names.get(pin),
                            self.names.get(d),
                            self.names.get(default)
                        );
                        self.viol.push(Violation {
                            code: Code::E036,
                            cell,
                            object: ObjId::NONE,
                            path: p,
                            param: pin,
                            detail,
                        });
                    }
                    Some(_) => {}
                    None => propagated.push((pin, default)),
                }
                pin
            };
            tmp.clear();
            let text = self.names.get(via).to_owned();
            let _ = netexpr::expand(&text, &mut self.names, &mut tmp, &mut buf);
            let nets = self.nets[ci as usize].as_ref().expect("prepared");
            conn.extend(
                tmp.iter()
                    .map(|&b| nets.bit(b).map_or(b, |n| nets.bit_name[n as usize])),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_def(
        &mut self,
        ci: u32,
        key: String,
        pairs: Vec<(Name, Name)>,
        sensitive: bool,
        rows: Vec<RowTmp>,
        mut bufs: RowBufs,
        propagated: Vec<(Name, Name)>,
        is_top: bool,
    ) -> u32 {
        let names = &self.names;
        // By name text (stable): a radix sort of (prefix, row) keys, then runs of equal
        // prefixes (names sharing their first 8 bytes) by text. The rows stay put and are
        // read in that order (no sorted copy of 10^6 rows).
        let keys: Vec<(u64, u32)> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| (r.prefix, i as u32))
            .collect();
        let mut keys = crate::connect::radix_sort(keys);
        let mut lo = 0;
        while lo < keys.len() {
            let hi = lo + keys[lo..].iter().take_while(|k| k.0 == keys[lo].0).count();
            if hi - lo > 1 {
                keys[lo..hi].sort_by(|a, b| {
                    names
                        .cmp(rows[a.1 as usize].name, rows[b.1 as usize].name)
                        .then(a.1.cmp(&b.1))
                });
            }
            lo = hi;
        }
        let order: Vec<u32> = keys.into_iter().map(|k| k.1).collect();
        debug_assert!(order.windows(2).all(|w| {
            let (a, b) = (&rows[w[0] as usize], &rows[w[1] as usize]);
            (a.prefix, names.get(a.name)) <= (b.prefix, names.get(b.name))
        }));
        // The children signature keys reuse of an identical definition; the top is never
        // instantiated, so it has nothing to be shared with (and the longest signature).
        let mut sig = String::new();
        for r in order.iter().map(|&i| &rows[i as usize]).filter(|_| !is_top) {
            sig.push_str(names.get(r.name));
            sig.push('=');
            let (tag, n) = match r.target {
                Target::Def(d) => ('d', d),
                Target::Leaf(l) => ('l', l),
            };
            sig.push(tag);
            crate::connect::push_decimal(n, &mut sig);
            sig.push(',');
        }
        let full = (ci, key.clone(), sig.clone());
        if let Some(&d) = self.by_key.get(&full) {
            if !sensitive {
                self.memo.insert((ci, key), d);
            }
            return d;
        }
        let d = self.defs.cell.len() as u32;
        for (name, text) in pairs {
            self.defs.param_name.push(name);
            self.defs.param_text.push(text);
        }
        // Ports: ordinary pins, declared inherited pins, propagated inherited pins.
        let f = &self.ifaces[ci as usize];
        let nets = self.nets[ci as usize].as_ref().expect("prepared");
        for p in 0..f.pin_name.len() {
            if f.pin_inherit[p] == Name::NONE && f.pin_alias[p] == NONE {
                self.defs
                    .port_net
                    .extend(nets.port(p).iter().map(|&b| nets.bit_name[b as usize]));
            }
        }
        for p in 0..f.pin_name.len() {
            if f.pin_inherit[p] != Name::NONE {
                self.defs
                    .port_net
                    .extend(nets.port(p).iter().map(|&b| nets.bit_name[b as usize]));
                self.defs.inh_pin.push(f.pin_name[p]);
                self.defs.inh_default.push(f.pin_inherit[p]);
            }
        }
        if !is_top {
            let mut extra: Vec<(Name, Name)> = propagated
                .into_iter()
                .filter(|(p, _)| {
                    !(0..f.pin_name.len())
                        .any(|q| f.pin_name[q] == *p && f.pin_inherit[q] != Name::NONE)
                })
                .collect();
            extra.sort_by(|a, b| self.names.cmp(a.0, b.0));
            let mut tmp = Vec::new();
            let mut buf = String::new();
            for (pin, default) in extra {
                tmp.clear();
                let text = self.names.get(pin).to_owned();
                let _ = netexpr::expand(&text, &mut self.names, &mut tmp, &mut buf);
                let nets = self.nets[ci as usize].as_ref().expect("prepared");
                self.defs.port_net.extend(
                    tmp.iter()
                        .map(|&b| nets.bit(b).map_or(b, |n| nets.bit_name[n as usize])),
                );
                self.defs.inh_pin.push(pin);
                self.defs.inh_default.push(default);
            }
        }
        if rows.iter().any(|r| r.ignore == Ignore::Short) {
            let port0 = *self.defs.port_start.last().expect("seeded") as usize;
            short_nets(
                &rows,
                &mut bufs.conn,
                &mut self.defs.port_net[port0..],
                &self.globals,
                &self.names,
            );
        }
        let body = self.def_body(ci);
        self.defs.body.push(body);
        self.defs.cell.push(ci);
        self.push_def_code(ci);
        self.defs.name.push(Name::NONE);
        self.defs.key.push(key.clone());
        self.defs.sig.push(sig);
        self.defs
            .param_start
            .push(self.defs.param_name.len() as u32);
        self.defs.port_start.push(self.defs.port_net.len() as u32);
        self.defs.inh_start.push(self.defs.inh_pin.len() as u32);
        let i = &mut self.insts;
        i.name.reserve(rows.len());
        i.id.reserve(rows.len());
        i.target.reserve(rows.len());
        i.ignore.reserve(rows.len());
        i.conn_start.reserve(rows.len());
        i.param_start.reserve(rows.len());
        i.conn.reserve(bufs.conn.len());
        i.param_name.reserve(bufs.params.len());
        i.param_text.reserve(bufs.params.len());
        i.param_val.reserve(bufs.params.len());
        i.param_flags.reserve(bufs.params.len());
        for r in order.iter().map(|&i| &rows[i as usize]) {
            self.insts.name.push(r.name);
            self.insts.id.push(r.id);
            self.insts.target.push(r.target);
            self.insts.ignore.push(r.ignore);
            let conn = r.conn.start as usize..r.conn.end as usize;
            self.insts.conn.extend_from_slice(&bufs.conn[conn]);
            self.insts.conn_start.push(self.insts.conn.len() as u32);
            for (name, text, val, flags) in
                &bufs.params[r.params.start as usize..r.params.end as usize]
            {
                self.insts.param_name.push(*name);
                self.insts.param_text.push(*text);
                self.insts.param_val.push(val.clone());
                self.insts.param_flags.push(*flags);
            }
            self.insts
                .param_start
                .push(self.insts.param_name.len() as u32);
        }
        self.defs.inst_start.push(self.insts.name.len() as u32);
        self.by_key.insert(full, d);
        if !sensitive {
            self.memo.insert((ci, key), d);
        }
        assert_eq!(self.defs.inst_start.len(), self.defs.cell.len() + 1);
        d
    }

    /// Names every definition (spec §6): `<cell>` for the default tuple, else
    /// `<cell>__<6 base32 of FNV-1a 64 of the tuple text>`. Definitions sharing a cell
    /// and tuple but differing in children (config `inst` rules) hash the tuple text
    /// plus `|` and their children's `name=target` list. Cells sharing a stem get
    /// `<stem>__<6 base32 of FNV-1a 64 of the display name>` as their base.
    fn name_defs(&mut self) {
        let nd = self.defs.cell.len();
        // Base names.
        let mut cells_used: Vec<u32> = self.defs.cell.clone();
        cells_used.sort_unstable();
        cells_used.dedup();
        let mut base: HashMap<u32, String> = HashMap::new();
        for &ci in &cells_used {
            let stem = self.names.get(self.ifaces[ci as usize].stem);
            let clash = cells_used
                .iter()
                .any(|&o| o != ci && self.names.get(self.ifaces[o as usize].stem) == stem);
            let mut b = stem.to_owned();
            if clash {
                b.push_str("__");
                push_base32(
                    fnv1a64(self.names.get(self.ifaces[ci as usize].cell).as_bytes()),
                    6,
                    &mut b,
                );
            }
            base.insert(ci, b);
        }
        // Group sizes per (cell, tuple).
        let mut group: HashMap<(u32, &str), u32> = HashMap::new();
        for d in 0..nd {
            *group
                .entry((self.defs.cell[d], self.defs.key[d].as_str()))
                .or_insert(0) += 1;
        }
        let extended: Vec<bool> = (0..nd)
            .map(|d| group[&(self.defs.cell[d], self.defs.key[d].as_str())] > 1)
            .collect();
        drop(group);
        let mut texts: Vec<String> = vec![String::new(); nd];
        for d in 0..nd {
            let ci = self.defs.cell[d];
            let _ = self.defaults(ci);
            let is_default = !extended[d]
                && self.cells[ci as usize].default_key.as_deref() == Some(&self.defs.key[d]);
            let mut name = base[&ci].clone();
            if !is_default {
                let mut text = self.defs.key[d].clone();
                if extended[d] {
                    text.push('|');
                    for r in self.defs.inst_start[d] as usize..self.defs.inst_start[d + 1] as usize
                    {
                        text.push_str(self.names.get(self.insts.name[r]));
                        text.push('=');
                        match self.insts.target[r] {
                            Target::Def(c) => {
                                text.push_str(self.names.get(self.defs.name[c as usize]))
                            }
                            Target::Leaf(l) => {
                                let f = &self.ifaces[self.impls.cell[l as usize] as usize];
                                text.push_str(self.names.get(f.cell));
                                text.push(':');
                                text.push_str(
                                    self.names
                                        .get(f.impl_name[self.impls.index[l as usize] as usize]),
                                );
                            }
                        }
                        text.push(',');
                    }
                }
                name.push_str("__");
                push_base32(fnv1a64(text.as_bytes()), 6, &mut name);
                texts[d] = text;
            }
            self.defs.name[d] = self.names.intern(&name);
        }
        // 30-bit hash collisions between different tuples: use all 13 digits.
        let mut order: Vec<usize> = (0..nd).collect();
        order.sort_by(|a, b| self.names.cmp(self.defs.name[*a], self.defs.name[*b]));
        for w in 0..order.len().saturating_sub(1) {
            let (a, b) = (order[w], order[w + 1]);
            if self.defs.name[a] == self.defs.name[b] {
                for d in [a, b] {
                    let ci = self.defs.cell[d];
                    let mut name = base[&ci].clone();
                    name.push_str("__");
                    push_base32(fnv1a64(texts[d].as_bytes()), 13, &mut name);
                    self.defs.name[d] = self.names.intern(&name);
                }
            }
        }
    }

    fn bench_lines(&mut self, doc: &Document, corner: Option<usize>) -> Bench {
        let mut b = Bench::default();
        let s = doc.strings();
        // Vars: the top graph's own nodes in the default context.
        let g = &self.cells[0].graph;
        let (mut vals, mut unv) = (Vec::new(), Vec::new());
        g.eval_own(&mut vals, &mut unv, &self.ifaces, self.funcs, &mut self.ws);
        let mut vars: Vec<(Name, Name)> = Vec::new();
        for v in 0..g.vars as usize {
            let val = &vals[g.params as usize + v];
            if val.is_err() {
                continue;
            }
            let mut text = String::new();
            val.write_text(&mut text);
            vars.push((g.var_name[v], self.names.intern(&text)));
        }
        vars.sort_by(|a, c| self.names.cmp(a.0, c.0));
        b.var_name = vars.iter().map(|v| v.0).collect();
        b.var_text = vars.iter().map(|v| v.1).collect();
        // Corner lines in file order.
        b.corner_start.push(0);
        if let Some(r) = corner {
            for (k, v) in doc.pool().attrs(doc.corners().attrs[r]) {
                let kind = match s.get(k) {
                    "lib" => CornerLine::Lib,
                    "include" => CornerLine::Include,
                    "temp" => CornerLine::Temp,
                    _ => continue,
                };
                if kind == CornerLine::Temp {
                    let text = s.get(v[0]);
                    let value = eval_in_bench(text, &b, &self.names);
                    match value {
                        Ok(t) => b.corner_args.push(self.names.intern(&t)),
                        Err(e) => {
                            self.viol.push(Violation {
                                code: Code::E008,
                                cell: self.ifaces[0].cell,
                                object: ObjId::NONE,
                                path: Name::NONE,
                                param: self.names.intern("temp"),
                                detail: format!("{}: {}", e.code.as_str(), e.msg),
                            });
                            continue;
                        }
                    }
                } else {
                    // The path expands `${NAME}`; a `lib` section word is literal.
                    for (j, &x) in v.iter().enumerate() {
                        let n = if j == 0 {
                            self.expand_vars(self.ifaces[0].cell, s.get(x))
                        } else {
                            self.names.intern(s.get(x))
                        };
                        b.corner_args.push(n);
                    }
                }
                b.corner_kind.push(kind);
                b.corner_start.push(b.corner_args.len() as u32);
            }
        }
        // Code blocks for the dialect, sorted by name; E033 for `.control` on xyce.
        let (codes, top) = (doc.codes(), self.ifaces[0].cell);
        b.code_start.push(0);
        for r in self.code_rows(doc, top) {
            let lines = &doc.pool().lines[codes.verbatim[r].range()];
            b.code_name
                .push(self.names.intern(s.get(codes.fields[r][0])));
            b.code_lang
                .push(self.names.intern(s.get(codes.fields[r][1])));
            for &l in lines {
                let n = self.expand_vars(top, s.get(l));
                b.code_lines.push(n);
            }
            b.code_start.push(b.code_lines.len() as u32);
        }
        b
    }
}

/// `ignore short` rows of one definition (spec §7.1): all nets of a shorted row become
/// one net, renamed everywhere in the definition to the set's best name — global,
/// then a port of the definition, then bytewise smallest. (An instance attr was
/// already merged by `connect`; this is what an impl-level `ignore short` needs.)
fn short_nets(
    rows: &[RowTmp],
    conns: &mut [Name],
    ports: &mut [Name],
    globals: &[Name],
    names: &Names,
) {
    let range = |r: &RowTmp| r.conn.start as usize..r.conn.end as usize;
    let mut ids: Vec<Name> = rows
        .iter()
        .flat_map(|r| conns[range(r)].iter().copied())
        .chain(ports.iter().copied())
        .filter(|n| *n != Name::NONE)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let idx = |n: Name| ids.binary_search(&n).expect("collected above") as u32;
    let mut parent: Vec<u32> = (0..ids.len() as u32).collect();
    for r in rows.iter().filter(|r| r.ignore == Ignore::Short) {
        let mut first = NONE;
        for &n in conns[range(r)].iter().filter(|n| **n != Name::NONE) {
            if first == NONE {
                first = idx(n);
            } else {
                crate::connect::union(&mut parent, first, idx(n));
            }
        }
    }
    let rank = |n: Name| -> u8 {
        if is_global(names.get(n)) || globals.binary_search(&n).is_ok() {
            0
        } else if ports.contains(&n) {
            1
        } else {
            2
        }
    };
    let mut best: Vec<Name> = ids.clone();
    for i in 0..ids.len() {
        let r = crate::connect::find(&mut parent, i as u32) as usize;
        let (a, b) = (ids[i], best[r]);
        if (rank(a), names.get(a)) < (rank(b), names.get(b)) {
            best[r] = a;
        }
    }
    let mut rename = |n: &mut Name| {
        if *n != Name::NONE {
            *n = best[crate::connect::find(&mut parent, idx(*n)) as usize];
        }
    };
    for r in rows {
        conns[range(r)].iter_mut().for_each(&mut rename);
    }
    ports.iter_mut().for_each(&mut rename);
}

/// `${NAME}` expansion (spec §7.1): `$$` → `$`, `${NAME}` → `lookup(NAME)`, any other
/// `$` literal. Nothing is executed.
///
/// # Errors
///
/// The first undefined name (or the unclosed `${…` text).
pub(crate) fn expand_dollars<'v>(
    text: &str,
    lookup: impl Fn(&str) -> Option<&'v str>,
) -> Result<String, String> {
    expand_depth(text, &lookup, 0)
}

/// Bound on nested `${NAME}` expansion; deeper (or cyclic) definitions are `E034`.
const EXPAND_DEPTH_MAX: u8 = 16;

/// Expands `text`, recursively expanding the values it substitutes (project `set`
/// values such as `${PDK_ROOT}/${PDK}/libs.tech` are themselves templates).
fn expand_depth<'v>(
    text: &str,
    lookup: &impl Fn(&str) -> Option<&'v str>,
    depth: u8,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("$$") {
            out.push('$');
            rest = after;
        } else if let Some(after) = tail.strip_prefix("${") {
            let end = after.find('}').ok_or_else(|| tail.to_owned())?;
            let name = &after[..end];
            let value = lookup(name).ok_or_else(|| name.to_owned())?;
            if depth >= EXPAND_DEPTH_MAX {
                return Err(name.to_owned());
            }
            out.push_str(&expand_depth(value, lookup, depth + 1)?);
            rest = &after[end + 1..];
        } else {
            out.push('$');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Evaluates a corner `temp` expression with the bench vars in scope.
fn eval_in_bench(text: &str, b: &Bench, names: &Names) -> Result<String, schemify_expr::Error> {
    let p = Program::compile(text, Mode::Scalar)?;
    let vals: Vec<Val> = b
        .var_text
        .iter()
        .map(|t| Val::parse_cached(names.get(*t)))
        .collect();
    let mut inputs = Vec::new();
    p.bind(&mut inputs, |n| {
        let i = b.var_name.iter().position(|v| names.get(*v) == n)?;
        vals[i].value()
    })?;
    let mut scratch = Scratch::default();
    let v: Value<'_> = p.eval(&inputs, &mut scratch)?;
    let mut out = String::new();
    Val::from_value(v).write_text(&mut out);
    Ok(out)
}

/// The canonical text of a param tuple (spec §6): non-computed params as `k=v`, sorted
/// bytewise by name, joined by single spaces; string values quoted with expression
/// string escapes so the text is unambiguous. Also returns the `(name, value text)`
/// pairs in that order.
pub(crate) fn tuple(f: &Iface, vals: &[Val], names: &mut Names) -> (String, Vec<(Name, Name)>) {
    let mut pairs: Vec<(Name, String, String)> = Vec::new();
    for k in 0..f.param_name.len().min(vals.len()) {
        if f.param_computed[k] {
            continue;
        }
        let (mut quoted, mut plain) = (String::new(), String::new());
        match &vals[k] {
            v if v.is_err() => plain.push('?'),
            v => v.write_text(&mut plain),
        }
        if let Val::Str(s) = &vals[k] {
            quoted.push('"');
            for ch in s.chars() {
                match ch {
                    '"' => quoted.push_str("\\\""),
                    '\\' => quoted.push_str("\\\\"),
                    '\n' => quoted.push_str("\\n"),
                    '\t' => quoted.push_str("\\t"),
                    c => quoted.push(c),
                }
            }
            quoted.push('"');
        } else {
            quoted.clone_from(&plain);
        }
        pairs.push((f.param_name[k], quoted, plain));
    }
    pairs.sort_by(|a, b| names.get(a.0).cmp(names.get(b.0)));
    let mut key = String::new();
    for (i, (k, v, _)) in pairs.iter().enumerate() {
        if i > 0 {
            key.push(' ');
        }
        key.push_str(names.get(*k));
        key.push('=');
        key.push_str(v);
    }
    let out = pairs
        .into_iter()
        .map(|(k, _, plain)| (k, names.intern(&plain)))
        .collect();
    (key, out)
}
