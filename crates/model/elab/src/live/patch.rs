//! Live connectivity's patch (D39): the ops `doc` just applied and the document after them
//! in, [`CellNets`] patched in place and the connectivity findings the edit removed and
//! added out. The algorithm is in the parent module's header; component and bit
//! bookkeeping is `comps`'.
//!
//! Contents:
//! - `Live::update` (`pub(crate)`); `Live::write_row`, `Live::enter`, `Live::index_wire`
//!   (`pub(super)`)

use super::{
    Ctx, Delta, INSTS, Kind, LABELS, Live, NOCONNS, PORTS, WIRES, changed_rows, geometric,
    table_ids,
};
use crate::names::{Name, Names};
use crate::netcheck::port_findings;
use crate::nets::{CellNets, find, label_is_global, on_segment, place, union};
use crate::values::NONE;
use schemify_doc::{Document, ObjId, Op};
use schemify_format::RecordKind;
use std::collections::HashMap;

/// The objects one edit touched, per geometry table (`WIRES`..`NOCONNS`, `INSTS` for an
/// instance's `at`).
struct Rows {
    /// (table, first node) of objects gone from the document.
    removed: Vec<(usize, u32)>,
    /// (table, first node, new row) of objects the document moved to another row.
    moved: Vec<(usize, u32, u32)>,
    /// (table, row) of new objects.
    added: Vec<(usize, u32)>,
    /// (table, row) of objects that stayed but whose content an op changed, sorted.
    modified: Vec<(usize, u32)>,
}

impl Live {
    /// Patches `cn` for `ops`, already applied to `cx.doc`. Instance ops count only as
    /// `at` attributes: the caller rebuilds for anything that moves instance rows or
    /// changes element names, `ignore` or `inherit`. Returns the findings removed and
    /// added, or `None` when the edit needs a full build (then `self` and `cn` are garbage
    /// until it runs).
    ///
    /// Cost: O(touched components and their bits) plus O(log n) per touched node, plus
    /// one vectorized compare per wire, label, port and noconn row (the id diff).
    pub(crate) fn update(
        &mut self,
        ops: &[Op],
        cx: &Ctx<'_>,
        cn: &mut CellNets,
        names: &mut Names,
    ) -> Option<Delta> {
        if self.merged
            || self.dead > self.x.len() / 2 + (1 << 16)
            || cn.net_bits.len() > 2 * self.live_bits + (1 << 16)
        {
            return None; // names merge, or compact by rebuilding
        }
        let doc = cx.doc;
        assert_eq!(cx.inst_iface.len(), doc.instances().id.len());
        assert_eq!(
            self.first[INSTS].len(),
            cx.inst_iface.len(),
            "instance rows fixed"
        );

        // ---- what changed: removed, moved, added and modified objects
        let Rows {
            removed,
            moved,
            added,
            modified,
        } = self.diff(ops, doc);
        let global = |&(t, r): &(usize, u32)| t == LABELS && label_is_global(doc, r as usize);
        let label = |&(t, _): &(usize, u32)| t == LABELS;
        let labels = removed.iter().any(label) || added.iter().any(label);
        if added.iter().any(global)
            || modified.iter().any(global)
            || (self.global_labels > 0 && (labels || modified.iter().any(label)))
        {
            return None; // the global name set may change
        }

        // ---- removed objects: dead nodes, dirty components
        self.begin();
        let mut olds = Vec::new();
        for &(t, first) in &removed {
            if t == WIRES {
                self.unindex_wire(first);
            }
            for v in first..first + if t == WIRES { 2 } else { 1 } {
                olds.push(self.comp[v as usize]);
                self.unindex(v);
                self.comp[v as usize] = NONE;
                self.dead += 1;
            }
        }
        for c in olds {
            self.make_dirty(c, cx, cn, names);
        }

        // ---- row mirrors and row outputs
        for t in 0..INSTS {
            let len = table_ids(doc, t).len();
            self.ids[t].resize(len, NONE);
            self.first[t].resize(len, NONE);
            match t {
                WIRES => cn.wire_net.resize(len, NONE),
                LABELS => cn.label_net.resize(len, NONE),
                PORTS => cn.port_net.resize(len, NONE),
                _ => {}
            }
        }
        for &(t, first, row) in &moved {
            self.ids[t][row as usize] = table_ids(doc, t)[row as usize].0;
            self.first[t][row as usize] = first;
            for v in first..first + if t == WIRES { 2 } else { 1 } {
                self.row[v as usize] = row;
            }
            self.write_row(first, cn);
        }

        // ---- added objects: new nodes, in the region
        for &(t, row) in &added {
            let first = self.x.len() as u32;
            let kind = [Kind::Wire, Kind::Label, Kind::Port, Kind::Noconn][t];
            for sub in 0..if t == WIRES { 2 } else { 1 } {
                self.x.push(0);
                self.y.push(0);
                self.kind.push(kind);
                self.row.push(row);
                self.sub.push(sub);
                self.comp.push(NONE);
                self.next.push(NONE);
                self.mark.push(0);
                self.uf.push(0);
                self.enter(first + u32::from(sub));
            }
            self.ids[t][row as usize] = table_ids(doc, t)[row as usize].0;
            self.first[t][row as usize] = first;
            self.place(t, row as usize, cx);
        }

        // ---- changed objects: same nodes, new geometry or text
        for &(t, row) in &modified {
            let first = self.first[t][row as usize];
            let count = match t {
                WIRES => 2,
                INSTS => cx
                    .ifaces
                    .get(cx.inst_iface[row as usize] as usize)
                    .map_or(0, |f| geometric(f).count() as u32),
                _ => 1,
            };
            if count == 0 {
                continue;
            }
            if t == WIRES {
                self.unindex_wire(first);
            }
            for v in first..first + count {
                self.unindex(v);
                let c = self.comp[v as usize];
                self.make_dirty(c, cx, cn, names);
            }
            self.place(t, row as usize, cx);
        }

        // ---- the region: joined, split into pieces, each rebuilt as a component
        if !self.rebuild_region(cx, cn, names) {
            return None; // names short two nets here (E003): rebuild
        }

        // ---- ports, then the bits touched
        let port = |&(t, _): &(usize, u32)| t == PORTS;
        if removed.iter().any(port) || added.iter().any(port) || modified.iter().any(port) {
            self.gone.append(&mut self.ports);
            port_findings(doc, cx.own, names, &mut self.ports);
            self.new.extend(self.ports.iter().cloned());
        }
        let mut rows = Vec::new();
        for k in 0..self.touched.len() {
            let b = self.touched[k] as usize;
            if cn.bit_name[b] != Name::NONE {
                self.bit_rows(b, cn, cx.own, names, &mut rows);
            }
        }
        self.new.append(&mut rows);
        if cfg!(debug_assertions) && self.x.len() < 1 << 14 {
            self.check(doc, cn);
        }
        Some((
            std::mem::take(&mut self.gone),
            std::mem::take(&mut self.new),
        ))
    }

    /// Writes the row output (`wire_net`, `label_net`, `port_net`) of the object whose
    /// first node is `first`.
    pub(super) fn write_row(&self, first: u32, cn: &mut CellNets) {
        let (v, c) = (first as usize, self.comp[first as usize]);
        let r = self.row[v] as usize;
        match self.kind[v] {
            Kind::Wire => cn.wire_net[r] = c,
            Kind::Label => cn.label_net[r] = c,
            Kind::Port => cn.port_net[r] = c,
            Kind::Pin | Kind::Noconn => {}
        }
    }

    pub(super) fn enter(&mut self, v: u32) {
        debug_assert!(self.mark[v as usize] != self.epoch, "entered once");
        self.mark[v as usize] = self.epoch;
        self.uf[v as usize] = v;
        self.region.push(v);
    }

    pub(super) fn index_wire(&mut self, first: u32) {
        let (x1, y1, x2, y2) = self.ends(first);
        if x1 == x2 {
            self.vw.insert((x1, y1.min(y2), y1.max(y2), first));
        } else if y1 == y2 {
            self.hw.insert((y1, x1.min(x2), x1.max(x2), first));
        } else {
            self.dw.push(first);
        }
    }
}

// ---- private ----

impl Live {
    /// The objects `ops` (applied to `doc`) removed, moved, added or modified, from a diff
    /// of each geometry table's id column plus the records the ops name.
    fn diff(&self, ops: &[Op], doc: &Document) -> Rows {
        let s = doc.strings();
        // Records named by content ops.
        let mut named: Vec<(usize, ObjId)> = Vec::new();
        for op in ops {
            let (kind, key, attr) = crate::state::subject(op);
            let t = match kind {
                RecordKind::Wire => WIRES,
                RecordKind::Label => LABELS,
                RecordKind::Port => PORTS,
                RecordKind::Noconn => NOCONNS,
                RecordKind::Inst if attr == Some("at") => INSTS,
                _ => continue,
            };
            if !matches!(op, Op::Insert { .. }) {
                named.extend(ObjId::find(key, s).map(|id| (t, id)));
            }
        }

        // (table, id, first node) displaced from a row; (table, id, row) now in a row.
        let mut gone: Vec<(usize, u32, u32)> = Vec::new();
        let mut came: Vec<(usize, u32, u32)> = Vec::new();
        let mut rows = Vec::new();
        // A rejected transaction permutes rows without ops reaching here (its replayed
        // `delete` inverse re-inserts at the end), so every table is diffed, every time.
        for t in 0..INSTS {
            let ids = table_ids(doc, t);
            rows.clear();
            changed_rows(&self.ids[t], ids, &mut rows);
            for &j in &rows {
                if j < self.ids[t].len() {
                    gone.push((t, self.ids[t][j], self.first[t][j]));
                }
                if j < ids.len() {
                    came.push((t, ids[j].0, j as u32));
                }
            }
        }
        gone.sort_unstable();
        came.sort_unstable();
        // Same (table, id) on both sides: a row move (doc swap-removes); else removed/added.
        let (mut removed, mut moved, mut added) = (Vec::new(), Vec::new(), Vec::new());
        let (mut a, mut b) = (0, 0);
        while a < gone.len() || b < came.len() {
            let ka = gone.get(a).map(|g| (g.0, g.1));
            let kb = came.get(b).map(|c| (c.0, c.1));
            if ka.is_some() && ka == kb {
                moved.push((gone[a].0, gone[a].2, came[b].2));
                (a, b) = (a + 1, b + 1);
            } else if kb.is_none() || ka.is_some_and(|x| Some(x) < kb) {
                removed.push((gone[a].0, gone[a].2));
                a += 1;
            } else {
                added.push((came[b].0, came[b].2));
                b += 1;
            }
        }
        // Records named by content ops that stayed (an added one is placed fresh anyway).
        let mut modified: Vec<(usize, u32)> = Vec::new();
        for &(t, id) in &named {
            let Some(loc) = doc.find_id(id) else {
                continue; // deleted: the diff has it
            };
            if !added.contains(&(t, loc.row)) {
                modified.push((t, loc.row));
            }
        }
        modified.sort_unstable();
        modified.dedup();
        assert!(modified.len() <= named.len());
        Rows {
            removed,
            moved,
            added,
            modified,
        }
    }

    /// Joins the region over `connect`'s rules (§3) and rebuilds each piece as a component.
    /// `false` when two names short (`E003`): only a full build merges bits by name.
    fn rebuild_region(&mut self, cx: &Ctx<'_>, cn: &mut CellNets, names: &mut Names) -> bool {
        // Flood: a clean component the region reaches is merged in.
        let mut nb: Vec<u32> = Vec::new();
        let mut i = 0;
        while i < self.region.len() {
            assert!(
                self.region.len() <= self.x.len(),
                "a node enters the region once"
            );
            let v = self.region[i];
            i += 1;
            nb.clear();
            self.neighbors(v, cx, &mut nb);
            for &m in &nb {
                if self.mark[m as usize] != self.epoch {
                    let c = self.comp[m as usize];
                    assert!(c != NONE, "an indexed node is live");
                    self.make_dirty(c, cx, cn, names);
                }
                union(&mut self.uf, v, m);
            }
        }

        // Pieces → components, in region order (deterministic).
        let mut piece: HashMap<u32, usize> = HashMap::new();
        let mut pieces: Vec<Vec<u32>> = Vec::new();
        for k in 0..self.region.len() {
            let v = self.region[k];
            let r = find(&mut self.uf, v);
            let at = *piece.entry(r).or_insert_with(|| {
                pieces.push(Vec::new());
                pieces.len() - 1
            });
            pieces[at].push(v);
        }
        for members in &pieces {
            let c = self.alloc_comp(cn);
            if !self.build_comp(c, members, cx, cn, names) {
                return false;
            }
        }
        debug_assert!(self.region.iter().all(|&v| self.comp[v as usize] != NONE));
        true
    }

    /// Asserts the mirrors, node rows, member lists and row outputs agree with `doc`
    /// (debug builds, small cells: O(n)).
    fn check(&self, doc: &Document, cn: &CellNets) {
        for t in 0..INSTS {
            let ids = table_ids(doc, t);
            assert_eq!(self.ids[t].len(), ids.len(), "table {t} length");
            for r in 0..ids.len() {
                assert_eq!(self.ids[t][r], ids[r].0, "table {t} row {r} id");
                let f = self.first[t][r] as usize;
                assert_eq!(self.row[f] as usize, r, "table {t} row {r} node row");
                let c = self.comp[f];
                assert!(
                    c != NONE && self.head[c as usize] != NONE,
                    "table {t} row {r} live"
                );
                let out = match t {
                    WIRES => cn.wire_net[r],
                    LABELS => cn.label_net[r],
                    PORTS => cn.port_net[r],
                    _ => c,
                };
                assert_eq!(out, c, "table {t} row {r} output");
            }
        }
        for c in 0..self.head.len() {
            let head = self.head[c];
            if head == NONE {
                continue;
            }
            let mut v = head;
            loop {
                assert_eq!(self.comp[v as usize], c as u32, "member of {c}");
                v = self.next[v as usize];
                if v == head {
                    break;
                }
            }
        }
        assert_eq!(self.by_x.len(), self.by_y.len());
    }

    /// Starts an edit: a fresh stamp and empty scratch.
    fn begin(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.mark.fill(0);
            self.cmark.fill(0);
            self.bmark.fill(0);
            self.epoch = 1;
        }
        self.touched.clear();
        self.region.clear();
        self.gone.clear();
        self.new.clear();
    }

    /// Places the nodes of table `t`'s object at `row` from the document and indexes them.
    fn place(&mut self, t: usize, row: usize, cx: &Ctx<'_>) {
        let doc = cx.doc;
        let first = self.first[t][row];
        let at = |lv: &mut Live, v: u32, (x, y): (i32, i32)| {
            lv.x[v as usize] = x;
            lv.y[v as usize] = y;
            lv.index(v);
        };
        match t {
            WIRES => {
                let w = doc.wires();
                at(self, first, (w.x1[row], w.y1[row]));
                at(self, first + 1, (w.x2[row], w.y2[row]));
                self.index_wire(first);
            }
            LABELS => at(self, first, (doc.labels().x[row], doc.labels().y[row])),
            PORTS => at(self, first, (doc.ports().x[row], doc.ports().y[row])),
            NOCONNS => at(self, first, (doc.noconns().x[row], doc.noconns().y[row])),
            _ => {
                let f = &cx.ifaces[cx.inst_iface[row] as usize];
                let inst = doc.instances();
                let (m, q) = (
                    inst.orient[row].mirrored(),
                    inst.orient[row].quarter_turns(),
                );
                for (k, p) in geometric(f).enumerate() {
                    let xy = place(f.pin_x[p], f.pin_y[p], m, q, inst.x[row], inst.y[row]);
                    at(self, first + k as u32, xy);
                }
            }
        }
    }

    fn index(&mut self, v: u32) {
        let (x, y) = (self.x[v as usize], self.y[v as usize]);
        assert!(self.by_x.insert((x, y, v)) && self.by_y.insert((y, x, v)));
    }

    fn unindex(&mut self, v: u32) {
        let (x, y) = (self.x[v as usize], self.y[v as usize]);
        assert!(self.by_x.remove(&(x, y, v)) && self.by_y.remove(&(y, x, v)));
    }

    /// The wire whose ends are nodes `first`, `first + 1`: `(x1, y1, x2, y2)`.
    fn ends(&self, first: u32) -> (i32, i32, i32, i32) {
        let (a, b) = (first as usize, first as usize + 1);
        (self.x[a], self.y[a], self.x[b], self.y[b])
    }

    fn unindex_wire(&mut self, first: u32) {
        let (x1, y1, x2, y2) = self.ends(first);
        if x1 == x2 {
            assert!(self.vw.remove(&(x1, y1.min(y2), y1.max(y2), first)));
        } else if y1 == y2 {
            assert!(self.hw.remove(&(y1, x1.min(x2), x1.max(x2), first)));
        } else {
            let at = self.dw.iter().position(|&w| w == first).expect("indexed");
            self.dw.swap_remove(at);
        }
    }

    /// Appends every node joined to `v` by one rule of `connect` (§3): the same point, the
    /// other end and the nodes on the wire `v` ends, the ends of the wires through `v`, and
    /// alias pins.
    fn neighbors(&self, v: u32, cx: &Ctx<'_>, out: &mut Vec<u32>) {
        let vi = v as usize;
        let (x, y) = (self.x[vi], self.y[vi]);
        let same = self.by_x.range((x, y, 0)..=(x, y, u32::MAX));
        out.extend(same.map(|e| e.2).filter(|&m| m != v));
        match self.kind[vi] {
            Kind::Wire => {
                let first = v - u32::from(self.sub[vi]);
                self.on_wire(first, out);
            }
            Kind::Pin => {
                let i = self.row[vi] as usize;
                let f = &cx.ifaces[cx.inst_iface[i] as usize];
                let p = usize::from(self.sub[vi]);
                let first = self.first[INSTS][i];
                for (k, q) in geometric(f).enumerate() {
                    if q != p && (f.pin_alias[p] == q as u32 || f.pin_alias[q] == p as u32) {
                        out.push(first + k as u32);
                    }
                }
            }
            Kind::Label | Kind::Port | Kind::Noconn => {}
        }
        // ponytail: the wires through a point scan their row (column) up to the point; an
        // interval tree if long runs of collinear wires show up in profiles.
        let row = (y, i32::MIN, i32::MIN, 0)..=(y, x, i32::MAX, u32::MAX);
        for &(_, _, xb, w) in self.hw.range(row) {
            if xb >= x {
                out.extend([w, w + 1]);
            }
        }
        let col = (x, i32::MIN, i32::MIN, 0)..=(x, y, i32::MAX, u32::MAX);
        for &(_, _, yb, w) in self.vw.range(col) {
            if yb >= y {
                out.extend([w, w + 1]);
            }
        }
        for &w in &self.dw {
            let (x1, y1, x2, y2) = self.ends(w);
            if on_segment(x, y, x1, y1, x2, y2) {
                out.extend([w, w + 1]);
            }
        }
    }

    /// Appends the nodes lying on the wire whose first node is `first`, its ends included.
    fn on_wire(&self, first: u32, out: &mut Vec<u32>) {
        let (x1, y1, x2, y2) = self.ends(first);
        if x1 == x2 {
            let r = (x1, y1.min(y2), 0)..=(x1, y1.max(y2), u32::MAX);
            out.extend(self.by_x.range(r).map(|e| e.2));
        } else if y1 == y2 {
            let r = (y1, x1.min(x2), 0)..=(y1, x1.max(x2), u32::MAX);
            out.extend(self.by_y.range(r).map(|e| e.2));
        } else {
            // ponytail: as in `connect`, a diagonal wire scans every node in its x-range.
            let r = (x1.min(x2), i32::MIN, 0)..=(x1.max(x2), i32::MAX, u32::MAX);
            let on = self
                .by_x
                .range(r)
                .filter(|e| on_segment(e.0, e.1, x1, y1, x2, y2));
            out.extend(on.map(|e| e.2));
        }
    }
}
