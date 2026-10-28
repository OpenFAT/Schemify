//! The flattened netlist, a test oracle (testing.md `netlist`: hierarchical vs flattened
//! emission must be equivalent): one `Elab` in, one SPICE deck with every leaf at the top
//! level out. Cites `docs/spec/netlist.md` §7.6.15 (component code is left out).
//!
//! Contents:
//! - `pub fn emit_flat(elab: &Elab, dialect: Dialect) -> String`

use crate::inst::{self, Tmp};
use crate::plan::Plans;
use crate::{Lines, NONE, deck, is_spice, net_text};
use schemify_elab::{DEPTH_MAX, Dialect, Elab, Ignore, Name, Target};

/// One flattening: the read-only inputs, the scratch, and the leaf lines so far.
struct Flat<'e> {
    e: &'e Elab,
    d: Dialect,
    plans: &'e Plans,
    tmp: Tmp,
    lines: Vec<String>,
}

/// Emits `elab` with the hierarchy flattened into top-level lines: leaf `name`s become
/// `<local>__<instance path, `__`-joined>` and internal nets `<path>.<net>`. Test oracle
/// only; SPICE dialects only. A component's own `code` lines are left out: their text
/// names nets and devices in the scope of its subckt, which flattening cannot rewrite.
/// Failed and empty rows are left out too.
///
/// # Panics
///
/// Panics for spectre and verilog, and as [`crate::emit`].
#[doc(hidden)]
pub fn emit_flat(elab: &Elab, dialect: Dialect) -> String {
    assert!(
        is_spice(dialect),
        "flat emission is a SPICE-dialect test oracle"
    );
    assert_eq!(elab.defs.inst_start.len(), elab.defs.cell.len() + 1);
    let plans = Plans::new(elab, dialect);
    let mut l = deck::frame(elab, dialect);
    let mut flat = Flat {
        e: elab,
        d: dialect,
        plans: &plans,
        tmp: Tmp::new(elab),
        lines: Vec::new(),
    };
    let mut top = Lines::default();
    if elab.top != NONE {
        let ports: Vec<String> = elab
            .ports(elab.top)
            .iter()
            .map(|n| net_text(elab, *n).to_owned())
            .collect();
        flat.def(elab.top, "", "", &ports, 0);
        flat.lines.sort();
        flat.lines.iter().for_each(|line| top.push(line));
        l.ir.top.name = elab.names.get(elab.defs.name[elab.top as usize]).to_owned();
        l.ir.top.ports = deck::ports(elab, elab.top, &[]);
        if dialect == Dialect::Cdl {
            l.ir.top.parameters = deck::cdl_params(elab, elab.top);
        }
    }
    assert_eq!(top.len(), flat.lines.len());
    l.lines = vec![vec![top]];
    deck::once_only(&mut l, elab, dialect, &flat.tmp.uses);
    crate::write(&mut l, dialect)
}

// ---- private ----

impl Flat<'_> {
    /// Flattens definition `d` at instance path `path` (`__`-joined local names, empty at
    /// the top; `dotted` is the same path `.`-joined) whose ports are bound to `ports`
    /// (flat net names), appending its leaf lines.
    fn def(&mut self, d: u32, path: &str, dotted: &str, ports: &[String], depth: u32) {
        let e = self.e;
        assert!(depth <= DEPTH_MAX, "deeper than any elaboration");
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
        let join = |a: &str, sep: &str, b: &str| {
            if a.is_empty() {
                b.to_owned()
            } else {
                format!("{a}{sep}{b}")
            }
        };
        for r in e.insts_of(d).filter(|&r| e.insts.ignore[r] == Ignore::No) {
            let local = e.names.get(e.insts.name[r]);
            let nets: Vec<(Name, String)> = e.conns(r).iter().map(|n| (*n, flat(*n))).collect();
            match e.insts.target[r] {
                Target::Def(c) => {
                    let child: Vec<String> = nets.into_iter().map(|n| n.1).collect();
                    let (p, dt) = (join(path, "__", local), join(dotted, ".", local));
                    self.def(c, &p, &dt, &child, depth + 1);
                }
                Target::Leaf(_) => {
                    // `<local>__<path>`: the device letter stays first.
                    let name = match path {
                        "" => local.to_owned(),
                        _ => format!("{local}__{path}"),
                    };
                    let net = |n: Name| -> &str {
                        nets.iter().find(|x| x.0 == n).map_or("?", |x| x.1.as_str())
                    };
                    let mut out = String::new();
                    let rendered = inst::line(
                        e,
                        self.d,
                        self.plans,
                        r,
                        &name,
                        net,
                        &mut out,
                        &mut self.tmp,
                    );
                    if rendered.is_ok() && !out.is_empty() {
                        self.lines.push(out);
                    }
                }
            }
        }
    }
}
