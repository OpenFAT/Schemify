//! Junction dots: wire ends and runs (xschem's connection cues), kept up to date per changed
//! wire. In: wire segments. Out: where a dot belongs.

use crate::geometry::Pt;
use std::collections::{BTreeMap, HashMap};

/// Wire ends and runs (xschem's connection cues): what decides where junction dots go, kept
/// up to date per changed wire.
#[derive(Clone, Debug, Default)]
pub(super) struct Wiring {
    /// Wire ends sorted by (x, y), one entry per end.
    // ponytail: sorted vectors (a bulk build is one sort; an edit moves up to 1 MB per end at
    // 10^5 wires, ~0.02 ms); ordered maps cost the full build ~8 ms more at 10^5 wires.
    pub(super) xy: Vec<Pt>,
    /// The same ends as (y, x), sorted.
    yx: Vec<Pt>,
    /// Horizontal runs: y → (x min, x max) of each wire on it.
    horiz: HashMap<i32, Vec<(i32, i32)>>,
    /// Vertical runs: x → (y min, y max).
    vert: HashMap<i32, Vec<(i32, i32)>>,
    /// Diagonal wires `[x1, y1, x2, y2]`.
    diag: Vec<[i32; 4]>,
    /// Center of each dot drawn → its arc row.
    pub(super) dots: BTreeMap<Pt, u32>,
}

impl Wiring {
    /// The wiring of every wire row of `w` (bulk: one sort, not an insert per end).
    pub(super) fn of(w: &schemify_doc::Wires) -> Wiring {
        let mut out = Wiring::default();
        for i in 0..w.id.len() {
            let p = [w.x1[i], w.y1[i], w.x2[i], w.y2[i]];
            out.xy.extend([(p[0], p[1]), (p[2], p[3])]);
            out.runs(p, true);
        }
        out.xy.sort_unstable();
        out.yx = out.xy.iter().map(|p| (p.1, p.0)).collect();
        out.yx.sort_unstable();
        out
    }

    /// Adds (`add`) or removes the two ends of wire `w`.
    pub(super) fn ends(&mut self, w: [i32; 4], add: bool) {
        for p in [(w[0], w[1]), (w[2], w[3])] {
            for (v, k) in [(&mut self.xy, p), (&mut self.yx, (p.1, p.0))] {
                let i = v.partition_point(|q| *q < k);
                if add {
                    v.insert(i, k);
                } else {
                    assert_eq!(v.get(i), Some(&k), "a drawn wire end");
                    v.remove(i);
                }
            }
        }
        debug_assert_eq!(self.xy.len(), self.yx.len());
    }

    /// Adds (`add`) or removes the run of wire `w` from the per-row/column maps.
    pub(super) fn runs(&mut self, w: [i32; 4], add: bool) {
        let (map, key, run) = if w[1] == w[3] {
            (&mut self.horiz, w[1], (w[0].min(w[2]), w[0].max(w[2])))
        } else if w[0] == w[2] {
            (&mut self.vert, w[0], (w[1].min(w[3]), w[1].max(w[3])))
        } else {
            if add {
                self.diag.push(w);
            } else {
                let i = self
                    .diag
                    .iter()
                    .position(|d| *d == w)
                    .expect("a drawn wire");
                self.diag.swap_remove(i);
            }
            return;
        };
        let runs = map.entry(key).or_default();
        if add {
            runs.push(run);
        } else {
            let i = runs.iter().position(|r| *r == run).expect("a drawn run");
            runs.swap_remove(i);
        }
    }

    pub(super) fn add(&mut self, w: [i32; 4]) {
        self.ends(w, true);
        self.runs(w, true);
    }

    pub(super) fn remove(&mut self, w: [i32; 4]) {
        self.ends(w, false);
        self.runs(w, false);
    }

    /// Wire ends at `p`.
    fn ends_at(&self, p: Pt) -> usize {
        let i = self.xy.partition_point(|q| *q < p);
        self.xy[i..].iter().take_while(|q| **q == p).count()
    }

    /// Whether `p` lies strictly inside some wire.
    fn interior(&self, p: Pt) -> bool {
        let h = self
            .horiz
            .get(&p.1)
            .is_some_and(|v| v.iter().any(|&(a, c)| a < p.0 && p.0 < c));
        let v = self
            .vert
            .get(&p.0)
            .is_some_and(|v| v.iter().any(|&(a, c)| a < p.1 && p.1 < c));
        let d = self.diag.iter().any(|w| {
            let (ax, ay, bx, by) = (
                i64::from(w[0]),
                i64::from(w[1]),
                i64::from(w[2]),
                i64::from(w[3]),
            );
            let (px, py) = (i64::from(p.0), i64::from(p.1));
            let cross = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
            let dot = (px - ax) * (bx - ax) + (py - ay) * (by - ay);
            let len2 = (bx - ax).pow(2) + (by - ay).pow(2);
            cross == 0 && dot > 0 && dot < len2
        });
        h || v || d
    }

    /// xschem's rule: a dot at a wire end where three or more ends meet, or one touches
    /// another wire's interior.
    pub(super) fn dot_at(&self, p: Pt) -> bool {
        let n = self.ends_at(p);
        n >= 3 || (n >= 1 && self.interior(p))
    }

    /// Wire ends lying strictly inside wire `w` (where `w` decides their dots), plus `w`'s
    /// own ends: every point whose dot `w` can change.
    pub(super) fn touched(&self, w: [i32; 4], out: &mut Vec<Pt>) {
        out.extend([(w[0], w[1]), (w[2], w[3])]);
        if w[1] == w[3] {
            let (a, c) = (w[0].min(w[2]), w[0].max(w[2]));
            let i = self.yx.partition_point(|q| *q <= (w[1], a));
            let run = self.yx[i..].iter().take_while(|q| **q < (w[1], c));
            out.extend(run.map(|q| (q.1, q.0)));
        } else if w[0] == w[2] {
            let (a, c) = (w[1].min(w[3]), w[1].max(w[3]));
            let i = self.xy.partition_point(|q| *q <= (w[0], a));
            out.extend(self.xy[i..].iter().take_while(|q| **q < (w[0], c)));
        } else {
            // ponytail: a diagonal wire checks every end, O(ends); a 2-D index if
            // diagonal-heavy schematics show up in profiles.
            let mut probe = Wiring::default();
            probe.diag.push(w);
            out.extend(self.xy.iter().filter(|q| probe.interior(**q)));
        }
    }
}
