//! Dependency graph oracle: incremental recompute (dirty → affected, in
//! topological order) equals a full recompute after random edits, over real
//! compiled expressions; cycles are found and named.

use proptest::prelude::*;
use schemify_expr::{
    Code, Deps, Mode, Program, Scratch, Value, affected, reverse, topo_order, topo_order_all,
};

/// Deterministic xorshift for structure choices inside one proptest case.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random acyclic set of named expressions. Node `k`'s text is
/// `c + coef*p<j> + …`; storage order is a permutation of generation order,
/// so index order is not a topological order.
struct Design {
    consts: Vec<i64>,
    /// Per storage index: (coef, storage index of the dependency).
    terms: Vec<Vec<(i64, usize)>>,
}

impl Design {
    fn random(n: usize, seed: u64) -> Design {
        let mut rng = Rng(seed | 1);
        let mut perm: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            perm.swap(i, rng.below(i as u64 + 1) as usize);
        }
        let mut terms = vec![Vec::new(); n];
        let mut consts = vec![0; n];
        for g in 0..n {
            consts[perm[g]] = rng.below(21) as i64 - 10;
            for h in 0..g {
                if rng.below(4) == 0 && terms[perm[g]].len() < 4 {
                    terms[perm[g]].push((rng.below(5) as i64 - 2, perm[h]));
                }
            }
        }
        Design { consts, terms }
    }

    fn text(&self, k: usize) -> String {
        let mut s = self.consts[k].to_string();
        for (c, j) in &self.terms[k] {
            s.push_str(&format!(" + {c}*p{j}"));
        }
        s
    }

    fn programs(&self) -> Vec<Program> {
        (0..self.consts.len())
            .map(|k| Program::compile(&self.text(k), Mode::Scalar).unwrap())
            .collect()
    }
}

/// CSR "depends on" table from each program's identifiers (`p<j>` → j).
fn deps_of(progs: &[Program]) -> (Vec<u32>, Vec<u32>) {
    let mut offsets = vec![0u32];
    let mut targets = Vec::new();
    for p in progs {
        targets.extend(p.idents().map(|n| n[1..].parse::<u32>().unwrap()));
        offsets.push(targets.len() as u32);
    }
    (offsets, targets)
}

/// Values grow geometrically along long chains, so ints may overflow into
/// nums; both are kept.
fn eval_node(p: &Program, values: &[Value<'static>], s: &mut Scratch) -> Value<'static> {
    let inputs: Vec<Value> = p
        .idents()
        .map(|n| values[n[1..].parse::<usize>().unwrap()])
        .collect();
    match p.eval(&inputs, s).unwrap() {
        Value::Int(v) => Value::Int(v),
        Value::Num(v) => Value::Num(v),
        v => panic!("{v:?}"),
    }
}

fn full(progs: &[Program], order: &[u32]) -> Vec<Value<'static>> {
    let mut values = vec![Value::Int(0); progs.len()];
    let mut s = Scratch::default();
    for &k in order {
        values[k as usize] = eval_node(&progs[k as usize], &values, &mut s);
    }
    values
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn incremental_equals_full(n in 1usize..40, seed in any::<u64>(), edits in prop::collection::vec((any::<usize>(), -50i64..50), 1..6)) {
        let mut d = Design::random(n, seed);
        let mut progs = d.programs();
        let (offsets, targets) = deps_of(&progs);
        let deps = Deps { offsets: &offsets, targets: &targets };
        let mut order = Vec::new();
        topo_order(deps, &mut order).expect("generated designs are acyclic");
        let mut rank = vec![0u32; n];
        for (pos, &k) in order.iter().enumerate() {
            rank[k as usize] = pos as u32;
            for &j in deps.edges(k) {
                prop_assert!(order[..pos].contains(&j), "dependency after dependent");
            }
        }
        let mut values = full(&progs, &order);

        // Edit constants (identifiers unchanged, so the order stays valid).
        let mut dirty = Vec::new();
        for (k, c) in edits {
            let k = k % n;
            d.consts[k] = c;
            progs[k] = Program::compile(&d.text(k), Mode::Scalar).unwrap();
            dirty.push(k as u32);
        }
        let (mut roff, mut rtgt) = (Vec::new(), Vec::new());
        reverse(deps, &mut roff, &mut rtgt);
        let mut todo = Vec::new();
        affected(Deps { offsets: &roff, targets: &rtgt }, &rank, &dirty, &mut todo);
        prop_assert!(todo.windows(2).all(|w| rank[w[0] as usize] < rank[w[1] as usize]));
        let mut s = Scratch::default();
        for &k in &todo {
            values[k as usize] = eval_node(&progs[k as usize], &values, &mut s);
        }
        prop_assert_eq!(values, full(&progs, &order));
    }

    #[test]
    fn cycles_are_found_and_real(n in 2usize..30, seed in any::<u64>(), pick in any::<(usize, usize)>()) {
        let d = Design::random(n, seed);
        let progs = d.programs();
        let (mut offsets, mut targets) = deps_of(&progs);
        // Close a loop: a -> b plus some path b -> … -> a (or b = a).
        let (a, b) = (pick.0 % n, pick.1 % n);
        let mut order = Vec::new();
        topo_order(Deps { offsets: &offsets, targets: &targets }, &mut order).unwrap();
        let (pa, pb) = (order.iter().position(|&k| k == a as u32).unwrap(), order.iter().position(|&k| k == b as u32).unwrap());
        let (early, late) = if pa <= pb { (a, b) } else { (b, a) };
        // `early` now also depends on `late`, and `late` on `early`: a cycle.
        let insert = |offsets: &mut Vec<u32>, targets: &mut Vec<u32>, from: usize, to: u32| {
            targets.insert(offsets[from + 1] as usize, to);
            for o in &mut offsets[from + 1..] { *o += 1; }
        };
        insert(&mut offsets, &mut targets, early, late as u32);
        insert(&mut offsets, &mut targets, late, early as u32);
        let deps = Deps { offsets: &offsets, targets: &targets };
        let cycle = topo_order(deps, &mut order).unwrap_err();
        let c = &cycle.nodes;
        for i in 0..c.len() {
            let (from, to) = (c[i], c[(i + 1) % c.len()]);
            prop_assert!(deps.edges(from).contains(&to), "{from} -> {to} is not an edge");
        }
        let names: Vec<String> = (0..n).map(|k| format!("p{k}")).collect();
        let e = cycle.error(|k| &names[k as usize]);
        prop_assert_eq!(e.code, Code::X005);
        prop_assert!(e.msg.contains(&names[c[0] as usize]));
    }
}

#[test]
fn self_loop_and_empty() {
    let (offsets, targets) = ([0u32, 1], [0u32]);
    let e = topo_order(
        Deps {
            offsets: &offsets,
            targets: &targets,
        },
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(e.nodes, [0]);
    assert_eq!(e.error(|_| "w").msg, "dependency cycle: w -> w");
    let mut order = vec![9];
    topo_order(
        Deps {
            offsets: &[0],
            targets: &[],
        },
        &mut order,
    )
    .unwrap();
    assert!(order.is_empty());
}

/// A random graph with self-loops and cycles allowed: `n` nodes, each edge present with
/// probability 1/`sparsity`, as a CSR table.
fn random_graph(n: usize, sparsity: u64, seed: u64) -> (Vec<u32>, Vec<u32>) {
    let mut rng = Rng(seed | 1);
    let mut offsets = vec![0u32];
    let mut targets = Vec::new();
    for _ in 0..n {
        for j in 0..n as u32 {
            if rng.below(sparsity) == 0 {
                targets.push(j);
            }
        }
        offsets.push(targets.len() as u32);
    }
    (offsets, targets)
}

/// Naive reachability (`reach[a][b]`: a path of at least one edge from a to b).
fn reachability(deps: Deps<'_>) -> Vec<Vec<bool>> {
    let n = deps.len();
    (0..n as u32)
        .map(|a| {
            let mut seen = vec![false; n];
            let mut todo: Vec<u32> = deps.edges(a).to_vec();
            while let Some(v) = todo.pop() {
                if !std::mem::replace(&mut seen[v as usize], true) {
                    todo.extend_from_slice(deps.edges(v));
                }
            }
            seen
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// `topo_order_all` against naive reachability: `order` is a permutation in which
    /// every edge between different components points backwards and each component's
    /// nodes are contiguous; the reported cycles are real and cover exactly the nodes
    /// that reach themselves; acyclic graphs get `topo_order`'s order.
    #[test]
    fn all_cycles_in_one_pass(n in 1usize..30, sparsity in 2u64..12, seed in any::<u64>()) {
        let (offsets, targets) = random_graph(n, sparsity, seed);
        let deps = Deps { offsets: &offsets, targets: &targets };
        let (mut order, mut cycles) = (Vec::new(), Vec::new());
        topo_order_all(deps, &mut order, &mut cycles);
        let reach = reachability(deps);
        let same_component = |a: usize, b: usize| a == b || (reach[a][b] && reach[b][a]);
        let mut pos = vec![usize::MAX; n];
        for (p, &k) in order.iter().enumerate() {
            prop_assert_eq!(pos[k as usize], usize::MAX, "node {} twice", k);
            pos[k as usize] = p;
        }
        prop_assert!(pos.iter().all(|&p| p < n), "every node ordered");
        for a in 0..n as u32 {
            for &b in deps.edges(a) {
                let (a, b) = (a as usize, b as usize);
                if !same_component(a, b) {
                    prop_assert!(pos[b] < pos[a], "{} depends on {} but comes first", a, b);
                }
            }
        }
        for (i, &a) in order.iter().enumerate() {
            for &b in &order[i + 1..] {
                if same_component(a as usize, b as usize) {
                    let between = order[i..].iter().position(|&x| x == b).unwrap();
                    prop_assert!(
                        order[i..i + between].iter().all(|&x| same_component(x as usize, a as usize)),
                        "component of {} is not contiguous", a
                    );
                }
            }
        }
        let mut covered = vec![false; n];
        for c in &cycles {
            prop_assert!(!c.nodes.is_empty());
            for (i, &from) in c.nodes.iter().enumerate() {
                let to = c.nodes[(i + 1) % c.nodes.len()];
                prop_assert!(deps.edges(from).contains(&to), "{} -> {} is not an edge", from, to);
                covered[from as usize] = true;
            }
        }
        for v in 0..n {
            prop_assert_eq!(covered[v], reach[v][v], "node {} on a cycle", v);
        }
        let mut plain = Vec::new();
        if topo_order(deps, &mut plain).is_ok() {
            prop_assert!(cycles.is_empty());
            prop_assert_eq!(&plain, &order);
        } else {
            prop_assert!(!cycles.is_empty());
        }
    }
}

#[test]
fn every_cycle_of_two_disjoint_loops_and_a_self_loop() {
    // 0 -> 1 -> 0, 2 -> 2, 3 -> 4 -> 5 -> 3, and 6 -> 0 (reads a cycle, is not on one).
    let offsets = [0u32, 1, 2, 3, 4, 5, 6, 7];
    let targets = [1u32, 0, 2, 4, 5, 3, 0];
    let deps = Deps {
        offsets: &offsets,
        targets: &targets,
    };
    let (mut order, mut cycles) = (vec![9], Vec::new());
    topo_order_all(deps, &mut order, &mut cycles);
    let nodes: Vec<Vec<u32>> = cycles.iter().map(|c| c.nodes.clone()).collect();
    assert_eq!(nodes, [vec![0, 1], vec![2], vec![3, 4, 5]]);
    assert_eq!(order, [1, 0, 2, 5, 4, 3, 6]);
    let names = ["a", "b", "c", "d", "e", "f", "g"];
    assert_eq!(
        cycles[2].error(|k| names[k as usize]).msg,
        "dependency cycle: d -> e -> f -> d"
    );
    // One component, two overlapping cycles (0 -> 1 -> 0 and 1 -> 2 -> 1): both are
    // reported, so 2 is named although the first cycle found does not pass through it.
    let offsets = [0u32, 1, 3, 4];
    let targets = [1u32, 0, 2, 1];
    let deps = Deps {
        offsets: &offsets,
        targets: &targets,
    };
    topo_order_all(deps, &mut order, &mut cycles);
    let nodes: Vec<Vec<u32>> = cycles.iter().map(|c| c.nodes.clone()).collect();
    assert_eq!(nodes, [vec![0, 1], vec![2, 1]]);
    let empty = Deps {
        offsets: &[0],
        targets: &[],
    };
    topo_order_all(empty, &mut order, &mut cycles);
    assert!(order.is_empty() && cycles.is_empty());
}
