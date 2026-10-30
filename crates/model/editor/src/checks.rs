//! `checks`: the live check overlay (D13), net highlight (xschem `k` / `Shift+K`) and the
//! annotation overlay on the canvas, plus the checks panel. Everything here is derived: it
//! reads the active level's `elab::CellState` (violations, connectivity), the editor's
//! `elab::Annotations` (sources `checks` and `op`) and the scene, and writes canvas primitives
//! on the app's decor layers or UI records for the state contribution.
//!
//! Data design:
//! 1. In: one document, its [`CellState`], the annotation overlay, the highlighted net keys.
//!    Out: [`Members`] of the highlighted nets, decor primitives, checks panel records.
//! 2. How many: ≤ 2^22 objects per cell; violations ≤ 2^20 (a 100k-object cell with floating
//!    pins has tens of thousands of `W004`); the panel lists at most [`PANEL_ROWS`].
//! 3. Widths: components and rows `u32`, coordinates `i32` milli-units, layers `u8`.
//! 4. Access: one linear pass over wires, labels, ports and instance pins per decor rebuild;
//!    marks are found by range lookups in the ordered overlay. The violation markers are kept
//!    per marked object ([`Markers`]): an edit re-places only the objects it touched or whose
//!    marks it changed (O(change), D39 edit frames).
//! 5. Lifetime: the decor is rebuilt when the overlay, the highlight or the staged edit
//!    change, or the scene does while it draws something; the markers follow every scene
//!    change (never per pan/zoom frame).
//! 6. Parallel: no; the rebuild is one pass and runs only after edits.
//!
//! Contents:
//! - `pub const LAYER_HILITE`
//! - `pub const LAYER_ERROR`
//! - `pub const LAYER_WARN`
//! - `pub const LAYER_INFO`
//! - `pub const LAYER_ANNOT`
//! - `pub const LAYER_DIFF`
//! - `pub const PANEL_ROWS`
//! - `pub const HIGHLIGHT_MAX`
//! - `pub const SOURCE_CHECKS`
//! - `pub const SOURCE_OP`
//! - `pub struct Members`
//! - `pub struct Diff`
//! - `pub struct Markers`, `Markers::edits`, `Markers::clear_edits`, `Markers::generation`
//! - `pub fn bit_names`
//! - `pub fn net_name`
//! - `pub fn pins_of`
//! - `pub fn component_of`
//! - `pub fn members`
//! - `pub fn violation_request`
//! - `pub fn violation_marks_of`
//! - `pub fn line`
//! - `pub fn decor`
//! - `pub fn place_markers`
//! - `pub fn contribute`

mod markers;
mod panel;

use schemify_canvas::CanvasPrims;
use schemify_doc::{Document, ObjId, RecordBuf, TableId};
use schemify_elab::{
    AnnotationRequest, Annotations, CellState, Mark, Name, Names, Severity, Violation, Violations,
};

use crate::geometry::{Pt, Xform};
use crate::scene::{ObjKey, Scene, UNIT};

pub use markers::{Markers, place_markers};
pub use panel::contribute;

/// Highlighted nets (thick, xschem's highlight colour).
pub const LAYER_HILITE: u8 = 26;

/// Error markers (`E…`).
pub const LAYER_ERROR: u8 = 27;

/// Warning markers (`W…`).
pub const LAYER_WARN: u8 = 28;

/// Informational markers (`I005` waived).
pub const LAYER_INFO: u8 = 29;

/// Annotation texts (OP back-annotation, plugin marks).
pub const LAYER_ANNOT: u8 = 30;

/// Objects a staged edit changes (the visual diff).
pub const LAYER_DIFF: u8 = 31;

/// Violation rows the checks panel shows (the rest is counted).
pub const PANEL_ROWS: usize = 200;

/// Highlighted net keys at once.
pub const HIGHLIGHT_MAX: usize = 1 << 16;

/// The annotation source of the live checks.
pub const SOURCE_CHECKS: &str = "checks";

/// The annotation source of OP back-annotation.
pub const SOURCE_OP: &str = "op";

/// The objects and pin points of a set of components (what a highlight covers).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Members {
    /// Wires, labels and ports on the components, sorted.
    pub keys: Vec<ObjKey>,
    /// Instance pins on the components: (instance row, ordinary pin index, world point),
    /// sorted.
    pub pins: Vec<(u32, u32, Pt)>,
}

/// The visual diff of the staged edit under review: the objects it changes and deletes
/// (outlined; a deleted one crossed out in the error colour) and the records it inserts
/// (drawn as outlines where they will be: they are not in the document until accepted).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diff {
    /// Objects the stage modifies, sorted.
    pub changed: Vec<ObjKey>,
    /// Objects the stage deletes, sorted.
    pub deleted: Vec<ObjKey>,
    /// Records the stage inserts.
    pub inserted: Vec<RecordBuf>,
}

/// Text height of an annotation (an OP voltage, a device's `gm`) in milli-units: a little
/// smaller than a net label (16 units), so the value reads as belonging to it.
const ANNOT_SIZE: i32 = 11 * UNIT;

/// Every bit name of component `c`, in bit order: a bus component carries several, and each
/// keys it across the hierarchy (highlight).
///
/// # Panics
///
/// `c` is not a component handle of `cell`.
pub fn bit_names(cell: &CellState, c: u32) -> impl Iterator<Item = &str> + '_ {
    assert!(
        (c as usize) < cell.nets.net_width.len(),
        "component {c} of {}",
        cell.nets.net_width.len()
    );
    // Empty for a hole (a component handle an edit freed: elab patches nets in place).
    cell.nets
        .bits_of(c as usize)
        .iter()
        .map(move |&b| cell.names.get(cell.nets.bit_name[b as usize]))
}

/// The bit name of component `c` that keys it across the hierarchy (its first bit); empty
/// for a hole (a component handle an edit freed: elab patches nets in place).
///
/// # Panics
///
/// `c` is not a component handle of `cell`.
#[must_use]
pub fn net_name(cell: &CellState, c: u32) -> &str {
    assert!(
        (c as usize) < cell.nets.net_width.len(),
        "component {c} of {}",
        cell.nets.net_width.len()
    );
    let bits = cell.nets.bits_of(c as usize);
    assert!(
        bits.is_empty() == (cell.nets.net_width[c as usize] == 0),
        "a live component carries at least one bit"
    );
    bits.first()
        .map_or("", |&b| cell.names.get(cell.nets.bit_name[b as usize]))
}

/// World position of every ordinary pin of instance row `i` with its component, in pin
/// order. Empty for an unresolved instance.
///
/// # Panics
///
/// `i` is not an instance row of `doc`, or `cell` is not `doc`'s connectivity.
#[must_use]
pub fn pins_of(doc: &Document, cell: &CellState, i: usize) -> Vec<(u32, Pt)> {
    let inst = doc.instances();
    assert!(i < inst.id.len(), "instance row {i}");
    assert!(
        i < cell.inst_iface.len(),
        "the cell state was elaborated from this document"
    );
    let Some(f) = cell.ifaces.get(cell.inst_iface[i] as usize) else {
        return Vec::new();
    };
    let comps = cell.nets.pins(i);
    let o = inst.orient[i];
    let mut out = Vec::with_capacity(comps.len());
    let ordinary = (0..f.pin_name.len()).filter(|&k| f.pin_inherit[k] == Name::NONE);
    for (k, &c) in ordinary.zip(comps) {
        let at = schemify_elab::place(
            f.pin_x[k],
            f.pin_y[k],
            o.mirrored(),
            o.quarter_turns(),
            inst.x[i],
            inst.y[i],
        );
        out.push((c, at));
    }
    assert!(out.len() <= comps.len());
    assert!(out.len() <= f.pin_name.len());
    out
}

/// The component an object is on: a wire's, label's or port's net; for an instance, the net
/// of its pin nearest `near` within `tol` milli-units. `None` for anything else.
///
/// # Panics
///
/// A negative `tol`.
#[must_use]
pub fn component_of(
    doc: &Document,
    cell: &CellState,
    key: ObjKey,
    near: Pt,
    tol: i64,
) -> Option<(u32, Option<(u32, u32)>)> {
    assert!(tol >= 0, "a pick tolerance is a distance, got {tol}");
    let loc = doc.find_id(ObjId(key.key))?;
    assert_eq!(loc.table, key.table, "an object key names its id's table");
    let row = loc.row as usize;
    let n = &cell.nets;
    let comp = match key.table {
        TableId::Wires => n.wire_net.get(row).copied(),
        TableId::Labels => n.label_net.get(row).copied(),
        TableId::Ports => n.port_net.get(row).copied(),
        TableId::Instances if row < cell.inst_iface.len() => {
            let d = |p: Pt| {
                (i64::from(p.0) - i64::from(near.0)).pow(2)
                    + (i64::from(p.1) - i64::from(near.1)).pow(2)
            };
            let pins = pins_of(doc, cell, row);
            let best = (0..pins.len()).min_by_key(|&k| d(pins[k].1))?;
            if d(pins[best].1) > tol * tol {
                return None;
            }
            let row = u32::try_from(row).expect("instance rows fit a u32");
            let best = u32::try_from(best).expect("pin indexes fit a u32");
            return Some((pins[best as usize].0, Some((row, best))));
        }
        _ => None,
    };
    comp.filter(|&c| (c as usize) < n.net_name.len())
        .map(|c| (c, None))
}

/// Every wire, label, port and instance pin on components `comps` (sorted, distinct): the
/// highlight of those nets. The oracle is elab's own membership: a wire is on `c` exactly
/// when `cell.nets.wire_net[row] == c`.
///
/// Cost: one pass over wires, labels, ports and instance pins.
///
/// # Panics
///
/// `cell` is not `doc`'s connectivity (a table's net column is out of step with it).
#[must_use]
pub fn members(doc: &Document, cell: &CellState, comps: &[u32]) -> Members {
    debug_assert!(comps.windows(2).all(|w| w[0] < w[1]), "sorted components");
    let on = |c: u32| comps.binary_search(&c).is_ok();
    let mut out = Members::default();
    if comps.is_empty() {
        return out;
    }
    let n = &cell.nets;
    let tables = [
        (TableId::Wires, &doc.wires().id, &n.wire_net),
        (TableId::Labels, &doc.labels().id, &n.label_net),
        (TableId::Ports, &doc.ports().id, &n.port_net),
    ];
    for (table, ids, nets) in tables {
        assert_eq!(ids.len(), nets.len(), "connectivity of this document");
        for (id, &c) in ids.iter().zip(nets) {
            if on(c) {
                out.keys.push(ObjKey { table, key: id.0 });
            }
        }
    }
    for i in 0..doc.instances().id.len().min(cell.inst_iface.len()) {
        for (k, (c, at)) in pins_of(doc, cell, i).into_iter().enumerate() {
            if on(c) {
                let row = u32::try_from(i).expect("instance rows fit a u32");
                let pin = u32::try_from(k).expect("pin indexes fit a u32");
                out.pins.push((row, pin, at));
            }
        }
    }
    out.keys.sort_unstable();
    out.pins.sort_unstable();
    assert!(
        out.keys.windows(2).all(|w| w[0] < w[1]),
        "each object is listed once"
    );
    assert!(
        out.pins.windows(2).all(|w| w[0] < w[1]),
        "each pin is listed once"
    );
    out
}

/// The `checks` source's annotation request: one mark per violation of `cell`, targeted at
/// its object id in `doc` (or the cell name when it has none).
///
/// # Panics
///
/// Only on a broken `violation_marks` contract (one mark per violation).
#[must_use]
pub fn violation_request(cell: &CellState, doc: &Document) -> AnnotationRequest {
    let s = doc.strings();
    let req = schemify_elab::violation_marks(&cell.violations, &cell.names, |raw| {
        // An id from another arena (an alias the document lost) cannot be drawn: name it.
        let id = ObjId(raw);
        if id.is_alias() && doc.find_id(id).is_none() {
            return format!("#{raw}");
        }
        id.text(s)
    });
    assert_eq!(req.marks.len(), cell.violations.rows.len());
    req
}

/// The marks of violation rows `rows` of `cell` as [`violation_request`] targets them, or
/// `None` when one names an aliased object `doc` no longer has (its posted mark used the
/// alias text, which is gone: the caller re-posts everything).
///
/// # Panics
///
/// Only on a broken `violation_marks` contract (one mark per violation).
#[must_use]
pub fn violation_marks_of(
    rows: &[Violation],
    cell: &CellState,
    doc: &Document,
) -> Option<Vec<Mark>> {
    let lost = |v: &Violation| {
        v.path == Name::NONE
            && v.object != ObjId::NONE
            && v.object.is_alias()
            && doc.find_id(v.object).is_none()
    };
    if rows.iter().any(lost) {
        return None;
    }
    let v = Violations {
        rows: rows.to_vec(),
    };
    let s = doc.strings();
    let req = schemify_elab::violation_marks(&v, &cell.names, |raw| ObjId(raw).text(s));
    assert_eq!(req.marks.len(), rows.len());
    Some(req.marks)
}

/// One violation as a panel line (`spines::violation_line` over a cell state's names).
#[must_use]
pub fn line(names: &Names, v: &Violation) -> String {
    let text = crate::spines::violation_line(names, v);
    debug_assert!(!text.contains('\n'), "a panel line is one line");
    text
}

/// Builds the decor primitives of the active level into `out` (cleared first):
/// highlighted members, violation markers of the `checks` source, annotation texts of every
/// other source on nets (`net_key` maps a net's bit name at this level to its overlay
/// target) and instances (`inst_key` maps an instance name), and the visual diff of a
/// staged edit (`diff`).
///
/// # Panics
///
/// `hl` or `diff` lists an object key twice or unsorted (they are built sorted), or the
/// written text columns fall out of step.
#[allow(
    clippy::too_many_arguments,
    reason = "one pass over the scene, document, overlay and the three overlays it draws"
)]
pub fn decor(
    out: &mut CanvasPrims,
    scene: &Scene,
    doc: &Document,
    cell: Option<&CellState>,
    ann: &Annotations,
    hl: &Members,
    net_key: impl Fn(&str) -> Option<String>,
    inst_key: impl Fn(&str) -> String,
    diff: &Diff,
) {
    debug_assert!(
        hl.keys.windows(2).all(|w| w[0] < w[1]),
        "highlight keys sorted"
    );
    assert!(
        diff.changed.windows(2).all(|w| w[0] < w[1]),
        "diff changes sorted"
    );
    assert!(
        diff.deleted.windows(2).all(|w| w[0] < w[1]),
        "diff deletes sorted"
    );
    *out = CanvasPrims::default();
    // Highlight: wires as thick segments, labels/ports/pins as squares.
    let r = 3 * UNIT;
    for k in &hl.keys {
        let Some(o) = scene.find(*k) else { continue };
        let g = scene.objs.geom[o as usize];
        if scene.objs.seg[o as usize] {
            out.line(g[0], g[1], g[2], g[3], LAYER_HILITE);
        } else {
            out.rect(g[0] - r, g[1] - r, g[2] + r, g[3] + r, LAYER_HILITE);
        }
    }
    for &(_, _, p) in &hl.pins {
        out.rect(p.0 - r, p.1 - r, p.0 + r, p.1 + r, LAYER_HILITE);
    }
    // (The violation markers of the `checks` source are their own table: [`Markers`].)
    let s = doc.strings();
    // Annotation texts (every source but the checks) on nets and instances.
    let others = ann.len() > marks_of_checks(ann);
    if let (Some(cell), true) = (cell, others) {
        let at = anchors(doc, cell);
        for (c, p) in at.iter().enumerate() {
            let Some(p) = p else { continue };
            let c = u32::try_from(c).expect("components fit a u32");
            let Some(target) = net_key(net_name(cell, c)) else {
                continue;
            };
            let mut dy = 0;
            for m in marks_on(ann, &target) {
                out.text(
                    p.0 + 6 * UNIT,
                    p.1 + 6 * UNIT + dy,
                    ANNOT_SIZE,
                    LAYER_ANNOT,
                    &m.text,
                );
                dy += ANNOT_SIZE + 2 * UNIT;
            }
        }
        let inst = doc.instances();
        for row in 0..inst.id.len() {
            let target = inst_key(s.get(inst.name[row]));
            let marks = marks_on(ann, &target);
            if marks.is_empty() {
                continue;
            }
            let key = ObjKey {
                table: TableId::Instances,
                key: inst.id[row].0,
            };
            let Some(o) = scene.find(key) else { continue };
            let b = scene.objs.bbox(o as usize);
            let mut y = b[3] + 3 * UNIT;
            for m in marks.iter().filter(|m| m.severity == Severity::Info) {
                out.text(b[0], y, ANNOT_SIZE, LAYER_ANNOT, &m.text);
                y += ANNOT_SIZE + 2 * UNIT;
            }
        }
    }
    // The visual diff of a staged edit: changed objects outlined, deleted ones crossed out,
    // inserted records drawn where they will be.
    let pad = 4 * UNIT;
    for (keys, layer) in [(&diff.changed, LAYER_DIFF), (&diff.deleted, LAYER_ERROR)] {
        for k in keys {
            let Some(o) = scene.find(*k) else { continue };
            let b = scene.objs.bbox(o as usize);
            let (x0, y0, x1, y1) = (b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad);
            out.rect(x0, y0, x1, y1, layer);
            if layer == LAYER_ERROR {
                out.line(x0, y0, x1, y1, layer);
                out.line(x0, y1, x1, y0, layer);
            }
        }
    }
    let identity = Xform::translate(0, 0);
    crate::scene::preview_records_on(&diff.inserted, identity, LAYER_DIFF, out);
    assert_eq!(out.texts.x.len(), out.texts.end.len());
}

// ---- private ----

/// The marks on `target`, every source's (a range of the ordered overlay).
fn marks_on<'a>(ann: &'a Annotations, target: &str) -> Vec<&'a Mark> {
    ann.on(target).collect()
}

/// How many marks the `checks` source has (O(log sources)).
fn marks_of_checks(ann: &Annotations) -> usize {
    ann.count_of(SOURCE_CHECKS)
}

/// The world anchor of each component: its first label, else its first wire's midpoint,
/// else its first port (where an OP voltage is written).
fn anchors(doc: &Document, cell: &CellState) -> Vec<Option<Pt>> {
    let n = &cell.nets;
    assert_eq!(
        n.label_net.len(),
        doc.labels().x.len(),
        "label nets in step"
    );
    assert_eq!(n.wire_net.len(), doc.wires().x1.len(), "wire nets in step");
    assert_eq!(n.port_net.len(), doc.ports().x.len(), "port nets in step");
    let mut at: Vec<Option<Pt>> = vec![None; n.net_name.len()];
    let l = doc.labels();
    for (i, &c) in n.label_net.iter().enumerate() {
        if let Some(slot) = at.get_mut(c as usize).filter(|s| s.is_none()) {
            *slot = Some((l.x[i], l.y[i]));
        }
    }
    let w = doc.wires();
    for (i, &c) in n.wire_net.iter().enumerate() {
        if let Some(slot) = at.get_mut(c as usize).filter(|s| s.is_none()) {
            *slot = Some(((w.x1[i] + w.x2[i]) / 2, (w.y1[i] + w.y2[i]) / 2));
        }
    }
    let p = doc.ports();
    for (i, &c) in n.port_net.iter().enumerate() {
        if let Some(slot) = at.get_mut(c as usize).filter(|s| s.is_none()) {
            *slot = Some((p.x[i], p.y[i]));
        }
    }
    assert_eq!(at.len(), n.net_name.len());
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemify_elab::Severity;

    #[test]
    fn marks_on_finds_exactly_the_target_run() {
        let mut a = Annotations::default();
        let mark = |t: &str, x: &str| Mark {
            target: t.to_owned(),
            severity: Severity::Info,
            text: x.to_owned(),
        };
        a.replace(AnnotationRequest {
            source: SOURCE_OP.to_owned(),
            marks: vec![
                mark("/b", "2"),
                mark("/a", "1"),
                mark("/b", "3"),
                mark("/c", "4"),
            ],
        });
        let texts =
            |t: &str| -> Vec<&str> { marks_on(&a, t).iter().map(|m| m.text.as_str()).collect() };
        assert_eq!(texts("/b"), ["2", "3"]);
        assert_eq!(texts("/a"), ["1"]);
        assert!(texts("/").is_empty() && texts("/bb").is_empty() && texts("/z").is_empty());
    }
}
