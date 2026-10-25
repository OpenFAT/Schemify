//! Dependency-graph algorithms (spec §4) over caller-owned index tables.
//! `elab` owns the graph (`expr_graph`); these are the transforms it runs.
//!
//! Edges are "depends on": node `i` reads the values of `deps.edges(i)`.
//! Determinism: results depend only on node and edge order, never on hashing.

use crate::{Code, Error, Span};

/// Adjacency in compressed sparse rows: node `i`'s edges are
/// `targets[offsets[i]..offsets[i + 1]]`. `offsets.len()` is the node count
/// plus one.
#[derive(Clone, Copy, Debug)]
pub struct Deps<'a> {
    /// Non-decreasing, starts at 0, ends at `targets.len()`.
    pub offsets: &'a [u32],
    /// Node indices, each `< offsets.len() - 1`.
    pub targets: &'a [u32],
}

impl Deps<'_> {
    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.offsets.len() - 1
    }

    /// True when there are no nodes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Edges of node `i`.
    pub fn edges(&self, i: u32) -> &[u32] {
        &self.targets[self.offsets[i as usize] as usize..self.offsets[i as usize + 1] as usize]
    }

    fn check(&self) {
        assert!(
            !self.offsets.is_empty() && self.offsets[0] == 0,
            "CSR offsets must start with 0"
        );
        assert!(
            self.offsets.windows(2).all(|w| w[0] <= w[1]),
            "CSR offsets must not decrease"
        );
        assert_eq!(
            *self.offsets.last().expect("non-empty") as usize,
            self.targets.len(),
            "CSR offsets must end at targets.len()"
        );
        let n = self.len();
        assert!(
            self.targets.iter().all(|&t| (t as usize) < n),
            "CSR target out of range"
        );
    }
}

/// A dependency cycle: `nodes[0]` depends on `nodes[1]`, …, and the last
/// depends on `nodes[0]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cycle {
    /// The nodes on the cycle, in dependency order; never empty.
    pub nodes: Vec<u32>,
}

impl Cycle {
    /// `X005` naming the cycle as `a -> b -> a`.
    pub fn error<'n>(&self, name: impl Fn(u32) -> &'n str) -> Error {
        assert!(!self.nodes.is_empty(), "a cycle has at least one node");
        let mut msg = String::from("dependency cycle: ");
        for &n in &self.nodes {
            msg.push_str(name(n));
            msg.push_str(" -> ");
        }
        msg.push_str(name(self.nodes[0]));
        Error::new(Code::X005, Span::default(), msg)
    }
}

/// Writes a topological order into `order` (cleared first): every node after
/// all nodes it depends on. Among independent nodes, lower indices come first.
///
/// Cost: O(nodes + edges) time, O(nodes) scratch.
///
/// # Errors
/// The first cycle found (searching from node 0 upward); `order` is then partial.
///
/// # Panics
/// If `deps` is not a valid CSR table.
pub fn topo_order(deps: Deps<'_>, order: &mut Vec<u32>) -> Result<(), Cycle> {
    deps.check();
    let n = deps.len();
    order.clear();
    // 0 unvisited, 1 on the DFS path, 2 emitted.
    let mut state = vec![0u8; n];
    let mut path: Vec<(u32, u32)> = Vec::new();
    for root in 0..n as u32 {
        if state[root as usize] != 0 {
            continue;
        }
        state[root as usize] = 1;
        path.push((root, deps.offsets[root as usize]));
        while let Some(top) = path.last_mut() {
            let (v, next) = *top;
            if next == deps.offsets[v as usize + 1] {
                state[v as usize] = 2;
                order.push(v);
                path.pop();
                continue;
            }
            top.1 += 1;
            let w = deps.targets[next as usize];
            match state[w as usize] {
                0 => {
                    state[w as usize] = 1;
                    path.push((w, deps.offsets[w as usize]));
                }
                1 => {
                    let at = path
                        .iter()
                        .position(|&(u, _)| u == w)
                        .expect("a node on the path");
                    return Err(Cycle {
                        nodes: path[at..].iter().map(|&(u, _)| u).collect(),
                    });
                }
                _ => {}
            }
            debug_assert!(path.len() <= n, "DFS path longer than the node count");
        }
    }
    assert_eq!(order.len(), n, "every node emitted exactly once");
    Ok(())
}

/// Writes every node into `order` (cleared first) and every cycle into `cycles`
/// (cleared first), in one pass: [`topo_order`] for graphs that may have cycles.
///
/// `order` lists strongly connected components dependencies first, each component's
/// nodes together, so every edge between components is satisfied; edges inside a
/// cyclic component cannot be. On an acyclic graph `order` equals [`topo_order`]'s.
///
/// `cycles` covers every node that lies on a cycle (a component of two or more nodes,
/// or one with a self-loop), and no other: per cyclic component, in `order`'s
/// component order, the shortest cycle through its lowest uncovered node, repeated
/// until each of the component's nodes is on a reported cycle. Each [`Cycle`] is a
/// real cycle of edges.
///
/// Cost: O(nodes + edges) for the order; covering a component of k nodes is at most
/// k breadth-first searches inside it, O(k · (k + edges inside)).
///
/// # Panics
/// If `deps` is not a valid CSR table.
pub fn topo_order_all(deps: Deps<'_>, order: &mut Vec<u32>, cycles: &mut Vec<Cycle>) {
    deps.check();
    let n = deps.len();
    order.clear();
    cycles.clear();
    let mut t = Tarjan {
        index: vec![NONE; n],
        low: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        path: Vec::new(),
        next: 0,
    };
    let mut cover = Cover::new(n);
    for root in 0..n as u32 {
        if t.index[root as usize] != NONE {
            continue;
        }
        t.visit(deps, root);
        while let Some(&(v, next)) = t.path.last() {
            if next < deps.offsets[v as usize + 1] {
                t.path.last_mut().expect("non-empty").1 += 1;
                let w = deps.targets[next as usize];
                if t.index[w as usize] == NONE {
                    t.visit(deps, w);
                } else if t.on_stack[w as usize] {
                    t.low[v as usize] = t.low[v as usize].min(t.index[w as usize]);
                }
                continue;
            }
            t.path.pop();
            if let Some(&(u, _)) = t.path.last() {
                t.low[u as usize] = t.low[u as usize].min(t.low[v as usize]);
            }
            if t.low[v as usize] == t.index[v as usize] {
                let start = order.len();
                loop {
                    let w = t.stack.pop().expect("the component root is on the stack");
                    t.on_stack[w as usize] = false;
                    order.push(w);
                    if w == v {
                        break;
                    }
                }
                let component = &order[start..];
                if component.len() > 1 || deps.edges(v).contains(&v) {
                    cover.component(deps, component, cycles);
                }
            }
        }
    }
    assert_eq!(order.len(), n, "every node emitted exactly once");
    assert!(t.stack.is_empty() && t.path.is_empty());
}

/// Marker for "no index yet" / "no parent" in [`topo_order_all`].
const NONE: u32 = u32::MAX;

/// Tarjan's strongly connected components, iterative: discovery index ([`NONE`] =
/// unseen), low link, the component stack and the DFS path of (node, next edge offset).
struct Tarjan {
    index: Vec<u32>,
    low: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<u32>,
    path: Vec<(u32, u32)>,
    next: u32,
}

impl Tarjan {
    fn visit(&mut self, deps: Deps<'_>, v: u32) {
        assert_eq!(self.index[v as usize], NONE, "each node is discovered once");
        self.index[v as usize] = self.next;
        self.low[v as usize] = self.next;
        self.next += 1;
        self.stack.push(v);
        self.on_stack[v as usize] = true;
        self.path.push((v, deps.offsets[v as usize]));
        assert!(
            self.path.len() <= self.index.len(),
            "DFS path within the node count"
        );
    }
}

/// Scratch for covering cyclic components with shortest cycles: component membership,
/// coverage and breadth-first-search parents, one slot per node, reset after each use.
struct Cover {
    in_component: Vec<bool>,
    covered: Vec<bool>,
    parent: Vec<u32>,
    queue: Vec<u32>,
}

impl Cover {
    fn new(n: usize) -> Cover {
        Cover {
            in_component: vec![false; n],
            covered: vec![false; n],
            parent: vec![NONE; n],
            queue: Vec::new(),
        }
    }

    /// Appends cycles to `cycles` until every node of the strongly connected
    /// `component` is on one.
    fn component(&mut self, deps: Deps<'_>, component: &[u32], cycles: &mut Vec<Cycle>) {
        assert!(!component.is_empty());
        let mut members = component.to_vec();
        members.sort_unstable();
        for &v in &members {
            self.in_component[v as usize] = true;
        }
        for &v in &members {
            if self.covered[v as usize] {
                continue;
            }
            let cycle = self.shortest_cycle(deps, v);
            for &u in &cycle {
                self.covered[u as usize] = true;
            }
            cycles.push(Cycle { nodes: cycle });
        }
        for &v in &members {
            assert!(
                self.covered[v as usize],
                "every component node is on a cycle"
            );
            self.in_component[v as usize] = false;
        }
    }

    /// Returns the shortest cycle through `v` inside the marked component, starting at
    /// `v`. One exists: the component is strongly connected and cyclic.
    fn shortest_cycle(&mut self, deps: Deps<'_>, v: u32) -> Vec<u32> {
        self.queue.clear();
        self.queue.push(v);
        let mut head = 0;
        let mut last = NONE; // the node whose edge closes the cycle back to `v`
        'search: while head < self.queue.len() {
            let u = self.queue[head];
            head += 1;
            for &w in deps.edges(u) {
                if w == v {
                    last = u;
                    break 'search;
                }
                if self.in_component[w as usize] && self.parent[w as usize] == NONE {
                    self.parent[w as usize] = u;
                    self.queue.push(w);
                }
            }
        }
        assert!(
            last != NONE,
            "a strongly connected cyclic component closes every cycle"
        );
        let mut cycle = vec![last];
        while *cycle.last().expect("non-empty") != v {
            let p = self.parent[*cycle.last().expect("non-empty") as usize];
            assert!(
                p != NONE && cycle.len() <= self.queue.len(),
                "parents lead back to v"
            );
            cycle.push(p);
        }
        cycle.reverse();
        for &u in &self.queue {
            self.parent[u as usize] = NONE;
        }
        assert_eq!(cycle[0], v);
        cycle
    }
}

/// Builds the reverse table (dependents) of `deps` into `offsets`/`targets`
/// (both cleared). Each node's dependents are in increasing index order.
///
/// # Panics
/// If `deps` is not a valid CSR table.
pub fn reverse(deps: Deps<'_>, offsets: &mut Vec<u32>, targets: &mut Vec<u32>) {
    deps.check();
    let n = deps.len();
    offsets.clear();
    offsets.resize(n + 1, 0);
    for &t in deps.targets {
        offsets[t as usize + 1] += 1;
    }
    for i in 0..n {
        offsets[i + 1] += offsets[i];
    }
    targets.clear();
    targets.resize(deps.targets.len(), 0);
    let mut fill: Vec<u32> = offsets[..n].to_vec();
    for i in 0..n as u32 {
        for &t in deps.edges(i) {
            targets[fill[t as usize] as usize] = i;
            fill[t as usize] += 1;
        }
    }
    assert_eq!(offsets[n] as usize, targets.len());
}

/// Incremental recompute set: writes into `out` (cleared first) the `dirty`
/// nodes and everything that transitively depends on them, sorted by `rank`,
/// so re-evaluating `out` in order equals a full recompute. `rank[i]` is
/// node `i`'s position in a [`topo_order`] result. Valid only while the edges
/// are unchanged: an edited expression whose identifiers changed needs a new
/// order first.
///
/// Cost: O(affected nodes + their edges + log) plus O(nodes) scratch.
///
/// # Panics
/// If `dependents` is invalid, `rank.len()` differs from the node count, or
/// a dirty index is out of range.
pub fn affected(dependents: Deps<'_>, rank: &[u32], dirty: &[u32], out: &mut Vec<u32>) {
    dependents.check();
    let n = dependents.len();
    assert_eq!(rank.len(), n, "one rank per node");
    out.clear();
    let mut seen = vec![false; n];
    for &d in dirty {
        assert!((d as usize) < n, "dirty node {d} out of range");
        if !seen[d as usize] {
            seen[d as usize] = true;
            out.push(d);
        }
    }
    let mut i = 0;
    while i < out.len() {
        for &w in dependents.edges(out[i]) {
            if !seen[w as usize] {
                seen[w as usize] = true;
                out.push(w);
            }
        }
        i += 1;
    }
    out.sort_unstable_by_key(|&v| rank[v as usize]);
    debug_assert!(out.len() <= n);
}
