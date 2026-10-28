//! The deck around the blocks (netlist spec §7.1–§7.6): elab's bench, globals, ports and
//! leaf impls in; the title, globals, setup lines, bench vars and code, definition ports
//! and parameter tuples, net respellings and once-only text (OSDI loads, models, impl
//! bodies) of a [`crate::Lowered`] out.
//!
//! Contents:
//! - `pub fn compiled_source(e: &Elab, l: usize) -> bool`
//! - `pub(crate) fn frame(e: &Elab, d: Dialect) -> Lowered`
//! - `pub(crate) fn cdl_params(e: &Elab, d: u32) -> Vec<ParamDef>`
//! - `pub(crate) fn ports(e: &Elab, d: u32, rs: &[(Name, String)]) -> Vec<Port>`
//! - `pub(crate) fn xspice_spellings(e: &Elab, plans: &Plans, d: Dialect) -> Vec<(Name, String)>`
//! - `pub(crate) fn once_only(l: &mut Lowered, e: &Elab, d: Dialect, uses: &Uses)`

use crate::inst::Uses;
use crate::plan::Plans;
use crate::{Lowered, SetupLine, spelled};
use schemify_elab::{CornerLine, Dialect, Elab, Ignore, Name, PinDir};
use spicerack::ir::{CircuitIR, ModelDef, ParamDef, Port, PortDirection, Subcircuit, Testbench};
use std::borrow::Cow;
use std::collections::HashSet;

/// `(name, kind, d_cosim simulation file)`: `.model <name> <kind> [simulation="…"]`.
type ModelLine = (String, String, Option<String>);

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
    let has_file = f.impl_file[k] != Name::NONE;
    match e.names.get(f.impl_lang[k]) {
        "veriloga" => {
            has_file
                || body(e, l).iter().any(|&n| {
                    let w = e.names.get(n).split_whitespace().next();
                    w.is_some_and(|w| w.starts_with("endmodule"))
                })
        }
        "verilog" => has_file || !body(e, l).is_empty(),
        _ => false,
    }
}

/// The deck around the blocks (spec §7.6.2–3): the title `<top cell> (schemify
/// <dialect>)`, the globals, the bench vars in name order (the top's parameters), the
/// corner lines in file order and the bench code (elab selected the code for the
/// dialect: none for CDL). SpiceRack leaves out what a dialect has no line for.
pub(crate) fn frame(e: &Elab, d: Dialect) -> Lowered {
    let b = &e.bench;
    assert_eq!(b.var_name.len(), b.var_text.len());
    // A non-bench top has empty bench tables (no CSR seed either).
    if !b.corner_kind.is_empty() {
        assert_eq!(b.corner_start.len(), b.corner_kind.len() + 1);
    }
    assert!(b.code_lines.is_empty() || b.code_start.len() == b.code_name.len() + 1);
    let text = |n: Name| e.names.get(n).to_owned();
    let setup = (0..b.corner_kind.len())
        .map(|k| {
            let args = &b.corner_args[b.corner_start[k] as usize..b.corner_start[k + 1] as usize];
            let arg = |i: usize| args.get(i).map_or(String::new(), |n| text(*n));
            match b.corner_kind[k] {
                CornerLine::Lib => SetupLine::Lib {
                    path: arg(0),
                    section: arg(1),
                },
                CornerLine::Include => SetupLine::Include { path: arg(0) },
                CornerLine::Temp => SetupLine::Temp { value: arg(0) },
            }
        })
        .collect();
    let code: Vec<String> = b.code_lines.iter().map(|l| text(*l)).collect();
    let parameters = (0..b.var_name.len())
        .map(|v| ParamDef {
            name: text(b.var_name[v]),
            default: Some(text(b.var_text[v])),
        })
        .collect();
    let top = e.cells.first().map_or("top", |c| e.names.get(c.cell));
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
        globals: e.globals.iter().map(|g| text(*g)).collect(),
        setup,
        ..Lowered::default()
    }
}

/// Definition `d`'s parameter tuple (spec §7.6.10): CDL has no `.param`, so instance text
/// that names a cell parameter (`R1 in out R`) resolves only from the `.SUBCKT` line, as
/// Virtuoso's auCdl writes it (`.SUBCKT rcfilter gnd in out R=1K C=1u`).
pub(crate) fn cdl_params(e: &Elab, d: u32) -> Vec<ParamDef> {
    let (a, b) = (
        e.defs.param_start[d as usize] as usize,
        e.defs.param_start[d as usize + 1] as usize,
    );
    assert!(a <= b);
    assert!(b <= e.defs.param_name.len());
    (a..b)
        .map(|k| ParamDef {
            name: e.names.get(e.defs.param_name[k]).to_owned(),
            default: Some(e.names.get(e.defs.param_text[k]).to_owned()),
        })
        .collect()
}

/// Definition `d`'s ports: their nets under the respellings `rs`, with directions (CDL
/// `*.PININFO`, Verilog declarations): ordinary pins bit by bit, then declared inherited
/// pins, then inherited pins propagated from the subtree (`inout`). Tolerates a port list
/// that disagrees with the pins (`E012` designs): missing directions are `inout`.
pub(crate) fn ports(e: &Elab, d: u32, rs: &[(Name, String)]) -> Vec<Port> {
    let f = &e.cells[e.defs.cell[d as usize] as usize];
    let nets = e.ports(d);
    let mut dirs = Vec::with_capacity(nets.len());
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
    dirs.resize(nets.len(), PortDirection::InOut);
    assert_eq!(dirs.len(), nets.len());
    nets.iter()
        .zip(dirs)
        .map(|(p, direction)| Port {
            name: spelled(e, rs, *p).to_owned(),
            direction,
        })
        .collect()
}

/// Respelled net names, sorted by [`Name`] (spec §7.2): ngspice's tokenizer splits
/// `d[0]` inside an XSPICE port vector `[ … ]`, so every net some emitted row writes there
/// is spelled with `[k]` → `_k` (`d[0]` → `d_0`) throughout the netlist; a respelling
/// that would equal another net's name gets `_` appended until it is unique. Empty for
/// other dialects. Cost: one pass over the emitted rows' connections.
pub(crate) fn xspice_spellings(e: &Elab, plans: &Plans, d: Dialect) -> Vec<(Name, String)> {
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
                let (a, w) = plan.slots[p];
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
    let mut taken: HashSet<Cow<'_, str>> = e
        .insts
        .conn
        .iter()
        .chain(&e.defs.port_net)
        .filter(|n| **n != Name::NONE)
        .map(|n| Cow::Borrowed(e.names.get(*n)))
        .collect();
    let mut out: Vec<(Name, String)> = Vec::with_capacity(hits.len());
    for n in hits {
        let mut s: String = e.names.get(n).replace('[', "_").replace(']', "");
        while taken.contains(s.as_str()) {
            s.push('_');
        }
        taken.insert(Cow::Owned(s.clone()));
        out.push((n, s));
    }
    out.sort_unstable_by_key(|p| p.0.0);
    assert!(out.windows(2).all(|w| w[0].0 != w[1].0));
    out
}

/// Once-only text (spec §7.1, §7.6.3, §7.6.8): Verilog-A loads (`<stem>.va`; SpiceRack
/// writes ngspice's OSDI load or espice's `.hdl`), the generated models, the impl bodies
/// by (cell, impl). Only impls some emitted row uses.
pub(crate) fn once_only(l: &mut Lowered, e: &Elab, d: Dialect, uses: &Uses) {
    assert_eq!(uses.impl_used.len(), e.impls.cell.len());
    let (mut osdi, mut models) = (Vec::new(), Vec::new());
    // OSDI (`.hdl` for espice) loads and models: ngspice and espice only (elab rejects
    // veriloga elsewhere, and verilog outside ngspice and the verilog dialect).
    if matches!(d, Dialect::Ngspice | Dialect::Espice) {
        loads_and_models(e, d, uses, &mut osdi, &mut models);
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
    l.bodies = bodies(e, d, uses);
}

// ---- private ----

/// The body lines of leaf impl row `l`.
fn body(e: &Elab, l: usize) -> &[Name] {
    &e.impls.body[e.impls.body_start[l] as usize..e.impls.body_start[l + 1] as usize]
}

/// The lang word of leaf impl row `l`.
fn lang(e: &Elab, l: usize) -> &str {
    let f = &e.cells[e.impls.cell[l] as usize];
    e.names.get(f.impl_lang[e.impls.index[l] as usize])
}

/// Collects the OSDI stems and `.model` lines of the used impls. A Verilog-A source (a
/// `file`, or a `body` that is source) is compiled by the runner: one OSDI load per stem
/// and a `.model` per model name (the impl's `model` attr, and each default-template
/// row's model). Any other `veriloga` impl is precompiled: its body carries that text.
fn loads_and_models(
    e: &Elab,
    d: Dialect,
    uses: &Uses,
    osdi: &mut Vec<String>,
    models: &mut Vec<ModelLine>,
) {
    assert!(osdi.is_empty());
    assert!(models.is_empty());
    let compiled_va = |l: usize| lang(e, l) == "veriloga" && compiled_source(e, l);
    // Every used Verilog-A source loads once; a model's impl is always a used one.
    let used = (0..e.impls.cell.len()).filter(|&l| uses.impl_used[l]);
    osdi.extend(used.clone().filter(|&l| compiled_va(l)).map(|l| stem(e, l)));
    let named = used.filter_map(|l| {
        let f = &e.cells[e.impls.cell[l] as usize];
        let m = f.impl_model[e.impls.index[l] as usize];
        (m != Name::NONE).then_some((m, l as u32))
    });
    for (m, l) in named.chain(uses.models.iter().copied()) {
        let l = l as usize;
        assert!(uses.impl_used[l], "a model's impl is used");
        let name = e.names.get(m).to_owned();
        match lang(e, l) {
            "veriloga" if compiled_source(e, l) => models.push((name, stem(e, l), None)),
            "verilog" if d == Dialect::Ngspice => {
                let sim = format!("\"{}.vvp\"", stem(e, l));
                models.push((name, "d_cosim".into(), Some(sim)));
            }
            _ => {}
        }
    }
}

/// The source stem of leaf impl row `l`: its `file`'s base name without extension, else
/// the cell stem.
fn stem(e: &Elab, l: usize) -> String {
    let f = &e.cells[e.impls.cell[l] as usize];
    match f.impl_file[e.impls.index[l] as usize] {
        Name::NONE => e.names.get(f.stem).to_owned(),
        file => {
            let base = e.names.get(file).rsplit(['/', '\\']).next().unwrap_or("");
            base.rsplit_once('.').map_or(base, |(s, _)| s).to_owned()
        }
    }
}

/// The impl bodies written once, by (cell, impl name) (spec §7.6.8): SPICE text for the
/// SPICE dialects (spectre wraps it in `lang=spice`): `spice` impls and, except for cdl,
/// precompiled Verilog-A ones and the schematic impls of emitted definitions (model
/// cards); Verilog source for verilog. A Verilog-A source is the runner's.
fn bodies(e: &Elab, d: Dialect, uses: &Uses) -> Vec<String> {
    let mut used = uses.impl_used.clone();
    for (def, &l) in e.defs.body.iter().enumerate() {
        if l != u32::MAX && e.defs.used[def] {
            used[l as usize] = true;
        }
    }
    let mut rows: Vec<usize> = (0..e.impls.cell.len())
        .filter(|&l| {
            let wanted = match (lang(e, l), d) {
                ("verilog", Dialect::Verilog) => true,
                (_, Dialect::Verilog) => false,
                ("spice", _) => true,
                // Simulation-only text (OSDI load, model card): not for LVS.
                ("veriloga", _) => !compiled_source(e, l) && d != Dialect::Cdl,
                ("schematic", _) => d != Dialect::Cdl,
                _ => false,
            };
            used[l] && wanted && !body(e, l).is_empty()
        })
        .collect();
    let key = |l: usize| {
        let f = &e.cells[e.impls.cell[l] as usize];
        (
            e.names.get(f.cell),
            e.names.get(f.impl_name[e.impls.index[l] as usize]),
        )
    };
    rows.sort_by(|a, b| key(*a).cmp(&key(*b)));
    let out: Vec<String> = rows
        .into_iter()
        .map(|l| {
            let mut text = String::new();
            for &line in body(e, l) {
                text.push_str(e.names.get(line));
                text.push('\n');
            }
            text
        })
        .collect();
    assert!(out.iter().all(|b| b.ends_with('\n')));
    out
}
