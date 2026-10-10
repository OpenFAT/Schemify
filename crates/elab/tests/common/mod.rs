//! Shared test helpers: projects from file texts, ids, a seeded RNG, random schematics,
//! the naive O(n²) connectivity oracle, and partition comparison.

#![allow(dead_code)] // each test binary uses a subset

use schemify_doc::{DocHandle, Document, Project};
use schemify_elab::{CellNets, Dialect, Elab, Request, elaborate, no_funcs};
use std::collections::BTreeMap;

/// splitmix64.
pub struct Mix(pub u64);

impl Mix {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0);
        self.next() % n
    }
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// The 6-char id of `n` (a bijection on 30 bits, so ids are unique and scattered).
pub fn id(n: u64) -> String {
    const A: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let v = n.wrapping_mul(0x9e37_79b1) & ((1 << 30) - 1);
    (0..6).rev().map(|k| A[((v >> (5 * k)) & 31) as usize] as char).collect()
}

/// Coordinate text of milli-units.
pub fn c(milli: i64) -> String {
    let mut s = String::new();
    schemify_format::format_coord(milli as i32, &mut s);
    s
}

/// A project with every file indexed in the project library and opened.
pub fn project(files: &[(&str, &str)]) -> (Project, Vec<DocHandle>) {
    let mut p = Project::new(None, None, 11).unwrap();
    let listing: Vec<(&str, &[u8])> = files.iter().map(|(n, t)| (*n, t.as_bytes())).collect();
    let (_, diags) = p.index_library("", "", &listing).unwrap();
    assert!(diags.is_empty(), "index diags: {diags:?}");
    let handles = files
        .iter()
        .map(|(n, t)| p.open("", n, t.as_bytes(), None).unwrap_or_else(|e| panic!("{n}: {e:?}")).handle)
        .collect();
    (p, handles)
}

pub fn ngspice() -> Request<'static> {
    Request {
        config: None,
        corner: None,
        overrides: &[],
        dialect: Dialect::Ngspice,
    }
}

pub fn elab(p: &Project, top: DocHandle) -> Elab {
    elaborate(p, top, &ngspice(), &mut no_funcs)
}

/// Codes of an elaboration's violations, as text.
pub fn codes(e: &Elab) -> Vec<String> {
    e.violations.rows.iter().map(|v| v.code.as_str().to_owned()).collect()
}

/// A three-pin device: pins at (0,0), (20,0) and (0,30) symbol-local units.
pub const DEV: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 20 0\npin C inout 0 30\n\nparam w 1\n\nimpl spice spice\n  format spice \"X{name} {A} {B} {C} dev\"\n";

/// A random bench of `n` objects on a coarse grid (so coincidences and T-junctions are
/// common): instances of `./dev` in all 8 orientations, axis-aligned and diagonal
/// wires, labels from a small name set, noconns. Ids are `id(base + k)`.
pub fn random_bench(rng: &mut Mix, n: usize, base: u64) -> String {
    let mut insts = String::new();
    let mut rest = String::new();
    let g = |rng: &mut Mix| rng.range(-6, 6) * 10_000;
    let orients = ["R0", "R90", "R180", "R270", "MR0", "MR90", "MR180", "MR270"];
    for k in 0..n {
        let i = id(base + k as u64);
        match rng.below(10) {
            0..=2 => {
                let o = orients[rng.below(8) as usize];
                insts.push_str(&format!(
                    "inst {i} ./dev\n  at {} {} {o}\n  name X{k}\n",
                    c(g(rng)),
                    c(g(rng))
                ));
            }
            3..=7 => {
                let (x1, y1) = (g(rng), g(rng));
                let (x2, y2) = match rng.below(5) {
                    0 => (x1 + rng.range(-3, 3) * 10_000, y1 + rng.range(-3, 3) * 10_000), // diagonal-ish
                    1 | 2 => (x1, y1 + rng.range(1, 6) * 10_000),
                    _ => (x1 + rng.range(1, 6) * 10_000, y1),
                };
                if (x1, y1) == (x2, y2) {
                    continue;
                }
                rest.push_str(&format!("wire {i} {} {} {} {}\n", c(x1), c(y1), c(x2), c(y2)));
            }
            8 => {
                let name = ["a", "b", "c", "0", "vdd!"][rng.below(5) as usize];
                rest.push_str(&format!("label {i} {} {} R0 {name}\n", c(g(rng)), c(g(rng))));
            }
            _ => rest.push_str(&format!("noconn {i} {} {}\n", c(g(rng)), c(g(rng)))),
        }
    }
    format!("schemify 1 shbench\n\n{insts}{rest}")
}

/// Places a symbol-local point (format spec §3), written independently of elab.
pub fn place_ref(x: i64, y: i64, orient: &str, at: (i64, i64)) -> (i64, i64) {
    let mirrored = orient.starts_with('M');
    let turns = match orient.trim_start_matches('M') {
        "R0" => 0,
        "R90" => 1,
        "R180" => 2,
        _ => 3,
    };
    let x = if mirrored { -x } else { x };
    let (x, y) = match turns {
        0 => (x, y),
        1 => (-y, x),
        2 => (-x, -y),
        _ => (y, -x),
    };
    (x + at.0, y + at.1)
}

/// Object keys of a schematic: `w:<id>`, `l:<id>`, `p:<inst id>:<pin index>`.
pub type Partition = BTreeMap<String, usize>;

/// The naive oracle (TESTING.md): nodes are wire ends, instance pins, labels and
/// noconns; union every coincident pair, every node lying on a wire (endpoints
/// included), and the two ends of each wire. O(n²) pairs. Returns the geometric
/// partition of wires, labels and pins.
pub fn naive_partition(doc: &Document, pins: &[(i64, i64)]) -> Partition {
    let s = doc.strings();
    let (w, l, n, inst) = (doc.wires(), doc.labels(), doc.noconns(), doc.instances());
    let mut pts: Vec<(i64, i64)> = Vec::new();
    let mut keys: Vec<Option<String>> = Vec::new();
    for k in 0..w.id.len() {
        pts.push((w.x1[k] as i64, w.y1[k] as i64));
        keys.push(Some(format!("w:{}", w.id[k].text(s))));
        pts.push((w.x2[k] as i64, w.y2[k] as i64));
        keys.push(None);
    }
    for i in 0..inst.id.len() {
        let o = inst.orient[i].as_str();
        for (p, &(px, py)) in pins.iter().enumerate() {
            pts.push(place_ref(px, py, o, (inst.x[i] as i64, inst.y[i] as i64)));
            keys.push(Some(format!("p:{}:{p}", inst.id[i].text(s))));
        }
    }
    for k in 0..l.id.len() {
        pts.push((l.x[k] as i64, l.y[k] as i64));
        keys.push(Some(format!("l:{}", l.id[k].text(s))));
    }
    for k in 0..n.id.len() {
        pts.push((n.x[k] as i64, n.y[k] as i64));
        keys.push(None);
    }
    let len = pts.len();
    let mut parent: Vec<usize> = (0..len).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            x = p[x];
        }
        x
    }
    let union = |p: &mut Vec<usize>, a: usize, b: usize| {
        let (ra, rb) = (find(p, a), find(p, b));
        p[ra] = rb;
    };
    for k in 0..w.id.len() {
        union(&mut parent, 2 * k, 2 * k + 1);
    }
    for a in 0..len {
        for b in a + 1..len {
            if pts[a] == pts[b] {
                union(&mut parent, a, b);
            }
        }
    }
    for k in 0..w.id.len() {
        let ((x1, y1), (x2, y2)) = (pts[2 * k], pts[2 * k + 1]);
        for (a, &(x, y)) in pts.iter().enumerate() {
            let cross = (x - x1) * (y2 - y1) - (y - y1) * (x2 - x1);
            let inside = x >= x1.min(x2) && x <= x1.max(x2) && y >= y1.min(y2) && y <= y1.max(y2);
            if cross == 0 && inside {
                union(&mut parent, a, 2 * k);
            }
        }
    }
    let mut out = Partition::new();
    for (a, key) in keys.iter().enumerate() {
        if let Some(k) = key {
            out.insert(k.clone(), find(&mut parent, a));
        }
    }
    out
}

/// The extractor's geometric partition in the same keys.
pub fn real_partition(doc: &Document, nets: &CellNets) -> Partition {
    let s = doc.strings();
    let mut out = Partition::new();
    for (k, &net) in nets.wire_net.iter().enumerate() {
        out.insert(format!("w:{}", doc.wires().id[k].text(s)), net as usize);
    }
    for (k, &net) in nets.label_net.iter().enumerate() {
        out.insert(format!("l:{}", doc.labels().id[k].text(s)), net as usize);
    }
    for i in 0..doc.instances().id.len() {
        for (p, &net) in nets.pins(i).iter().enumerate() {
            out.insert(format!("p:{}:{p}", doc.instances().id[i].text(s)), net as usize);
        }
    }
    out
}

/// Canonical form of a partition: each key mapped to the smallest key of its block, so
/// two partitions are equal exactly when their blocks are.
pub fn canon(p: &Partition) -> BTreeMap<String, String> {
    let mut first: BTreeMap<usize, &String> = BTreeMap::new();
    for (k, b) in p {
        first.entry(*b).or_insert(k); // keys iterate in order: the first is the smallest
    }
    p.iter().map(|(k, b)| (k.clone(), first[b].clone())).collect()
}
