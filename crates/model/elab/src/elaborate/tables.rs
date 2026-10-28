//! The tables [`elaborate`](super::elaborate) writes and a netlister walks (netlist spec
//! §1–§7): definitions, their instance rows, leaf impls and the bench header, all as SoA
//! columns with CSR ranges, names as [`Name`]s into [`Elab::names`]. No `doc` access is
//! needed to read them.
//!
//! Six questions: in, nothing (filled by `elaborate`); out, read by `netlist`, `run`,
//! the editor and plugins. Up to 2^24 definition rows and their instance rows (`u32`
//! offsets); flags `u8`. Access: per definition, its instance rows in order. Lifetime: one
//! elaboration. Parallel: read-only once built.
//!
//! Contents:
//! - `pub enum Dialect`: `as_str`, `format_words`, `takes_code`
//! - `pub enum Target`
//! - `pub const COMPUTED`, `pub const DEFAULT`, `pub const OVERRIDDEN`
//! - `pub struct Defs`, `pub struct Insts`, `pub struct Impls`
//! - `pub enum CornerLine`, `pub struct Bench`
//! - `pub struct Elab`: `insts_of`, `conns`, `ports`, `params_of`, `flat_leaves`

use crate::iface::Iface;
use crate::names::{Name, Names};
use crate::nets::{CellNets, Ignore};
use crate::values::Val;
use crate::violations::Violations;
use schemify_doc::ObjId;

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
    /// ESPice: ngspice text, except that Verilog-A sources load with `.hdl` (netlist
    /// spec §7.2a) instead of OSDI, and `verilog` leaves have no default template.
    Espice,
    /// HSPICE (netlist only, §7.2b): SPICE text with quoted corner paths.
    Hspice,
    /// LTspice (§7.2b): SPICE text, no `.control` blocks, no Verilog-A.
    Ltspice,
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
    /// The tuple text of each row (spec §6), read by the definition naming.
    pub(super) key: Vec<String>,
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
    /// Per corner line: its path began with a `${NAME}` the project's `set` table expanded,
    /// so a relative result is relative to the project root, not to the bench (run spec §5).
    pub corner_project: Vec<bool>,
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

impl Dialect {
    /// Returns the dialect word (`ngspice`, …).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        [
            "ngspice", "xyce", "cdl", "spectre", "verilog", "espice", "hspice", "ltspice",
        ][self as usize]
    }

    /// `format <dialect>` words an impl may use for this dialect, most specific first
    /// (format spec §4: `spice` serves every SPICE dialect; netlist spec §7.2a:
    /// `format espice` overrides it where ESPice needs another line, e.g. `mfactor=`).
    #[must_use]
    pub fn format_words(self) -> &'static [&'static str] {
        match self {
            Dialect::Ngspice => &["spice"],
            Dialect::Espice => &["espice", "spice"],
            Dialect::Xyce => &["xyce", "spice"],
            Dialect::Hspice => &["hspice", "spice"],
            Dialect::Ltspice => &["ltspice", "spice"],
            Dialect::Cdl => &["cdl", "spice"],
            Dialect::Spectre => &["spectre"],
            Dialect::Verilog => &["verilog"],
        }
    }

    /// Returns whether a bench `code` block of `lang` is emitted (spec §7: `spice` for
    /// the SPICE simulators, else the dialect's own word; CDL takes none).
    #[must_use]
    pub fn takes_code(self, lang: &str) -> bool {
        match self {
            Dialect::Cdl => false,
            Dialect::Ngspice => matches!(lang, "spice" | "ngspice"),
            // ESPice reads ngspice decks: their code too.
            Dialect::Espice => matches!(lang, "spice" | "ngspice" | "espice"),
            Dialect::Xyce | Dialect::Hspice | Dialect::Ltspice => {
                lang == "spice" || lang == self.as_str()
            }
            Dialect::Spectre | Dialect::Verilog => lang == self.as_str(),
        }
    }
}

impl Elab {
    /// Returns the instance rows of definition `d`.
    ///
    /// # Panics
    ///
    /// Panics if `d` is not a definition row.
    #[must_use]
    pub fn insts_of(&self, d: u32) -> std::ops::Range<usize> {
        assert!((d as usize) < self.defs.cell.len(), "a definition row");
        self.defs.inst_start[d as usize] as usize..self.defs.inst_start[d as usize + 1] as usize
    }

    /// Returns the connection names of instance row `r`.
    ///
    /// # Panics
    ///
    /// Panics if `r` is not an instance row.
    #[must_use]
    pub fn conns(&self, r: usize) -> &[Name] {
        assert!(r < self.insts.name.len(), "an instance row");
        &self.insts.conn[self.insts.conn_start[r] as usize..self.insts.conn_start[r + 1] as usize]
    }

    /// Returns the port nets of definition `d`.
    ///
    /// # Panics
    ///
    /// Panics if `d` is not a definition row.
    #[must_use]
    pub fn ports(&self, d: u32) -> &[Name] {
        let d = d as usize;
        assert!(d < self.defs.cell.len(), "a definition row");
        &self.defs.port_net[self.defs.port_start[d] as usize..self.defs.port_start[d + 1] as usize]
    }

    /// Returns the param entries of instance row `r` as a range into the param columns.
    ///
    /// # Panics
    ///
    /// Panics if `r` is not an instance row.
    #[must_use]
    pub fn params_of(&self, r: usize) -> std::ops::Range<usize> {
        assert!(r < self.insts.name.len(), "an instance row");
        self.insts.param_start[r] as usize..self.insts.param_start[r + 1] as usize
    }

    /// Counts leaf instances of the flattened hierarchy under definition `d` (ignored
    /// instances excluded). Cost: O(definitions + rows) with memoization.
    #[must_use]
    pub fn flat_leaves(&self, d: u32) -> u64 {
        let mut memo = vec![u64::MAX; self.defs.cell.len()];
        self.leaves(d, &mut memo)
    }
}

// ---- private ----

impl Elab {
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
                Target::Def(c) => {
                    // Children are built before parents: the recursion descends.
                    assert!(c < d, "a child definition precedes its parent");
                    self.leaves(c, memo)
                }
            };
        }
        memo[d as usize] = n;
        n
    }
}
