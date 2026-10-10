//! Statement tables → one flat device/net graph with canonical signatures.
//!
//! Roots: the top level (when it has instances) and every subckt no line
//! instantiates (a CDL top cell). Calls to subckts defined in the text are
//! expanded; calls to anything else (PDK subckts) stay leaf devices.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::eval::{canon_number, eval, strip_quotes};
use crate::parse::{Body, Inst, Netlist};

/// Hierarchy depth beyond which expansion stops (recursive definitions).
const DEPTH_MAX: u32 = 64;
/// Flat device capacity; expansion stops (with a warning) past it.
const DEVICES_MAX: usize = 10_000_000;

/// The flat graph, SoA. Device `d`'s terminals are
/// `term_net[term_start[d]..term_start[d + 1]]`, in netlist order.
#[derive(Default)]
pub(crate) struct Flat {
    /// Hash of `dev_text`: the colour WL refinement starts from.
    pub dev_sig: Vec<u64>,
    pub term_start: Vec<u32>,
    pub term_net: Vec<u32>,
    /// Initial net colour: global and root-port nets are anchored by name,
    /// every other net starts anonymous.
    pub net_anchor: Vec<u64>,
    // Cold columns, for diff output only.
    pub dev_name: Vec<String>,
    pub dev_text: Vec<String>,
    pub net_name: Vec<String>,
    /// Top-level `.param` values, `name=canonical`.
    pub params: Vec<String>,
    /// `.model` cards, canonical text.
    pub models: Vec<String>,
    pub warnings: Vec<String>,
    net_index: HashMap<String, u32>,
}

#[derive(Clone)]
enum Val {
    Num(f64),
    Text(String),
}

type Scope = HashMap<String, Val>;

pub(crate) fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Flattens `nl`. Cost: O(flat devices × params); bounded by `DEVICES_MAX`.
pub(crate) fn flatten(nl: &Netlist) -> Flat {
    let mut flat = Flat {
        term_start: vec![0],
        ..Flat::default()
    };
    let mut subckts: HashMap<&str, usize> = HashMap::new();
    for (i, s) in nl.subckts.iter().enumerate() {
        if subckts.insert(&s.name, i).is_some() {
            flat.warnings.push(format!(
                "subckt {} defined twice; the last definition wins",
                s.name
            ));
        }
    }
    let globals: HashSet<&str> = nl.globals.iter().map(String::as_str).collect();
    let mut top = Scope::new();
    eval_into(&mut top, &nl.top.params, &Scope::new());
    let ctx = Ctx {
        nl,
        subckts: &subckts,
        globals: &globals,
        top: &top,
    };

    for (k, _) in &nl.top.params {
        flat.params.push(format!("{k}={}", show(&top[k])));
    }
    for m in &nl.models {
        let mut text = format!(
            "{}{} {} {}",
            m.scope,
            if m.scope.is_empty() { "" } else { ":" },
            m.name,
            m.kind
        );
        for v in &m.values {
            text.push(' ');
            text.push_str(&canon_value(v, &top));
        }
        let mut params: Vec<String> = m
            .params
            .iter()
            .map(|(k, v)| format!("{k}={}", canon_value(v, &top)))
            .collect();
        params.sort();
        for p in params {
            text.push(' ');
            text.push_str(&p);
        }
        flat.models.push(text);
    }

    if !nl.top.insts.is_empty() {
        expand(&ctx, &mut flat, &nl.top, "", &HashMap::new(), &top, 0);
    }
    let called: HashSet<&str> = std::iter::once(&nl.top)
        .chain(nl.subckts.iter().map(|s| &s.body))
        .flat_map(|b| b.insts.iter())
        .filter(|i| i.kind == b'x')
        .map(|i| i.model.as_str())
        .collect();
    for (i, s) in nl.subckts.iter().enumerate() {
        if called.contains(s.name.as_str()) || subckts[s.name.as_str()] != i {
            continue;
        }
        let mut ports = HashMap::new();
        for p in &s.ports {
            let id = flat.net(format!("{}:{p}", s.name), &format!("port:{}:{p}", s.name));
            ports.insert(p.clone(), id);
        }
        let mut scope = top.clone();
        eval_into(&mut scope, &s.defaults, &top);
        expand(
            &ctx,
            &mut flat,
            &s.body,
            &format!("{}/", s.name),
            &ports,
            &scope,
            1,
        );
    }

    debug_assert_eq!(flat.term_start.len(), flat.dev_sig.len() + 1);
    debug_assert_eq!(flat.net_anchor.len(), flat.net_name.len());
    flat
}

struct Ctx<'a> {
    nl: &'a Netlist,
    subckts: &'a HashMap<&'a str, usize>,
    globals: &'a HashSet<&'a str>,
    /// Evaluated top-level `.param`s: the base of every subckt scope.
    top: &'a Scope,
}

impl Flat {
    /// Net id for `name`, created with `anchor` on first use.
    fn net(&mut self, name: String, anchor: &str) -> u32 {
        if let Some(&id) = self.net_index.get(&name) {
            return id;
        }
        let id =
            u32::try_from(self.net_name.len()).expect("net count fits u32 (DEVICES_MAX bound)");
        self.net_anchor.push(hash_str(anchor));
        self.net_index.insert(name.clone(), id);
        self.net_name.push(name);
        id
    }

    fn resolve(
        &mut self,
        ctx: &Ctx,
        node: &str,
        prefix: &str,
        ports: &HashMap<String, u32>,
    ) -> u32 {
        // ngspice treats `gnd` as node 0; CDL treats `name!` as global.
        let node = if node == "gnd" { "0" } else { node };
        if node == "0" || node.ends_with('!') || ctx.globals.contains(node) {
            return self.net(node.to_string(), &format!("global:{node}"));
        }
        if let Some(&id) = ports.get(node) {
            return id;
        }
        self.net(format!("{prefix}{node}"), "net")
    }
}

fn expand(
    ctx: &Ctx,
    flat: &mut Flat,
    body: &Body,
    prefix: &str,
    ports: &HashMap<String, u32>,
    parent: &Scope,
    depth: u32,
) {
    debug_assert!(depth <= DEPTH_MAX, "callers stop recursing at DEPTH_MAX");
    debug_assert!(
        ports
            .values()
            .all(|&id| (id as usize) < flat.net_name.len())
    );
    let mut scope = parent.clone();
    eval_into(&mut scope, &body.params, parent);
    for inst in &body.insts {
        if flat.dev_sig.len() >= DEVICES_MAX {
            flat.warnings.push(format!(
                "flat device limit {DEVICES_MAX} reached; rest not expanded"
            ));
            return;
        }
        let nets: Vec<u32> = inst
            .nodes
            .iter()
            .map(|n| flat.resolve(ctx, n, prefix, ports))
            .collect();
        let sub = (inst.kind == b'x')
            .then(|| ctx.subckts.get(inst.model.as_str()))
            .flatten();
        if let Some(&si) = sub {
            let s = &ctx.nl.subckts[si];
            if s.ports.len() != nets.len() {
                flat.warnings.push(format!(
                    "{prefix}{}: {} nodes for subckt {} with {} ports; kept as a leaf",
                    inst.name,
                    nets.len(),
                    s.name,
                    s.ports.len()
                ));
            } else if depth >= DEPTH_MAX {
                flat.warnings.push(format!(
                    "{prefix}{}: hierarchy deeper than {DEPTH_MAX}; kept as a leaf",
                    inst.name
                ));
            } else {
                // Lexical scoping as in ngspice: the child sees top-level
                // `.param`s, its defaults, then the call's overrides (those
                // evaluated in the caller's scope).
                let mut child = ctx.top.clone();
                eval_into(&mut child, &s.defaults, ctx.top);
                let mut overrides = Scope::new();
                eval_into(&mut overrides, &inst.params, &scope);
                child.extend(overrides);
                let child_ports: HashMap<String, u32> = s.ports.iter().cloned().zip(nets).collect();
                expand(
                    ctx,
                    flat,
                    &s.body,
                    &format!("{prefix}{}.", inst.name),
                    &child_ports,
                    &child,
                    depth + 1,
                );
                continue;
            }
        }
        leaf(flat, inst, prefix, &nets, &scope);
    }
}

fn leaf(flat: &mut Flat, inst: &Inst, prefix: &str, nets: &[u32], scope: &Scope) {
    debug_assert_eq!(nets.len(), inst.nodes.len());
    debug_assert!(nets.iter().all(|&n| (n as usize) < flat.net_name.len()));
    let mut text = format!("{} {}", inst.kind as char, inst.model);
    if !inst.shape.is_empty() {
        text.push(' ');
        text.push_str(&inst.shape);
    }
    for v in &inst.values {
        text.push(' ');
        text.push_str(&canon_value(v, scope));
    }
    for s in &inst.spec {
        text.push(' ');
        text.push_str(&canon_value(s, scope));
    }
    let mut own = Scope::new();
    eval_into(&mut own, &inst.params, scope);
    let mut params: Vec<String> = inst
        .params
        .iter()
        .filter_map(|(k, _)| {
            let v = show(&own[k]);
            // `m=1` is every device's default multiplier: absent == present.
            (!(k == "m" && v == canon_number(1.0))).then(|| format!("{k}={v}"))
        })
        .collect();
    params.sort();
    params.dedup();
    for p in params {
        text.push(' ');
        text.push_str(&p);
    }
    flat.dev_sig.push(hash_str(&text));
    flat.dev_text.push(text);
    flat.dev_name.push(format!("{prefix}{}", inst.name));
    flat.term_net.extend_from_slice(nets);
    flat.term_start
        .push(u32::try_from(flat.term_net.len()).expect("terminal count fits u32"));
}

/// Evaluates `pairs` into `out`: names resolve in `base` first, then in the
/// same list (xschem/GF180 lines reference sibling params), in any order.
/// Cost: O(n²) in the list length for reversed dependency chains.
fn eval_into(out: &mut Scope, pairs: &[(String, String)], base: &Scope) {
    let mut got: Vec<Option<f64>> = vec![None; pairs.len()];
    for _pass in 0..pairs.len() {
        let mut progress = false;
        for (i, (_, raw)) in pairs.iter().enumerate() {
            if got[i].is_some() {
                continue;
            }
            let lookup = |n: &str| {
                num(base.get(n)).or_else(|| {
                    pairs
                        .iter()
                        .zip(&got)
                        .find(|((k, _), v)| k == n && v.is_some())
                        .and_then(|(_, v)| *v)
                })
            };
            got[i] = eval(raw, &lookup);
            progress |= got[i].is_some();
        }
        if !progress {
            break;
        }
    }
    for ((k, raw), v) in pairs.iter().zip(got) {
        let val = match v {
            Some(x) => Val::Num(x),
            None => Val::Text(norm_text(raw)),
        };
        out.insert(k.clone(), val);
    }
}

fn num(v: Option<&Val>) -> Option<f64> {
    match v {
        Some(Val::Num(x)) => Some(*x),
        _ => None,
    }
}

fn show(v: &Val) -> String {
    match v {
        Val::Num(x) => canon_number(*x),
        Val::Text(t) => t.clone(),
    }
}

/// Evaluated number text, or the normalized source text when not numeric.
fn canon_value(raw: &str, scope: &Scope) -> String {
    if raw.starts_with('"') {
        return raw.to_string();
    }
    if let Some((k, v)) = raw.split_once('=')
        && !k.is_empty()
        && !k.contains(['\'', '{', '('])
    {
        return format!("{k}={}", canon_value(v, scope));
    }
    match eval(raw, &|n: &str| num(scope.get(n))) {
        Some(x) => canon_number(x),
        None => norm_text(raw),
    }
}

fn norm_text(raw: &str) -> String {
    strip_quotes(raw.trim())
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}
