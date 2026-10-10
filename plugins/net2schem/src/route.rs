//! Route transform: placed devices of one cell → Manhattan wires plus one name marker
//! (label or port) per connected piece of every net.
//!
//! Connectivity follows netlist spec §3 exactly: a wire endpoint, pin, label or port
//! lying anywhere on a wire joins it, and wires crossing in their interiors do not. The
//! router keeps an occupancy grid of 10-unit points (device bodies, nodes with their
//! net, and the unit edges each wire covers) and accepts a segment only if it touches
//! nothing of another net except by a clean interior crossing, never overlaps another
//! wire collinearly and never enters a device body except at its own pin. A pin the
//! router cannot reach gets its own label: labels with equal names connect, so falling
//! back to labels is always correct.
//!
//! Each signal net is grown as a tree from its leftmost pin (Prim order): every next
//! pin tries straight, L, Z and pin-stub paths to the nearest tree points, cheapest
//! (length + 20 per bend) first. Rails are never routed: every rail pin gets a label.

use crate::place::{Placed, Role, extent, role, xform};
use crate::recognize::Recognized;
use crate::spice::{Deck, Dir};

/// Devices drawn with wires per conversion (cells in file order, each wholly wired or
/// wholly labelled): bounds the work within the plugin's default fuel budget (routing
/// costs ~0.4–0.7M fuel per device; a 117-device bus array converts in ~85M of 10^8).
// ponytail: a cheaper router (channel routing, or candidate checks over a coarser grid)
// would lift the cap; until then big cells are drawn with labels, which stay correct.
pub const ROUTE_DEVICES_MAX: usize = 120;
/// Grid points per cell above which wires are skipped (labels only).
pub const GRID_POINTS_MAX: usize = 1 << 20;
/// Tree targets tried per pin.
const TARGETS_MAX: usize = 6;
/// Candidate paths validated per pin before it falls back to a label (bounds the work
/// on crowded nets: the plugin's whole import runs in one fuel budget).
const TRIES_MAX: usize = 40;
/// Cost of one bend, in xschem units of wire length.
const BEND_COST: i32 = 20;
const NONE: u16 = u16::MAX;

/// Router output for one cell. Coordinates are xschem units on the 10 grid.
#[derive(Debug, Default)]
pub struct Routed {
    /// Wire segments `[x1, y1, x2, y2]`, axis-aligned, non-zero length.
    pub wire: Vec<[i32; 4]>,
    /// Cell-local net of each wire.
    pub wire_net: Vec<u32>,
    /// Marker anchors.
    pub mark_x: Vec<i32>,
    /// Marker anchors.
    pub mark_y: Vec<i32>,
    /// Cell-local net each marker names.
    pub mark_net: Vec<u32>,
    /// Marker orientation index (`place::ORIENT_WORDS`).
    pub mark_orient: Vec<u8>,
    /// `true` for a `port` record, `false` for a `label`.
    pub mark_port: Vec<bool>,
    /// Signal nets with ≥ 2 pins.
    pub nets_multi: u32,
    /// Of those, nets drawn as one wired tree (no label needed to connect them).
    pub nets_wired: u32,
}

/// One grid point: the node net there (pin, wire end, label), the nets of the wires
/// covering the unit edge east (`h`) and south (`v`) of it, and whether a device body
/// covers it. Read together on every check, so one array of them (AoS) with one bounds
/// check per point.
#[derive(Clone, Copy)]
struct Cell {
    node: u16,
    h: u16,
    v: u16,
    body: bool,
}

const EMPTY: Cell = Cell { node: NONE, h: NONE, v: NONE, body: false };

/// Occupancy grid of 10-unit points (row-major, `w` per row).
struct Grid {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    cells: Vec<Cell>,
}

type Pt = (i32, i32);

impl Grid {
    fn new(b: [i32; 4]) -> Grid {
        let w = ((b[2] - b[0]) / 10 + 1) as usize;
        let h = ((b[3] - b[1]) / 10 + 1) as usize;
        assert!(w * h <= GRID_POINTS_MAX, "grid of {w}×{h} points");
        Grid { x0: b[0], y0: b[1], w, h, cells: vec![EMPTY; w * h] }
    }

    fn at(&self, p: Pt) -> Option<usize> {
        let (dx, dy) = (p.0 - self.x0, p.1 - self.y0);
        if dx < 0 || dy < 0 || dx % 10 != 0 || dy % 10 != 0 {
            return None;
        }
        let (i, j) = ((dx / 10) as usize, (dy / 10) as usize);
        (i < self.w && j < self.h).then_some(j * self.w + i)
    }

    /// Nets of the horizontal and vertical wires through or ending at point `i`.
    fn wires_at(&self, i: usize) -> (u16, u16) {
        let cells = &self.cells[..];
        let c = cells[i];
        let h = if c.h != NONE || i % self.w == 0 { c.h } else { cells[i - 1].h };
        let v = if c.v != NONE || i < self.w { c.v } else { cells[i - self.w].v };
        (h, v)
    }

    /// First point index, index step and point count of segment `a`–`b` (both ends
    /// inclusive), and whether it is horizontal; `None` when it leaves the grid.
    fn span(&self, a: Pt, b: Pt) -> Option<(usize, usize, usize, bool)> {
        assert!(a != b && (a.0 == b.0 || a.1 == b.1), "axis-aligned, non-zero segment");
        let horiz = a.1 == b.1;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let first = self.at(lo)?;
        self.at(hi)?;
        let count = (((hi.0 - lo.0) + (hi.1 - lo.1)) / 10 + 1) as usize;
        Some((first, if horiz { 1 } else { self.w }, count, horiz))
    }

    /// Whether a wire of net `n` from `a` to `b` keeps every other net's connectivity:
    /// no collinear overlap with any wire (a shared unit edge), no foreign node on it (a
    /// collinear wire that only touches it ends there, so this covers touching too), no
    /// body point except its own pin at an end, and foreign wires only crossing interior
    /// to both (no node at the crossing).
    fn seg_ok(&self, n: u16, a: Pt, b: Pt) -> bool {
        let Some((first, step, count, horiz)) = self.span(a, b) else {
            return false;
        };
        let cells = &self.cells[..];
        let foreign = |x: u16| x != NONE && x != n;
        let last = count - 1;
        for k in 0..count {
            let i = first + k * step;
            let c = cells[i];
            let end = k == 0 || k == last;
            let used = if horiz { c.h } else { c.v };
            if foreign(c.node) || (c.body && !(end && c.node == n)) || (k < last && used != NONE) {
                return false;
            }
            // The cross wire through i covers the edge at i or the one before it.
            let cross = match (horiz, if horiz { c.v } else { c.h }) {
                (_, x) if x != NONE => x,
                (true, _) if i >= self.w => cells[i - self.w].v,
                (false, _) if i % self.w > 0 => cells[i - 1].h,
                _ => NONE,
            };
            if foreign(cross) && (end || c.node != NONE) {
                return false;
            }
        }
        true
    }

    fn commit(&mut self, n: u16, a: Pt, b: Pt) {
        let (first, step, count, horiz) = self.span(a, b).expect("checked by seg_ok");
        let cells = &mut self.cells[..];
        for k in 0..count - 1 {
            let c = &mut cells[first + k * step];
            if horiz {
                c.h = n;
            } else {
                c.v = n;
            }
        }
        cells[first].node = n;
        cells[first + (count - 1) * step].node = n;
    }

    /// Whether a marker (label/port anchor) of net `n` may sit at `p`.
    fn mark_ok(&self, n: u16, p: Pt) -> bool {
        let Some(i) = self.at(p) else {
            return false;
        };
        let ok = |x: u16| x == NONE || x == n;
        let c = self.cells[i];
        let (h, v) = self.wires_at(i);
        ok(c.node) && ok(h) && ok(v) && (!c.body || c.node == n)
    }
}

/// A pin in absolute coordinates.
#[derive(Clone, Copy)]
struct Pin {
    p: Pt,
    dir: Pt,
    net: u32,
}

/// Routes cell `c` placed as `placed`.
///
/// Cost: O(Σ over nets of pins² × path length) on the grid; skipped (labels only)
/// when the cell has more devices than `budget` holds (which the cell then spends) or
/// above [`GRID_POINTS_MAX`] points.
///
/// # Panics
///
/// Panics if `placed` does not have one row per device of cell `c`.
pub fn route(deck: &Deck, rec: &Recognized, placed: &Placed, c: usize, budget: &mut usize) -> Routed {
    let devs = deck.devs(c);
    assert_eq!(placed.x.len(), devs.len(), "placement of another cell");
    let nets = deck.net_count(c);
    let pins = absolute_pins(deck, rec, placed, c);

    // Bounds: every device extent, plus room for detours and markers.
    let mut b = [0i32, 0, 0, 0];
    for (i, d) in devs.clone().enumerate() {
        let e = extent(rec, rec.parts.sym[d] as usize, placed.orient[i]);
        let e = [e[0] + placed.x[i], e[1] + placed.y[i], e[2] + placed.x[i], e[3] + placed.y[i]];
        b = if i == 0 { e } else { [b[0].min(e[0]), b[1].min(e[1]), b[2].max(e[2]), b[3].max(e[3])] };
    }
    let b = [b[0] - 200, b[1] - 200, b[2] + 200, b[3] + 200];
    let points = (((b[2] - b[0]) / 10 + 1) * ((b[3] - b[1]) / 10 + 1)) as usize;
    // The grid stores net ids as u16 (NONE excluded).
    let wires_allowed = devs.len() <= *budget && points <= GRID_POINTS_MAX && nets < NONE as usize;
    if wires_allowed {
        *budget -= devs.len();
    }
    let mut out = Routed::default();
    let roles: Vec<Role> = (0..nets as u32).map(|k| role(deck.net_key(c, k))).collect();

    // Pins per net (CSR), in pin order.
    let mut start = vec![0u32; nets + 1];
    for p in &pins {
        start[p.net as usize + 1] += 1;
    }
    for k in 0..nets {
        start[k + 1] += start[k];
    }
    let mut fill = start.clone();
    let mut by_net = vec![0u32; pins.len()];
    for (j, p) in pins.iter().enumerate() {
        by_net[fill[p.net as usize] as usize] = j as u32;
        fill[p.net as usize] += 1;
    }
    let net_pins = |k: usize| &by_net[start[k] as usize..start[k + 1] as usize];

    // Union-find over pins: which pins one wired tree joins.
    let mut comp: Vec<u32> = (0..pins.len() as u32).collect();
    let mut wired = vec![false; pins.len()];
    let mut grid = if wires_allowed { Some(Grid::new(b)) } else { None };
    if let Some(g) = grid.as_mut() {
        for (i, d) in devs.clone().enumerate() {
            let s = rec.parts.sym[d] as usize;
            let body = rec.syms.body[s];
            let (ax, ay) = xform(placed.orient[i], body[0], body[1]);
            let (bx, by) = xform(placed.orient[i], body[2], body[3]);
            let (x0, x1) = (ax.min(bx) + placed.x[i], ax.max(bx) + placed.x[i]);
            let (y0, y1) = (ay.min(by) + placed.y[i], ay.max(by) + placed.y[i]);
            for y in (y0..=y1).step_by(10) {
                for x in (x0..=x1).step_by(10) {
                    let at = g.at((x, y)).expect("bodies inside the bounds");
                    g.cells[at].body = true;
                }
            }
        }
        for p in &pins {
            let at = g.at(p.p).expect("pins inside the bounds");
            let node = &mut g.cells[at].node;
            assert!(*node == NONE || *node == p.net as u16, "two nets on one pin point");
            *node = p.net as u16;
        }
        // Short nets first: sort keys (half perimeter << 32 | net).
        let mut order: Vec<u64> = (0..nets)
            .filter(|&k| roles[k] == Role::Signal && net_pins(k).len() > 1)
            .map(|k| {
                let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
                for &j in net_pins(k) {
                    let q = pins[j as usize].p;
                    (x0, y0, x1, y1) = (x0.min(q.0), y0.min(q.1), x1.max(q.0), y1.max(q.1));
                }
                (((x1 - x0) + (y1 - y0)) as u64) << 32 | k as u64
            })
            .collect();
        order.sort_unstable();
        for key in order {
            let k = (key & 0xffff_ffff) as usize;
            route_net(g, &pins, net_pins(k), k as u16, &mut comp, &mut wired, &mut out);
        }
    }

    // Markers: one per connected piece; rails per pin; ports first.
    let ports = deck.ports(c);
    let mut free_ports = 0i32;
    for k in 0..nets {
        let members = net_pins(k);
        let mut port_left = ports.contains(&(k as u32));
        if roles[k] == Role::Signal && members.len() > 1 {
            out.nets_multi += 1;
        }
        let mut roots: Vec<u32> = Vec::new();
        for &j in members {
            let r = find(&mut comp, j);
            if !roots.contains(&r) {
                roots.push(r);
            }
        }
        if roles[k] == Role::Signal && members.len() > 1 && roots.len() == 1 && wired[members[0] as usize] {
            out.nets_wired += 1;
        }
        // The wired piece first: its marker may sit on any wire of the net only while
        // no other piece has a marker stub yet.
        let (mut first, rest): (Vec<u32>, Vec<u32>) =
            roots.iter().partition(|&&r| members.iter().any(|&j| wired[j as usize] && find(&mut comp, j) == r));
        first.extend(rest);
        let roots = first;
        for &r in &roots {
            let piece: Vec<u32> = members.iter().copied().filter(|&j| find(&mut comp, j) == r).collect();
            let is_port = std::mem::take(&mut port_left);
            mark(grid.as_mut(), &pins, &piece, wired[piece[0] as usize], k as u32, is_port, &mut out);
        }
        if port_left {
            // A port whose net has no pins in this cell: on the margin, inputs left.
            let left = crate::recognize::port_dir(deck, c, k as u32) == Dir::In;
            free_port(grid.as_mut(), b, left, k as u32, &mut free_ports, &mut out);
        }
    }
    assert_eq!(out.wire.len(), out.wire_net.len());
    assert!(out.mark_net.len() >= roots_lower_bound(nets, &pins));
    out
}

/// At least one marker per net that has a pin.
fn roots_lower_bound(nets: usize, pins: &[Pin]) -> usize {
    let mut seen = vec![false; nets];
    for p in pins {
        seen[p.net as usize] = true;
    }
    seen.iter().filter(|&&s| s).count()
}

fn find(comp: &mut [u32], mut j: u32) -> u32 {
    while comp[j as usize] != j {
        let up = comp[comp[j as usize] as usize];
        comp[j as usize] = up;
        j = up;
    }
    j
}

/// Every device pin of cell `c` at its absolute position, with its outward direction.
fn absolute_pins(deck: &Deck, rec: &Recognized, placed: &Placed, c: usize) -> Vec<Pin> {
    let mut out = Vec::new();
    for (i, d) in deck.devs(c).enumerate() {
        let s = rec.parts.sym[d] as usize;
        let body = rec.syms.body[s];
        for (k, &net) in rec.syms.pins(s).zip(deck.nets_of(d)) {
            let (lx, ly) = (rec.syms.pin_x[k], rec.syms.pin_y[k]);
            let local = if ly == body[1] {
                (0, -1)
            } else if ly == body[3] {
                (0, 1)
            } else if lx == body[0] {
                (-1, 0)
            } else {
                (1, 0)
            };
            let (x, y) = xform(placed.orient[i], lx, ly);
            let dir = xform(placed.orient[i], local.0, local.1);
            out.push(Pin { p: (x + placed.x[i], y + placed.y[i]), dir, net });
        }
    }
    assert!(out.iter().all(|p| p.p.0 % 10 == 0 && p.p.1 % 10 == 0));
    out
}

/// Grows the tree of net `n` over its pins `members` (Prim order from the leftmost pin).
fn route_net(g: &mut Grid, pins: &[Pin], members: &[u32], n: u16, comp: &mut [u32], wired: &mut [bool], out: &mut Routed) {
    assert!(members.len() > 1);
    let root = *members.iter().min_by_key(|&&j| (pins[j as usize].p, j)).expect("non-empty");
    let mut tree: Vec<u32> = vec![root];
    let mut segs: Vec<(Pt, Pt)> = Vec::new();
    let mut left: Vec<u32> = members.iter().copied().filter(|&j| j != root).collect();
    while !left.is_empty() {
        // Nearest remaining pin to the tree.
        let near = |q: Pt, tree: &[u32], segs: &[(Pt, Pt)]| -> i32 {
            let a = tree.iter().map(|&t| dist(q, pins[t as usize].p));
            let b = segs.iter().map(|s| dist(q, project(q, *s)));
            a.chain(b).min().unwrap_or(i32::MAX)
        };
        let (pos, _) = left
            .iter()
            .enumerate()
            .min_by_key(|(_, j)| (near(pins[**j as usize].p, &tree, &segs), **j))
            .expect("non-empty");
        let j = left.swap_remove(pos);
        let q = pins[j as usize];
        // Targets: the nearest distinct tree pins (with their stub direction), segment
        // ends and projections, kept sorted in a fixed array (no allocation per pin).
        let mut targets: [(i32, Pt, Option<Pt>); TARGETS_MAX] = [(i32::MAX, (0, 0), None); TARGETS_MAX];
        let mut offer = |p: Pt, d: Option<Pt>| {
            let key = dist(q.p, p);
            if key >= targets[TARGETS_MAX - 1].0 || targets.iter().any(|t| t.0 != i32::MAX && t.1 == p) {
                return;
            }
            let mut k = TARGETS_MAX - 1;
            while k > 0 && targets[k - 1].0 > key {
                targets[k] = targets[k - 1];
                k -= 1;
            }
            targets[k] = (key, p, d);
        };
        for &t in &tree {
            offer(pins[t as usize].p, Some(pins[t as usize].dir));
        }
        for sg in &segs {
            offer(sg.0, None);
            offer(sg.1, None);
            offer(project(q.p, *sg), None);
        }
        let mut tries = TRIES_MAX;
        for &(key, t, tdir) in &targets {
            if key == i32::MAX {
                break;
            }
            if let Some(path) = best_path(g, n, q.p, q.dir, t, tdir, &mut tries) {
                for w in path.pts[..path.len].windows(2) {
                    g.commit(n, w[0], w[1]);
                    segs.push((w[0], w[1]));
                    let (a, b) = (w[0].min(w[1]), w[0].max(w[1]));
                    out.wire.push([a.0, a.1, b.0, b.1]);
                    out.wire_net.push(u32::from(n));
                }
                let (ra, rb) = (find(comp, j), find(comp, root));
                comp[ra as usize] = rb;
                wired[j as usize] = true;
                wired[root as usize] = true;
                tree.push(j);
                break;
            }
        }
        // An unreached pin keeps its own piece and gets a label.
    }
}

fn dist(a: Pt, b: Pt) -> i32 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs()
}

/// Nearest point of axis-aligned segment `s` to `q`.
fn project(q: Pt, s: (Pt, Pt)) -> Pt {
    let (lo, hi) = (s.0.min(s.1), s.0.max(s.1));
    (q.0.clamp(lo.0.min(hi.0), lo.0.max(hi.0)), q.1.clamp(lo.1.min(hi.1), lo.1.max(hi.1)))
}

/// Points a candidate path can have: start, two stub points, two bends, end stub, end.
const PATH_MAX: usize = 7;

/// A Manhattan path, at most [`PATH_MAX`] points, fixed size so candidates cost no
/// allocation.
#[derive(Clone, Copy)]
struct Path {
    pts: [Pt; PATH_MAX],
    len: usize,
}

impl Path {
    /// Joins `parts`, dropping repeated points and collinear interior points; `None`
    /// for a path that doubles back on itself or is not axis-aligned.
    fn join(parts: &[&[Pt]]) -> Option<Path> {
        let mut p = Path { pts: [(0, 0); PATH_MAX], len: 0 };
        for &pt in parts.iter().flat_map(|s| s.iter()) {
            if p.len > 0 && p.pts[p.len - 1] == pt {
                continue;
            }
            if p.len >= 2 {
                let (a, b) = (p.pts[p.len - 2], p.pts[p.len - 1]);
                if (a.0 == b.0 && b.0 == pt.0) || (a.1 == b.1 && b.1 == pt.1) {
                    let back = (b.0 - a.0).signum() != (pt.0 - b.0).signum() || (b.1 - a.1).signum() != (pt.1 - b.1).signum();
                    if back {
                        return None;
                    }
                    p.len -= 1;
                }
            }
            assert!(p.len < PATH_MAX, "candidate paths have at most {PATH_MAX} points");
            p.pts[p.len] = pt;
            p.len += 1;
        }
        let axis = p.pts[..p.len].windows(2).all(|w| w[0].0 == w[1].0 || w[0].1 == w[1].1);
        (p.len >= 2 && axis).then_some(p)
    }

    /// Length plus [`BEND_COST`] per bend.
    fn cost(&self) -> i32 {
        let len: i32 = self.pts[..self.len].windows(2).map(|w| dist(w[0], w[1])).sum();
        len + BEND_COST * (self.len as i32 - 2)
    }
}

/// Candidates per target: 3 starts × 2 ends × 4 shapes.
const CANDIDATES_MAX: usize = 24;

/// The cheapest valid path from pin `q` (leaving along `qd`) to `t` (entered against
/// `td` when `t` is a pin), or `None`; validates at most `tries` candidates (decremented).
fn best_path(g: &Grid, n: u16, q: Pt, qd: Pt, t: Pt, td: Option<Pt>, tries: &mut usize) -> Option<Path> {
    let mid = |a: i32, b: i32| ((a + b) / 2).div_euclid(10) * 10;
    let step = |p: Pt, d: Pt, k: i32| (p.0 + d.0 * k, p.1 + d.1 * k);
    let starts: [&[Pt]; 3] = [&[q], &[q, step(q, qd, 20)], &[q, step(q, qd, 40)]];
    let tail = td.map(|d| step(t, d, 20));
    let mut cands = [Path { pts: [(0, 0); PATH_MAX], len: 0 }; CANDIDATES_MAX];
    let mut keys = [0u64; CANDIDATES_MAX];
    let mut count = 0;
    for s in starts {
        // Straight into `t`, and (for a pin) through a stub out of it.
        for e in [None, tail].into_iter().take(1 + usize::from(tail.is_some())) {
            let end: [Pt; 2] = [e.unwrap_or(t), t];
            let end: &[Pt] = if e.is_some() { &end } else { &end[1..] };
            let (a, z) = (*s.last().expect("non-empty"), end[0]);
            let (mx, my) = (mid(a.0, z.0), mid(a.1, z.1));
            let shapes: [[Pt; 2]; 4] = [[(z.0, a.1), (z.0, a.1)], [(a.0, z.1), (a.0, z.1)], [(mx, a.1), (mx, z.1)], [(a.0, my), (z.0, my)]];
            for v in shapes {
                if let Some(p) = Path::join(&[s, &v, end]) {
                    keys[count] = (p.cost() as u64) << 32 | count as u64;
                    cands[count] = p;
                    count += 1;
                }
            }
        }
    }
    let keys = &mut keys[..count];
    keys.sort_unstable();
    for &k in keys.iter() {
        if *tries == 0 {
            return None;
        }
        *tries -= 1;
        let p = cands[(k & 0xffff_ffff) as usize];
        if p.pts[..p.len].windows(2).all(|w| g.seg_ok(n, w[0], w[1])) {
            return Some(p);
        }
    }
    None
}

/// Orientation index of a marker reached by moving along `d`.
fn orient_of(d: Pt) -> u8 {
    match d {
        (1, 0) => 0,
        (0, 1) => 1,
        (-1, 0) => 2,
        _ => 3,
    }
}

fn push_mark(out: &mut Routed, p: Pt, net: u32, orient: u8, port: bool) {
    out.mark_x.push(p.0);
    out.mark_y.push(p.1);
    out.mark_net.push(net);
    out.mark_orient.push(orient);
    out.mark_port.push(port);
}

/// Names one connected piece of net `net` (pins `piece`): on its longest wire when it is
/// wired, else on a stub off its first pin, else on the pin itself.
fn mark(grid: Option<&mut Grid>, pins: &[Pin], piece: &[u32], wired: bool, net: u32, port: bool, out: &mut Routed) {
    assert!(!piece.is_empty());
    let n = net as u16;
    let first = pins[piece[0] as usize];
    let Some(g) = grid else {
        push_mark(out, first.p, net, orient_of(first.dir), port);
        return;
    };
    if wired {
        // Longest wire of this net touching the piece's tree: try its interior points
        // from the middle outward.
        let mut best: Option<[i32; 4]> = None;
        for (w, &wn) in out.wire.iter().zip(&out.wire_net) {
            let len = (w[2] - w[0]) + (w[3] - w[1]);
            if wn == net && len >= 20 && best.is_none_or(|b| len > (b[2] - b[0]) + (b[3] - b[1])) {
                best = Some(*w);
            }
        }
        if let Some(w) = best {
            let horiz = w[1] == w[3];
            let (lo, hi) = if horiz { (w[0], w[2]) } else { (w[1], w[3]) };
            let midv = ((lo + hi) / 2).div_euclid(10) * 10;
            for off in 0..((hi - lo) / 10) {
                for sign in [1, -1] {
                    let v = midv + sign * off * 10;
                    if v <= lo || v >= hi {
                        continue;
                    }
                    let p = if horiz { (v, w[1]) } else { (w[0], v) };
                    if g.mark_ok(n, p) {
                        let i = g.at(p).expect("checked");
                        g.cells[i].node = n;
                        push_mark(out, p, net, if horiz { 0 } else { 3 }, port);
                        return;
                    }
                }
            }
        }
    }
    for &j in piece {
        let q = pins[j as usize];
        for k in [20, 10] {
            let s = (q.p.0 + q.dir.0 * k, q.p.1 + q.dir.1 * k);
            if g.seg_ok(n, q.p, s) && g.mark_ok(n, s) {
                g.commit(n, q.p, s);
                out.wire.push([q.p.0.min(s.0), q.p.1.min(s.1), q.p.0.max(s.0), q.p.1.max(s.1)]);
                out.wire_net.push(net);
                push_mark(out, s, net, orient_of(q.dir), port);
                return;
            }
        }
    }
    push_mark(out, first.p, net, orient_of(first.dir), port);
}

/// Places the port of a net with no pins in the cell on the left (inputs) or right
/// margin of `b`, at the first free point from the top; past the bottom of the grid
/// (where nothing is ever drawn) when the margin is full. `count` numbers those.
fn free_port(grid: Option<&mut Grid>, b: [i32; 4], left: bool, net: u32, count: &mut i32, out: &mut Routed) {
    let x = if left { b[0] + 40 } else { b[2] - 40 };
    let n = net as u16;
    let found = grid.and_then(|g| {
        let y = (b[1] + 40..b[3] - 40).step_by(20).find(|&y| g.mark_ok(n, (x, y)))?;
        let i = g.at((x, y)).expect("inside");
        g.cells[i].node = n;
        Some(y)
    });
    let y = found.unwrap_or_else(|| {
        *count += 1;
        b[3] + 20 * *count
    });
    push_mark(out, (x, y), net, if left { 2 } else { 0 }, true);
}
