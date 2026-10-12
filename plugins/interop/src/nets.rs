//! Schematic connectivity of one cell (netlist.md §3, elab §9.2 naming), recomputed here
//! because the plugin cannot link `elab`: re-mastered instances and label stubs (spec
//! `interop.md` §6.2, §6.4) need each pin's net name.
//!
//! The oracle is elab itself: `tests/nets.rs` compares these partitions and names with
//! `schemify_elab`'s on every fixture.

use std::collections::HashMap;

use crate::design::{Design, Geom, place, sym_pins};

/// Node points per cell this extractor accepts (one per wire end, pin, label, port, noconn).
pub const NODES_MAX: usize = 1 << 22;

/// Marks a pin without a geometric node (inherited pins).
pub const NO_NODE: u32 = u32::MAX;

/// Nets of one cell.
#[derive(Clone, Debug, Default)]
pub struct Nets {
    /// Point of each node.
    pub node_at: Vec<[i32; 2]>,
    /// Net of each node.
    pub node_net: Vec<u32>,
    /// Per instance, per master pin (port order), its node or [`NO_NODE`].
    pub inst_pin: Vec<Vec<u32>>,
    /// Both end nodes of each wire.
    pub wire_ends: Vec<[u32; 2]>,
    /// Node of each label, port and noconn (same order as [`Geom`]).
    pub label_node: Vec<u32>,
    /// See [`Nets::label_node`].
    pub port_node: Vec<u32>,
    /// See [`Nets::label_node`].
    pub noconn_node: Vec<u32>,
    /// Chosen name of each net (Schemify syntax).
    pub net_name: Vec<String>,
    /// The name is global (`0`, `…!`, or a `global true` label).
    pub net_global: Vec<bool>,
    /// The name comes from a label or port (not generated).
    pub net_named: Vec<bool>,
}

struct Uf(Vec<u32>);

impl Uf {
    fn find(&mut self, mut a: u32) -> u32 {
        while self.0[a as usize] != a {
            let p = self.0[a as usize];
            self.0[a as usize] = self.0[p as usize];
            a = p;
        }
        a
    }
    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.0[hi as usize] = lo;
        }
    }
}

/// True when `p` lies on the closed segment `w` (exact integer test).
pub fn on_segment(p: [i32; 2], w: [i32; 4]) -> bool {
    let (x, y) = (i64::from(p[0]), i64::from(p[1]));
    let (x1, y1, x2, y2) = (
        i64::from(w[0]),
        i64::from(w[1]),
        i64::from(w[2]),
        i64::from(w[3]),
    );
    let cross = (x2 - x1) * (y - y1) - (y2 - y1) * (x - x1);
    cross == 0 && x >= x1.min(x2) && x <= x1.max(x2) && y >= y1.min(y2) && y <= y1.max(y2)
}

/// Extracts the nets of cell `ci` from its geometry `g`.
///
/// # Panics
///
/// When the cell has more than [`NODES_MAX`] nodes.
pub fn extract(d: &Design, ci: usize, g: &Geom) -> Nets {
    let mut n = Nets::default();
    let mut owner_id: Vec<Option<String>> = Vec::new(); // id-bearing owner per node (for naming)
    let mut push = |n: &mut Nets, at: [i32; 2], id: Option<String>| -> u32 {
        n.node_at.push(at);
        owner_id.push(id);
        (n.node_at.len() - 1) as u32
    };
    for (k, w) in g.wire.iter().enumerate() {
        let a = push(&mut n, [w[0], w[1]], Some(g.wire_id[k].clone()));
        let b = push(&mut n, [w[2], w[3]], Some(g.wire_id[k].clone()));
        n.wire_ends.push([a, b]);
    }
    // (instance id, pin index) of instance pin nodes, for generated names.
    let mut pin_owner: HashMap<u32, (String, usize)> = HashMap::new();
    let mut alias_joins = Vec::new();
    for k in 0..g.inst_id.len() {
        let pins = sym_pins(&d.cells[g.inst_master[k] as usize]);
        let mut nodes = Vec::with_capacity(pins.name.len());
        for p in 0..pins.name.len() {
            if pins.inherit[p].is_some() {
                nodes.push(NO_NODE);
                continue;
            }
            let at = place(pins.at[p], g.inst_orient[k], g.inst_at[k]).unwrap_or(g.inst_at[k]);
            let node = push(&mut n, at, None);
            pin_owner.insert(node, (g.inst_id[k].clone(), p));
            nodes.push(node);
            if let Some(target) = &pins.alias[p]
                && let Some(t) = pins.name.iter().position(|x| x == target)
            {
                alias_joins.push((k, p, t));
            }
        }
        n.inst_pin.push(nodes);
    }
    for k in 0..g.label_at.len() {
        let v = push(&mut n, g.label_at[k], Some(g.label_id[k].clone()));
        n.label_node.push(v);
    }
    for k in 0..g.port_at.len() {
        let v = push(&mut n, g.port_at[k], Some(g.port_id[k].clone()));
        n.port_node.push(v);
    }
    for k in 0..g.noconn_at.len() {
        let v = push(&mut n, g.noconn_at[k], Some(g.noconn_id[k].clone()));
        n.noconn_node.push(v);
    }
    let count = n.node_at.len();
    assert!(
        count <= NODES_MAX,
        "cell {} has {count} nodes (max {NODES_MAX})",
        d.cells[ci].path
    );
    let mut uf = Uf((0..count as u32).collect());
    for e in &n.wire_ends {
        uf.union(e[0], e[1]);
    }
    for &(k, p, t) in &alias_joins {
        let (a, b) = (n.inst_pin[k][p], n.inst_pin[k][t]);
        if a != NO_NODE && b != NO_NODE {
            uf.union(a, b);
        }
    }
    // Identical coordinates.
    let mut first: HashMap<[i32; 2], u32> = HashMap::new();
    for (v, at) in n.node_at.iter().enumerate() {
        match first.get(at) {
            Some(&u) => uf.union(u, v as u32),
            None => {
                first.insert(*at, v as u32);
            }
        }
    }
    // Nodes on wire segments (T-junctions). ponytail: O(nodes × wires); a sweep over sorted
    // segments when a 10^5-wire cell needs it.
    for (v, at) in n.node_at.iter().enumerate() {
        for (k, w) in g.wire.iter().enumerate() {
            if on_segment(*at, *w) {
                uf.union(v as u32, n.wire_ends[k][0]);
            }
        }
    }
    // Number nets by their smallest node.
    let mut root_net: HashMap<u32, u32> = HashMap::new();
    n.node_net = Vec::with_capacity(count);
    for v in 0..count as u32 {
        let r = uf.find(v);
        let next = root_net.len() as u32;
        let id = *root_net.entry(r).or_insert(next);
        n.node_net.push(id);
    }
    let nets = root_net.len();
    // Names: (class, text) candidates; class 0 global, 1 port, 2 label (elab §9.2).
    let mut best: Vec<Option<(u8, String, bool)>> = vec![None; nets];
    let offer = |best: &mut Vec<Option<(u8, String, bool)>>,
                 net: u32,
                 class: u8,
                 text: &str,
                 global: bool| {
        let slot = &mut best[net as usize];
        let better = match slot {
            None => true,
            Some((c, t, _)) => (class, text) < (*c, t.as_str()),
        };
        if better {
            *slot = Some((class, text.to_owned(), global));
        }
    };
    for k in 0..g.port_at.len() {
        let net = n.node_net[n.port_node[k] as usize];
        offer(&mut best, net, 1, &g.port_pin[k], false);
    }
    for k in 0..g.label_at.len() {
        let net = n.node_net[n.label_node[k] as usize];
        let t = &g.label_net[k];
        let global = g.label_global[k] || t == "0" || t.ends_with('!');
        offer(&mut best, net, if global { 0 } else { 2 }, t, global);
    }
    let mut gen_id: Vec<Option<String>> = vec![None; nets];
    let mut gen_pin: Vec<Option<(String, usize)>> = vec![None; nets];
    for (v, owner) in owner_id.iter().enumerate() {
        let net = n.node_net[v] as usize;
        if let Some(id) = owner
            && gen_id[net].as_ref().is_none_or(|x| id < x)
        {
            gen_id[net] = Some(id.clone());
        }
        if let Some(po) = pin_owner.get(&(v as u32))
            && gen_pin[net].as_ref().is_none_or(|x| po < x)
        {
            gen_pin[net] = Some(po.clone());
        }
    }
    for k in 0..nets {
        match &best[k] {
            Some((_, t, global)) => {
                n.net_name.push(t.clone());
                n.net_global.push(*global);
                n.net_named.push(true);
            }
            None => {
                let name = match (&gen_id[k], &gen_pin[k]) {
                    (Some(id), _) => format!("net_{id}"),
                    (None, Some((inst, pin))) => format!("net_{inst}_{pin}"),
                    (None, None) => unreachable!("every net has a node"),
                };
                n.net_name.push(name);
                n.net_global.push(false);
                n.net_named.push(false);
            }
        }
    }
    assert_eq!(n.net_name.len(), nets);
    assert_eq!(n.node_net.len(), count);
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::{Files, geom, load};
    use std::collections::BTreeSet;

    #[test]
    fn t_junction_label_and_pin_names() {
        let f: Files = [(
            "c.shbench".to_owned(),
            b"schemify 1 shbench\n\ninst aaaaaa core:res\n  at 0 0 R0\n  name R1\n\nwire bbbbbb 0 30 0 60\nwire cccccc -20 60 20 60\n\nlabel dddddd 20 60 R0 out\n".to_vec(),
        )]
        .into();
        let absent: BTreeSet<String> = ["schemify.project".to_owned()].into();
        let d = load(&f, &absent, &["c.shbench"]).unwrap();
        let g = geom(&d, 0);
        let n = extract(&d, 0, &g);
        let p = n.node_net[n.inst_pin[0][1] as usize] as usize; // res pin n at (0,30)
        assert_eq!(n.net_name[p], "out");
        let top = n.node_net[n.inst_pin[0][0] as usize] as usize; // pin p at (0,-30): alone
        assert_eq!(n.net_name[top], "net_aaaaaa_0");
        assert!(!n.net_named[top] && n.net_named[p]);
        assert!(on_segment([0, 5], [0, 0, 0, 10]) && !on_segment([1, 5], [0, 0, 0, 10]));
    }
}
