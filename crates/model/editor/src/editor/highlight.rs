//! Net highlight: hierarchy-wide net keys, highlight commands, mismatches, plotted nets.
//!
//! Contents:
//! - `pub(super) const PIN_PX`
//! - `pub(super) fn env_refs`
//! - `Editor::level_prefix`
//! - `Editor::hier_map`
//! - `Editor::net_key`
//! - `Editor::highlight_cmd`
//! - `Editor::highlighted_local_names`
//! - `Editor::pin_mismatch`
//! - `Editor::highlight_plotted`
//! - `Editor::auto_highlight`
//! - `Editor::colored_highlight`
//! - `pub(super) fn duplicate_names`
//! - `pub(super) fn overlapped`

use schemify_canvas::CanvasPrims;
use schemify_doc::{DocHandle, Document, Op, TableId, Target as DocTarget};
use schemify_elab::{Dialect, Request};
use schemify_expr::Value;
use schemify_format::RecordKind;
use schemify_netlist::NameMap;

use crate::commands::Cmd;
use crate::scene::{ObjKey, UNIT};
use crate::{checks, edit, spines};

use super::Editor;
use super::compare::HILITE_LAYERS;

/// Hit tolerance for a pin under the pointer, in screen pixels.
pub(super) const PIN_PX: f64 = 8.0;

/// Borrows the session's `${NAME}` environment pairs in the shape elab and run take.
pub(super) fn env_refs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

impl Editor {
    /// The hierarchy prefix of the active level: `/x1/x2` (empty at the top), the instance
    /// path elab and the name map use.
    pub(super) fn level_prefix(&self) -> String {
        let Some(tab) = self.session.tabs().get(self.session.active()) else {
            return String::new();
        };
        let mut p = String::new();
        for l in &tab.stack[1..] {
            p.push('/');
            p.push_str(&l.inst);
        }
        p
    }
    /// The name map of the active tab's top cell (ngspice, default config and corner),
    /// elaborated again only after an edit (`generation`). `None` when elaboration fails.
    pub(super) fn hier_map(&mut self) -> Option<&NameMap> {
        let tab = self.session.tabs().get(self.session.active())?;
        let top = tab.stack.first()?.doc;
        let fresh = self
            .hier
            .as_ref()
            .is_some_and(|(d, g, _)| *d == top && *g == self.generation);
        if !fresh {
            let env = env_refs(&self.env);
            let req = Request {
                config: None,
                corner: None,
                overrides: &[],
                env: &env,
                dialect: Dialect::Ngspice,
            };
            let host = &mut self.host;
            let mut funcs = |name: &str, args: &[Value<'_>]| host.call_fn(name, args);
            let map = spines::elaborate_all(&mut self.session, top, &req, &mut funcs)
                .map(|e| schemify_netlist::emit(&e, Dialect::Ngspice).name_map)
                .ok()?;
            self.hier = Some((top, self.generation, map));
        }
        self.hier.as_ref().map(|h| &h.2)
    }
    /// The hierarchy-wide key of the net named `name` at the active level: its emitted
    /// hierarchical name (`x1.n1`, a port resolves to the parent's net), lowercase; the level
    /// path when the name map does not know it.
    pub(super) fn net_key(&mut self, name: &str) -> String {
        let path = format!("{}/{name}", self.level_prefix());
        match self.hier_map().and_then(|m| m.net_name(&path)) {
            Some(e) => spice_case(&e),
            None => path,
        }
    }
    /// `k` / `Ctrl+K` / `Shift+K`: highlight (or unhighlight) the nets of the selected wires,
    /// labels and ports and of the instance pin under the pointer — the object under the
    /// pointer when nothing is selected — or clear every highlight.
    pub(super) fn highlight_cmd(&mut self, h: DocHandle, cmd: Cmd) {
        if cmd == Cmd::Unhighlight {
            self.highlight.clear();
            self.session.say("highlights cleared");
            return;
        }
        let near = self.pointer_world_snapped();
        let tol = (PIN_PX / self.camera().scale) as i64;
        let mut keys: Vec<ObjKey> = self.selection().to_vec();
        if keys.is_empty()
            && let Some(o) = self.hit_at(self.pointer)
        {
            keys.push(self.scene.objs.key[o as usize]);
        }
        let comps: Vec<u32> = {
            let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
                self.session.say("highlight: the cell is not elaborated");
                return;
            };
            let mut c: Vec<u32> = keys
                .iter()
                .filter_map(|&k| checks::component_of(doc, cell, k, near, tol).map(|x| x.0))
                .collect();
            c.sort_unstable();
            c.dedup();
            c
        };
        if comps.is_empty() {
            self.session
                .say("highlight: no net under the pointer or in the selection");
            return;
        }
        // Every bit of each component: a bus highlights all its bits, so a single-bit net
        // carrying one of them lights up too.
        let names: Vec<String> = {
            let cell = self.cell.as_ref().expect("checked above");
            let mut n: Vec<String> = comps
                .iter()
                .flat_map(|&c| checks::bit_names(cell, c).map(str::to_owned))
                .collect();
            n.dedup();
            n
        };
        let mut shown = Vec::with_capacity(names.len());
        for n in &names {
            let key = self.net_key(n);
            match cmd {
                Cmd::Highlight => {
                    if let Err(i) = self.highlight.binary_search(&key)
                        && self.highlight.len() < checks::HIGHLIGHT_MAX
                    {
                        self.highlight.insert(i, key.clone());
                    }
                }
                _ => {
                    if let Ok(i) = self.highlight.binary_search(&key) {
                        self.highlight.remove(i);
                    }
                }
            }
            shown.push(key);
        }
        let verb = if cmd == Cmd::Highlight {
            "highlighted"
        } else {
            "unhighlighted"
        };
        self.session.say(format!("{verb}: {}", shown.join(" ")));
        debug_assert!(self.highlight.windows(2).all(|w| w[0] < w[1]));
    }
    /// Local names of the highlighted nets at the active level.
    pub(super) fn highlighted_local_names(&mut self) -> Vec<String> {
        let names: Vec<String> = match self.cell.as_ref() {
            Some(cell) => (0..cell.nets.net_name.len() as u32)
                .map(|c| checks::net_name(cell, c).to_owned())
                .collect(),
            None => return Vec::new(),
        };
        let mut out: Vec<String> = names
            .into_iter()
            .filter(|n| {
                let key = self.net_key(n);
                self.highlight.binary_search(&key).is_ok()
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
    /// Highlight → net/pin mismatches: highlights the nets of the selected instances' pins
    /// whose net is named differently from the pin.
    pub(super) fn pin_mismatch(&mut self, h: DocHandle) {
        let mut nets: Vec<String> = Vec::new();
        {
            let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
                self.session.say("the cell is not elaborated");
                return;
            };
            let p = &self.session.project;
            let instances = self
                .selection()
                .iter()
                .filter(|k| k.table == TableId::Instances);
            for k in instances {
                let Some(loc) = edit::loc_of(doc, *k) else {
                    continue;
                };
                let row = loc.row as usize;
                if row >= cell.inst_iface.len() {
                    continue;
                }
                let cellref = doc.strings().get(doc.instances().cell[row]);
                let Some(iface) = p.resolve(h, cellref).ok().and_then(|r| p.interface(r)) else {
                    continue;
                };
                let ordinary: Vec<String> = (0..iface.pins().name.len())
                    .filter(|&i| {
                        iface
                            .pool()
                            .find_attr(iface.strings(), iface.pins().attrs[i], "inherit")
                            .is_none()
                    })
                    .map(|i| iface.strings().get(iface.pins().name[i]).to_owned())
                    .collect();
                for (pin, (c, _)) in ordinary.iter().zip(checks::pins_of(doc, cell, row)) {
                    let net = checks::net_name(cell, c);
                    if net != pin {
                        nets.push(net.to_owned());
                    }
                }
            }
        }
        nets.sort_unstable();
        nets.dedup();
        for n in &nets {
            let key = self.net_key(n);
            if let Err(i) = self.highlight.binary_search(&key)
                && self.highlight.len() < checks::HIGHLIGHT_MAX
            {
                self.highlight.insert(i, key);
            }
        }
        self.session
            .say(format!("{} net/pin mismatch(es)", nets.len()));
    }
    /// Adds the nets the waves dock plots (`v(<net>)`) to the highlight.
    pub(super) fn highlight_plotted(&mut self) {
        let traces = self.waves_traces();
        for t in traces {
            let Some(net) = t.strip_prefix("v(").and_then(|r| r.strip_suffix(')')) else {
                continue;
            };
            let key = net.to_ascii_lowercase();
            if let Err(i) = self.highlight.binary_search(&key)
                && self.highlight.len() < checks::HIGHLIGHT_MAX
            {
                self.highlight.insert(i, key);
            }
        }
    }
    /// Highlight → auto-highlight: the selection's nets replace the highlight on every
    /// selection change; an empty selection clears it (xschem `auto_hilight`).
    pub(super) fn auto_highlight(&mut self) {
        let Some((h, _)) = self.active() else { return };
        self.highlight.clear();
        if !self.selection().is_empty() {
            self.highlight_cmd(h, Cmd::Highlight);
        }
    }
    /// One color per highlighted net (Highlight → increment highlight color): each key gets a
    /// palette layer from `HILITE_LAYERS` by its position in the sorted highlight list.
    pub(super) fn colored_highlight(&mut self, out: &mut CanvasPrims) {
        let Some((h, _)) = self.active() else { return };
        let names: Vec<String> = match self.cell.as_ref() {
            Some(cell) => (0..cell.nets.net_name.len() as u32)
                .map(|c| checks::net_name(cell, c).to_owned())
                .collect(),
            None => return,
        };
        let keyed: Vec<(u32, usize)> = names
            .iter()
            .enumerate()
            .filter_map(|(c, n)| {
                let key = self.net_key(n);
                self.highlight
                    .binary_search(&key)
                    .ok()
                    .map(|i| (c as u32, i))
            })
            .collect();
        let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
            return;
        };
        if cell.inst_iface.len() != doc.instances().id.len() {
            return;
        }
        let r = 3 * UNIT;
        for (c, i) in keyed {
            let layer = HILITE_LAYERS[i % HILITE_LAYERS.len()];
            let m = checks::members(doc, cell, &[c]);
            for k in &m.keys {
                let Some(o) = self.scene.find(*k) else {
                    continue;
                };
                let g = self.scene.objs.geom[o as usize];
                if self.scene.objs.seg[o as usize] {
                    out.line(g[0], g[1], g[2], g[3], layer);
                } else {
                    out.rect(g[0] - r, g[1] - r, g[2] + r, g[3] + r, layer);
                }
            }
            for &(_, _, p) in &m.pins {
                out.rect(p.0 - r, p.1 - r, p.0 + r, p.1 + r, layer);
            }
        }
    }
}

/// Instances whose name is used more than once (every holder), and the ops renaming all
/// but the first holder of each name to the next free `<prefix><n>`.
pub(super) fn duplicate_names(doc: &Document) -> (Vec<ObjKey>, Vec<Op>) {
    let s = doc.strings();
    let inst = doc.instances();
    let mut by_name: std::collections::BTreeMap<&str, Vec<usize>> =
        std::collections::BTreeMap::new();
    for i in 0..inst.id.len() {
        by_name.entry(s.get(inst.name[i])).or_default().push(i);
    }
    let mut keys = Vec::new();
    let mut ops = Vec::new();
    let mut taken = std::collections::HashSet::new();
    for (name, rows) in by_name.iter().filter(|e| e.1.len() > 1) {
        for (k, &i) in rows.iter().enumerate() {
            keys.push(ObjKey {
                table: TableId::Instances,
                key: inst.id[i].0,
            });
            if k == 0 {
                continue;
            }
            let end = name
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(name.len());
            let prefix = if end == 0 { "X" } else { &name[..end] };
            let new = edit::unique_name(doc, prefix, &mut taken);
            ops.push(Op::SetAttr {
                target: DocTarget::new(RecordKind::Inst, &inst.id[i].text(s)),
                key: "name".into(),
                values: vec![new],
            });
        }
    }
    keys.sort_unstable();
    (keys, ops)
}

/// Instances placed exactly like an earlier instance of the same cell (all but the first of
/// each group).
pub(super) fn overlapped(doc: &Document) -> Vec<ObjKey> {
    let inst = doc.instances();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for i in 0..inst.id.len() {
        let place = (inst.cell[i], inst.x[i], inst.y[i], inst.orient[i] as u8);
        if !seen.insert(place) {
            out.push(ObjKey {
                table: TableId::Instances,
                key: inst.id[i].0,
            });
        }
    }
    out.sort_unstable();
    out
}

// ---- private ----

/// The ngspice spelling of an emitted name: ngspice lowercases every name it writes.
fn spice_case(name: &str) -> String {
    name.to_ascii_lowercase()
}
