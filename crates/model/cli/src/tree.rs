//! `schemify tree` (docs/guide/cli.md § tree): elaborations → one [`Node`] row per instance
//! path of a `.shcomp` cell → the text tree or one JSON line, with `count_va`, the
//! Verilog-A nodes still selected (the burn-down count of system-spec-veriloga.md §2.5,
//! Analog-Skills' stage-1 reference, where `va-tree` printed it).
//!
//! Impl selection is the netlister's (netlist spec §2): a node's impl is the target of its
//! `elab::Insts` row, never chosen here. Elaboration leaves two places out, and both are
//! filled without re-implementing the rules:
//!
//! - below a node that does not elaborate to its schematic (a Verilog-A model, a SPICE
//!   body) nothing is elaborated, so its children come from an elaboration of the node's
//!   cell on its own, without a config (each child's own default), marked not elaborated;
//! - a cell's tree has the cell as its root, which no elaboration selects for (the top of
//!   an elaboration is always its schematic): it takes its `default` (rule 4), as an
//!   instance of the cell in a bench without config rules does.
//!
//! A node is an instance of a `.shcomp`; instances of `.shprim` cells (core, `tech:`, PDK
//! and project primitives) are devices, counted per node and never Verilog-A. Ignored
//! instances (`ignore`) are left out, as the netlist leaves them out. A node is Verilog-A
//! when its impl is `veriloga`, or `spice` with a body that loads an `.hdl` (the reference's
//! fallback form, §2.6). `va` counts the Verilog-A nodes of a subtree, elaborated or not,
//! so every node that has not retired counts, also below a behavioral parent.
//!
//! Data: rows in pre-order, at most [`NODES_MAX`]; a node's subtree is its rows up to
//! `end`. Lifetime: one command.
//!
//! Contents:
//! - `pub const NODES_MAX: usize`
//! - `pub struct Node { depth, path, cell, imp, lang, veriloga, fundamental, elaborated,
//!   devices, va, end }`
//! - `pub struct Walk`: `Walk::new`, `Walk::bench`, `Walk::cell`
//! - `pub fn request(Option<&str>, &[(&str, &str)]) -> Request`
//! - `pub fn count_va(&[Node]) -> u32`
//! - `pub fn text(&[Node], &mut String)`
//! - `pub fn json(Option<&str>, &[Node]) -> Json`

use std::collections::HashMap;
use std::fmt::Write as _;

use schemify_doc::FileKind;
use schemify_elab::{DEPTH_MAX, Dialect, Elab, Ignore, Request, Target, no_funcs};

use crate::rpc::Json;
use crate::spines::{Cells, Opened, elaborate_all};

/// Most rows one tree holds: it is flat (a row per instance path).
// ponytail: flat rows; count per definition (elab's memo) if a design needs more.
pub const NODES_MAX: usize = 1 << 16;

/// One node: an instance path of a `.shcomp` cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Depth below the roots, which are 0.
    pub depth: u16,
    /// `/`-joined local names from the top (`/XDUT/XS1`, what config `inst` rules name);
    /// empty for the root of a cell's tree.
    pub path: String,
    /// Cell display name (`lib:cell`; the stem in the project's own library).
    pub cell: String,
    /// The impl it elaborates to; empty when the cell has none to select.
    pub imp: String,
    /// That impl's lang.
    pub lang: String,
    /// Whether the impl is a Verilog-A model.
    pub veriloga: bool,
    /// Header `category fundamental`: a leaf by definition.
    pub fundamental: bool,
    /// Whether this view's netlist instantiates it: false below a node that does not
    /// elaborate to its schematic.
    pub elaborated: bool,
    /// Device instances in its schematic (elaborated or not).
    pub devices: u32,
    /// Verilog-A nodes of its subtree, itself included.
    pub va: u32,
    /// One past the last row of its subtree.
    pub end: u32,
}

/// Builds the rows of each view of one command from the view's elaboration, elaborating
/// a cell on its own wherever the view elaborates nothing below a node. Those
/// elaborations take no config, so they are kept across views.
pub struct Walk<'a> {
    /// The request of a cell elaborated on its own.
    alone: Request<'a>,
    /// Cell row → that cell elaborated on its own.
    elabs: HashMap<u32, Elab>,
    /// Cell rows on the path being walked: a cell below itself is shown, not expanded.
    stack: Vec<u32>,
    nodes: Vec<Node>,
}

/// No elab row or cell (`Elab::top` without a top definition).
const NONE: u32 = u32::MAX;

// `Node::end` and `Node::va` hold row counts.
const _: () = assert!(NODES_MAX <= u32::MAX as usize);

/// The request every tree elaboration uses: `check`'s dialect (ngspice), which has a
/// default template for every core lang, so no node drops out for want of a format.
#[must_use]
pub fn request<'a>(config: Option<&'a str>, env: &'a [(&'a str, &'a str)]) -> Request<'a> {
    Request {
        config,
        corner: None,
        overrides: &[],
        env,
        dialect: Dialect::Ngspice,
    }
}

impl<'a> Walk<'a> {
    /// Starts a walk whose own elaborations expand `${NAME}` from `env` (spec §7.1).
    #[must_use]
    pub fn new(env: &'a [(&'a str, &'a str)]) -> Walk<'a> {
        Walk {
            alone: request(None, env),
            elabs: HashMap::new(),
            stack: Vec::new(),
            nodes: Vec::new(),
        }
    }

    /// The rows of bench `o.top` as `e` elaborates it: one root per instance of a
    /// `.shcomp` in the bench (its DUTs); the bench's own devices belong to no node.
    ///
    /// # Errors
    ///
    /// A cell below a non-schematic node cannot be opened or elaborated, or the tree is
    /// larger than [`NODES_MAX`] or deeper than `DEPTH_MAX`.
    ///
    /// # Panics
    ///
    /// Panics if the last view's rows were not taken (a walk left mid-way by an error is
    /// not reused).
    pub fn bench(&mut self, o: &mut Opened, e: &Elab) -> Result<Vec<Node>, String> {
        assert!(self.nodes.is_empty(), "the last view's rows were taken");
        assert!(self.stack.is_empty());
        if e.top != NONE {
            self.rows(o, e, e.top, None, "", 0, true)?;
        }
        assert!(self.stack.is_empty());
        assert!(
            self.nodes
                .iter()
                .all(|n| n.end as usize <= self.nodes.len())
        );
        Ok(std::mem::take(&mut self.nodes))
    }

    /// The rows of cell `o.top`, `e` being its elaboration as the top: the cell is the root
    /// with its `default` impl (spec §2 rule 4), its schematic's instances below it, those
    /// elaborated only when the root's impl is its schematic.
    ///
    /// # Errors
    ///
    /// As [`Walk::bench`].
    ///
    /// # Panics
    ///
    /// Panics if `o.top` is not an open `.shcomp`, or as [`Walk::bench`].
    pub fn cell(&mut self, o: &mut Opened, e: &Elab) -> Result<Vec<Node>, String> {
        assert!(self.nodes.is_empty(), "the last view's rows were taken");
        assert!(self.stack.is_empty());
        let doc = o.project.doc(o.top).expect("the cell is open");
        assert_eq!(doc.kind(), FileKind::Shcomp);
        let f = &e.cells[0];
        // Rule 4 here, because elaboration selects per instance and its top is none.
        let k = match doc.header_value("default") {
            Some(d) => f.impl_name.iter().position(|n| e.names.get(*n) == d),
            None => (f.impl_name.len() == 1).then_some(0),
        };
        let (imp, lang) = k.map_or(("", ""), |k| {
            (e.names.get(f.impl_name[k]), e.names.get(f.impl_lang[k]))
        });
        let (s, bodies) = (doc.strings(), doc.bodies());
        let body = (0..bodies.kind.len()).find(|&r| s.get(bodies.fields[r][0]) == imp);
        let lines = body.map_or(&[][..], |r| &doc.pool().lines[bodies.verbatim[r].range()]);
        let root = Node {
            depth: 0,
            path: String::new(),
            cell: e.names.get(f.cell).to_owned(),
            imp: imp.to_owned(),
            lang: lang.to_owned(),
            veriloga: is_veriloga(lang, lines.iter().map(|l| s.get(*l))),
            fundamental: doc.header_value("category") == Some("fundamental"),
            elaborated: true,
            devices: 0,
            va: 0,
            end: 0,
        };
        let (row, schematic) = (f.row, lang == "schematic");
        self.push(root)?;
        self.stack.extend((row != NONE).then_some(row));
        let below = match e.top {
            NONE => 0,
            top => self.rows(o, e, top, Some(0), "", 1, schematic)?,
        };
        self.stack.clear();
        let n = self.nodes.len() as u32;
        let root = &mut self.nodes[0];
        root.va = u32::from(root.veriloga) + below;
        root.end = n;
        assert!(root.va <= n, "at most one Verilog-A count per row");
        Ok(std::mem::take(&mut self.nodes))
    }
}

/// The Verilog-A nodes of all of `nodes`' trees.
///
/// # Panics
///
/// Panics if the roots' `va` add up to more than `nodes` has rows (not [`Walk`]'s rows).
#[must_use]
pub fn count_va(nodes: &[Node]) -> u32 {
    let n: u32 = nodes.iter().filter(|n| n.depth == 0).map(|n| n.va).sum();
    assert!(n as usize <= nodes.len(), "at most one per row");
    n
}

/// Appends `nodes` as text, a line per node (tree lines, `<instance> <cell>`, `*` when
/// fundamental, the impl with its lang, `va=`, `devices=` when any, `-` when not
/// elaborated), then `count_va = N`. Columns are padded to the widest entry.
///
/// # Panics
///
/// Panics if `nodes` is not in pre-order (a node before its parent).
pub fn text(nodes: &[Node], out: &mut String) {
    let later = |i: usize| {
        let end = nodes[i].end as usize;
        end < nodes.len() && nodes[end].depth == nodes[i].depth
    };
    // Per depth on the current path: whether that ancestor has a sibling after it.
    let mut bars: Vec<bool> = Vec::new();
    let mut lines: Vec<(String, String)> = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        let d = usize::from(n.depth);
        assert!(bars.len() >= d, "pre-order: a node follows its parent");
        bars.truncate(d);
        let mut left: String = bars
            .iter()
            .skip(1)
            .map(|&b| if b { "│  " } else { "   " })
            .collect();
        if d > 0 {
            left.push_str(if later(i) { "├─ " } else { "└─ " });
        }
        bars.push(later(i));
        if let Some(inst) = n.path.rsplit('/').next().filter(|_| !n.path.is_empty()) {
            left.push_str(inst);
            left.push(' ');
        }
        left.push_str(&n.cell);
        if n.fundamental {
            left.push_str(" *");
        }
        let imp = match (n.imp.as_str(), n.lang.as_str()) {
            ("", _) => "-".to_owned(),
            (i, l) if i == l => i.to_owned(),
            (i, "spice") if n.veriloga => format!("{i} (spice .hdl)"),
            (i, l) => format!("{i} ({l})"),
        };
        lines.push((left, imp));
    }
    assert_eq!(lines.len(), nodes.len());
    let width = |f: fn(&(String, String)) -> &String| {
        lines
            .iter()
            .map(|l| f(l).chars().count())
            .max()
            .unwrap_or(0)
    };
    let (w0, w1) = (width(|l| &l.0), width(|l| &l.1));
    for (n, (left, imp)) in nodes.iter().zip(&lines) {
        write!(out, "{left:<w0$}  {imp:<w1$}  va={}", n.va).expect("writing to a String");
        if n.devices > 0 {
            write!(out, "  devices={}", n.devices).expect("writing to a String");
        }
        if !n.elaborated {
            out.push_str("  -");
        }
        out.push('\n');
    }
    writeln!(out, "count_va = {}", count_va(nodes)).expect("writing to a String");
}

/// `{"config", "roots", "count_va"}` for the view of `config` (`null`: none named), each
/// node `{"path", "cell", "impl", "lang", "veriloga", "fundamental", "elaborated", "va",
/// "devices", "children"}`.
///
/// # Panics
///
/// Panics if `nodes` is not the pre-order rows of [`Walk`] (a subtree's `end` out of
/// order).
#[must_use]
pub fn json(config: Option<&str>, nodes: &[Node]) -> Json {
    let mut roots = Vec::new();
    let mut i = 0;
    // Bounded by `nodes`: each root's `end` lies past it.
    while i < nodes.len() {
        assert_eq!(nodes[i].depth, 0);
        assert!(nodes[i].end as usize > i, "a subtree holds its node");
        roots.push(json_node(nodes, i));
        i = nodes[i].end as usize;
    }
    Json::obj(vec![
        (
            "config",
            config.map_or(Json::Null, |c| Json::Str(c.to_owned())),
        ),
        ("roots", Json::Arr(roots)),
        ("count_va", Json::Num(f64::from(count_va(nodes)))),
    ])
}

// ---- private ----

impl Walk<'_> {
    /// Appends the nodes among the instance rows of definition `def` of `e`, children of
    /// row `parent` (which gets their devices) at `path`; returns their Verilog-A count.
    #[allow(
        clippy::too_many_arguments,
        reason = "one recursion step: the walk's position, not a context worth a struct"
    )]
    fn rows(
        &mut self,
        o: &mut Opened,
        e: &Elab,
        def: u32,
        parent: Option<usize>,
        path: &str,
        depth: u16,
        elaborated: bool,
    ) -> Result<u32, String> {
        assert!((def as usize) < e.defs.cell.len());
        if let Some(p) = parent {
            assert!(p < self.nodes.len());
        }
        if u32::from(depth) >= DEPTH_MAX {
            return Err(format!("{path}: hierarchy deeper than {DEPTH_MAX}"));
        }
        let mut va = 0;
        for r in e.insts_of(def) {
            if e.insts.ignore[r] != Ignore::No {
                continue;
            }
            let (c, k, sub) = match e.insts.target[r] {
                Target::Def(d) => {
                    let c = e.defs.cell[d as usize];
                    let k = e.cells[c as usize].schematic_impl(&e.names);
                    (
                        c,
                        k.expect("a definition elaborates a schematic impl"),
                        Some(d),
                    )
                }
                Target::Leaf(id) => (
                    e.impls.cell[id as usize],
                    e.impls.index[id as usize] as usize,
                    None,
                ),
            };
            let f = &e.cells[c as usize];
            let kind = o.project.cells().kind.get(f.row as usize);
            if kind == Some(&FileKind::Shprim) {
                if let Some(p) = parent {
                    self.nodes[p].devices += 1;
                }
                continue;
            }
            let lang = e.names.get(f.impl_lang[k]);
            let body = match e.insts.target[r] {
                Target::Leaf(id) => {
                    let id = id as usize;
                    &e.impls.body
                        [e.impls.body_start[id] as usize..e.impls.body_start[id + 1] as usize]
                }
                Target::Def(_) => &[][..],
            };
            let child = format!("{path}/{}", e.names.get(e.insts.name[r]));
            let header = |key| o.project.interface(f.row as usize)?.header_value(key);
            let node = Node {
                depth,
                path: child,
                cell: e.names.get(f.cell).to_owned(),
                imp: e.names.get(f.impl_name[k]).to_owned(),
                lang: lang.to_owned(),
                veriloga: is_veriloga(lang, body.iter().map(|l| e.names.get(*l))),
                fundamental: f.row != NONE && header("category") == Some("fundamental"),
                elaborated,
                devices: 0,
                va: 0,
                end: 0,
            };
            let row = f.row;
            let at = self.push(node)?;
            let mut sub_va = u32::from(self.nodes[at].veriloga);
            // A cell below itself (only possible through an unelaborated part, elab
            // refuses the rest: E014) is shown once, not expanded again.
            if row != NONE && !self.stack.contains(&row) {
                self.stack.push(row);
                let path = self.nodes[at].path.clone();
                sub_va += match sub {
                    Some(d) => self.rows(o, e, d, Some(at), &path, depth + 1, elaborated)?,
                    None => self.alone(o, row, at, &path, depth + 1)?,
                };
                assert_eq!(self.stack.pop(), Some(row));
            }
            let end = self.nodes.len() as u32;
            let node = &mut self.nodes[at];
            node.va = sub_va;
            node.end = end;
            va += sub_va;
        }
        assert!(va as usize <= self.nodes.len(), "at most one per row");
        Ok(va)
    }

    /// Appends the children of a node of cell `row` that does not elaborate to its
    /// schematic: the rows of the cell elaborated on its own, none of them elaborated in
    /// this view. Returns their Verilog-A count.
    fn alone(
        &mut self,
        o: &mut Opened,
        row: u32,
        parent: usize,
        path: &str,
        depth: u16,
    ) -> Result<u32, String> {
        assert!(parent < self.nodes.len());
        assert_ne!(row, NONE);
        let e = if let Some(e) = self.elabs.remove(&row) {
            e
        } else {
            if o.project.open_handle(row as usize).is_none() {
                o.open_row(row)?;
            }
            let top = o.project.open_handle(row as usize).expect("opened above");
            elaborate_all(o, top, &self.alone, &mut no_funcs)?
        };
        let va = match e.top {
            NONE => Ok(0),
            top => self.rows(o, &e, top, Some(parent), path, depth, false),
        };
        // Taken out while walked (the walk opens cells); a cell below itself is never
        // walked again inside its own walk (`stack`), so nothing looks for it meanwhile.
        assert!(self.elabs.insert(row, e).is_none());
        va
    }

    /// Appends `n`, its `end` and `va` still unset; returns its row.
    fn push(&mut self, n: Node) -> Result<usize, String> {
        if self.nodes.len() >= NODES_MAX {
            return Err(format!("the hierarchy has more than {NODES_MAX} nodes"));
        }
        assert_eq!(n.end, 0, "filled after the subtree");
        assert_eq!(n.va, 0, "filled after the subtree");
        self.nodes.push(n);
        Ok(self.nodes.len() - 1)
    }
}

/// Whether an impl of `lang` whose body is `body` is a Verilog-A model: `veriloga`, or
/// `spice` loading an `.hdl` source (ESPice's form, netlist spec §7.2a).
fn is_veriloga<'s>(lang: &str, mut body: impl Iterator<Item = &'s str>) -> bool {
    let hdl = |l: &str| {
        let w = l.trim_start().get(..4);
        w.is_some_and(|w| w.eq_ignore_ascii_case(".hdl"))
    };
    lang == "veriloga" || (lang == "spice" && body.any(hdl))
}

/// Node `i` of `nodes` as JSON, its children nested.
fn json_node(nodes: &[Node], i: usize) -> Json {
    let n = &nodes[i];
    assert!(n.end as usize <= nodes.len());
    let mut children = Vec::new();
    let mut j = i + 1;
    // Bounded by the subtree: each child's `end` lies past it.
    while j < n.end as usize {
        assert!(nodes[j].end as usize > j, "a subtree holds its node");
        assert_eq!(nodes[j].depth, n.depth + 1);
        children.push(json_node(nodes, j));
        j = nodes[j].end as usize;
    }
    let s = |t: &str| Json::Str(t.to_owned());
    Json::obj(vec![
        ("path", s(&n.path)),
        ("cell", s(&n.cell)),
        ("impl", s(&n.imp)),
        ("lang", s(&n.lang)),
        ("veriloga", Json::Bool(n.veriloga)),
        ("fundamental", Json::Bool(n.fundamental)),
        ("elaborated", Json::Bool(n.elaborated)),
        ("va", Json::Num(f64::from(n.va))),
        ("devices", Json::Num(f64::from(n.devices))),
        ("children", Json::Arr(children)),
    ])
}

#[cfg(test)]
mod tests {
    use super::{Node, is_veriloga, text};

    fn node(depth: u16, path: &str, cell: &str, imp: &str, lang: &str, va: u32, end: u32) -> Node {
        Node {
            depth,
            path: path.to_owned(),
            cell: cell.to_owned(),
            imp: imp.to_owned(),
            lang: lang.to_owned(),
            veriloga: lang == "veriloga",
            fundamental: false,
            elaborated: true,
            devices: 0,
            va,
            end,
        }
    }

    /// The `.hdl` rule (issue #64): a `spice` body counts only when a line loads an `.hdl`
    /// (any case, indented or not); other langs never by their body.
    #[test]
    fn veriloga_is_the_lang_or_a_spice_body_loading_an_hdl() {
        let none: [&str; 0] = [];
        assert!(is_veriloga("veriloga", none.into_iter()));
        assert!(is_veriloga(
            "spice",
            [".model m x", "  .HDL \"a.va\""].into_iter()
        ));
        assert!(!is_veriloga(
            "spice",
            [".include \"a.hdl\"", "* .hdl"].into_iter()
        ));
        assert!(!is_veriloga("spice", none.into_iter()));
        assert!(!is_veriloga("schematic", [".hdl \"a.va\""].into_iter()));
        assert!(!is_veriloga("verilog", [".hdl \"a.v\""].into_iter()));
    }

    /// Tree lines: a node's bar runs down while it has a later sibling, roots have none.
    #[test]
    fn text_draws_siblings_and_pads_columns() {
        let nodes = [
            node(0, "", "top", "schematic", "schematic", 1, 4),
            node(1, "/A", "a", "schematic", "schematic", 1, 3),
            node(2, "/A/B", "b", "beh", "veriloga", 1, 3),
            node(1, "/C", "c", "schematic", "schematic", 0, 4),
        ];
        let mut out = String::new();
        text(&nodes, &mut out);
        let want = "\
top        schematic       va=1
├─ A a     schematic       va=1
│  └─ B b  beh (veriloga)  va=1
└─ C c     schematic       va=0
count_va = 1
";
        assert_eq!(out, want);
    }
}
