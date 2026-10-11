//! Derived design data (D11a, D13, D19, D20, D29, D30): connectivity, the expression
//! graph and its values, hierarchy elaboration with impl selection and subckt
//! specialization, resolved params, violations and the annotation overlay.
//!
//! Reads `doc` tables; owns everything it derives; pure (no I/O, no clock, no threads)
//! and builds for `wasm32-unknown-unknown`. Normative spec: `docs/spec/netlist.md`
//! §1–§6 and §8 (with the amendments listed in its §9).
//!
//! # Spines
//!
//! - frame (editor): [`CellState::build`] once per open document, then
//!   [`CellState::recompute`]`(project, doc, ops)` after `doc::apply_edits` →
//!   [`Annotations::apply`]`(annotation_requests)`.
//! - netlist: [`elaborate`]`(project, top, request)` → gate on
//!   [`Elab::violations`] (`blocking`) → `netlist::emit(&elab, dialect)`.
//!
//! # What the netlister reads
//!
//! [`Elab`] is self-contained: walk [`Elab::defs`] (each a subckt: name, port nets),
//! their [`Elab::insts`] rows (local name, [`Target`], connection net names in pin
//! order then inherited pins, params as canonical text), [`Elab::impls`] (leaf
//! templates and once-only bodies), [`Elab::globals`] and [`Elab::bench`]. Names are
//! [`Name`]s into [`Elab::names`]. The name map (path ↔ emitted name) is the
//! netlister's: it is a fact about emitted text.
//!
//! ```
//! use schemify_doc::Project;
//! use schemify_elab::{Dialect, Request, Target, elaborate, no_funcs};
//!
//! let mut p = Project::new(None, None, 1).unwrap();
//! let res = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nparam R 1k\n\nimpl spice spice\n  format spice \"R{name} {A} {B} {R}\"\n";
//! let bench = "schemify 1 shbench\n\ninst 0aaaaa ./res\n  at 0 0 R0\n  name 1\n  R 2k\n\nlabel 0bbbbb 0 0 R0 in\nlabel 0ccccc 0 20 R0 0\n";
//! p.index_library("", "", &[("res.shprim", res.as_bytes()), ("tb.shbench", bench.as_bytes())]).unwrap();
//! p.open("", "res.shprim", res.as_bytes(), None).unwrap();
//! let top = p.open("", "tb.shbench", bench.as_bytes(), None).unwrap().handle;
//! let req = Request { config: None, corner: None, overrides: &[], env: &[], dialect: Dialect::Ngspice };
//! let e = elaborate(&p, top, &req, &mut no_funcs);
//! assert!(!e.violations.blocking());
//! let row = e.insts_of(e.top).start;
//! let nets: Vec<&str> = e.conns(row).iter().map(|n| e.names.get(*n)).collect();
//! assert_eq!(nets, ["in", "0"]);
//! assert!(matches!(e.insts.target[row], Target::Leaf(_)));
//! assert_eq!(e.names.get(e.insts.param_text[e.params_of(row).start]), "2000");
//! ```

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
// Tables are SoA: loops index several parallel columns by one row, which is clearer
// than zipping four iterators.
#![allow(clippy::needless_range_loop)]

mod annotate;
mod cells;
mod check;
mod connect;
mod elaborate;
mod graph;
mod iface;
mod names;
mod netexpr;
mod state;
mod values;

pub use annotate::{ANNOTATIONS_MAX, AnnotationRequest, Annotations, Mark, violation_marks};
pub use check::{Code, Severity, VIOLATIONS_MAX, Violation, Violations};
pub use connect::{CellNets, Ignore, NO_NET, connect, global_labels, place};
pub use elaborate::{
    Bench, COMPUTED, CornerLine, DEFAULT, DEPTH_MAX, Defs, Dialect, Elab, Impls, Insts, OVERRIDDEN,
    Request, Target, elaborate,
};
pub use graph::Graph;
pub use iface::{Domain, Iface, ParamKind};
pub use names::{NAMES_MAX, Name, Names};
pub use netexpr::{WIDTH_MAX, is_global};
pub use state::{CellState, Recomputed};
pub use values::{Funcs, Val, cached_text, no_funcs};

#[doc(hidden)]
pub mod test_support {
    //! Internals the integration tests drive directly.
    pub use crate::iface::Iface;
    pub use crate::names::Names;

    /// Builds the interface digest of `doc` (for tests that call [`crate::connect`]).
    pub fn iface(doc: &schemify_doc::Document, cell: &str, names: &mut Names) -> Iface {
        Iface::from_doc(doc, cell, cell, u32::MAX, true, names)
    }

    /// Expands a net expression into bit names.
    pub fn expand(text: &str, names: &mut Names) -> Option<Vec<String>> {
        let (mut out, mut buf) = (Vec::new(), String::new());
        crate::netexpr::expand(text, names, &mut out, &mut buf).ok()?;
        Some(out.iter().map(|n| names.get(*n).to_owned()).collect())
    }
}
