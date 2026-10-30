//! D19 oracle (docs/dev/testing.md `expr`/`elab`): after any sequence of edits, undos and redos
//! applied with `doc::apply_edits` (`recompute` takes the ops `Project::applied` lends),
//! the incrementally recomputed state equals a state built from scratch — values,
//! unverified flags, nets and violations.

#[path = "../common/mod.rs"]
mod common;
mod geometry;
mod values;

use common::Mix;
use schemify_doc::{DocHandle, Edit, EditRequest, Mode, Op, Outcome, Project, Transaction};
use schemify_elab::CellState;

const DEV: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 20 0\n\nparam w 1\n  min 0.5\n  max 8\nparam l w*2\n  grid 0.5\nparam ad 0\n  computed w*l\n\nimpl spice spice\n";

/// A cell with vars, an own param, and `n` instances with overrides that read them.
fn cell(n: usize) -> String {
    let mut s =
        String::from("schemify 1 shcomp\n\npin P inout 0 0\n\nparam p 3\n\nimpl sch schematic\n\n");
    for i in 0..n {
        let ovr = match i % 4 {
            0 => "  w a+k2\n".to_owned(),
            1 => "  w b*2\n  l c\n".to_owned(),
            2 => "  l p+a\n".to_owned(),
            _ => "  w 2\n".to_owned(),
        };
        s.push_str(&format!(
            "inst {} ./dev\n  at {} 0 R0\n  name M{i}\n{ovr}",
            common::id(i as u64 + 1),
            common::c(i as i64 * 40_000)
        ));
    }
    s.push_str(&format!("\nwire {} 0 0 20 0\n\nport {} 0 0 R0 P\n\nvar a 1\nvar b a+1\nvar c b*a+k1\nvar k1 5\nvar k2 7\n", common::id(9_001), common::id(9_002)));
    s
}

fn commit(p: &mut Project, h: DocHandle, ops: Vec<Op>) -> bool {
    let tx = Transaction {
        label: "t".into(),
        mode: Mode::Commit,
        ops,
    };
    matches!(edit(p, h, Edit::Tx(tx)).outcome, Outcome::Committed { .. })
}

/// One request's outcome and the ops it applied.
struct Applied {
    outcome: Outcome,
    ops: Vec<Op>,
}

/// Applies one request; its applied ops (`Project::applied`: a commit's resolved ops, an
/// undo's inverse ops, a redo's ops) are what the editor hands `recompute`.
fn edit(p: &mut Project, h: DocHandle, edit: Edit) -> Applied {
    let mut out = schemify_doc::apply_edits(
        p,
        &[EditRequest {
            doc: h,
            edit,
            unix_ms: 1,
        }],
    );
    let outcome = out.pop().expect("one outcome per request");
    let ops = p.applied(h, &outcome).map_or_else(Vec::new, <[Op]>::to_vec);
    Applied { outcome, ops }
}

/// The ops of a random undo or redo, or `None` when there was nothing to revert.
fn revert(p: &mut Project, h: DocHandle, rng: &mut Mix) -> Option<Vec<Op>> {
    let e = if rng.chance(50) {
        Edit::Undo
    } else {
        Edit::Redo
    };
    let done = edit(p, h, e);
    matches!(
        done.outcome,
        Outcome::Undone { .. } | Outcome::Redone { .. }
    )
    .then_some(done.ops)
}

/// Everything comparable between two states, as text.
fn snapshot(st: &CellState) -> (Vec<String>, Vec<bool>, Vec<String>, Vec<String>) {
    let vals = st.graph.vals.iter().map(|v| format!("{v:?}")).collect();
    let elems = (0..st.inst_iface.len()).map(|i| {
        let names: Vec<&str> = st.nets.elems(i).iter().map(|&e| st.names.get(e)).collect();
        format!("elems {i} {names:?}")
    });
    let nets = st
        .nets
        .conns
        .iter()
        .map(|&b| {
            if b == u32::MAX {
                "-".into()
            } else {
                st.names.get(st.nets.bit_name[b as usize]).to_owned()
            }
        })
        .chain(elems)
        .collect();
    let n = |x: schemify_elab::Name| {
        if x == schemify_elab::Name::NONE {
            String::new()
        } else {
            st.names.get(x).to_owned()
        }
    };
    let viol = st
        .violations
        .rows
        .iter()
        .map(|v| {
            format!(
                "{} {} {} {} {} {}",
                v.code.as_str(),
                n(v.cell),
                v.object.0,
                n(v.path),
                n(v.param),
                v.detail
            )
        })
        .collect();
    (vals, st.graph.unverified.clone(), nets, viol)
}

/// Every node's value and `unverified` flag, as text.
fn node_vals(st: &CellState) -> Vec<String> {
    let g = &st.graph;
    (0..g.len())
        .map(|n| format!("{:?} {}", g.vals[n], g.unverified[n]))
        .collect()
}

/// The instance rows owning a node whose text differs from `before` (same graph).
fn revalued(st: &CellState, before: &[String]) -> Vec<u32> {
    let now = node_vals(st);
    assert_eq!(now.len(), before.len(), "same nodes");
    let mut rows: Vec<u32> = (0..now.len())
        .filter(|&n| now[n] != before[n])
        .map(|n| st.graph.owner[n])
        .filter(|&o| o != u32::MAX)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}
