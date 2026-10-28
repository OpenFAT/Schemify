//! Live connectivity's spatial index: nodes ordered by `(x, y)` and `(y, x)`, axis-aligned
//! wires by row and column, diagonal wires in a list, and the point and segment queries the
//! patch's flood runs over them (the rules of `connect`, §3).
//!
//! Contents:
//! - `Live::index_wire`, `Live::index`, `Live::unindex`, `Live::unindex_wire`,
//!   `Live::ends`, `Live::neighbors`, `Live::on_wire` (`pub(super)`)

use super::{Ctx, INSTS, Kind, Live, geometric};
use crate::nets;

impl Live {
    pub(super) fn index_wire(&mut self, first: u32) {
        let (x1, y1, x2, y2) = self.ends(first);
        if x1 == x2 {
            let fresh = self.vw.insert((x1, y1.min(y2), y1.max(y2), first));
            assert!(fresh, "wire {first} indexed once");
        } else if y1 == y2 {
            let fresh = self.hw.insert((y1, x1.min(x2), x1.max(x2), first));
            assert!(fresh, "wire {first} indexed once");
        } else {
            debug_assert!(!self.dw.contains(&first), "wire {first} indexed once");
            self.dw.push(first);
        }
    }

    pub(super) fn index(&mut self, v: u32) {
        let (x, y) = (self.x[v as usize], self.y[v as usize]);
        let fresh_x = self.by_x.insert((x, y, v));
        let fresh_y = self.by_y.insert((y, x, v));
        assert!(fresh_x, "node {v} indexed once by x");
        assert!(fresh_y, "node {v} indexed once by y");
    }

    pub(super) fn unindex(&mut self, v: u32) {
        let (x, y) = (self.x[v as usize], self.y[v as usize]);
        let held_x = self.by_x.remove(&(x, y, v));
        let held_y = self.by_y.remove(&(y, x, v));
        assert!(held_x, "node {v} was indexed by x");
        assert!(held_y, "node {v} was indexed by y");
    }

    /// The wire whose ends are nodes `first`, `first + 1`: `(x1, y1, x2, y2)`.
    pub(super) fn ends(&self, first: u32) -> (i32, i32, i32, i32) {
        let (a, b) = (first as usize, first as usize + 1);
        assert_eq!(self.kind[a], Kind::Wire, "node {first} is a wire end");
        assert_eq!(self.sub[a], 0, "node {first} is a wire's first end");
        (self.x[a], self.y[a], self.x[b], self.y[b])
    }

    pub(super) fn unindex_wire(&mut self, first: u32) {
        let (x1, y1, x2, y2) = self.ends(first);
        if x1 == x2 {
            let held = self.vw.remove(&(x1, y1.min(y2), y1.max(y2), first));
            assert!(held, "wire {first} was indexed");
        } else if y1 == y2 {
            let held = self.hw.remove(&(y1, x1.min(x2), x1.max(x2), first));
            assert!(held, "wire {first} was indexed");
        } else {
            let at = self.dw.iter().position(|&w| w == first).expect("indexed");
            self.dw.swap_remove(at);
        }
    }

    /// Appends every node joined to `v` by one rule of `connect` (§3): the same point, the
    /// other end and the nodes on the wire `v` ends, the ends of the wires through `v`, and
    /// alias pins.
    pub(super) fn neighbors(&self, v: u32, cx: &Ctx<'_>, out: &mut Vec<u32>) {
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
            if nets::on_segment(x, y, x1, y1, x2, y2) {
                out.extend([w, w + 1]);
            }
        }
    }

    /// Appends the nodes lying on the wire whose first node is `first`, its ends included.
    pub(super) fn on_wire(&self, first: u32, out: &mut Vec<u32>) {
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
                .filter(|e| nets::on_segment(e.0, e.1, x1, y1, x2, y2));
            out.extend(on.map(|e| e.2));
        }
    }
}
