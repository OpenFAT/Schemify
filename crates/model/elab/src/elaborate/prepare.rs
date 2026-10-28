//! The request and the reached cells in, each cell's resolution, connectivity, graph,
//! evaluated values and context-free findings out (netlist spec §1, §3, §8): config rules,
//! instance resolution (`tech:` classes per `flavor`, pdk spec §5), the global label set,
//! and per-cell preparation, parallel for a large cell (D37: identical to serial).
//!
//! Contents:
//! - `Elaborator::{request_finding, read_config, unmatched_inst_rules, push_cell, discover,
//!   prepare_eval, defaults}` (`pub(super)`)

use super::{CellData, Elaborator, record, tuple};
use crate::cells::{display, readable};
use crate::check::{CellCtx, check_overrides, check_values, waivers};
use crate::connect::connect;
use crate::graph::Graph;
use crate::iface::Iface;
use crate::names::{Name, Names};
use crate::values::{Funcs, NONE, Val};
use crate::violations::{Code, Violation};
use schemify_doc::{Document, ObjId, Sym};
use std::collections::HashMap;

impl<'p, F: Funcs> Elaborator<'p, '_, F> {
    pub(super) fn request_finding(&mut self, cell: Name, detail: &str) {
        self.viol.push(Violation::new(
            Code::E035,
            cell,
            ObjId::NONE,
            Name::NONE,
            detail.to_owned(),
        ));
    }

    pub(super) fn read_config(&mut self, doc: &Document, name: &str, top_cell: Name) {
        let Some(r) = record(doc.configs(), doc, name) else {
            self.request_finding(top_cell, &format!("config `{name}` does not exist"));
            return;
        };
        let s = doc.strings();
        for (k, vals) in doc.pool().attrs(doc.configs().attrs[r]) {
            let v: Vec<&str> = vals.iter().map(|v| s.get(*v)).collect();
            match (s.get(k), v.as_slice()) {
                ("order", names) => self
                    .config
                    .order
                    .extend(names.iter().map(|n| (*n).to_owned())),
                ("inst", [path, imp]) => {
                    self.config
                        .inst
                        .push(((*path).to_owned(), (*imp).to_owned()));
                    self.config.inst_hit.push(false);
                }
                ("cell", [cellref, imp]) => match self.project.resolve(self.top, cellref) {
                    Ok(row) => self.config.cell.push((row, (*imp).to_owned())),
                    Err(_) => self.viol.push(Violation::new(
                        Code::E001,
                        top_cell,
                        ObjId::NONE,
                        self.names.intern(cellref),
                        format!("config `{name}` names unknown cell `{cellref}`"),
                    )),
                },
                _ => {}
            }
        }
    }

    /// `W037` per config `inst` rule whose path no elaborated instance has (a typo, or a
    /// path under an impl this config does not select), on the top cell with the rule's
    /// path. Skipped while cells still need opening: their children were not visited.
    pub(super) fn unmatched_inst_rules(&mut self, top_cell: Name) {
        assert_eq!(self.config.inst.len(), self.config.inst_hit.len());
        if !self.needs_open.is_empty() {
            return;
        }
        for r in (0..self.config.inst.len()).filter(|&r| !self.config.inst_hit[r]) {
            let (path, imp) = &self.config.inst[r];
            let detail = format!(
                "config `inst` rule `{path} {imp}` names no instance (paths are `/`-joined \
                 instance names from the bench, e.g. `/X1/X3`)"
            );
            let (path, param) = (self.names.intern(path), self.names.intern(imp));
            self.viol.push(Violation {
                path,
                ..Violation::new(Code::W037, top_cell, ObjId::NONE, param, detail)
            });
        }
    }

    pub(super) fn push_cell(&mut self, doc: &'p Document, iface: Iface) -> u32 {
        let ci = self.ifaces.len() as u32;
        assert!(ci < NONE - 1, "cell capacity");
        self.ifaces.push(iface);
        self.nets.push(None);
        self.cells.push(CellData {
            doc,
            resolved: false,
            unresolved: Vec::new(),
            prepared: false,
            inst_iface: Vec::new(),
            graph: Graph::default(),
            waivers: Vec::new(),
            defaults: None,
            default_key: None,
        });
        assert_eq!(self.cells.len(), self.ifaces.len());
        assert_eq!(self.nets.len(), self.ifaces.len());
        ci
    }

    /// Resolves everything reachable from the top and collects the names of `global
    /// true` labels (spec §3: global anywhere in the elaborated hierarchy). Over every
    /// reachable schematic, selected or not: a superset, but path-independent.
    pub(super) fn discover(&mut self) {
        let mut globals = Vec::new();
        // Bounded: `resolve` adds each reached cell row once (`row_cell`).
        let mut ci = 0;
        while ci < self.cells.len() {
            assert!(ci < NONE as usize - 1, "cell capacity");
            if self.ifaces[ci].open {
                self.resolve(ci as u32);
                crate::nets::global_labels(self.cells[ci].doc, &mut self.names, &mut globals);
            }
            ci += 1;
        }
        globals.sort_unstable();
        globals.dedup();
        self.globals = globals;
        assert!(
            self.cells
                .iter()
                .zip(&self.ifaces)
                .all(|(c, f)| c.resolved || !f.open)
        );
    }

    /// Evaluates cell `ci`'s graph for own-param values `own_vals` and gates the values
    /// (`E008`, D13), building the cell's connectivity and graph first, once per cell, with
    /// its context-free findings. Returns the values and their unverified flags.
    ///
    /// A large cell (`crate::PARALLEL_MIN` instances, native targets) builds its connectivity on a
    /// second thread while this one builds, evaluates and gates the graph (`funcs` stays
    /// here). The two share only the name arena: the graph side interns nothing and reads
    /// a snapshot, and every interning call happens in the serial order, so names, values
    /// and findings are the serial ones (D37: jobs = 1 vs N byte-identical).
    pub(super) fn prepare_eval(&mut self, ci: u32, own_vals: &[Val]) -> (Vec<Val>, Vec<bool>) {
        let (mut vals, mut unv) = (Vec::new(), Vec::new());
        let mut gated = Vec::new();
        if self.cells[ci as usize].prepared {
            let cell = &self.cells[ci as usize];
            let ifaces = &self.ifaces;
            cell.graph.eval_all(
                Some(own_vals),
                &mut vals,
                &mut unv,
                ifaces,
                self.funcs,
                &mut self.ws,
            );
            let cx = CellCtx {
                doc: cell.doc,
                ifaces,
                own: ci,
                inst_iface: &cell.inst_iface,
                graph: &cell.graph,
                waivers: &cell.waivers,
            };
            check_values(
                &cx,
                &vals,
                &self.names,
                self.funcs,
                &mut self.ws,
                &mut gated,
            );
            self.viol.append(&mut gated);
            return (vals, unv);
        }
        self.resolve(ci);
        let doc = self.cells[ci as usize].doc;
        let inst_iface = std::mem::take(&mut self.cells[ci as usize].inst_iface);
        let unresolved = std::mem::take(&mut self.cells[ci as usize].unresolved);
        let subst = self.subst_of(ci);
        let subst: Vec<(Name, &str)> = subst.iter().map(|(n, t)| (*n, t.as_str())).collect();
        let waiv = waivers(doc, &self.names);
        let mut found = Vec::new();
        let parallel = crate::parallel_ok(doc.instances().id.len());
        let Elaborator {
            names,
            ifaces,
            globals,
            funcs,
            ws,
            ..
        } = self;
        let ifaces: &[Iface] = ifaces;
        // The graph side reads names only (a snapshot beside a parallel `connect`).
        let mut graph_side = |names: &Names| {
            let graph = Graph::build(doc, ci, ifaces, &inst_iface, &subst, names);
            graph.eval_all(Some(own_vals), &mut vals, &mut unv, ifaces, *funcs, ws);
            let cx = CellCtx {
                doc,
                ifaces,
                own: ci,
                inst_iface: &inst_iface,
                graph: &graph,
                waivers: &waiv,
            };
            check_values(&cx, &vals, names, *funcs, ws, &mut gated);
            graph
        };
        let own = &ifaces[ci as usize];
        let (nets, graph) = if parallel {
            let snapshot = names.clone();
            std::thread::scope(|sc| {
                let worker = sc.spawn(|| {
                    let mut found = Vec::new();
                    let nets = connect(doc, own, ifaces, &inst_iface, globals, names, &mut found);
                    (nets, found)
                });
                let graph = graph_side(&snapshot);
                let (nets, f) = worker.join().expect("connect does not panic");
                found = f;
                (nets, graph)
            })
        } else {
            let nets = connect(doc, own, ifaces, &inst_iface, globals, names, &mut found);
            (nets, graph_side(names))
        };
        {
            // Override keys are interned here, after connect, in both orders.
            let cx = CellCtx {
                doc,
                ifaces: &self.ifaces,
                own: ci,
                inst_iface: &inst_iface,
                graph: &graph,
                waivers: &waiv,
            };
            check_overrides(&cx, &unresolved, &mut self.names, &mut found);
        }
        for p in &self.ifaces[ci as usize].too_wide {
            found.push(Violation::new(
                Code::E011,
                self.ifaces[ci as usize].cell,
                ObjId::NONE,
                *p,
                "pin name wider than 65535 bits".to_owned(),
            ));
        }
        // The serial order of findings: connectivity, overrides, wide pins, then gating.
        self.viol.append(&mut found);
        self.viol.append(&mut gated);
        self.nets[ci as usize] = Some(nets);
        let cell = &mut self.cells[ci as usize];
        cell.inst_iface = inst_iface;
        cell.graph = graph;
        cell.waivers = waiv;
        cell.prepared = true;
        assert_eq!(vals.len(), cell.graph.len());
        (vals, unv)
    }

    /// Default values of cell `ci`'s own params (its defaults evaluated in its own
    /// scope) and their tuple text. Evaluated on a params-and-vars-only graph.
    pub(super) fn defaults(&mut self, ci: u32) -> Vec<Val> {
        if let Some(d) = &self.cells[ci as usize].defaults {
            return d.clone();
        }
        let doc = self.cells[ci as usize].doc;
        let none = vec![NONE; doc.instances().id.len()];
        let subst = self.subst_of(ci);
        let subst: Vec<(Name, &str)> = subst.iter().map(|(n, t)| (*n, t.as_str())).collect();
        // Own params and vars only: instance blocks are left out (`none`).
        let g = Graph::build(doc, ci, &self.ifaces, &none, &subst, &self.names);
        let (mut vals, mut unv) = (Vec::new(), Vec::new());
        g.eval_own(&mut vals, &mut unv, &self.ifaces, self.funcs, &mut self.ws);
        vals.truncate(g.params as usize);
        let (key, _) = tuple(&self.ifaces[ci as usize], &vals, &mut self.names);
        assert_eq!(vals.len(), self.ifaces[ci as usize].param_name.len());
        let cell = &mut self.cells[ci as usize];
        cell.defaults = Some(vals.clone());
        cell.default_key = Some(key);
        vals
    }
}

// ---- private ----

impl<'p, F: Funcs> Elaborator<'p, '_, F> {
    fn cell_of_row(&mut self, row: usize) -> u32 {
        if let Some(&ci) = self.row_cell.get(&(row as u32)) {
            return ci;
        }
        let doc = self
            .project
            .interface(row)
            .expect("resolution checked the interface");
        let (cell, stem) = display(self.project, row);
        // A derived `tech` cell has no file: its interface is the whole cell (pdk spec §5).
        let derived = self.project.libs().name[self.project.cells().lib[row] as usize]
            == schemify_doc::TECH_LIB;
        let open = derived || self.project.open_handle(row).is_some();
        let iface = Iface::from_doc(doc, &cell, &stem, row as u32, open, &mut self.names);
        let ci = self.push_cell(doc, iface);
        assert!(
            self.row_cell.insert(row as u32, ci).is_none(),
            "one cell per row"
        );
        ci
    }

    /// Resolves cell `ci`'s instances to cells (creating their interfaces). Once per cell.
    fn resolve(&mut self, ci: u32) {
        if self.cells[ci as usize].resolved {
            return;
        }
        let doc = self.cells[ci as usize].doc;
        let s = doc.strings();
        let ni = doc.instances().id.len();
        let mut inst_iface = vec![NONE; ni];
        let mut unresolved: Vec<Option<String>> = vec![None; ni];
        let mut seen: HashMap<Sym, Result<u32, String>> = HashMap::new();
        for i in 0..ni {
            let sym = doc.instances().cell[i];
            // A technology class instance resolves per its `flavor` override (pdk spec §5).
            let flavor = crate::graph::override_of(doc, i, "flavor")
                .map(|f| s.get(f))
                .filter(|f| *f != "core");
            if let (Some(class), Some(flavor)) = (s.get(sym).strip_prefix("tech:"), flavor) {
                let found = self.project.resolve_tech(class, flavor).ok();
                let text = format!("tech:{class} ({flavor})");
                match crate::cells::readable(self.project, found, &text) {
                    Ok(row) => inst_iface[i] = self.cell_of_row(row),
                    Err(why) => unresolved[i] = Some(why),
                }
                continue;
            }
            let r = match seen.get(&sym) {
                Some(r) => r.clone(),
                None => {
                    let text = s.get(sym);
                    let found = if ci == 0 {
                        self.project.resolve(self.top, text)
                    } else {
                        let row = self.ifaces[ci as usize].row as usize;
                        self.project.resolve_in(row, text)
                    };
                    let row = readable(self.project, found.ok(), text);
                    let r = row.map(|row| self.cell_of_row(row));
                    seen.insert(sym, r.clone());
                    r
                }
            };
            match r {
                Ok(c) => inst_iface[i] = c,
                Err(why) => unresolved[i] = Some(why),
            }
        }
        let cell = &mut self.cells[ci as usize];
        cell.inst_iface = inst_iface;
        cell.unresolved = unresolved;
        cell.resolved = true;
        assert_eq!(cell.inst_iface.len(), cell.unresolved.len());
    }

    /// The request's var substitutions for cell `ci` (the top only), names interned.
    fn subst_of(&mut self, ci: u32) -> Vec<(Name, String)> {
        if ci != 0 {
            return Vec::new();
        }
        let subst = self.subst.clone();
        subst
            .into_iter()
            .map(|(a, b)| (self.names.intern(&a), b))
            .collect()
    }
}
