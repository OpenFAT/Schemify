//! Symbol generation: make symbol / schematic, labels and pins from ports.

use schemify_doc::{DocHandle, Op, RecordBuf, TableId, Target as DocTarget};
use schemify_format::{FileKind, Orient, RecordKind};

use crate::geometry::Pt;
use crate::scene::{ObjKey, UNIT};
use crate::{edit, wiring};

use super::Editor;

/// Pin pitch of generated symbols and port columns (xschem units).
pub(super) const PITCH: i32 = 20 * UNIT;

impl Editor {
    /// Symbol → Make symbol from schematic: pins from the ports (inputs left, outputs and
    /// inouts right, in id order), a box, the cell name and pin names; replaces the cell's
    /// pins and symbol graphics (one undo).
    pub(super) fn make_symbol_ops(&mut self, h: DocHandle) -> Vec<Op> {
        let Some(doc) = self.session.project.doc(h) else {
            return Vec::new();
        };
        if doc.kind() != FileKind::Shcomp {
            self.session
                .say("make symbol: only a .shcomp has a symbol and a schematic");
            return Vec::new();
        }
        let s = doc.strings();
        let p = doc.ports();
        let ports: Vec<(String, Orient, Pt)> = (0..p.id.len())
            .map(|i| (s.get(p.text[i]).to_owned(), p.orient[i], (p.x[i], p.y[i])))
            .collect();
        let recs = symbol_records(&ports, self.opts.symbol_width);
        if recs.is_empty() {
            self.session.say("make symbol: the schematic has no ports");
            return Vec::new();
        }
        replace_symbol_ops(doc, recs)
    }
    /// Symbol → Make schematic from symbol: a port for every pin without one, in a column
    /// at the pointer (inputs R0, outputs R180).
    pub(super) fn make_schematic_ops(&mut self, h: DocHandle, at: Pt) -> Vec<Op> {
        let Some(doc) = self.session.project.doc(h) else {
            return Vec::new();
        };
        if doc.kind() != FileKind::Shcomp {
            self.session
                .say("make schematic: the active cell is not a .shcomp");
            return Vec::new();
        }
        let s = doc.strings();
        let ports: Vec<&str> = doc.ports().text.iter().map(|&t| s.get(t)).collect();
        let pins = doc.pins();
        let mut ops = Vec::new();
        let mut k = 0;
        for i in 0..pins.name.len() {
            let name = s.get(pins.name[i]);
            if ports.contains(&name) {
                continue;
            }
            let o = match pins.dir[i] {
                schemify_doc::PinDir::Out => Orient::R180,
                _ => Orient::R0,
            };
            let rec = edit::placed(
                RecordKind::Port,
                (at.0, at.1 + k * PITCH),
                o,
                name,
                edit::AUTO_ID,
            );
            ops.push(Op::Insert { rec, at: None });
            k += 1;
        }
        ops
    }
    /// Symbol → Attach net labels: a label named after the pin on every pin of the selected
    /// instances that touches nothing else.
    pub(super) fn attach_labels_ops(&mut self, h: DocHandle) -> Vec<Op> {
        let sel = self.selection().to_vec();
        let p = &self.session.project;
        let Some(doc) = p.doc(h) else {
            return Vec::new();
        };
        let mut occupied: Vec<Pt> = self.scene.conn_points().map(|c| c.0).collect();
        occupied.sort_unstable();
        let count = |q: Pt| {
            let lo = occupied.partition_point(|&x| x < q);
            let hi = occupied.partition_point(|&x| x <= q);
            hi - lo
        };
        let mut ops = Vec::new();
        for k in sel.iter().filter(|k| k.table == TableId::Instances) {
            let Some(loc) = edit::loc_of(doc, *k) else {
                continue;
            };
            let row = loc.row as usize;
            let cellref = doc.strings().get(doc.instances().cell[row]);
            let Some(iface) = p.resolve(h, cellref).ok().and_then(|r| p.interface(r)) else {
                continue;
            };
            let pts = wiring::inst_pins(p, h, doc, row);
            for (i, q) in pts.into_iter().enumerate() {
                // The pin itself is one connection point; anything else touching it is another.
                if count(q) > 1 {
                    continue;
                }
                let name = iface.strings().get(iface.pins().name[i]).to_owned();
                let rec = edit::placed(RecordKind::Label, q, Orient::R0, &name, edit::AUTO_ID);
                ops.push(Op::Insert { rec, at: None });
            }
        }
        if ops.is_empty() {
            self.session
                .say("no unconnected pins on the selected instances");
        }
        ops
    }
}

/// A generated symbol for ports `(name, orientation, position)` (Symbol → Make symbol):
/// pins first (inputs — `R0`/`MR180` ports — on the left, outputs `R180`/`MR0` and inouts
/// on the right, each side top to bottom in the ports' (y, x) order, one per name), then the
/// graphics (a box `width` wide, pin leads, pin names, the `{name}` text). Empty when there
/// are no ports.
pub(super) fn symbol_records(ports: &[(String, Orient, Pt)], width: i32) -> Vec<RecordBuf> {
    let mut order: Vec<usize> = (0..ports.len()).collect();
    order.sort_by_key(|&i| (ports[i].2.1, ports[i].2.0));
    let mut left: Vec<&str> = Vec::new();
    let mut right: Vec<(&str, &str)> = Vec::new();
    for i in order {
        let name = ports[i].0.as_str();
        if left.contains(&name) || right.iter().any(|r| r.0 == name) {
            continue;
        }
        match ports[i].1 {
            Orient::R0 | Orient::MR180 => left.push(name),
            Orient::R180 | Orient::MR0 => right.push((name, "out")),
            _ => right.push((name, "inout")),
        }
    }
    if left.is_empty() && right.is_empty() {
        return Vec::new();
    }
    let c = crate::geometry::coord;
    let n = left.len().max(right.len()) as i32;
    let half_w = (width / 2).max(2 * PITCH);
    let lead = PITCH;
    let top = -(n * PITCH) / 2 - PITCH / 2;
    let bottom = top + (n + 1) * PITCH;
    let pin = |name: &str, dir: &str, x: i32, y: i32| {
        RecordBuf::new(RecordKind::Pin)
            .field(name)
            .field(dir)
            .field(&c(x))
            .field(&c(y))
    };
    let label = |name: &str, x: i32, y: i32| {
        RecordBuf::new(RecordKind::Stext)
            .field(&c(x))
            .field(&c(y - 5 * UNIT))
            .field("R0")
            .field(name)
            .attr("size", &["0.2"])
    };
    let mut pins = Vec::with_capacity(left.len() + right.len());
    let mut graphics = vec![
        RecordBuf::new(RecordKind::Rect)
            .field(&c(-half_w + lead))
            .field(&c(top))
            .field(&c(half_w - lead))
            .field(&c(bottom)),
        RecordBuf::new(RecordKind::Stext)
            .field("0")
            .field(&c(top - 15 * UNIT))
            .field("R0")
            .field("{name}")
            .attr("hcenter", &[]),
    ];
    for (k, name) in left.iter().enumerate() {
        let y = top + (k as i32 + 1) * PITCH;
        pins.push(pin(name, "in", -half_w, y));
        graphics.push(edit::shape(false, (-half_w, y), (-half_w + lead, y)));
        graphics.push(label(name, -half_w + lead + 2 * UNIT, y));
    }
    for (k, (name, dir)) in right.iter().enumerate() {
        let y = top + (k as i32 + 1) * PITCH;
        pins.push(pin(name, dir, half_w, y));
        graphics.push(edit::shape(false, (half_w - lead, y), (half_w, y)));
        let w = (name.chars().count() as i32) * 6 * UNIT;
        graphics.push(label(name, half_w - lead - 2 * UNIT - w, y));
    }
    assert_eq!(pins.len(), left.len() + right.len());
    pins.extend(graphics);
    pins
}

/// Ops replacing every pin and symbol graphic of `doc` with `recs` (pins and graphics, as
/// [`symbol_records`] makes them).
pub(super) fn replace_symbol_ops(doc: &schemify_doc::Document, recs: Vec<RecordBuf>) -> Vec<Op> {
    assert!(!recs.is_empty());
    let (s, pins) = (doc.strings(), doc.pins());
    let mut ops: Vec<Op> = pins
        .name
        .iter()
        .map(|&n| Op::Delete(DocTarget::new(RecordKind::Pin, s.get(n))))
        .collect();
    // Row 0 each time: the rows of the graphics still to delete shift up.
    for &kind in &doc.graphics().shape {
        ops.push(Op::Delete(DocTarget::new(kind, "0")));
    }
    ops.extend(recs.into_iter().map(|rec| Op::Insert { rec, at: None }));
    ops
}

/// Cell panel → Symbol from ports: the [`symbol_records`] symbol for an impl body's port
/// list (`ports`: unique names, port order; `in` left, `out` and `inout` right), its pins
/// written in port order, replacing `doc`'s pins and symbol graphics. Any cell kind with a
/// symbol. Empty when `ports` is.
pub(super) fn symbol_from_ports_ops(
    doc: &schemify_doc::Document,
    ports: &[(String, schemify_doc::PinDir)],
    width: i32,
) -> Vec<Op> {
    use schemify_doc::PinDir;
    // Positions only order each side top to bottom: port order.
    let placed: Vec<(String, Orient, Pt)> = ports
        .iter()
        .enumerate()
        .map(|(k, (name, dir))| {
            let o = match dir {
                PinDir::In => Orient::R0,
                PinDir::Out => Orient::R180,
                PinDir::Inout => Orient::R90,
            };
            (name.clone(), o, (0, k as i32 * PITCH))
        })
        .collect();
    let mut recs = symbol_records(&placed, width);
    if recs.is_empty() {
        return Vec::new();
    }
    // symbol_records lists the left side's pins, then the right's: back to port order.
    let n = ports.len();
    assert!(recs[..n].iter().all(|r| r.kind == RecordKind::Pin));
    recs[..n].sort_by_key(|r| ports.iter().position(|p| p.0 == r.fields[0]));
    replace_symbol_ops(doc, recs)
}

/// Symbol → Create symbol pins from selected schematic ports: one pin per selected port
/// whose name has no pin yet, at the port's position (R180/MR0 ports are outputs).
pub(super) fn pins_from_ports(doc: &schemify_doc::Document, sel: &[ObjKey]) -> Vec<Op> {
    if doc.kind() != FileKind::Shcomp {
        return Vec::new();
    }
    let s = doc.strings();
    let have: Vec<&str> = doc.pins().name.iter().map(|&n| s.get(n)).collect();
    let mut made: Vec<String> = Vec::new();
    let mut ops = Vec::new();
    for (_, _, r) in edit::selected_records(doc, sel) {
        if r.kind != RecordKind::Port {
            continue;
        }
        let name = r.fields[4].clone();
        if have.contains(&name.as_str()) || made.contains(&name) {
            continue;
        }
        let dir = match r.fields[3].as_str() {
            "R180" | "MR0" => "out",
            "R0" | "MR180" => "in",
            _ => "inout",
        };
        let rec = RecordBuf::new(RecordKind::Pin)
            .field(&name)
            .field(dir)
            .field(&r.fields[1])
            .field(&r.fields[2]);
        ops.push(Op::Insert { rec, at: None });
        made.push(name);
    }
    ops
}
