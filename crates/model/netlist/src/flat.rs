//! The flattened netlist, a test oracle (testing.md `netlist`: hierarchical vs flattened
//! emission must be equivalent): one `Elab` in, one SPICE deck with every leaf at the top
//! level out. Cites `docs/spec/netlist.md` §7.6.15 (component code is left out).
//!
//! Contents:
//! - `pub fn emit_flat(elab: &Elab, dialect: Dialect) -> String`

use crate::inst;
use crate::inst::{Context, RowScratch};
use crate::plan::Plans;
use crate::{Lines, NONE, deck};
use schemify_elab::{DEPTH_MAX, Dialect, Elab, Ignore, Name, Target};

/// One flattening: the read-only inputs, the scratch, and the leaf lines so far.
struct Flat<'elab> {
    context: Context<'elab>,
    scratch: RowScratch,
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
#[must_use]
pub fn emit_flat(elab: &Elab, dialect: Dialect) -> String {
    assert!(
        crate::is_spice(dialect),
        "flat emission is a SPICE-dialect test oracle"
    );
    assert_eq!(elab.defs.inst_start.len(), elab.defs.cell.len() + 1);
    let plans = Plans::new(elab, dialect);
    let mut lowered = deck::frame(elab, dialect);
    let mut flat = Flat {
        context: Context {
            elab,
            dialect,
            plans: &plans,
        },
        scratch: RowScratch::new(elab),
        lines: Vec::new(),
    };
    let mut top = Lines::default();
    if elab.top != NONE {
        let ports: Vec<String> = elab
            .ports(elab.top)
            .iter()
            .map(|n| crate::net_text(elab, *n).to_owned())
            .collect();
        flat.def(elab.top, "", "", &ports, 0);
        flat.lines.sort();
        for line in &flat.lines {
            top.push(line);
        }
        lowered.ir.top.name = elab.names.get(elab.defs.name[elab.top as usize]).to_owned();
        lowered.ir.top.ports = deck::ports(elab, elab.top, &[]);
        if dialect == Dialect::Cdl {
            lowered.ir.top.parameters = deck::cdl_params(elab, elab.top);
        }
    }
    assert_eq!(top.len(), flat.lines.len());
    lowered.lines = vec![vec![top]];
    deck::once_only(&mut lowered, elab, dialect, &flat.scratch.uses);
    crate::write(&mut lowered, dialect)
}

// ---- private ----

impl Flat<'_> {
    /// Flattens definition `def` at instance path `path` (`__`-joined local names, empty
    /// at the top; `dotted` is the same path `.`-joined) whose ports are bound to `ports`
    /// (flat net names), appending its leaf lines.
    fn def(&mut self, def: u32, path: &str, dotted: &str, ports: &[String], depth: u32) {
        let Context { elab, .. } = self.context;
        assert!(depth <= DEPTH_MAX, "deeper than any elaboration");
        assert_eq!(ports.len(), elab.ports(def).len());
        assert_eq!(
            path.is_empty(),
            dotted.is_empty(),
            "two spellings of one path"
        );
        let flat = |n: Name| -> String {
            if n == Name::NONE {
                return "?".to_owned();
            }
            if let Some(j) = elab.ports(def).iter().position(|p| *p == n) {
                return ports[j].clone();
            }
            let text = elab.names.get(n);
            if dotted.is_empty() || elab.globals.contains(&n) {
                text.to_owned()
            } else {
                format!("{dotted}.{text}")
            }
        };
        let join = |a: &str, separator: &str, b: &str| {
            if a.is_empty() {
                b.to_owned()
            } else {
                format!("{a}{separator}{b}")
            }
        };
        let emitted = elab
            .insts_of(def)
            .filter(|&r| elab.insts.ignore[r] == Ignore::No);
        for r in emitted {
            let local = elab.names.get(elab.insts.name[r]);
            let nets: Vec<(Name, String)> = elab.conns(r).iter().map(|n| (*n, flat(*n))).collect();
            match elab.insts.target[r] {
                Target::Def(child) => {
                    let child_ports: Vec<String> = nets.into_iter().map(|(_, text)| text).collect();
                    let child_path = join(path, "__", local);
                    let child_dotted = join(dotted, ".", local);
                    self.def(child, &child_path, &child_dotted, &child_ports, depth + 1);
                }
                Target::Leaf(_) => {
                    // `<local>__<path>`: the device letter stays first.
                    let name = match path {
                        "" => local.to_owned(),
                        _ => format!("{local}__{path}"),
                    };
                    let net = |n: Name| -> &str {
                        nets.iter()
                            .find(|(net, _)| *net == n)
                            .map_or("?", |(_, text)| text.as_str())
                    };
                    let mut out = String::new();
                    let rendered =
                        inst::line(self.context, r, &name, net, &mut out, &mut self.scratch);
                    if rendered.is_ok() && !out.is_empty() {
                        self.lines.push(out);
                    }
                }
            }
        }
    }
}
