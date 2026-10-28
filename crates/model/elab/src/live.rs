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
//! changes an instance's element count. A rename that keeps the count only rewrites
//! element names and their duplicate findings ([`Live::rename`]).
//!
//! SIMD triage: the id-column diff is the one loop over whole tables; it is an
//! accumulator (xor-or per chunk) and vectorizes as written. Everything else walks the
//! few touched nodes.
//!
//! This file holds the state, its seeding and the row checks; `patch` runs an edit,
//! `comps` keeps components and bits, `index` holds the spatial index and its queries.
//!
//! Contents:
//! - `pub(crate) struct Delta`
//! - `pub(crate) struct Ctx`: `new`
//! - `pub(crate) struct Live`: `seed`, `instances_moved`, `rows_moved`, `rename`, `rows`;
//!   `update` (in `patch`)

mod comps;
mod index;
mod patch;

use crate::iface::Iface;
use crate::names::{Name, Names};
use crate::netexpr;
use crate::nets::{self, CellNets, OWN_BITS, OWN_PORTS, OWN_STATIC, Seed};
use crate::values::NONE;
use crate::violations::{Code, Violation};
use schemify_doc::{Document, ObjId};
use std::collections::BTreeSet;
use std::mem;

/// Findings an edit removed and added, unordered.
#[derive(Clone, Debug, Default)]
pub(crate) struct Delta {
    pub(crate) gone: Vec<Violation>,
    pub(crate) added: Vec<Violation>,
}

/// The document and the interfaces an update reads.
pub(crate) struct Ctx<'cell> {
    pub(crate) doc: &'cell Document,
    pub(crate) own: &'cell Iface,
    pub(crate) ifaces: &'cell [Iface],
    pub(crate) inst_iface: &'cell [u32],
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

impl<'cell> Ctx<'cell> {
    /// The context of `doc` whose own interface is `ifaces[0]`.
    pub(crate) fn new(
        doc: &'cell Document,
        ifaces: &'cell [Iface],
        inst_iface: &'cell [u32],
    ) -> Ctx<'cell> {
        assert_eq!(inst_iface.len(), doc.instances().id.len());
        assert!(!ifaces.is_empty(), "the own interface is ifaces[0]");
        Ctx {
            doc,
            own: &ifaces[0],
            ifaces,
            inst_iface,
        }
    }
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
        assert_eq!(
            seed.named.len(),
            cn.net_name.len(),
            "one named flag per component"
        );
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
        for c in 0..nc {
            if !lv.named[c] {
                continue;
            }
            for &b in cn.bits_of(c) {
                lv.slots[b as usize] += 1;
            }
        }
        let own_slots = cn.port_bits.iter().chain(&cn.inh_bits);
        for &b in own_slots {
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
            let (a, b) = nets::unkey(k);
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
            .filter(|&l| nets::label_is_global(doc, l))
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

    /// Whether `doc`'s wire, label, port or noconn rows differ from the ones this state
    /// mirrors (a rejected transaction permutes them without ops): [`Live::update`] must
    /// then run, even for an edit that names no geometry, to move the row outputs.
    ///
    /// Cost: one compare per row of those tables (vectorized).
    pub(crate) fn rows_moved(&self, doc: &Document) -> bool {
        let mut rows = Vec::new();
        for t in 0..INSTS {
            changed_rows(&self.ids[t], table_ids(doc, t), &mut rows);
            if !rows.is_empty() {
                return true;
            }
        }
        assert!(rows.is_empty());
        false
    }

    /// Patches the element names of the instances `ids` (their `name` attribute changed,
    /// already applied to `cx.doc`) into `cn`, and the duplicate-name findings (`E009`) of
    /// every name they left or took. Returns the findings removed and added, or `None` when
    /// a rename changes an instance's element count (a bus), its name is too wide, or the
    /// instance is gone: then the caller rebuilds. A name joins no net: components, bits and
    /// every other finding stay.
    ///
    /// Cost: O(new elements) to intern, plus one scan of every element name per call.
    pub(crate) fn rename(
        &mut self,
        ids: &[ObjId],
        cx: &Ctx<'_>,
        cn: &mut CellNets,
        names: &mut Names,
    ) -> Option<Delta> {
        let doc = cx.doc;
        let (s, inst) = (doc.strings(), doc.instances());
        assert_eq!(cn.elem_start.len(), inst.id.len() + 1, "element rows fixed");
        let mut touched: Vec<Name> = Vec::new();
        let (mut now, mut buf) = (Vec::new(), String::new());
        for &id in ids {
            let i = doc.find_id(id)?.row as usize;
            if self
                .fixed
                .iter()
                .any(|v| v.code == Code::E011 && v.object == id)
            {
                return None; // its old name was too wide
            }
            now.clear();
            netexpr::expand(s.get(inst.name[i]), names, &mut now, &mut buf).ok()?;
            let (a, b) = (cn.elem_start[i] as usize, cn.elem_start[i + 1] as usize);
            if now.len() != b - a {
                return None; // the element count changed: connections move
            }
            touched.extend_from_slice(&cn.elem_name[a..b]);
            touched.extend_from_slice(&now);
            cn.elem_name[a..b].copy_from_slice(&now);
        }
        touched.sort_unstable();
        touched.dedup();
        // `connect`'s E009 rule over the touched names: every element after the first of
        // its name (in id order) is a duplicate.
        let (gone, kept): (Vec<Violation>, Vec<Violation>) = mem::take(&mut self.fixed)
            .into_iter()
            .partition(|v| v.code == Code::E009 && touched.binary_search(&v.param).is_ok());
        self.fixed = kept;
        // ponytail: one scan of every element per rename (~10 µs per 10^4 instances); an
        // index of elements by name if renames in huge cells show up in profiles.
        let mut elems: Vec<(Name, ObjId)> = Vec::new();
        for i in 0..inst.id.len() {
            let (a, b) = (cn.elem_start[i] as usize, cn.elem_start[i + 1] as usize);
            for &e in &cn.elem_name[a..b] {
                if touched.binary_search(&e).is_ok() {
                    elems.push((e, inst.id[i]));
                }
            }
        }
        elems.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(nets::id_cmp(a.1, b.1, s)));
        let mut new = Vec::new();
        for w in elems.windows(2) {
            if w[0].0 == w[1].0 {
                new.push(Violation::new(
                    Code::E009,
                    cx.own.cell,
                    w[1].1,
                    w[1].0,
                    format!("instance name `{}` is used twice", names.get(w[1].0)),
                ));
            }
        }
        self.fixed.extend(new.iter().cloned());
        assert!(new.len() < elems.len().max(1), "a duplicate has a first");
        Some(Delta { gone, added: new })
    }

    /// Every connectivity finding, owner by owner (what a full check pass starts from).
    pub(crate) fn rows(&self, cn: &CellNets, own: &Iface, names: &Names) -> Vec<Violation> {
        assert_eq!(
            self.found.len(),
            cn.net_name.len(),
            "one finding list per component"
        );
        assert_eq!(self.real.len(), cn.bit_name.len(), "one count per bit");
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
}

// ---- private ----

fn table_ids(doc: &Document, t: usize) -> &[ObjId] {
    assert!(t <= INSTS, "table {t} is not mirrored");
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
    let before = out.len();
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
    assert!(
        out.len() - before <= old.len().max(ids.len()),
        "each row at most once"
    );
    debug_assert!(out[before..].is_sorted(), "rows ascend");
}

/// Geometric (non-inherited) pins of `f`, in order: an instance's node `k` is the `k`-th.
fn geometric(f: &Iface) -> impl Iterator<Item = usize> + '_ {
    (0..f.pin_name.len()).filter(|&p| f.pin_inherit[p] == Name::NONE)
}
