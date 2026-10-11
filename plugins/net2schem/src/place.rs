//! Place transform: one cell's devices → origin + orientation on the 10-unit grid.
//!
//! Heuristics (after the salvaged cktImg layout rules): signal flows left to right,
//! supplies sit on top and grounds at the bottom.
//! - **Roles.** Nets named like supplies (`vdd*`, `vcc*`, `avdd*`, `dvdd*`, `vpp*`) or
//!   grounds (`0`, `gnd`, `vss*`, `vee*`, `agnd`, `dgnd`, `*_gnd`, `*_vss`) are *rails*:
//!   labelled at every pin, never routed, never traversed.
//! - **Columns.** Shortest distance from the inputs (input ports, nets driven by
//!   independent sources) over signal nets: a step through a conduction pair (drain/
//!   source, a shunt two-terminal) costs 0, through a control pin (gate, base, controlled
//!   source input) 1, through a series two-terminal 2 (it sits in the column between).
//! - **Rows.** Conduction distance from the supplies (devices touching a supply on top);
//!   devices reached only from ground stack upward from the bottom.
//! - **Orientation.** Two-terminal passives between two signal nets lie horizontal, input
//!   side left; vertical devices put the pin nearer the supply up; transistors face their
//!   gate towards the devices that drive it.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::recognize::{Family, Recognized};
use crate::spice::{Deck, Dir};

/// Horizontal gap between device slots (wire channel and label room).
pub const GAP_X: i32 = 80;
/// Vertical gap between rows.
pub const GAP_Y: i32 = 80;
const INF: u32 = u32::MAX;

/// Orientation words indexed by `quarter_turns + 4 * mirrored` (format spec §3).
pub const ORIENT_WORDS: [&str; 8] = ["R0", "R90", "R180", "R270", "MR0", "MR90", "MR180", "MR270"];
const R0: u8 = 0;
const R90: u8 = 1;
const R180: u8 = 2;
const R270: u8 = 3;
const MR0: u8 = 4;
const MR180: u8 = 6;

/// Maps a symbol-local point through orientation `o` (mirror x, then clockwise quarter
/// turns, y down; format spec §3).
pub fn xform(o: u8, x: i32, y: i32) -> (i32, i32) {
    assert!(o < 8, "orientation index {o}");
    let x = if o >= 4 { -x } else { x };
    match o % 4 {
        0 => (x, y),
        1 => (-y, x),
        2 => (-x, -y),
        _ => (y, -x),
    }
}

/// Net role by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A positive supply rail.
    Supply,
    /// Ground or a negative rail.
    Sink,
    /// Everything else.
    Signal,
}

/// Role of a net by its lowercase name.
pub fn role(key: &str) -> Role {
    let k = key.trim_end_matches('!');
    let sink = ["0", "gnd", "agnd", "dgnd", "gnda", "vssa"].contains(&k)
        || k.starts_with("vss")
        || k.starts_with("vee")
        || k.ends_with("_gnd")
        || k.ends_with("_vss");
    let supply = ["vdd", "vcc", "avdd", "dvdd", "vpp"]
        .iter()
        .any(|p| k.starts_with(p))
        || k.ends_with("_vdd")
        || k.ends_with("_vcc");
    if sink {
        Role::Sink
    } else if supply {
        Role::Supply
    } else {
        Role::Signal
    }
}

/// Pin role for placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinRole {
    /// Carries the device's current (drain, source, two-terminal ends).
    Conduct,
    /// Controls it (gate, base, controlled-source input, box input).
    Control,
    /// Takes no part in placement (MOS bulk).
    Ignore,
}

/// Placement role of pin `k` (symbol pin row) of a symbol of family `fam`.
pub fn pin_role(rec: &Recognized, fam: Family, k: usize) -> PinRole {
    let is_bulk = matches!(fam, Family::Nmos | Family::Pmos) && rec.syms.pin_name[k] == "b";
    if is_bulk {
        PinRole::Ignore
    } else if rec.syms.pin_dir[k] == Dir::In {
        PinRole::Control
    } else {
        PinRole::Conduct
    }
}

/// Placement of one cell: rows parallel to `deck.devs(c)`.
#[derive(Debug, Default)]
pub struct Placed {
    /// Origin x (xschem units, multiple of 10).
    pub x: Vec<i32>,
    /// Origin y.
    pub y: Vec<i32>,
    /// Orientation index into [`ORIENT_WORDS`].
    pub orient: Vec<u8>,
}

/// Per-cell working tables: net roles, device ↔ net incidence.
struct Graph<'a> {
    deck: &'a Deck,
    rec: &'a Recognized,
    first: usize,
    roles: Vec<Role>,
    /// CSR net → (device row, pin row) incidences.
    net_start: Vec<u32>,
    inc_dev: Vec<u32>,
    inc_pin: Vec<u32>,
    horizontal: Vec<bool>,
}

impl Graph<'_> {
    fn sym(&self, i: usize) -> usize {
        self.rec.parts.sym[self.first + i] as usize
    }
    fn nets(&self, i: usize) -> &[u32] {
        self.deck.nets_of(self.first + i)
    }
    /// (pin row, net, role) of device `i`.
    fn pins(&self, i: usize) -> impl Iterator<Item = (usize, u32, PinRole)> + '_ {
        let s = self.sym(i);
        let fam = self.rec.syms.family[s];
        self.rec
            .syms
            .pins(s)
            .zip(self.nets(i).iter())
            .map(move |(k, &n)| (k, n, pin_role(self.rec, fam, k)))
    }
    fn signal(&self, n: u32) -> bool {
        self.roles[n as usize] == Role::Signal
    }
}

/// Places the devices of cell `c`.
///
/// Cost: O((devices + pins) log nets).
///
/// # Panics
///
/// Panics if `rec` was not built from `deck`.
pub fn place(deck: &Deck, rec: &Recognized, c: usize) -> Placed {
    assert_eq!(
        rec.parts.sym.len(),
        deck.dev_name.len(),
        "recognized another deck"
    );
    let devs = deck.devs(c);
    let n = devs.len();
    let nets = deck.net_count(c);
    let mut g = Graph {
        deck,
        rec,
        first: devs.start,
        roles: (0..nets as u32).map(|k| role(deck.net_key(c, k))).collect(),
        net_start: vec![0; nets + 1],
        inc_dev: Vec::new(),
        inc_pin: Vec::new(),
        horizontal: vec![false; n],
    };
    incidence(&mut g, n, nets);
    for i in 0..n {
        let s = g.sym(i);
        let letter = deck.dev_name[g.first + i].as_bytes()[0].to_ascii_lowercase();
        let ns = g.nets(i);
        g.horizontal[i] = rec.syms.family[s] == Family::Two
            && b"rcld".contains(&letter)
            && ns.len() == 2
            && ns[0] != ns[1]
            && g.signal(ns[0])
            && g.signal(ns[1]);
    }
    let dist = columns(&g, c, n);
    let (sd, kd) = (
        rail_distance(&g, nets, Role::Supply),
        rail_distance(&g, nets, Role::Sink),
    );
    let height = |net: u32| -> i64 {
        match g.roles[net as usize] {
            Role::Supply => -1,
            Role::Sink => i64::MAX,
            Role::Signal if sd[net as usize] != INF => i64::from(sd[net as usize]),
            Role::Signal if kd[net as usize] != INF => 1_000_000 - i64::from(kd[net as usize]),
            Role::Signal => 500_000,
        }
    };

    // Column and row keys per device.
    let mut col_key = vec![0u64; n];
    let mut row_key = vec![0u64; n];
    for i in 0..n {
        let (mut cond, mut ctrl) = (INF, INF);
        let (mut top, mut low) = (INF, INF);
        for (_, net, r) in g.pins(i) {
            let d = if g.signal(net) {
                dist[net as usize]
            } else {
                INF
            };
            match r {
                PinRole::Conduct => {
                    cond = cond.min(d);
                    top = top.min(if g.roles[net as usize] == Role::Supply {
                        0
                    } else {
                        sd[net as usize]
                    });
                    low = low.min(if g.roles[net as usize] == Role::Sink {
                        0
                    } else {
                        kd[net as usize]
                    });
                }
                PinRole::Control => ctrl = ctrl.min(d),
                PinRole::Ignore => {}
            }
        }
        col_key[i] = u64::from(if g.horizontal[i] {
            cond.saturating_add(1)
        } else if cond != INF {
            cond
        } else if ctrl != INF {
            ctrl + 1
        } else {
            0
        });
        // Band A (from supply) rows first, band B (from ground) bottom-up after it.
        row_key[i] = if top != INF {
            u64::from(top)
        } else if low != INF {
            (1 << 32) + u64::from(u32::MAX - low)
        } else {
            1 << 33
        };
    }
    let cols = dense(&col_key);
    let rows = dense(&row_key);
    let ncols = cols.iter().max().map_or(0, |m| m + 1);
    let nrows = rows.iter().max().map_or(0, |m| m + 1);

    // Orientation (vertical flips and horizontal direction), then extents.
    let mut orient = vec![R0; n];
    for (i, o) in orient.iter_mut().enumerate() {
        let s = g.sym(i);
        let ns = g.nets(i);
        *o = match rec.syms.family[s] {
            Family::Two if g.horizontal[i] => {
                if dist[ns[0] as usize] <= dist[ns[1] as usize] {
                    R270
                } else {
                    R90
                }
            }
            Family::Two => {
                if height(ns[0]) > height(ns[1]) {
                    R180
                } else {
                    R0
                }
            }
            Family::Nmos
            | Family::Pmos
            | Family::Npn
            | Family::Pnp
            | Family::Vcxs
            | Family::Vsw => {
                // Top and bottom conduction pins in R0.
                let (mut top, mut bot) = (None, None);
                for (k, net, r) in g.pins(i) {
                    if r == PinRole::Conduct {
                        let y = rec.syms.pin_y[k];
                        if y < 0 {
                            top = Some(net);
                        } else if y > 0 {
                            bot = Some(net);
                        }
                    }
                }
                match (top, bot) {
                    (Some(t), Some(b)) if height(t) > height(b) => MR180,
                    _ => R0,
                }
            }
            Family::Tline | Family::Box => R0,
        };
    }
    let ext: Vec<[i32; 4]> = (0..n).map(|i| extent(rec, g.sym(i), orient[i])).collect();

    // Slots: (column, row) cells hold devices side by side in deck order.
    let mut slot_w = vec![0i32; ncols * nrows];
    let mut row_h = vec![0i32; nrows];
    for i in 0..n {
        let (w, h) = (ext[i][2] - ext[i][0], ext[i][3] - ext[i][1]);
        slot_w[cols[i] * nrows + rows[i]] += w + GAP_X;
        row_h[rows[i]] = row_h[rows[i]].max(h + GAP_Y);
    }
    let mut col_x = vec![0i32; ncols + 1];
    for k in 0..ncols {
        let w = (0..nrows).map(|r| slot_w[k * nrows + r]).max().unwrap_or(0);
        col_x[k + 1] = col_x[k] + w;
    }
    let mut row_y = vec![0i32; nrows + 1];
    for r in 0..nrows {
        row_y[r + 1] = row_y[r] + row_h[r];
    }
    let mut out = Placed {
        x: vec![0; n],
        y: vec![0; n],
        orient,
    };
    // Two passes: deck order, then each slot reordered by the barycenter of the devices
    // its devices share signal nets with (fewer crossings between stacked rows).
    let mut order: Vec<u64> = (0..n as u64).collect();
    for pass in 0..2 {
        if pass == 1 {
            order = (0..n)
                .map(|i| {
                    let bary =
                        barycenter(&g, &out, &ext, i).clamp(-(1 << 20), (1 << 20) - 1) + (1 << 20);
                    let slot = (cols[i] * nrows + rows[i]) as u64;
                    assert!(slot < 1 << 21 && i < 1 << 21);
                    slot << 42 | (bary as u64) << 21 | i as u64
                })
                .collect();
            order.sort_unstable();
        }
        let mut cursor = vec![0i32; ncols * nrows];
        for &key in &order {
            let i = (key & ((1 << 21) - 1)) as usize;
            let slot = cols[i] * nrows + rows[i];
            let w = ext[i][2] - ext[i][0];
            let cx = col_x[cols[i]] + cursor[slot] + (w + GAP_X) / 2;
            cursor[slot] += w + GAP_X;
            let cy = row_y[rows[i]] + row_h[rows[i]] / 2;
            out.x[i] = snap(cx - (ext[i][0] + ext[i][2]) / 2);
            out.y[i] = snap(cy - (ext[i][1] + ext[i][3]) / 2);
        }
    }
    face_gates(&g, &mut out);
    assert!(
        out.x.iter().chain(&out.y).all(|v| v % 10 == 0),
        "origins on the grid"
    );
    assert_eq!(out.orient.len(), n);
    out
}

/// Mean x centre of the other devices sharing a signal net with device `i` (its own
/// centre when it shares none), from the placement so far.
fn barycenter(g: &Graph<'_>, p: &Placed, ext: &[[i32; 4]], i: usize) -> i32 {
    assert!(i < p.x.len() && p.x.len() == ext.len());
    let centre = |j: usize| p.x[j] + (ext[j][0] + ext[j][2]) / 2;
    let (mut sum, mut cnt) = (0i64, 0i64);
    for &net in g.nets(i) {
        if !g.signal(net) {
            continue;
        }
        let at = g.net_start[net as usize] as usize..g.net_start[net as usize + 1] as usize;
        for e in at {
            let j = g.inc_dev[e] as usize;
            if j != i {
                sum += i64::from(centre(j));
                cnt += 1;
            }
        }
    }
    if cnt == 0 {
        centre(i)
    } else {
        (sum / cnt) as i32
    }
}

/// Rounds down to the 10-unit grid.
fn snap(v: i32) -> i32 {
    v.div_euclid(10) * 10
}

/// Ranks of `keys` among their distinct values (dense, order-preserving).
fn dense(keys: &[u64]) -> Vec<usize> {
    let mut uniq: Vec<u64> = keys.to_vec();
    uniq.sort_unstable();
    uniq.dedup();
    let out: Vec<usize> = keys
        .iter()
        .map(|k| uniq.binary_search(k).expect("present"))
        .collect();
    assert!(out.iter().all(|&r| r < uniq.len().max(1)));
    out
}

/// Bounding box `[x0, y0, x1, y1]` of symbol `s`'s body and pins under orientation `o`.
pub fn extent(rec: &Recognized, s: usize, o: u8) -> [i32; 4] {
    let b = rec.syms.body[s];
    let mut e = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    let corners = [(b[0], b[1]), (b[2], b[3])];
    let pins = rec
        .syms
        .pins(s)
        .map(|k| (rec.syms.pin_x[k], rec.syms.pin_y[k]));
    for (x, y) in corners.into_iter().chain(pins) {
        let (x, y) = xform(o, x, y);
        e = [e[0].min(x), e[1].min(y), e[2].max(x), e[3].max(y)];
    }
    assert!(e[0] <= e[2] && e[1] <= e[3]);
    e
}

/// Builds the net → (device, pin) incidence CSR.
fn incidence(g: &mut Graph<'_>, n: usize, nets: usize) {
    let mut count = vec![0u32; nets + 1];
    for i in 0..n {
        for &net in g.nets(i) {
            count[net as usize + 1] += 1;
        }
    }
    for k in 0..nets {
        count[k + 1] += count[k];
    }
    let total = count[nets] as usize;
    let mut next = count.clone();
    let mut inc_dev = vec![0; total];
    let mut inc_pin = vec![0; total];
    for i in 0..n {
        let s = g.sym(i);
        for (k, &net) in g.rec.syms.pins(s).zip(g.nets(i)) {
            let at = next[net as usize] as usize;
            next[net as usize] += 1;
            inc_dev[at] = i as u32;
            inc_pin[at] = k as u32;
        }
    }
    g.inc_dev = inc_dev;
    g.inc_pin = inc_pin;
    g.net_start = count;
    assert_eq!(g.inc_dev.len(), total);
}

/// Column distance of every net from the inputs (Dijkstra, weights 0/1/2 as in the
/// module docs). Unreached signal nets seed further components to the right.
fn columns(g: &Graph<'_>, c: usize, n: usize) -> Vec<u32> {
    let nets = g.roles.len();
    let mut dist = vec![INF; nets];
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    let seed =
        |net: u32, d: u32, dist: &mut Vec<u32>, heap: &mut BinaryHeap<Reverse<(u32, u32)>>| {
            if g.signal(net) && d < dist[net as usize] {
                dist[net as usize] = d;
                heap.push(Reverse((d, net)));
            }
        };
    for &p in g.deck.ports(c) {
        if crate::recognize::port_dir(g.deck, c, p) == Dir::In {
            seed(p, 0, &mut dist, &mut heap);
        }
    }
    for i in 0..n {
        let letter = g.deck.dev_name[g.first + i].as_bytes()[0].to_ascii_lowercase();
        if letter == b'v' || letter == b'i' {
            for &net in g.nets(i) {
                seed(net, 0, &mut dist, &mut heap);
            }
        }
    }
    let mut next_seed = 0u32;
    loop {
        while let Some(Reverse((d, net))) = heap.pop() {
            if d > dist[net as usize] {
                continue;
            }
            let at = g.net_start[net as usize] as usize..g.net_start[net as usize + 1] as usize;
            for e in at {
                let (i, k) = (g.inc_dev[e] as usize, g.inc_pin[e] as usize);
                let s = g.sym(i);
                let from = pin_role(g.rec, g.rec.syms.family[s], k);
                if from == PinRole::Ignore {
                    continue;
                }
                for (_, to, r) in g.pins(i) {
                    if to == net || r == PinRole::Ignore || !g.signal(to) {
                        continue;
                    }
                    let w = if g.horizontal[i] {
                        2
                    } else if from == PinRole::Conduct && r == PinRole::Conduct {
                        0
                    } else {
                        1
                    };
                    seed(to, d + w, &mut dist, &mut heap);
                }
            }
        }
        // Another component: start it right of everything reached so far.
        let far = dist
            .iter()
            .filter(|&&d| d != INF)
            .max()
            .map_or(0, |m| m + 2);
        let fresh = (next_seed..nets as u32).find(|&k| {
            g.signal(k)
                && dist[k as usize] == INF
                && g.net_start[k as usize + 1] > g.net_start[k as usize]
        });
        match fresh {
            Some(k) => {
                next_seed = k + 1;
                seed(k, far, &mut dist, &mut heap);
            }
            None => break,
        }
    }
    assert_eq!(dist.len(), nets);
    dist
}

/// Conduction distance of every signal net from the rails of `from` (BFS over
/// conduction pins; series two-terminals cost 0, every other device 1).
fn rail_distance(g: &Graph<'_>, nets: usize, from: Role) -> Vec<u32> {
    assert_ne!(from, Role::Signal);
    let mut dist = vec![INF; nets];
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    for (k, d) in dist.iter_mut().enumerate() {
        if g.roles[k] == from {
            *d = 0;
            heap.push(Reverse((0, k as u32)));
        }
    }
    while let Some(Reverse((d, net))) = heap.pop() {
        if d > dist[net as usize] {
            continue;
        }
        let at = g.net_start[net as usize] as usize..g.net_start[net as usize + 1] as usize;
        for e in at {
            let (i, k) = (g.inc_dev[e] as usize, g.inc_pin[e] as usize);
            let s = g.sym(i);
            if pin_role(g.rec, g.rec.syms.family[s], k) != PinRole::Conduct {
                continue;
            }
            let w = u32::from(!g.horizontal[i]);
            for (_, to, r) in g.pins(i) {
                if to != net && r == PinRole::Conduct && g.signal(to) && d + w < dist[to as usize] {
                    dist[to as usize] = d + w;
                    heap.push(Reverse((d + w, to)));
                }
            }
        }
    }
    dist
}

/// Mirrors transistor-like devices whose gate is driven from the right, so the gate pin
/// faces its driver.
fn face_gates(g: &Graph<'_>, p: &mut Placed) {
    for i in 0..p.x.len() {
        let s = g.sym(i);
        let fam = g.rec.syms.family[s];
        if !matches!(fam, Family::Nmos | Family::Pmos | Family::Npn | Family::Pnp) {
            continue;
        }
        let Some((_, gate, _)) = g.pins(i).find(|x| x.2 == PinRole::Control) else {
            continue;
        };
        if !g.signal(gate) {
            continue;
        }
        let (mut sum, mut cnt) = (0i64, 0i64);
        let at = g.net_start[gate as usize] as usize..g.net_start[gate as usize + 1] as usize;
        for e in at {
            let j = g.inc_dev[e] as usize;
            if j != i {
                sum += i64::from(p.x[j]);
                cnt += 1;
            }
        }
        if cnt > 0 && sum > i64::from(p.x[i]) * cnt {
            // Mirror about the device's own axis: R0 ↔ MR0, MR180 ↔ R180.
            p.orient[i] = match p.orient[i] {
                R0 => MR0,
                MR180 => R180,
                o => o,
            };
        }
    }
}
