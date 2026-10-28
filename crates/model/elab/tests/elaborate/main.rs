//! Hierarchy elaboration (netlist spec §1, §2, §5, §6): impl-selection precedence,
//! specialization naming and its stability, recursion, inherited connections, param
//! scopes, bench lines, dialect support, and D20 cached values.

#[path = "../common/mod.rs"]
mod common;
mod hierarchy;
mod params;
mod selection;

use common::project;
use schemify_elab::{Dialect, Elab, Request, Target, elaborate, no_funcs};

/// Row of instance `name` in definition `d`.
fn row(e: &Elab, d: u32, name: &str) -> usize {
    e.insts_of(d)
        .find(|&r| e.names.get(e.insts.name[r]) == name)
        .unwrap_or_else(|| panic!("no {name} in def {d}"))
}

/// What row `r` instantiates: `def:<name>` or `<impl name>`.
fn target(e: &Elab, r: usize) -> String {
    match e.insts.target[r] {
        Target::Def(d) => format!("def:{}", e.names.get(e.defs.name[d as usize])),
        Target::Leaf(l) => {
            let f = &e.cells[e.impls.cell[l as usize] as usize];
            e.names
                .get(f.impl_name[e.impls.index[l as usize] as usize])
                .to_owned()
        }
    }
}

fn conn_names(e: &Elab, r: usize) -> Vec<&str> {
    e.conns(r).iter().map(|n| e.names.get(*n)).collect()
}

fn param(e: &Elab, r: usize, name: &str) -> String {
    let k = e
        .params_of(r)
        .find(|&k| e.names.get(e.insts.param_name[k]) == name)
        .expect("the instance has that param");
    e.names.get(e.insts.param_text[k]).to_owned()
}

const RES: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n  min 1\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n";

/// `amp`: one schematic impl (a resistor between IN and OUT whose value is `r`) and two
/// leaf impls.
const AMP: &str = "schemify 1 shcomp\n  default sch\n\npin IN in 0 0\npin OUT out 0 20\n\nparam r 1k\n\nimpl sch schematic\nimpl beh spice\n  format spice \"X{name} {pins} amp_beh\"\nimpl va veriloga\n  model ampva\n\ninst 0r0000 ./res\n  at 0 0 R0\n  name R1\n  R r\n\nport 0p0001 0 0 R0 IN\nport 0p0002 0 20 R0 OUT\n";

fn bench_with(config: &str) -> String {
    format!(
        "schemify 1 shbench\n\ninst 0x0001 ./amp\n  at 0 0 R0\n  name X1\ninst 0x0002 ./amp\n  at 100 0 R0\n  name X2\n\nlabel 0n0001 0 0 R0 a\nlabel 0n0002 0 20 R0 b\nlabel 0n0003 100 0 R0 c\nlabel 0n0004 100 20 R0 d\n\n{config}"
    )
}

fn elab_cfg(bench: &str, cfg: Option<&str>, dialect: Dialect) -> Elab {
    let (p, hs) = project(&[
        ("res.shprim", RES),
        ("amp.shcomp", AMP),
        ("tb.shbench", bench),
    ]);
    let req = Request {
        config: cfg,
        corner: None,
        overrides: &[],
        env: &[],
        dialect,
    };
    elaborate(&p, hs[2], &req, &mut no_funcs)
}
