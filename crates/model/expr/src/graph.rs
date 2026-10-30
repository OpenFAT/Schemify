//! Dependency-graph algorithms (spec §4) over caller-owned index tables.
//! `elab` owns the graph (`expr_graph`); these are the transforms it runs.
//!
//! Edges are "depends on": node `i` reads the values of `deps.edges(i)`.
//! Determinism: results depend only on node and edge order, never on hashing.
//!
//! Contents:
//! - `pub struct Deps<'table>`, `Deps::len(&self) -> usize`, `Deps::is_empty(&self) -> bool`,
//!   `Deps::edges(&self, i: u32) -> &[u32]`
//! - `pub struct Cycle`, `Cycle::error(&self, name) -> Error`
//! - `pub fn topo_order(deps: Deps<'_>, order: &mut Vec<u32>) -> Result<(), Cycle>`
//! - `pub fn topo_order_all(deps: Deps<'_>, order: &mut Vec<u32>, cycles: &mut Vec<Cycle>)`
//! - `pub fn reverse(deps: Deps<'_>, offsets: &mut Vec<u32>, targets: &mut Vec<u32>)`

use crate::{Code, Error, Span};

/// Adjacency in compressed sparse rows: node `i`'s edges are
/// `targets[offsets[i]..offsets[i + 1]]`. `offsets.len()` is the node count
/// plus one.
#[derive(Clone, Copy, Debug)]
pub struct Deps<'table> {
    /// Non-decreasing, starts at 0, ends at `targets.len()`.
    pub offsets: &'table [u32],
    /// Node indices, each `< offsets.len() - 1`.
    pub targets: &'table [u32],
}

/// A dependency cycle: `nodes[0]` depends on `nodes[1]`, …, and the last
/// depends on `nodes[0]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cycle {
    /// The nodes on the cycle, in dependency order; never empty.
    pub nodes: Vec<u32>,
}

/// Marker for "no index yet" / "no parent" in [`topo_order_all`].
const NONE: u32 = u32::MAX;

/// One node on an iterative depth-first path and the offset in `targets` of
/// the next edge to follow from it.
#[derive(Clone, Copy, Debug)]
struct PathEntry {
    node: u32,
    next_edge: u32,
}

/// Tarjan's strongly connected components, iterative: discovery index ([`NONE`] =
/// unseen), low link, the component stack and the DFS path.
struct Tarjan {
    index: Vec<u32>,
    low: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<u32>,
    path: Vec<PathEntry>,
    next: u32,
}

/// Scratch for covering cyclic components with shortest cycles: component membership,
/// coverage and breadth-first-search parents, one slot per node, reset after each use.
struct Cover {
    in_component: Vec<bool>,
    covered: Vec<bool>,
    parent: Vec<u32>,
    queue: Vec<u32>,
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
    let mut path: Vec<PathEntry> = Vec::new();
    for root in 0..n as u32 {
        if state[root as usize] != 0 {
            continue;
        }
        state[root as usize] = 1;
        path.push(PathEntry {
            node: root,
            next_edge: deps.offsets[root as usize],
        });
        while let Some(top) = path.last_mut() {
            let PathEntry { node: v, next_edge } = *top;
            if next_edge == deps.offsets[v as usize + 1] {
                state[v as usize] = 2;
                order.push(v);
                path.pop();
                continue;
            }
            top.next_edge += 1;
            let w = deps.targets[next_edge as usize];
            match state[w as usize] {
                0 => {
                    state[w as usize] = 1;
                    path.push(PathEntry {
                        node: w,
                        next_edge: deps.offsets[w as usize],
                    });
                }
                1 => {
                    let at = path
                        .iter()
                        .position(|entry| entry.node == w)
                        .expect("a node in state 1 is on the path");
                    let nodes: Vec<u32> = path[at..].iter().map(|entry| entry.node).collect();
                    debug_assert_eq!(nodes[0], w, "the cycle starts where it closes");
                    return Err(Cycle { nodes });
                }
                _ => {}
            }
            debug_assert!(path.len() <= n, "DFS path longer than the node count");
        }
    }
    assert_eq!(order.len(), n, "every node emitted exactly once");
    debug_assert!(state.iter().all(|&s| s == 2), "every node emitted");
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
        while let Some(top) = t.path.last_mut() {
            let PathEntry { node: v, next_edge } = *top;
            if next_edge < deps.offsets[v as usize + 1] {
                top.next_edge += 1;
                let w = deps.targets[next_edge as usize];
                if t.index[w as usize] == NONE {
                    t.visit(deps, w);
                } else if t.on_stack[w as usize] {
                    t.low[v as usize] = t.low[v as usize].min(t.index[w as usize]);
                }
                continue;
            }
            t.path.pop();
            if let Some(parent) = t.path.last() {
                let u = parent.node as usize;
                t.low[u] = t.low[u].min(t.low[v as usize]);
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
                    debug_assert!(order.len() - start <= n, "a component fits in the graph");
                }
                let component = &order[start..];
                debug_assert!(!component.is_empty(), "a component holds its root");
                if component.len() > 1 || deps.edges(v).contains(&v) {
                    cover.component(deps, component, cycles);
                }
            }
        }
    }
    assert_eq!(order.len(), n, "every node emitted exactly once");
    assert!(t.stack.is_empty(), "every component popped");
    assert!(t.path.is_empty(), "every DFS path unwound");
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
    // Each node's dependents filled its slot range exactly.
    debug_assert_eq!(fill[..], offsets[1..]);
}

impl Deps<'_> {
    /// Number of nodes.
    ///
    /// # Panics
    /// If `offsets` is empty (not a CSR table).
    #[must_use]
    pub fn len(&self) -> usize {
        assert!(
            !self.offsets.is_empty(),
            "CSR offsets hold at least the 0 start"
        );
        self.offsets.len() - 1
    }

    /// True when there are no nodes.
    ///
    /// # Panics
    /// As [`Deps::len`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The nodes node `i` depends on.
    ///
    /// # Panics
    /// If `i >= self.len()`.
    #[must_use]
    pub fn edges(&self, i: u32) -> &[u32] {
        let i = i as usize;
        assert!(i < self.len(), "node {i} of {}", self.len());
        let (start, end) = (self.offsets[i] as usize, self.offsets[i + 1] as usize);
        debug_assert!(start <= end, "CSR offsets must not decrease");
        &self.targets[start..end]
    }
}

impl Cycle {
    /// `X005` naming the cycle as `a -> b -> a`.
    ///
    /// # Panics
    /// If `nodes` is empty.
    pub fn error<'name>(&self, name: impl Fn(u32) -> &'name str) -> Error {
        use core::fmt::Write;

        assert!(!self.nodes.is_empty(), "a cycle has at least one node");
        let mut msg = String::from("dependency cycle: ");
        for &n in &self.nodes {
            write!(msg, "{} -> ", name(n)).expect("writing to a String cannot fail");
        }
        msg.push_str(name(self.nodes[0]));
        Error::new(Code::X005, Span::default(), msg)
    }
}

// ---- private ----

impl Deps<'_> {
    /// Asserts the CSR invariants of [`Deps`].
    fn check(&self) {
        assert!(
            !self.offsets.is_empty(),
            "CSR offsets must hold the 0 start"
        );
        assert_eq!(self.offsets[0], 0, "CSR offsets must start with 0");
        assert!(
            self.offsets.windows(2).all(|w| w[0] <= w[1]),
            "CSR offsets must not decrease"
        );
        assert_eq!(
            self.offsets[self.offsets.len() - 1] as usize,
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

impl Tarjan {
    fn visit(&mut self, deps: Deps<'_>, v: u32) {
        assert_eq!(self.index[v as usize], NONE, "each node is discovered once");
        self.index[v as usize] = self.next;
        self.low[v as usize] = self.next;
        self.next += 1;
        self.stack.push(v);
        self.on_stack[v as usize] = true;
        self.path.push(PathEntry {
            node: v,
            next_edge: deps.offsets[v as usize],
        });
        assert!(
            self.path.len() <= self.index.len(),
            "DFS path within the node count"
        );
    }
}

impl Cover {
    fn new(n: usize) -> Cover {
        assert!(n < NONE as usize, "node indices stay below the NONE marker");
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
        assert!(!component.is_empty(), "a component holds its root");
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
                debug_assert!(
                    self.in_component[u as usize],
                    "cycles stay in the component"
                );
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
        debug_assert!(
            self.in_component[v as usize],
            "start inside the marked component"
        );
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
            assert_ne!(p, NONE, "parents lead back to v");
            assert!(
                cycle.len() <= self.queue.len(),
                "the parent walk is bounded"
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
