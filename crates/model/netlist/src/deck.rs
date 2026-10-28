//! The deck around the blocks (netlist spec §7.1–§7.6): elab's bench, globals, ports and
//! leaf impls in; the title, globals, setup lines, bench vars and code, definition ports
//! and parameter tuples, net respellings and once-only text (OSDI loads, models, impl
//! bodies) of a [`crate::Lowered`] out.
//!
//! Contents:
//! - `pub fn compiled_source(elab: &Elab, leaf: usize) -> bool`
//! - `pub(crate) fn frame(elab: &Elab, dialect: Dialect) -> Lowered`
//! - `pub(crate) fn cdl_params(elab: &Elab, def: u32) -> Vec<ParamDef>`
//! - `pub(crate) fn ports(elab: &Elab, def: u32, respellings: &[Respelling]) -> Vec<Port>`
//! - `pub(crate) fn xspice_spellings(elab: &Elab, plans: &Plans, dialect: Dialect) -> Vec<Respelling>`
//! - `pub(crate) fn once_only(lowered: &mut Lowered, elab: &Elab, dialect: Dialect, uses: &Uses)`

use crate::inst::{ModelUse, Uses};
use crate::plan::Plans;
use crate::{Lowered, Respelling, SetupLine};
use schemify_elab::{CornerLine, Dialect, Elab, Ignore, Name, PinDir};
use spicerack::ir::{CircuitIR, ModelDef, ParamDef, Port, PortDirection, Subcircuit, Testbench};
use std::borrow::Cow;
use std::collections::HashSet;
use std::iter;

/// One `.model <name> <kind> [simulation="…"]` line. Field order is the sort order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ModelLine {
    name: String,
    kind: String,
    /// The d_cosim simulation file.
    simulation: Option<String>,
}

/// Returns whether leaf impl row `leaf` of `elab` is source text the runner compiles (spec
/// §7.6.8, run spec §5): a `veriloga` or `verilog` impl with a `file`, a `verilog` impl
/// with a `body`, or a `veriloga` impl whose `body` has an `endmodule` line. A `veriloga`
/// body without one is precompiled SPICE text (model card, OSDI load) and is emitted as is.
///
/// # Panics
///
/// Panics if `leaf` is not a row of `elab.impls`.
#[must_use]
pub fn compiled_source(elab: &Elab, leaf: usize) -> bool {
    assert!(
        leaf < elab.impls.cell.len(),
        "impl row {leaf} of {}",
        elab.impls.cell.len()
    );
    assert_eq!(elab.impls.index.len(), elab.impls.cell.len());
    let iface = &elab.cells[elab.impls.cell[leaf] as usize];
    let impl_index = elab.impls.index[leaf] as usize;
    let has_file = iface.impl_file[impl_index] != Name::NONE;
    match elab.names.get(iface.impl_lang[impl_index]) {
        "veriloga" => {
            has_file
                || body(elab, leaf).iter().any(|&n| {
                    let word = elab.names.get(n).split_whitespace().next();
                    word.is_some_and(|w| w.starts_with("endmodule"))
                })
        }
        "verilog" => has_file || !body(elab, leaf).is_empty(),
        _ => false,
    }
}

/// The deck around the blocks (spec §7.6.2–3): the title `<top cell> (schemify
/// <dialect>)`, the globals, the bench vars in name order (the top's parameters), the
/// corner lines in file order and the bench code (elab selected the code for the
/// dialect: none for CDL). SpiceRack leaves out what a dialect has no line for.
pub(crate) fn frame(elab: &Elab, dialect: Dialect) -> Lowered {
    let bench = &elab.bench;
    assert_eq!(bench.var_name.len(), bench.var_text.len());
    // A non-bench top has empty bench tables (no CSR seed either).
    if !bench.corner_kind.is_empty() {
        assert_eq!(bench.corner_start.len(), bench.corner_kind.len() + 1);
    }
    assert!(bench.code_lines.is_empty() || bench.code_start.len() == bench.code_name.len() + 1);
    let text = |n: Name| elab.names.get(n).to_owned();
    let setup: Vec<SetupLine> = (0..bench.corner_kind.len())
        .map(|k| {
            let args = &bench.corner_args
                [bench.corner_start[k] as usize..bench.corner_start[k + 1] as usize];
            let arg = |i: usize| args.get(i).map_or(String::new(), |n| text(*n));
            match bench.corner_kind[k] {
                CornerLine::Lib => SetupLine::Lib {
                    path: arg(0),
                    section: arg(1),
                },
                CornerLine::Include => SetupLine::Include { path: arg(0) },
                CornerLine::Temp => SetupLine::Temp { value: arg(0) },
            }
        })
        .collect();
    let code: Vec<String> = bench.code_lines.iter().map(|l| text(*l)).collect();
    let parameters: Vec<ParamDef> = (0..bench.var_name.len())
        .map(|v| ParamDef {
            name: text(bench.var_name[v]),
            default: Some(text(bench.var_text[v])),
        })
        .collect();
    let globals: Vec<String> = elab.globals.iter().map(|g| text(*g)).collect();
    assert_eq!(setup.len(), bench.corner_kind.len());
    let top = elab.cells.first().map_or("top", |c| elab.names.get(c.cell));
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
        title: format!("{top} (schemify {})", dialect.as_str()),
        globals,
        setup,
        ..Lowered::default()
    }
}

/// Definition `def`'s parameter tuple (spec §7.6.10): CDL has no `.param`, so instance
/// text that names a cell parameter (`R1 in out R`) resolves only from the `.SUBCKT` line,
/// as Virtuoso's auCdl writes it (`.SUBCKT rcfilter gnd in out R=1K C=1u`).
pub(crate) fn cdl_params(elab: &Elab, def: u32) -> Vec<ParamDef> {
    let defs = &elab.defs;
    let (start, end) = (
        defs.param_start[def as usize] as usize,
        defs.param_start[def as usize + 1] as usize,
    );
    assert!(start <= end);
    assert!(end <= defs.param_name.len());
    (start..end)
        .map(|k| ParamDef {
            name: elab.names.get(defs.param_name[k]).to_owned(),
            default: Some(elab.names.get(defs.param_text[k]).to_owned()),
        })
        .collect()
}

/// Definition `def`'s ports: their nets under `respellings`, with directions (CDL
/// `*.PININFO`, Verilog declarations): ordinary pins bit by bit, then declared inherited
/// pins, then inherited pins propagated from the subtree (`inout`). Tolerates a port list
/// that disagrees with the pins (`E012` designs): missing directions are `inout`.
pub(crate) fn ports(elab: &Elab, def: u32, respellings: &[Respelling]) -> Vec<Port> {
    let iface = &elab.cells[elab.defs.cell[def as usize] as usize];
    assert_eq!(iface.pin_alias.len(), iface.pin_name.len());
    let nets = elab.ports(def);
    let mut directions = Vec::with_capacity(nets.len());
    for inherited in [false, true] {
        for p in 0..iface.pin_name.len() {
            let alias = iface.pin_alias[p] != u32::MAX;
            if (iface.pin_inherit[p] != Name::NONE) == inherited && !alias {
                let direction = match iface.pin_dir[p] {
                    PinDir::In => PortDirection::Input,
                    PinDir::Out => PortDirection::Output,
                    PinDir::Inout => PortDirection::InOut,
                };
                directions.extend(iter::repeat_n(direction, iface.bits(p).len()));
            }
        }
    }
    directions.resize(nets.len(), PortDirection::InOut);
    assert_eq!(directions.len(), nets.len());
    nets.iter()
        .zip(directions)
        .map(|(net, direction)| Port {
            name: crate::spelled(elab, respellings, *net).to_owned(),
            direction,
        })
        .collect()
}

/// Respelled net names, sorted by [`Name`] (spec §7.2): ngspice's tokenizer splits
/// `d[0]` inside an XSPICE port vector `[ … ]`, so every net some emitted row writes there
/// is spelled with `[k]` → `_k` (`d[0]` → `d_0`) throughout the netlist; a respelling
/// that would equal another net's name gets `_` appended until it is unique. Empty for
/// other dialects. Cost: one pass over the emitted rows' connections.
pub(crate) fn xspice_spellings(elab: &Elab, plans: &Plans, dialect: Dialect) -> Vec<Respelling> {
    if dialect != Dialect::Ngspice {
        return Vec::new();
    }
    let mut hits: Vec<Name> = Vec::new();
    // The emitted definitions: the used ones (elab marks the top used).
    let used = (0..elab.defs.cell.len() as u32).filter(|&def| elab.defs.used[def as usize]);
    for def in used {
        let emitted = elab
            .insts_of(def)
            .filter(|&r| elab.insts.ignore[r] == Ignore::No);
        for r in emitted {
            let plan = plans.of_row(elab, r);
            let conns = elab.conns(r);
            for p in 0..plan.vector.len() {
                if !plan.vector[p] {
                    continue;
                }
                let bits = conns.get(plan.slots[p].bits()).unwrap_or(&[]);
                let bus_bits = bits
                    .iter()
                    .filter(|n| **n != Name::NONE && elab.names.get(**n).contains('['));
                hits.extend(bus_bits);
            }
        }
    }
    elab.names.sort(&mut hits);
    hits.dedup();
    if hits.is_empty() {
        return Vec::new();
    }
    // Every net text of the netlist: a respelling must not merge two nets.
    let mut taken: HashSet<Cow<'_, str>> = elab
        .insts
        .conn
        .iter()
        .chain(&elab.defs.port_net)
        .filter(|n| **n != Name::NONE)
        .map(|n| Cow::Borrowed(elab.names.get(*n)))
        .collect();
    let mut out: Vec<Respelling> = Vec::with_capacity(hits.len());
    for net in hits {
        let mut text: String = elab.names.get(net).replace('[', "_").replace(']', "");
        // Each `_` steps past one taken name, so `taken.len()` steps always suffice.
        let mut appended = 0;
        while taken.contains(text.as_str()) {
            text.push('_');
            appended += 1;
            assert!(appended <= taken.len(), "a free spelling exists");
        }
        taken.insert(Cow::Owned(text.clone()));
        out.push(Respelling { net, text });
    }
    out.sort_unstable_by_key(|r| r.net.0);
    assert!(out.windows(2).all(|w| w[0].net != w[1].net));
    assert!(
        out.iter().all(|r| !r.text.contains('[')),
        "no respelling opens a port vector"
    );
    out
}

/// Once-only text (spec §7.1, §7.6.3, §7.6.8): Verilog-A loads (`<stem>.va`; SpiceRack
/// writes ngspice's OSDI load or espice's `.hdl`), the generated models, the impl bodies
/// by (cell, impl). Only impls some emitted row uses.
pub(crate) fn once_only(lowered: &mut Lowered, elab: &Elab, dialect: Dialect, uses: &Uses) {
    assert_eq!(uses.impl_used.len(), elab.impls.cell.len());
    assert!(lowered.bodies.is_empty(), "once-only text is written once");
    assert!(
        lowered.ir.top.models.is_empty(),
        "once-only text is written once"
    );
    let (mut osdi, mut models) = (Vec::new(), Vec::new());
    // OSDI (`.hdl` for espice) loads and models: ngspice and espice only (elab rejects
    // veriloga elsewhere, and verilog outside ngspice and the verilog dialect).
    if matches!(dialect, Dialect::Ngspice | Dialect::Espice) {
        loads_and_models(elab, dialect, uses, &mut osdi, &mut models);
    }
    osdi.sort();
    osdi.dedup();
    models.sort();
    models.dedup();
    let osdi_loads: Vec<String> = osdi.into_iter().map(|stem| format!("{stem}.va")).collect();
    let model_defs: Vec<ModelDef> = models
        .into_iter()
        .map(|line| ModelDef {
            name: line.name,
            kind: line.kind,
            parameters: line
                .simulation
                .map(|file| ("simulation".to_owned(), file))
                .into_iter()
                .collect(),
        })
        .collect();
    lowered.ir.top.osdi_loads = osdi_loads;
    lowered.ir.top.models = model_defs;
    lowered.bodies = bodies(elab, dialect, uses);
}

// ---- private ----

/// The body lines of leaf impl row `leaf`.
fn body(elab: &Elab, leaf: usize) -> &[Name] {
    let impls = &elab.impls;
    assert_eq!(impls.body_start.len(), impls.cell.len() + 1);
    &impls.body[impls.body_start[leaf] as usize..impls.body_start[leaf + 1] as usize]
}

/// The lang word of leaf impl row `leaf`.
fn lang(elab: &Elab, leaf: usize) -> &str {
    let iface = &elab.cells[elab.impls.cell[leaf] as usize];
    elab.names
        .get(iface.impl_lang[elab.impls.index[leaf] as usize])
}

/// Collects the OSDI stems and `.model` lines of the used impls. A Verilog-A source (a
/// `file`, or a `body` that is source) is compiled by the runner: one OSDI load per stem
/// and a `.model` per model name (the impl's `model` attr, and each default-template
/// row's model). Any other `veriloga` impl is precompiled: its body carries that text.
fn loads_and_models(
    elab: &Elab,
    dialect: Dialect,
    uses: &Uses,
    osdi: &mut Vec<String>,
    models: &mut Vec<ModelLine>,
) {
    assert!(osdi.is_empty());
    assert!(models.is_empty());
    let compiled_va = |leaf: usize| lang(elab, leaf) == "veriloga" && compiled_source(elab, leaf);
    // Every used Verilog-A source loads once; a model's impl is always a used one.
    let used = (0..elab.impls.cell.len()).filter(|&leaf| uses.impl_used[leaf]);
    let sources = used.clone().filter(|&leaf| compiled_va(leaf));
    osdi.extend(sources.map(|leaf| stem(elab, leaf)));
    let named = used.filter_map(|leaf| {
        let iface = &elab.cells[elab.impls.cell[leaf] as usize];
        let model = iface.impl_model[elab.impls.index[leaf] as usize];
        (model != Name::NONE).then_some(ModelUse {
            model,
            leaf: leaf as u32,
        })
    });
    let all = named.chain(uses.models.iter().copied());
    for model_use in all {
        let leaf = model_use.leaf as usize;
        assert!(uses.impl_used[leaf], "a model's impl is used");
        let name = elab.names.get(model_use.model).to_owned();
        match lang(elab, leaf) {
            "veriloga" if compiled_source(elab, leaf) => models.push(ModelLine {
                name,
                kind: stem(elab, leaf),
                simulation: None,
            }),
            "verilog" if dialect == Dialect::Ngspice => {
                let simulation = format!("\"{}.vvp\"", stem(elab, leaf));
                models.push(ModelLine {
                    name,
                    kind: "d_cosim".to_owned(),
                    simulation: Some(simulation),
                });
            }
            _ => {}
        }
    }
}

/// The source stem of leaf impl row `leaf`: its `file`'s base name without extension,
/// else the cell stem.
fn stem(elab: &Elab, leaf: usize) -> String {
    let iface = &elab.cells[elab.impls.cell[leaf] as usize];
    match iface.impl_file[elab.impls.index[leaf] as usize] {
        Name::NONE => elab.names.get(iface.stem).to_owned(),
        file => {
            let base = elab
                .names
                .get(file)
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("");
            let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
            assert!(!stem.contains(['/', '\\']), "a base name has no directory");
            stem.to_owned()
        }
    }
}

/// The impl bodies written once, by (cell, impl name) (spec §7.6.8): SPICE text for the
/// SPICE dialects (spectre wraps it in `lang=spice`): `spice` impls and, except for cdl,
/// precompiled Verilog-A ones and the schematic impls of emitted definitions (model
/// cards); Verilog source for verilog. A Verilog-A source is the runner's.
fn bodies(elab: &Elab, dialect: Dialect, uses: &Uses) -> Vec<String> {
    assert_eq!(elab.defs.body.len(), elab.defs.used.len());
    let mut used = uses.impl_used.clone();
    for (def, &leaf) in elab.defs.body.iter().enumerate() {
        if leaf != u32::MAX && elab.defs.used[def] {
            used[leaf as usize] = true;
        }
    }
    let mut rows: Vec<usize> = (0..elab.impls.cell.len())
        .filter(|&leaf| {
            let wanted = match (lang(elab, leaf), dialect) {
                ("verilog", Dialect::Verilog) => true,
                (_, Dialect::Verilog) => false,
                ("spice", _) => true,
                // Simulation-only text (OSDI load, model card): not for LVS.
                ("veriloga", _) => !compiled_source(elab, leaf) && dialect != Dialect::Cdl,
                ("schematic", _) => dialect != Dialect::Cdl,
                _ => false,
            };
            used[leaf] && wanted && !body(elab, leaf).is_empty()
        })
        .collect();
    let key = |leaf: usize| {
        let iface = &elab.cells[elab.impls.cell[leaf] as usize];
        (
            elab.names.get(iface.cell),
            elab.names
                .get(iface.impl_name[elab.impls.index[leaf] as usize]),
        )
    };
    rows.sort_by(|a, b| key(*a).cmp(&key(*b)));
    let mut out: Vec<String> = Vec::with_capacity(rows.len());
    for leaf in rows {
        let mut text = String::new();
        for &line in body(elab, leaf) {
            text.push_str(elab.names.get(line));
            text.push('\n');
        }
        out.push(text);
    }
    assert!(out.iter().all(|b| b.ends_with('\n')));
    out
}
