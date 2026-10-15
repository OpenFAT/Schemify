//! Symbol → Make schematic and symbol from selected components (xschem Ctrl+Shift+H,
//! xschem's Make symbol from schematic): the selection moves into a new cell and
//! an instance of that cell takes its place, **wired** to the nets the selection boundary
//! cut, so the parent's connectivity is unchanged.
//!
//! Data design:
//! 1. In: the active schematic's rank-3 tables, its elab nets (`CellState`), the selection.
//!    Out: the new cell's text (written at once) and one transaction for the parent.
//! 2. How many: one crossing per net the boundary cuts; contacts per crossing are the points
//!    where the net's outside part touched its inside part.
//! 3. Widths: coordinates `i32` milli-units (D33); components `u32` (elab).
//! 4. Access: every connection point and wire of the cell is scanned per crossing.
//! 5. Lifetime: one command. 6. Parallel: no (interactive, cold).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, Document, EditRequest, Op, PinDir, RecordBuf, TableId};
use schemify_elab::{CellState, Name};
use schemify_format::{FileKind, Orient, RecordKind};

use super::Editor;
use super::parity::{PITCH, symbol_records};
use crate::checks;
use crate::edit::{self, Ids};
use crate::geometry::Pt;
use crate::platform;
use crate::scene::{self, ObjKey};
use crate::wiring::{on_interior, route};

/// Net of a connection point that belongs to no net (a noconn): foreign to every crossing.
const NO_COMP: u32 = u32::MAX;
/// Rings of grid offsets (pitch [`PITCH`]) searched around the selection for a place where
/// the new instance's pins touch nothing: `(2 · RINGS + 1)²` candidates.
const RINGS: i32 = 8;

/// Connection geometry of one side of the selection boundary, with each item's net.
#[derive(Debug, Default)]
struct Side {
    /// Instance pins, wire ends, label, port and noconn anchors.
    pts: Vec<(u32, Pt)>,
    /// Wires.
    segs: Vec<(u32, Pt, Pt)>,
}

/// A net the selection boundary cuts: it has objects inside and outside the selection.
#[derive(Debug)]
struct Crossing {
    /// The net's name in the parent: the new cell's port and pin name.
    name: String,
    /// `out` when an inside instance pin on it drives it, `in` when every inside instance
    /// pin on it is an input, else `inout`.
    dir: PinDir,
    /// Where the new cell's port goes: a point of the net's inside part.
    port_at: Pt,
    /// Points where the outside part touched the inside part, sorted and distinct; empty
    /// when the two parts met by name only (labels, ports).
    contacts: Vec<Pt>,
    /// The net (elab component) in the parent.
    net: u32,
}

/// Splits the cell's connection geometry at the selection `sel` and returns the nets the
/// boundary cuts (by component) and the outside side.
///
/// Cost: O(crossings · inside points · outside points) worst case.
// ponytail: quadratic scans per crossing; index the outside points by position if a
// subcircuit is ever cut from a 10^5-object schematic.
fn crossings(doc: &Document, cell: &CellState, sel: &HashSet<ObjKey>) -> (Vec<Crossing>, Side) {
    let n = &cell.nets;
    let w = doc.wires();
    assert_eq!(
        n.wire_net.len(),
        w.id.len(),
        "elab is current for the document"
    );
    let mut inside = Side::default();
    let mut outside = Side::default();
    let mut dirs: Vec<(u32, PinDir)> = Vec::new();
    for r in 0..w.id.len() {
        let key = ObjKey {
            table: TableId::Wires,
            key: w.id[r].0,
        };
        let side = if sel.contains(&key) {
            &mut inside
        } else {
            &mut outside
        };
        let (a, b, c) = ((w.x1[r], w.y1[r]), (w.x2[r], w.y2[r]), n.wire_net[r]);
        side.pts.push((c, a));
        side.pts.push((c, b));
        side.segs.push((c, a, b));
    }
    for (table, t, nets) in [
        (TableId::Labels, doc.labels(), &n.label_net),
        (TableId::Ports, doc.ports(), &n.port_net),
    ] {
        assert_eq!(nets.len(), t.id.len(), "elab is current for the document");
        for (r, &c) in nets.iter().enumerate() {
            let key = ObjKey {
                table,
                key: t.id[r].0,
            };
            let side = if sel.contains(&key) {
                &mut inside
            } else {
                &mut outside
            };
            side.pts.push((c, (t.x[r], t.y[r])));
        }
    }
    let nc = doc.noconns();
    for r in 0..nc.id.len() {
        let key = ObjKey {
            table: TableId::Noconns,
            key: nc.id[r].0,
        };
        if !sel.contains(&key) {
            outside.pts.push((NO_COMP, (nc.x[r], nc.y[r])));
        }
    }
    let inst = doc.instances();
    for i in 0..inst.id.len() {
        let key = ObjKey {
            table: TableId::Instances,
            key: inst.id[i].0,
        };
        let pins = checks::pins_of(doc, cell, i);
        if !sel.contains(&key) {
            outside.pts.extend(pins);
            continue;
        }
        if let Some(f) = cell.ifaces.get(cell.inst_iface[i] as usize) {
            let ordinary = (0..f.pin_name.len()).filter(|&p| f.pin_inherit[p] == Name::NONE);
            for (p, &(c, _)) in ordinary.zip(&pins) {
                dirs.push((c, f.pin_dir[p]));
            }
        }
        inside.pts.extend(pins);
    }
    let mut out_nets: Vec<u32> = outside
        .pts
        .iter()
        .map(|p| p.0)
        .filter(|&c| c != NO_COMP)
        .collect();
    out_nets.sort_unstable();
    out_nets.dedup();
    let mut nets: Vec<u32> = inside
        .pts
        .iter()
        .map(|p| p.0)
        .filter(|c| out_nets.binary_search(c).is_ok())
        .collect();
    nets.sort_unstable();
    nets.dedup();
    let mut out = Vec::with_capacity(nets.len());
    for c in nets {
        let mut contacts: Vec<Pt> = Vec::new();
        for &(_, p) in inside.pts.iter().filter(|q| q.0 == c) {
            let touches = outside.pts.iter().any(|&(d, q)| d == c && q == p)
                || outside
                    .segs
                    .iter()
                    .any(|&(d, a, b)| d == c && on_interior(p, a, b));
            if touches {
                contacts.push(p);
            }
        }
        for &(_, q) in outside.pts.iter().filter(|q| q.0 == c) {
            if inside
                .segs
                .iter()
                .any(|&(d, a, b)| d == c && on_interior(q, a, b))
            {
                contacts.push(q);
            }
        }
        contacts.sort_unstable();
        contacts.dedup();
        let first = inside
            .pts
            .iter()
            .find(|q| q.0 == c)
            .map(|q| q.1)
            .expect("a crossing has inside points");
        let pin_dirs: Vec<PinDir> = dirs.iter().filter(|d| d.0 == c).map(|d| d.1).collect();
        let dir = if pin_dirs.contains(&PinDir::Out) {
            PinDir::Out
        } else if !pin_dirs.is_empty() && pin_dirs.iter().all(|&d| d == PinDir::In) {
            PinDir::In
        } else {
            PinDir::Inout
        };
        out.push(Crossing {
            name: checks::net_name(cell, c).to_owned(),
            dir,
            port_at: contacts.first().copied().unwrap_or(first),
            contacts,
            net: c,
        });
    }
    assert!(out.windows(2).all(|p| p[0].net < p[1].net));
    (out, outside)
}

/// Whether point `q` touches the outside geometry (equals one of its points or lies on a
/// wire): a pin there would connect to it.
fn touches(q: Pt, outside: &Side) -> bool {
    outside.pts.iter().any(|p| p.1 == q)
        || outside
            .segs
            .iter()
            .any(|&(_, a, b)| q == a || q == b || on_interior(q, a, b))
}

/// The first place for the new instance around `centre` (ring by ring, grid offsets of
/// [`PITCH`]) where none of its `pins` (instance-local, orientation `R0`) touches the outside
/// geometry; `None` when every candidate does.
fn free_spot(centre: Pt, pins: &[Pt], outside: &Side) -> Option<Pt> {
    for ring in 0..=RINGS {
        for i in -ring..=ring {
            for j in -ring..=ring {
                if i.abs().max(j.abs()) != ring {
                    continue;
                }
                let at = (
                    centre.0.saturating_add(i * PITCH),
                    centre.1.saturating_add(j * PITCH),
                );
                let clear = pins.iter().all(|p| {
                    !touches(
                        (p.0.saturating_add(at.0), p.1.saturating_add(at.1)),
                        outside,
                    )
                });
                if clear {
                    return Some(at);
                }
            }
        }
    }
    None
}

/// An L wire (horizontal leg first, else vertical first) from pin `q` to contact `p` of net
/// `net` that touches nothing of another net: no foreign point on a leg, no corner on a
/// foreign point or wire. `None` when both shapes would.
fn clear_route(
    q: Pt,
    p: Pt,
    net: u32,
    pts: &[(u32, Pt)],
    segs: &[(u32, Pt, Pt)],
) -> Option<Vec<Pt>> {
    for mode in [1u8, 2] {
        let path = route(q, p, mode);
        assert!(!path.is_empty() && path[0] == q);
        let legs_clear = path.windows(2).all(|l| {
            pts.iter()
                .all(|&(d, f)| d == net || !on_interior(f, l[0], l[1]))
        });
        let corners = &path[1..path.len().saturating_sub(1).max(1)];
        let corners_clear = corners.iter().all(|&v| {
            pts.iter().all(|&(d, f)| d == net || f != v)
                && segs
                    .iter()
                    .all(|&(d, a, b)| d == net || (v != a && v != b && !on_interior(v, a, b)))
        });
        if legs_clear && corners_clear {
            return Some(path);
        }
    }
    None
}

impl Editor {
    /// Symbol → Make schematic and symbol from selected components: the selected schematic
    /// objects (ports excepted: they stay the parent's interface) move into a new cell
    /// `<cell>_sub<k>.shcomp` beside this one, with a port where each cut net touched the
    /// rest and a symbol made from those ports (inputs left, outputs and inouts right). An
    /// instance of it replaces the selection, placed where none of its pins touches the
    /// remaining geometry, and each pin is wired to every point where its net's outside part
    /// touched the moved objects, by wires that touch no other net; where no such wire exists,
    /// or the parts met by name only, a label with the net's name joins them. One undo here;
    /// the new file is written at once.
    pub(super) fn make_subcircuit(&mut self, reqs: &mut Vec<EditRequest>, h: DocHandle) {
        let sel: Vec<ObjKey> = self
            .selection()
            .iter()
            .copied()
            .filter(|k| {
                matches!(
                    k.table,
                    TableId::Instances
                        | TableId::Wires
                        | TableId::Labels
                        | TableId::Noconns
                        | TableId::Texts
                        | TableId::Shapes
                )
            })
            .collect();
        if sel.is_empty() {
            self.session.status = "select the components to move into a new cell".into();
            return;
        }
        let Some(path) = self.session.path(h).map(Path::to_path_buf) else {
            return;
        };
        let centre = self.selection_pivot();
        let width = self.opts.symbol_width;
        let made = {
            let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
                self.session.status = "the cell is not elaborated".into();
                return;
            };
            let n = &cell.nets;
            let stale = cell.inst_iface.len() != doc.instances().id.len()
                || n.wire_net.len() != doc.wires().id.len()
                || n.label_net.len() != doc.labels().id.len()
                || n.port_net.len() != doc.ports().id.len();
            if stale {
                self.session.status = "the cell is not elaborated".into();
                return;
            }
            let mut ids = Ids::new(&mut self.rng);
            plan(doc, cell, &sel, &path, centre, width, &mut ids)
        };
        let (file, text, ops, ports) = match made {
            Ok(m) => m,
            Err(e) => {
                self.session.status = format!("make subcircuit: {e}");
                return;
            }
        };
        if let Err(e) = platform::write_atomic(&file, text.as_bytes()) {
            self.session.status = e;
            return;
        }
        self.session.reindex(&file);
        self.symbols = scene::Symbols::default();
        self.symbols_key = None;
        self.invalidate_cells();
        self.scene_dirty = true; // cell rows shifted with the re-index: re-stamp every instance
        self.commit(reqs, h, "make subcircuit", ops);
        self.set_selection(Vec::new());
        self.set_partial(Vec::new());
        self.session.status = format!("new cell {} with {ports} port(s)", file.display());
    }
}

/// What `make_subcircuit` writes and commits: (new file, its text, the parent's ops, port
/// count).
type Plan = (PathBuf, String, Vec<Op>, usize);

/// Plans the subcircuit of `sel` in `doc` (see [`Editor::make_subcircuit`]); nothing is
/// written.
///
/// # Errors
///
/// No free file name, the new cell's text does not parse, or no place for the instance.
fn plan(
    doc: &Document,
    cell: &CellState,
    sel: &[ObjKey],
    path: &Path,
    centre: Pt,
    width: i32,
    ids: &mut Ids<'_>,
) -> Result<Plan, String> {
    assert!(!sel.is_empty());
    assert!(
        doc.kind() != FileKind::Shprim,
        "a primitive has no schematic to cut"
    );
    let selset: HashSet<ObjKey> = sel.iter().copied().collect();
    let (cuts, outside) = crossings(doc, cell, &selset);
    // The new cell file beside this one.
    let stem = path
        .file_stem()
        .map_or_else(|| "cell".to_owned(), |s| s.to_string_lossy().into_owned());
    let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    let file = (1..100)
        .map(|k| dir.join(format!("{stem}_sub{k}.shcomp")))
        .find(|p| !platform::exists(p))
        .ok_or_else(|| format!("{stem}_sub*.shcomp: no free name"))?;
    let sub_stem = file
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    // Its symbol: ports oriented by direction, so inputs land left, the rest right.
    let ports: Vec<(String, Orient, Pt)> = cuts
        .iter()
        .map(|c| {
            let o = match c.dir {
                PinDir::In => Orient::R0,
                PinDir::Out => Orient::R180,
                PinDir::Inout => Orient::R90,
            };
            (c.name.clone(), o, c.port_at)
        })
        .collect();
    let symbol = symbol_records(&ports, width);
    let local: Vec<(String, Pt)> = symbol
        .iter()
        .filter(|r| r.kind == RecordKind::Pin)
        .filter_map(|r| {
            let x = schemify_format::parse_coord(&r.fields[2]).ok()?;
            let y = schemify_format::parse_coord(&r.fields[3]).ok()?;
            Some((r.fields[0].clone(), (x, y)))
        })
        .collect();
    assert_eq!(local.len(), cuts.len(), "one pin per crossing");
    let mut frag: Vec<RecordBuf> = symbol;
    frag.extend(
        edit::selected_records(doc, sel)
            .into_iter()
            .map(|(_, _, r)| r)
            .filter(|r| r.kind.rank() == 3),
    );
    for (name, o, at) in &ports {
        let (id, _) = ids.draw(doc);
        frag.push(edit::placed(RecordKind::Port, *at, *o, name, &id));
    }
    // A clipboard fragment is a `.shcomp` with one schematic impl: the new cell's file.
    let text =
        crate::clip::encode(&frag).replacen("impl clipboard schematic", "impl sch schematic", 1);
    let text = match schemify_format::parse(text.as_bytes()) {
        Ok(d) => schemify_format::write(&d),
        Err(e) => return Err(format!("new cell: {:?}", e.diagnostics.first())),
    };
    // The instance where its pins touch nothing that remains.
    let pin_local: Vec<Pt> = local.iter().map(|p| p.1).collect();
    let at = free_spot(centre, &pin_local, &outside)
        .ok_or("no place near the selection where the new instance's pins touch nothing")?;
    let pin_of = |name: &str| {
        local
            .iter()
            .find(|p| p.0 == name)
            .map(|p| (p.1.0 + at.0, p.1.1 + at.1))
            .expect("every crossing has its pin")
    };
    let mut ops = edit::delete(doc, sel);
    let name = edit::unique_name(doc, "X", &mut HashSet::new());
    let (id, _) = ids.draw(doc);
    ops.push(Op::Insert {
        rec: edit::instance(&format!("./{sub_stem}"), at, Orient::R0, &name, &id),
        at: None,
    });
    // What a new wire must not touch unless it is of the same net: the remaining points and
    // wires, every new pin and contact, then each wire accepted so far.
    let mut pts: Vec<(u32, Pt)> = outside.pts.clone();
    let mut segs: Vec<(u32, Pt, Pt)> = outside.segs.clone();
    for c in &cuts {
        pts.push((c.net, pin_of(&c.name)));
        pts.extend(c.contacts.iter().map(|&p| (c.net, p)));
    }
    let mut label_at: Vec<(Pt, &str)> = Vec::new();
    for c in &cuts {
        let q = pin_of(&c.name);
        if c.contacts.is_empty() {
            label_at.push((q, c.name.as_str())); // the parts met by name: the pin joins by name too
        }
        for &p in &c.contacts {
            match clear_route(q, p, c.net, &pts, &segs) {
                Some(path) => {
                    for l in path.windows(2) {
                        segs.push((c.net, l[0], l[1]));
                        pts.push((c.net, l[1]));
                    }
                    ops.extend(edit::wires(doc, &path, ids).0);
                }
                None => {
                    label_at.push((q, c.name.as_str()));
                    label_at.push((p, c.name.as_str()));
                }
            }
        }
    }
    label_at.sort_unstable();
    label_at.dedup();
    for (p, net) in label_at {
        let (id, _) = ids.draw(doc);
        ops.push(Op::Insert {
            rec: edit::placed(RecordKind::Label, p, Orient::R0, net, &id),
            at: None,
        });
    }
    assert!(
        ops.iter().any(|o| matches!(o, Op::Insert { .. })),
        "the instance replaces the selection"
    );
    Ok((file, text, ops, cuts.len()))
}
