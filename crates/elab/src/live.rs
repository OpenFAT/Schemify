//! Live connectivity of an open document (D39): what lets [`crate::CellState`] patch its
//! [`CellNets`] per edit, recomputing only the components and bits the edit touches.
//!
//! Six questions:
//! 1. In: the ops `doc` just applied and the document after them; the previous
//!    [`CellNets`]. Out: the same [`CellNets`] patched in place, and the connectivity
//!    findings the edit removed and added.
//! 2. A 100k-object cell has ~300k nodes and ~70k components; an edit touches a handful of
//!    objects, the components on them and the bits of those components.
//! 3. Coordinates i32 milli-units (D33); node, component and bit handles `u32` (2^22
//!    objects per kind, a few nodes each); pin index `u16`; widths ≤ 2^16 − 1.
//! 4. Point and segment queries on the touched nodes only: ordered sets of nodes by
//!    `(x, y)` and `(y, x)`, ordered sets of axis-aligned wires by row and column. Node
//!    columns are SoA, read per node of a touched component.
//! 5. Lifetime: the open document; seeded by every full build (`connect`), so a fallback
//!    is always a full build.
//! 6. One document's edit is small and serial: nothing to split.
//!
//! The algorithm: an edit names the objects it removes, adds or changes (a diff of each
//! table's id column, plus the records the ops name). Every component holding one
//! of their nodes is dirty: its findings and the counts its pins gave its bits are taken
//! out and its handle is freed. The region is the surviving nodes of the dirty components
//! plus the new nodes; a flood over point and segment queries joins the region, and a
//! clean component it reaches becomes dirty too (a merge). Each piece of the region is
//! rebuilt as a component with the rules of `connect`, and the bits it touched are
//! re-checked. Untouched components keep their handle, name and bits.
//!
//! Bits: with one name per bit (no `E003` short, no `ignore short`), a named bit is the
//! set of slots carrying its name, so it lives while any slot carries it and keeps its
//! handle; an unnamed bit belongs to one component slot. Cells where names merge more
//! than that are rebuilt per edit (`merged`), as are label edits in a cell with
//! `global true` labels; the caller rebuilds anything that moves instance rows or
//! changes element names.
//!
//! SIMD triage: the id-column diff is the one loop over whole tables; it is an
//! accumulator (xor-or per chunk) and vectorizes as written. Everything else walks the
//! few touched nodes.

use crate::check::{Code, Violation};
use crate::connect::{
    CellNets, Ignore, NO_NET, NONE_PIN, OWN_BITS, OWN_PORTS, OWN_STATIC, Seed, bit_findings,
    id_cmp, place, port_findings, push_decimal, unkey, write_key,
};
use crate::iface::{Domain, Iface};
use crate::names::{Name, Names};
use crate::netexpr::{self, is_global};
use crate::values::NONE;
use schemify_doc::{Document, ObjId, Op};
use schemify_format::RecordKind;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

/// What a node is: a wire end, an instance pin, a label, port or noconn anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Kind {
    Wire,
    Pin,
    Label,
    Port,
    Noconn,
}

/// Row-mirrored tables, in this order.
const WIRES: usize = 0;
const LABELS: usize = 1;
const PORTS: usize = 2;
const NOCONNS: usize = 3;
const INSTS: usize = 4;

/// Name classes of `connect` (§3): globals win, then ports, then labels.
const GLOBAL: u8 = 0;
const PORT: u8 = 1;
const LABEL: u8 = 2;

/// Findings an edit removed and added (`connect` codes only), unordered.
pub(crate) type Delta = (Vec<Violation>, Vec<Violation>);

/// The document and the interfaces an update reads.
pub(crate) struct Ctx<'a> {
    pub(crate) doc: &'a Document,
    pub(crate) own: &'a Iface,
    pub(crate) ifaces: &'a [Iface],
    pub(crate) inst_iface: &'a [u32],
}

/// The incremental state. See the module docs.
#[derive(Debug, Default)]
pub(crate) struct Live {
    // ---- nodes (stable handles; a dead node has `comp == NONE` and is in no index)
    x: Vec<i32>,
    y: Vec<i32>,
    kind: Vec<Kind>,
    /// Document row of the owning object.
    row: Vec<u32>,
    /// Wire end (0, 1) or interface pin index.
    sub: Vec<u16>,
    comp: Vec<u32>,
    /// Circular list of each component's nodes.
    next: Vec<u32>,
    /// Region stamp of the current edit.
    mark: Vec<u32>,
    /// Union-find parent while the region is joined.
    uf: Vec<u32>,
    dead: usize,

    // ---- spatial index
    by_x: BTreeSet<(i32, i32, u32)>,
    by_y: BTreeSet<(i32, i32, u32)>,
    /// Horizontal wires `(y, x min, x max, first node)`, vertical `(x, y min, y max, …)`
    /// (a zero-length wire is vertical, as in `connect`), diagonal first nodes.
    hw: BTreeSet<(i32, i32, i32, u32)>,
    vw: BTreeSet<(i32, i32, i32, u32)>,
    dw: Vec<u32>,

    // ---- document rows: id column mirror and first node, per table
    ids: [Vec<u32>; 5],
    first: [Vec<u32>; 5],
    /// Labels carrying `global true`: any label edit then rebuilds.
    global_labels: usize,
    /// Names made global by `global true` labels (sorted), as `connect` got them.
    globals: Vec<Name>,

    // ---- components
    head: Vec<u32>,
    named: Vec<bool>,
    found: Vec<Vec<Violation>>,
    cmark: Vec<u32>,
    free_comps: Vec<u32>,

    // ---- bits
    /// Slots carrying a named bit's name (component slots, own pin and inherit slots).
    slots: Vec<u32>,
    /// Instance pin bits on the bit in the `real` domain and in the others (`W010`).
    real: Vec<u32>,
    other: Vec<u32>,
    bmark: Vec<u32>,
    free_bits: Vec<u32>,

    // ---- findings no component or bit owns
    fixed: Vec<Violation>,
    ports: Vec<Violation>,
    /// `net_bits` entries of live components (the rest is garbage).
    live_bits: usize,
    empty: Name,
    /// Names merge components beyond one name per bit: never patched.
    merged: bool,
    epoch: u32,

    // ---- per-edit scratch
    touched: Vec<u32>,
    region: Vec<u32>,
    gone: Vec<Violation>,
    new: Vec<Violation>,
}

fn table_ids(doc: &Document, t: usize) -> &[ObjId] {
    match t {
        WIRES => &doc.wires().id,
        LABELS => &doc.labels().id,
        PORTS => &doc.ports().id,
        NOCONNS => &doc.noconns().id,
        _ => &doc.instances().id,
    }
}

/// Appends the rows where `old` and `ids` differ, and the rows only one of them has.
fn changed_rows(old: &[u32], ids: &[ObjId], out: &mut Vec<usize>) {
    const CHUNK: usize = 64;
    let n = old.len().min(ids.len());
    let mut i = 0;
    while i < n {
        let end = (i + CHUNK).min(n);
        // Accumulator shape: xor-or over the chunk, no early exit, so it vectorizes; the
        // scalar scan below is the rare tail that names the rows.
        let diff = old[i..end]
            .iter()
            .zip(&ids[i..end])
            .fold(0u32, |a, (o, d)| a | (o ^ d.0));
        if diff != 0 {
            out.extend((i..end).filter(|&j| old[j] != ids[j].0));
        }
        i = end;
    }
    out.extend(n..old.len().max(ids.len()));
}

/// Whether label row `l` carries `global true`.
fn label_is_global(doc: &Document, l: usize) -> bool {
    let s = doc.strings();
    doc.pool()
        .find_attr(s, doc.labels().attrs[l], "global")
        .and_then(|v| v.first())
        .is_some_and(|v| s.get(*v) == "true")
}

/// Geometric (non-inherited) pins of `f`, in order: an instance's node `k` is the `k`-th.
fn geometric(f: &Iface) -> impl Iterator<Item = usize> + '_ {
    (0..f.pin_name.len()).filter(|&p| f.pin_inherit[p] == Name::NONE)
}

/// Whether `(x, y)` lies on the segment `(x1, y1)`–`(x2, y2)` (ends included).
fn on_segment(x: i32, y: i32, x1: i32, y1: i32, x2: i32, y2: i32) -> bool {
    let (dx, dy) = (i64::from(x2) - i64::from(x1), i64::from(y2) - i64::from(y1));
    let cross = (i64::from(x) - i64::from(x1)) * dy - (i64::from(y) - i64::from(y1)) * dx;
    cross == 0 && (x1.min(x2)..=x1.max(x2)).contains(&x) && (y1.min(y2)..=y1.max(y2)).contains(&y)
}

fn glob(globals: &[Name], names: &Names, n: Name) -> bool {
    is_global(names.get(n)) || globals.binary_search(&n).is_ok()
}

impl Live {
    /// Builds the incremental state from a full pass: `seed` and `found` (its findings,
    /// each owned per `seed.owner`) are `connect_seeded`'s, `cn` its result and `globals`
    /// what it was given.
    ///
    /// Cost: O(n) in nodes (the ordered sets are built from the sorted runs).
    pub(crate) fn seed(
        seed: Seed,
        found: Vec<Violation>,
        globals: Vec<Name>,
        cn: &CellNets,
        cx: &Ctx<'_>,
        names: &mut Names,
    ) -> Live {
        assert_eq!(found.len(), seed.owner.len(), "one owner per finding");
        let n = seed.px.len();
        let nc = cn.net_name.len();
        let doc = cx.doc;
        let mut lv = Live {
            empty: names.intern(""),
            merged: seed.merged,
            live_bits: cn.net_bits.len(),
            globals,
            ..Live::default()
        };
        lv.found = vec![Vec::new(); nc];
        for (v, o) in found.into_iter().zip(&seed.owner) {
            match *o {
                OWN_STATIC => lv.fixed.push(v),
                OWN_PORTS => lv.ports.push(v),
                OWN_BITS => {} // recomputed from the counts
                c => lv.found[c as usize].push(v),
            }
        }
        lv.real = seed.real;
        lv.other = seed.other;
        lv.bmark = vec![0; cn.bit_name.len()];
        lv.slots = vec![0; cn.bit_name.len()];
        lv.named = seed.named;
        lv.cmark = vec![0; nc];
        for c in (0..nc).filter(|&c| lv.named[c]) {
            for &b in cn.bits_of(c) {
                lv.slots[b as usize] += 1;
            }
        }
        for &b in cn.port_bits.iter().chain(&cn.inh_bits) {
            lv.slots[b as usize] += 1;
        }
        for t in 0..5 {
            lv.ids[t] = table_ids(doc, t).iter().map(|id| id.0).collect();
        }
        if lv.merged {
            return lv; // never patched: only the findings and id mirrors are kept
        }

        // ---- nodes in `connect`'s order
        lv.kind = Vec::with_capacity(n);
        lv.row = Vec::with_capacity(n);
        lv.sub = Vec::with_capacity(n);
        for v in 0..seed.pins0 {
            lv.kind.push(Kind::Wire);
            lv.row.push((v / 2) as u32);
            lv.sub.push((v % 2) as u16);
        }
        for k in 0..seed.labels0 - seed.pins0 {
            lv.kind.push(Kind::Pin);
            lv.row.push(seed.pin_inst[k]);
            lv.sub.push(seed.pin_pin[k]);
        }
        for (kind, a, b) in [
            (Kind::Label, seed.labels0, seed.ports0),
            (Kind::Port, seed.ports0, seed.noconns0),
            (Kind::Noconn, seed.noconns0, n),
        ] {
            for v in a..b {
                lv.kind.push(kind);
                lv.row.push((v - a) as u32);
                lv.sub.push(0);
            }
        }
        lv.comp = seed.comp;
        lv.head = vec![NONE; nc];
        let mut tail = vec![NONE; nc];
        lv.next = vec![NONE; n];
        for v in 0..n {
            let c = lv.comp[v] as usize;
            if lv.head[c] == NONE {
                lv.head[c] = v as u32;
            } else {
                lv.next[tail[c] as usize] = v as u32;
            }
            tail[c] = v as u32;
        }
        for c in 0..nc {
            lv.next[tail[c] as usize] = lv.head[c];
        }
        lv.mark = vec![0; n];
        lv.uf = vec![0; n];

        // ---- index: the sorted runs build the sets in O(n)
        let decode = |&(k, v): &(u64, u32)| {
            let (a, b) = unkey(k);
            (a, b, v)
        };
        lv.by_x = seed.xy.iter().map(decode).collect();
        lv.by_y = if seed.yx.len() == n {
            seed.yx.iter().map(decode).collect()
        } else {
            (0..n).map(|v| (seed.py[v], seed.px[v], v as u32)).collect()
        };
        lv.x = seed.px;
        lv.y = seed.py;
        for w in 0..doc.wires().id.len() {
            lv.index_wire(2 * w as u32);
        }

        // ---- row mirrors
        let len = |t: usize| lv.ids[t].len() as u32;
        lv.first[WIRES] = (0..len(WIRES)).map(|w| 2 * w).collect();
        lv.first[LABELS] = (0..len(LABELS)).map(|l| seed.labels0 as u32 + l).collect();
        lv.first[PORTS] = (0..len(PORTS)).map(|p| seed.ports0 as u32 + p).collect();
        lv.first[NOCONNS] = (0..len(NOCONNS))
            .map(|k| seed.noconns0 as u32 + k)
            .collect();
        lv.first[INSTS] = cn.pin_start[..cn.pin_start.len() - 1]
            .iter()
            .map(|&p| seed.pins0 as u32 + p)
            .collect();
        lv.global_labels = (0..doc.labels().id.len())
            .filter(|&l| label_is_global(doc, l))
            .count();
        assert_eq!(lv.kind.len(), n);
        assert_eq!(lv.first[INSTS].len(), doc.instances().id.len());
        lv
    }

    /// Whether `doc`'s instance rows differ from the ones this state was built on (a
    /// rejected transaction permutes them without ops): row-indexed state is then stale.
    ///
    /// Cost: one compare per instance row (vectorized).
    pub(crate) fn instances_moved(&self, doc: &Document) -> bool {
        let mut rows = Vec::new();
        changed_rows(&self.ids[INSTS], &doc.instances().id, &mut rows);
        !rows.is_empty()
    }

    /// Every connectivity finding, owner by owner (what a full check pass starts from).
    pub(crate) fn rows(&self, cn: &CellNets, own: &Iface, names: &Names) -> Vec<Violation> {
        let mut out = self.fixed.clone();
        for f in &self.found {
            out.extend(f.iter().cloned());
        }
        for b in 0..cn.bit_name.len() {
            if cn.bit_name[b] != Name::NONE {
                self.bit_rows(b, cn, own, names, &mut out);
            }
        }
        out.extend(self.ports.iter().cloned());
        out
    }

    fn bit_rows(
        &self,
        b: usize,
        cn: &CellNets,
        own: &Iface,
        names: &Names,
        out: &mut Vec<Violation>,
    ) {
        bit_findings(
            own.cell,
            cn.bit_name[b],
            cn.bit_global[b],
            cn.bit_conns[b],
            self.real[b],
            self.other[b],
            names,
            out,
        );
    }

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
        let s = doc.strings();
        assert_eq!(cx.inst_iface.len(), doc.instances().id.len());
        assert_eq!(
            self.first[INSTS].len(),
            cx.inst_iface.len(),
            "instance rows fixed"
        );

        // ---- what changed: tables to diff, records named by content ops
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

        // ---- flood: join the region; a clean component it reaches is merged in
        let mut nb: Vec<u32> = Vec::new();
        let mut i = 0;
        while i < self.region.len() {
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
                self.union(v, m);
            }
        }

        // ---- pieces → components, in region order (deterministic)
        let mut piece: HashMap<u32, usize> = HashMap::new();
        let mut pieces: Vec<Vec<u32>> = Vec::new();
        for k in 0..self.region.len() {
            let v = self.region[k];
            let r = self.find(v);
            let at = *piece.entry(r).or_insert_with(|| {
                pieces.push(Vec::new());
                pieces.len() - 1
            });
            pieces[at].push(v);
        }
        for members in &pieces {
            let c = self.alloc_comp(cn);
            if !self.build_comp(c, members, cx, cn, names) {
                return None; // names short two nets here (E003): rebuild
            }
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
        debug_assert!(self.region.iter().all(|&v| self.comp[v as usize] != NONE));
        if cfg!(debug_assertions) && self.x.len() < 1 << 14 {
            self.check(doc, cn);
        }
        Some((
            std::mem::take(&mut self.gone),
            std::mem::take(&mut self.new),
        ))
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

    /// Writes the row output (`wire_net`, `label_net`, `port_net`) of the object whose
    /// first node is `first`.
    fn write_row(&self, first: u32, cn: &mut CellNets) {
        let (v, c) = (first as usize, self.comp[first as usize]);
        let r = self.row[v] as usize;
        match self.kind[v] {
            Kind::Wire => cn.wire_net[r] = c,
            Kind::Label => cn.label_net[r] = c,
            Kind::Port => cn.port_net[r] = c,
            Kind::Pin | Kind::Noconn => {}
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

    fn index_wire(&mut self, first: u32) {
        let (x1, y1, x2, y2) = self.ends(first);
        if x1 == x2 {
            self.vw.insert((x1, y1.min(y2), y1.max(y2), first));
        } else if y1 == y2 {
            self.hw.insert((y1, x1.min(x2), x1.max(x2), first));
        } else {
            self.dw.push(first);
        }
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

    fn enter(&mut self, v: u32) {
        debug_assert!(self.mark[v as usize] != self.epoch, "entered once");
        self.mark[v as usize] = self.epoch;
        self.uf[v as usize] = v;
        self.region.push(v);
    }

    fn find(&mut self, mut v: u32) -> u32 {
        while self.uf[v as usize] != v {
            let p = self.uf[self.uf[v as usize] as usize];
            self.uf[v as usize] = p;
            v = p;
        }
        v
    }

    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.uf[ra.max(rb) as usize] = ra.min(rb);
        }
    }

    /// Takes component `c` out (once per edit): its findings leave, its pins' counts leave
    /// its bits, its bits are released (a named one when no slot carries its name any
    /// more), its live nodes join the region and its handle is freed.
    fn make_dirty(&mut self, c: u32, cx: &Ctx<'_>, cn: &mut CellNets, names: &Names) {
        let ci = c as usize;
        if self.cmark[ci] == self.epoch {
            return;
        }
        assert!(self.head[ci] != NONE, "a live component");
        self.cmark[ci] = self.epoch;
        self.gone.append(&mut self.found[ci]);
        let (start, width) = (cn.net_bit_start[ci] as usize, usize::from(cn.net_width[ci]));
        for k in start..start + width {
            self.touch(cn.net_bits[k], cx, cn, names);
        }
        let head = self.head[ci];
        let mut v = head;
        loop {
            if self.kind[v as usize] == Kind::Pin {
                self.pin(v, c, false, cx, cn, names);
            }
            if self.comp[v as usize] == c {
                self.enter(v);
            }
            v = self.next[v as usize];
            if v == head {
                break;
            }
        }
        for k in start..start + width {
            let b = cn.net_bits[k];
            if self.named[ci] {
                self.slots[b as usize] -= 1;
                if self.slots[b as usize] > 0 {
                    continue;
                }
            }
            self.free_bit(b, cn);
        }
        self.live_bits -= width;
        cn.net_name[ci] = self.empty;
        cn.net_width[ci] = 0;
        cn.net_global[ci] = false;
        cn.net_pins[ci] = 0;
        cn.net_bit_start[ci] = 0;
        self.head[ci] = NONE;
        self.free_comps.push(c);
    }

    /// Records bit `b`'s findings as gone the first time this edit changes it.
    fn touch(&mut self, b: u32, cx: &Ctx<'_>, cn: &CellNets, names: &Names) {
        let bi = b as usize;
        if self.bmark[bi] == self.epoch {
            return;
        }
        self.bmark[bi] = self.epoch;
        self.touched.push(b);
        if cn.bit_name[bi] != Name::NONE {
            let mut rows = std::mem::take(&mut self.gone);
            self.bit_rows(bi, cn, cx.own, names, &mut rows);
            self.gone = rows;
        }
    }

    /// Adds (`add`) or takes out the connections and domains pin node `v` gives the bits
    /// of component `c` (`connect`'s bus slicing). Adding writes its `conns` and returns
    /// its `E011` when its width fits neither the net nor a slice of it.
    fn pin(
        &mut self,
        v: u32,
        c: u32,
        add: bool,
        cx: &Ctx<'_>,
        cn: &mut CellNets,
        names: &Names,
    ) -> Option<Violation> {
        let i = self.row[v as usize] as usize;
        let p = usize::from(self.sub[v as usize]);
        let f = &cx.ifaces[cx.inst_iface[i] as usize];
        let w = f.bits(p).len();
        let elems = (cn.elem_start[i + 1] - cn.elem_start[i]) as usize;
        let nwidth = usize::from(cn.net_width[c as usize]);
        // An alias pin shares its target's connection slots (same component, same width):
        // the target writes and counts them.
        let alias = f.pin_alias[p] != NONE;
        let (base, stride) = (
            cn.conn_start[i] as usize + f.pin_off[p] as usize,
            f.bits_total as usize,
        );
        let sliced = if nwidth == w {
            false
        } else if nwidth == w * elems {
            true
        } else {
            if !add {
                return None;
            }
            if !alias {
                for e in 0..elems {
                    cn.conns[base + e * stride..base + e * stride + w].fill(NO_NET);
                }
            }
            let detail = format!(
                "pin `{}` ({w} bits × {elems}) on a {nwidth}-bit net",
                names.get(f.pin_name[p])
            );
            return Some(Violation {
                code: Code::E011,
                cell: cx.own.cell,
                object: cx.doc.instances().id[i],
                path: Name::NONE,
                param: f.pin_name[p],
                detail,
            });
        };
        let start = cn.net_bit_start[c as usize] as usize;
        let counted = !alias && cn.ignore[i] != Ignore::Open;
        let real = f.pin_domain[p] == Domain::Real;
        for e in 0..elems {
            for j in 0..w {
                let b = cn.net_bits[start + if sliced { e * w + j } else { j }] as usize;
                let dom = if real {
                    &mut self.real[b]
                } else {
                    &mut self.other[b]
                };
                if add {
                    *dom += 1;
                } else {
                    *dom -= 1;
                }
                if counted {
                    if add {
                        cn.bit_conns[b] += 1;
                        cn.conns[base + e * stride + j] = b as u32;
                    } else {
                        cn.bit_conns[b] -= 1;
                    }
                }
            }
        }
        None
    }

    fn alloc_comp(&mut self, cn: &mut CellNets) -> u32 {
        if let Some(c) = self.free_comps.pop() {
            return c;
        }
        let c = cn.net_name.len() as u32;
        cn.net_name.push(self.empty);
        cn.net_width.push(0);
        cn.net_global.push(false);
        cn.net_pins.push(0);
        cn.net_bit_start.push(0);
        self.head.push(NONE);
        self.named.push(false);
        self.found.push(Vec::new());
        self.cmark.push(self.epoch);
        assert_eq!(self.head.len(), cn.net_name.len());
        c
    }

    /// A fresh bit named `name` (touched first, so a reused handle's old findings leave).
    fn alloc_bit(&mut self, name: Name, cx: &Ctx<'_>, cn: &mut CellNets, names: &Names) -> u32 {
        let b = match self.free_bits.pop() {
            Some(b) => b,
            None => {
                cn.bit_name.push(Name::NONE);
                cn.bit_global.push(false);
                cn.bit_conns.push(0);
                self.slots.push(0);
                self.real.push(0);
                self.other.push(0);
                self.bmark.push(0);
                (cn.bit_name.len() - 1) as u32
            }
        };
        self.touch(b, cx, cn, names);
        let bi = b as usize;
        assert!(cn.bit_name[bi] == Name::NONE && cn.bit_conns[bi] == 0);
        cn.bit_name[bi] = name;
        cn.bit_global[bi] = glob(&self.globals, names, name);
        b
    }

    /// The bit named `name`, made when no slot carried it; counts one more slot.
    fn named_bit(&mut self, name: Name, cx: &Ctx<'_>, cn: &mut CellNets, names: &Names) -> u32 {
        let b = match cn.by_name.binary_search_by_key(&name, |p| p.0) {
            Ok(k) => {
                let b = cn.by_name[k].1;
                self.touch(b, cx, cn, names);
                b
            }
            Err(k) => {
                let b = self.alloc_bit(name, cx, cn, names);
                cn.by_name.insert(k, (name, b));
                b
            }
        };
        self.slots[b as usize] += 1;
        b
    }

    fn free_bit(&mut self, b: u32, cn: &mut CellNets) {
        let bi = b as usize;
        assert!(
            self.real[bi] == 0 && self.other[bi] == 0 && cn.bit_conns[bi] == 0,
            "no pin on a freed bit"
        );
        if let Ok(k) = cn.by_name.binary_search_by_key(&cn.bit_name[bi], |p| p.0)
            && cn.by_name[k].1 == b
        {
            cn.by_name.remove(k);
        }
        cn.bit_name[bi] = Name::NONE;
        cn.bit_global[bi] = false;
        self.slots[bi] = 0;
        self.free_bits.push(b);
    }

    /// Builds component `c` from `members` with `connect`'s rules: counts, naming key,
    /// sources and their findings, width, bits, pins, `W004` and the row outputs. Returns
    /// `false` when two names short (`E003`): bits would merge by name, which only a full
    /// build handles.
    fn build_comp(
        &mut self,
        c: u32,
        members: &[u32],
        cx: &Ctx<'_>,
        cn: &mut CellNets,
        names: &mut Names,
    ) -> bool {
        let (doc, ci) = (cx.doc, c as usize);
        let s = doc.strings();
        let (wires, labels, ports, noconns, inst) = (
            doc.wires(),
            doc.labels(),
            doc.ports(),
            doc.noconns(),
            doc.instances(),
        );
        assert!(!members.is_empty() && self.head[ci] == NONE);
        for (k, &v) in members.iter().enumerate() {
            self.next[v as usize] = members[(k + 1) % members.len()];
            self.comp[v as usize] = c;
        }
        self.head[ci] = members[0];
        let finding = |code, object, param, detail| Violation {
            code,
            cell: cx.own.cell,
            object,
            path: Name::NONE,
            param,
            detail,
        };
        let mut found: Vec<Violation> = Vec::new();

        // ---- counts, naming key, sources (class, text, object, bits range)
        let (mut pins, mut pinw) = (0u32, 1usize);
        let (mut minid, mut minpin) = (ObjId::NONE, (ObjId::NONE, 0u32));
        let mut srcs: Vec<(u8, Name, ObjId, usize, usize)> = Vec::new();
        let (mut bits, mut buf): (Vec<Name>, String) = (Vec::new(), String::new());
        for &v in members {
            let r = self.row[v as usize] as usize;
            let (id, src) = match self.kind[v as usize] {
                Kind::Wire => (wires.id[r], None),
                Kind::Noconn => (noconns.id[r], None),
                Kind::Label => (labels.id[r], Some((LABEL, labels.text[r]))),
                Kind::Port => {
                    pins += 1;
                    (ports.id[r], Some((PORT, ports.text[r])))
                }
                Kind::Pin => {
                    pins += 1;
                    let f = &cx.ifaces[cx.inst_iface[r] as usize];
                    let p = self.sub[v as usize];
                    pinw = pinw.max(f.bits(usize::from(p)).len());
                    let take = minpin.0 == ObjId::NONE
                        || match id_cmp(inst.id[r], minpin.0, s) {
                            Ordering::Less => true,
                            Ordering::Equal => u32::from(p) < minpin.1,
                            Ordering::Greater => false,
                        };
                    if take {
                        minpin = (inst.id[r], u32::from(p));
                    }
                    continue;
                }
            };
            if minid == ObjId::NONE || id_cmp(id, minid, s) == Ordering::Less {
                minid = id;
            }
            let Some((class, sym)) = src else {
                continue;
            };
            let text = s.get(sym);
            let before = bits.len();
            if netexpr::expand(text, names, &mut bits, &mut buf).is_err() {
                let detail = format!("`{text}` is wider than 65535 bits");
                found.push(finding(Code::E011, id, Name::NONE, detail));
                continue;
            }
            let global = bits[before..]
                .iter()
                .all(|&b| glob(&self.globals, names, b));
            let class = if global { GLOBAL } else { class };
            srcs.push((class, names.intern(text), id, before, bits.len()));
        }
        let key = if minid == ObjId::NONE {
            minpin
        } else {
            (minid, NONE_PIN)
        };
        assert!(
            key.0 != ObjId::NONE,
            "every component holds an object with an id"
        );

        // ---- best name, E011 (unequal widths); a short (E003) needs a full build
        let mut best: Option<usize> = None;
        for k in 0..srcs.len() {
            let better = best.is_none_or(|b| {
                (srcs[k].0, names.get(srcs[k].1)) < (srcs[b].0, names.get(srcs[b].1))
            });
            if better {
                best = Some(k);
            }
        }
        let width = match best {
            Some(b) => {
                let bb = &bits[srcs[b].3..srcs[b].4];
                for k in (0..srcs.len()).filter(|&k| k != b) {
                    let bk = &bits[srcs[k].3..srcs[k].4];
                    if bk == bb {
                        continue;
                    }
                    if bk.len() == bb.len() {
                        return false;
                    }
                    let detail = format!(
                        "`{}` ({} bits) and `{}` ({} bits) name one net",
                        names.get(srcs[k].1),
                        bk.len(),
                        names.get(srcs[b].1),
                        bb.len()
                    );
                    found.push(finding(Code::E011, srcs[k].2, srcs[k].1, detail));
                }
                bb.len()
            }
            None => pinw,
        };
        assert!(width <= usize::from(u16::MAX), "widths fit u16");

        // ---- bits: a named slot is its name's bit; an unnamed slot gets its own
        let start = cn.net_bits.len();
        for j in 0..width {
            let b = match best {
                Some(b) => self.named_bit(bits[srcs[b].3 + j], cx, cn, names),
                None => {
                    buf.clear();
                    write_key(key, s, &mut buf);
                    if width > 1 {
                        buf.push('[');
                        push_decimal(j as u32, &mut buf);
                        buf.push(']');
                    }
                    let name = names.intern(&buf);
                    self.alloc_bit(name, cx, cn, names)
                }
            };
            cn.net_bits.push(b);
        }
        self.live_bits += width;
        self.named[ci] = best.is_some();
        cn.net_bit_start[ci] = start as u32;
        cn.net_width[ci] = width as u16;
        cn.net_pins[ci] = pins;
        cn.net_global[ci] = cn.net_bits[start..]
            .iter()
            .any(|&b| cn.bit_global[b as usize]);
        cn.net_name[ci] = match best {
            Some(b) => srcs[b].1,
            None if width == 1 => cn.bit_name[cn.net_bits[start] as usize],
            None => {
                buf.clear();
                write_key(key, s, &mut buf);
                names.intern(&buf)
            }
        };

        // ---- pins, W004, row outputs
        for &v in members {
            if self.kind[v as usize] == Kind::Pin {
                found.extend(self.pin(v, c, true, cx, cn, names));
            }
        }
        let lone = members[0] as usize;
        if members.len() == 1 && self.kind[lone] == Kind::Pin {
            let i = self.row[lone] as usize;
            let f = &cx.ifaces[cx.inst_iface[i] as usize];
            let pin = f.pin_name[usize::from(self.sub[lone])];
            let detail = format!("pin `{}` is unconnected", names.get(pin));
            found.push(finding(Code::W004, inst.id[i], pin, detail));
        }
        for &v in members {
            let vi = v as usize;
            match self.kind[vi] {
                Kind::Pin => {
                    let i = self.row[vi] as usize;
                    let k = (v - self.first[INSTS][i]) as usize;
                    cn.pin_net[cn.pin_start[i] as usize + k] = c;
                }
                Kind::Wire if self.sub[vi] == 1 => {}
                _ => self.write_row(v, cn),
            }
        }
        self.new.extend(found.iter().cloned());
        self.found[ci] = found;
        true
    }
}
