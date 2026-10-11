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
//! 4. Access: one linear pass over wires, labels, ports and instance pins per rebuild; marks
//!    are found by binary search on the overlay's sorted targets.
//! 5. Lifetime: rebuilt when the scene, the overlay, the highlight or the staged edit change
//!    (never per pan/zoom frame).
//! 6. Parallel: no; the rebuild is one pass and runs only after edits.

use schemify_doc::{Document, ObjId, TableId};
use schemify_elab::{
    AnnotationRequest, Annotations, CellState, Mark, Name, Names, Severity, Violation,
    violation_marks,
};
use schemify_render::canvas::CanvasPrims;

use crate::geometry::Pt;
use crate::scene::{ObjKey, Scene, UNIT};

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

/// Text height of an annotation (an OP voltage, a device's `gm`) in milli-units: a little
/// smaller than a net label (16 units), so the value reads as belonging to it.
const ANNOT_SIZE: i32 = 11 * UNIT;

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

/// The bit name of component `c` that keys it across the hierarchy (its first bit).
pub fn net_name(cell: &CellState, c: u32) -> &str {
    let bits = cell.nets.bits_of(c as usize);
    assert!(!bits.is_empty(), "a component carries at least one bit");
    cell.names.get(cell.nets.bit_name[bits[0] as usize])
}

/// World position of every ordinary pin of instance row `i` with its component, in pin
/// order. Empty for an unresolved instance.
pub fn pins_of(doc: &Document, cell: &CellState, i: usize) -> Vec<(u32, Pt)> {
    let inst = doc.instances();
    assert!(i < inst.id.len(), "instance row {i}");
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
    debug_assert!(out.len() <= comps.len());
    out
}

/// The component an object is on: a wire's, label's or port's net; for an instance, the net
/// of its pin nearest `near` within `tol` milli-units. `None` for anything else.
pub fn component_of(
    doc: &Document,
    cell: &CellState,
    key: ObjKey,
    near: Pt,
    tol: i64,
) -> Option<(u32, Option<(u32, u32)>)> {
    let loc = doc.find_id(ObjId(key.key))?;
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
            return Some((pins[best].0, Some((row as u32, best as u32))));
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
                out.pins.push((i as u32, k as u32, at));
            }
        }
    }
    out.keys.sort_unstable();
    out.pins.sort_unstable();
    out
}

/// The `checks` source's annotation request: one mark per violation of `cell`, targeted at
/// its object id in `doc` (or the cell name when it has none).
pub fn violation_request(cell: &CellState, doc: &Document) -> AnnotationRequest {
    let s = doc.strings();
    let req = violation_marks(&cell.violations, &cell.names, |raw| {
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

/// The marks on `target` (binary search: the overlay is sorted by target).
pub fn marks_on<'a>(ann: &'a Annotations, target: &str) -> &'a [Mark] {
    let m = &ann.marks;
    let lo = m.partition_point(|x| x.target.as_str() < target);
    let hi = lo + m[lo..].partition_point(|x| x.target == target);
    &m[lo..hi]
}

/// One violation as a panel line (`spines::violation_line` over a cell state's names).
pub fn line(names: &Names, v: &Violation) -> String {
    crate::spines::violation_line(names, v)
}

/// The world anchor of each component: its first label, else its first wire's midpoint,
/// else its first port (where an OP voltage is written).
fn anchors(doc: &Document, cell: &CellState) -> Vec<Option<Pt>> {
    let n = &cell.nets;
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
    at
}

/// Builds the decor primitives of the active level into `out` (cleared first):
/// highlighted members, violation markers of the `checks` source, annotation texts of every
/// other source on nets (`net_key` maps a net's bit name at this level to its overlay
/// target) and instances (`inst_key` maps an instance name), and the objects a staged edit
/// changes (`diff`).
#[allow(clippy::too_many_arguments)]
pub fn decor(
    out: &mut CanvasPrims,
    scene: &Scene,
    doc: &Document,
    cell: Option<&CellState>,
    ann: &Annotations,
    hl: &Members,
    net_key: impl Fn(&str) -> Option<String>,
    inst_key: impl Fn(&str) -> String,
    diff: &[ObjKey],
) {
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
    // Violation markers on their objects.
    let s = doc.strings();
    let mut i = 0;
    while i < ann.marks.len() {
        let m = &ann.marks[i];
        let src = &ann.source[i];
        i += 1;
        if src != SOURCE_CHECKS {
            continue;
        }
        let Some(id) = ObjId::find(&m.target, s) else {
            continue;
        };
        let Some(loc) = doc.find_id(id) else { continue };
        let key = ObjKey {
            table: loc.table,
            key: id.0,
        };
        let Some(o) = scene.find(key) else { continue };
        let b = scene.objs.bbox(o as usize);
        let layer = match m.severity {
            Severity::Error => LAYER_ERROR,
            Severity::Warning => LAYER_WARN,
            Severity::Info => LAYER_INFO,
        };
        let pad = 2 * UNIT;
        out.rect(b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad, layer);
        let code = m.text.split(' ').next().unwrap_or("");
        out.text(b[0] - pad, b[1] - pad - 7 * UNIT, 6 * UNIT, layer, code);
    }
    // Annotation texts (every source but the checks) on nets and instances.
    let others = ann.source.iter().any(|x| x != SOURCE_CHECKS);
    if let (Some(cell), true) = (cell, others) {
        let at = anchors(doc, cell);
        for (c, p) in at.iter().enumerate() {
            let Some(p) = p else { continue };
            let Some(target) = net_key(net_name(cell, c as u32)) else {
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
    // The visual diff of a staged edit.
    for k in diff {
        let Some(o) = scene.find(*k) else { continue };
        let b = scene.objs.bbox(o as usize);
        let pad = 4 * UNIT;
        out.rect(b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad, LAYER_DIFF);
    }
    assert_eq!(out.texts.x.len(), out.texts.end.len());
}

/// Appends the checks panel to the state contribution `t`: a header with the counts and a
/// close button, the `blocked` lines of the last refused netlist or run, then one clickable
/// row (`checks.row.<i>`, command `checks.pick`) per violation of `cell`, at most
/// [`PANEL_ROWS`].
pub fn contribute(t: &mut String, cell: Option<&CellState>, blocked: &[String]) {
    use std::fmt::Write as _;
    let rows = cell.map_or(&[][..], |c| &c.violations.rows[..]);
    let count = |s: Severity| rows.iter().filter(|v| v.code.severity() == s).count();
    let (e, w) = (count(Severity::Error), count(Severity::Warning));
    let q = crate::docs::quote;
    let _ = writeln!(
        t,
        "node checks.pane panel\n  parent bottom\n  style bottom-pane"
    );
    let _ = writeln!(
        t,
        "node checks.bar panel\n  parent checks.pane\n  style bottom-bar"
    );
    let title = format!(
        "Checks: {e} error(s), {w} warning(s), {} row(s)",
        rows.len()
    );
    let _ = writeln!(
        t,
        "node checks.title label\n  parent checks.bar\n  label {}\n  style bottom-title",
        q(&title)
    );
    let _ = writeln!(
        t,
        "node checks.close button\n  parent checks.bar\n  label x\n  command view.checks"
    );
    let _ = writeln!(
        t,
        "node checks.list scroll\n  parent checks.pane\n  style bottom-list"
    );
    for (i, b) in blocked.iter().enumerate() {
        let _ = writeln!(
            t,
            "node checks.blocked.{i} label\n  parent checks.list\n  label {}\n  style check-error",
            q(b)
        );
    }
    let Some(cell) = cell else { return };
    for (i, v) in rows.iter().take(PANEL_ROWS).enumerate() {
        let style = match v.code.severity() {
            Severity::Error => "check-error",
            Severity::Warning => "check-warning",
            Severity::Info => "check-info",
        };
        let _ = writeln!(
            t,
            "node checks.row.{i} item\n  parent checks.list\n  label {}\n  command checks.pick\n  style {style}",
            q(&line(&cell.names, v))
        );
    }
    if rows.len() > PANEL_ROWS {
        let more = format!("… {} more", rows.len() - PANEL_ROWS);
        let _ = writeln!(
            t,
            "node checks.more label\n  parent checks.list\n  label {}",
            q(&more)
        );
    }
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
        a.apply(&[AnnotationRequest {
            source: SOURCE_OP.to_owned(),
            marks: vec![
                mark("/b", "2"),
                mark("/a", "1"),
                mark("/b", "3"),
                mark("/c", "4"),
            ],
        }]);
        let texts =
            |t: &str| -> Vec<&str> { marks_on(&a, t).iter().map(|m| m.text.as_str()).collect() };
        assert_eq!(texts("/b"), ["2", "3"]);
        assert_eq!(texts("/a"), ["1"]);
        assert!(texts("/").is_empty() && texts("/bb").is_empty() && texts("/z").is_empty());
    }
}
