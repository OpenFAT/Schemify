//! Netlists (D22): one [`schemify_elab::Elab`] in, one [`Netlist`] out: elab's tables
//! lowered to SpiceRack's circuit IR, written by SpiceRack's xschem-layout codegen in any
//! [`Dialect`], plus the [`NameMap`] between hierarchical paths and emitted names.
//!
//! Reads elab's tables only, so every dialect sees the same resolved values. Pure (no I/O,
//! no clock, no environment) and deterministic (D37): one elaboration gives the same bytes
//! on every platform. Cites `docs/spec/netlist.md` §7 (decisions in §7.6).
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
//! use schemify_elab::{Dialect, Request};
//!
//! let mut p = Project::new(None, None, 1).expect("an empty project");
//! let res = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n";
//! let bench = "schemify 1 shbench\n\ninst 0aaaaa ./res\n  at 0 0 R0\n  name 1\n  R 2k\n\nlabel 0bbbbb 0 0 R0 in\nlabel 0ccccc 0 20 R0 0\n";
//! p.index_library("", "", &[("res.shprim", res.as_bytes()), ("tb.shbench", bench.as_bytes())])
//!     .expect("the library indexes");
//! p.open("", "res.shprim", res.as_bytes(), None).expect("the cell opens");
//! let top = p.open("", "tb.shbench", bench.as_bytes(), None).expect("the bench opens").handle;
//! let req = Request { config: None, corner: None, overrides: &[], env: &[], dialect: Dialect::Ngspice };
//! let e = schemify_elab::elaborate(&p, top, &req, &mut schemify_elab::no_funcs);
//! assert!(!e.violations.blocking());
//! let n = schemify_netlist::emit(&e, Dialect::Ngspice);
//! assert_eq!(n.text, "* tb (schemify ngspice)\nR1 in 0 2000\n.end\n");
//! assert_eq!(n.name_map.inst_name("/1").as_deref(), Some("R1"));
//! assert_eq!(n.name_map.inst_path("r1").as_deref(), Some("/1"));
//! ```
//!
//! Contents:
//! - `pub struct Netlist`, `pub struct Lowered`
//! - `pub fn emit(elab: &Elab, dialect: Dialect) -> Netlist`
//! - `pub fn lower(elab: &Elab, dialect: Dialect) -> Lowered`
//! - re-exported: `emit_flat` (flat.rs), `compiled_source` (deck.rs), `set_serial`
//!   (block.rs), `NameMap` (name_map.rs), SpiceRack's `Lines`, `SetupLine`, `CircuitIR`
//! - crate-wide: `const NONE`, `struct Respelling`,
//!   `fn write(lowered: &mut Lowered, dialect: Dialect) -> String`,
//!   `fn spicerack_dialect(dialect: Dialect) -> SpiceRackDialect`,
//!   `fn is_spice(dialect: Dialect) -> bool`,
//!   `fn push_name(out: &mut String, name: &str, dialect: Dialect)`,
//!   `fn net_text(elab: &Elab, net: Name) -> &str`,
//!   `fn spelled<'elab>(elab: &'elab Elab, respellings: &'elab [Respelling], net: Name) -> &'elab str`

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
// 6. Parallel: a large block's rows render in chunks on scoped threads (`block.rs`),
//    appended in row order; the name map stays sequential.

mod block;
mod deck;
mod flat;
mod inst;
mod name_map;
mod plan;

pub use block::set_serial;
pub use deck::compiled_source;
pub use flat::emit_flat;
pub use name_map::NameMap;
pub use spicerack::codegen::xschem::{Lines, SetupLine};
pub use spicerack::ir::CircuitIR;

use block::Writer;
use inst::Context;
use plan::Plans;
use schemify_elab::{Dialect, Elab, Name};
use spicerack::codegen::Dialect as SpiceRackDialect;
use spicerack::codegen::xschem;
use spicerack::codegen::xschem::Deck;
use std::mem;

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

/// One net written under another name (spec §7.2: an XSPICE bus bit `d[0]` is `d_0`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Respelling {
    pub(crate) net: Name,
    /// Never empty.
    pub(crate) text: String,
}

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
#[must_use]
pub fn emit(elab: &Elab, dialect: Dialect) -> Netlist {
    let mut lowered = lower(elab, dialect);
    let text = write(&mut lowered, dialect);
    // The title `frame` wrote is the deck's first line.
    assert!(
        text.starts_with(xschem::comment(spicerack_dialect(dialect))),
        "the deck opens with its title comment"
    );
    Netlist {
        text,
        name_map: lowered.name_map,
    }
}

/// Lowers `elab` to SpiceRack's IR for `dialect` (spec §7.6.0): every row rendered as a
/// packed element line, with the name map built from the element names the deck sorts
/// and writes.
///
/// # Panics
///
/// As [`emit`].
#[must_use]
pub fn lower(elab: &Elab, dialect: Dialect) -> Lowered {
    assert_eq!(elab.defs.inst_start.len(), elab.defs.cell.len() + 1);
    assert_eq!(elab.defs.code_start.len(), elab.defs.cell.len() + 1);
    assert_eq!(elab.insts.conn_start.len(), elab.insts.name.len() + 1);
    assert!(
        elab.top == NONE || (elab.top as usize) < elab.defs.cell.len(),
        "the top is a definition row or NONE"
    );
    let plans = Plans::new(elab, dialect);
    let order = defs_by_name(elab);
    let context = Context {
        elab,
        dialect,
        plans: &plans,
    };
    let mut writer = Writer::new(context, &order);
    let mut lowered = deck::frame(elab, dialect);
    lowered.lines.reserve_exact(order.len() + 1);
    lowered.nets.reserve_exact(order.len() + 1);
    // The top block: a bench's vars stay the top's parameters; CDL's top takes its tuple.
    if elab.top == NONE {
        lowered.lines.push(Vec::new());
        lowered.nets.push(Vec::new());
    } else {
        let vars = mem::take(&mut lowered.ir.top.parameters);
        let top = writer.def_block(elab.top);
        lowered.ir.top = top.subcircuit;
        if dialect != Dialect::Cdl {
            lowered.ir.top.parameters = vars;
        }
        lowered.lines.push(top.lines);
        lowered.nets.push(top.nets);
    }
    for &def in &order {
        let block = writer.def_block(def);
        lowered.ir.subcircuit_defs.push(block.subcircuit);
        lowered.lines.push(block.lines);
        lowered.nets.push(block.nets);
    }
    let (uses, name_map) = writer.finish();
    deck::once_only(&mut lowered, elab, dialect, &uses);
    lowered.name_map = name_map;
    assert_eq!(lowered.ir.subcircuit_defs.len(), order.len());
    assert_eq!(lowered.lines.len(), order.len() + 1);
    assert_eq!(lowered.nets.len(), lowered.lines.len());
    lowered
}

/// SpiceRack writes `lowered` (its title, globals, setup, bodies, lines and nets are
/// moved into the deck; the IR and name map stay).
pub(crate) fn write(lowered: &mut Lowered, dialect: Dialect) -> String {
    let blocks = lowered.ir.subcircuit_defs.len() + 1;
    assert!(
        lowered.lines.len() <= blocks,
        "line runs of the top and each definition"
    );
    assert!(
        lowered.nets.len() <= blocks,
        "wires of the top and each definition"
    );
    assert!(!lowered.title.is_empty(), "`deck::frame` names the deck");
    let deck = Deck {
        ir: &lowered.ir,
        title: Some(mem::take(&mut lowered.title)),
        globals: mem::take(&mut lowered.globals),
        setup: mem::take(&mut lowered.setup),
        bodies: mem::take(&mut lowered.bodies),
        lines: mem::take(&mut lowered.lines),
        nets: mem::take(&mut lowered.nets),
    };
    let text = xschem::write(&deck, spicerack_dialect(dialect))
        .expect("every dialect has an xschem layout");
    assert!(text.ends_with('\n'));
    text
}

/// SpiceRack's dialect of `dialect`.
pub(crate) fn spicerack_dialect(dialect: Dialect) -> SpiceRackDialect {
    match dialect {
        Dialect::Ngspice => SpiceRackDialect::Ngspice,
        Dialect::Espice => SpiceRackDialect::Espice,
        Dialect::Xyce => SpiceRackDialect::Xyce,
        Dialect::Cdl => SpiceRackDialect::Cdl,
        Dialect::Spectre => SpiceRackDialect::Spectre,
        Dialect::Verilog => SpiceRackDialect::Verilog,
        Dialect::Hspice => SpiceRackDialect::Hspice,
        Dialect::Ltspice => SpiceRackDialect::Ltspice,
    }
}

/// Whether `dialect` is a SPICE-syntax dialect: verbatim names, `*` comments, `.subckt`
/// blocks, case-insensitive names.
pub(crate) fn is_spice(dialect: Dialect) -> bool {
    xschem::is_spice(spicerack_dialect(dialect))
}

/// Appends `name` as the dialect spells an identifier (SpiceRack's `push_ident`: SPICE
/// verbatim, spectre `\`-escaped, verilog escaped identifiers).
#[inline]
pub(crate) fn push_name(out: &mut String, name: &str, dialect: Dialect) {
    let start = out.len();
    xschem::push_ident(out, name, spicerack_dialect(dialect));
    debug_assert!(
        out.len() >= start + name.len(),
        "escaping only adds characters"
    );
}

/// The text of net `net`; `?` for an unconnected bit ([`Name::NONE`]).
pub(crate) fn net_text(elab: &Elab, net: Name) -> &str {
    if net == Name::NONE {
        "?"
    } else {
        elab.names.get(net)
    }
}

/// The emitted text of net `net` under `respellings` (sorted by [`Name`], spec §7.2).
pub(crate) fn spelled<'elab>(
    elab: &'elab Elab,
    respellings: &'elab [Respelling],
    net: Name,
) -> &'elab str {
    match respellings.binary_search_by_key(&net.0, |r| r.net.0) {
        Ok(i) => {
            debug_assert!(!respellings[i].text.is_empty(), "a respelling has text");
            &respellings[i].text
        }
        Err(_) => net_text(elab, net),
    }
}

// ---- private ----

/// Used definitions other than the top, sorted by subckt name (spec §7.1).
fn defs_by_name(elab: &Elab) -> Vec<u32> {
    assert_eq!(elab.defs.used.len(), elab.defs.cell.len());
    assert_eq!(elab.defs.name.len(), elab.defs.cell.len());
    let used = (0..elab.defs.cell.len() as u32)
        .filter(|&def| elab.defs.used[def as usize] && def != elab.top);
    let mut order: Vec<(Name, u32)> = used
        .map(|def| (elab.defs.name[def as usize], def))
        .collect();
    elab.names.sort_by_text(&mut order);
    assert!(order.len() < elab.defs.cell.len().max(1));
    let defs: Vec<u32> = order.into_iter().map(|(_, def)| def).collect();
    assert!(
        !defs.contains(&elab.top),
        "the top is written first, not by name"
    );
    defs
}
