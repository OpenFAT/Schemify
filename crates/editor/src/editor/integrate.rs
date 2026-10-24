//! The editor's integration steps (milestone M2): elab checks and net highlight, netlisting,
//! the run spine, the waves dock's cross-probe, OP back-annotation and staged edits. Kept
//! beside the frame spine in [`super`] as more `impl Editor` steps: each is called once per
//! frame from `update` (or from a command), reads the tables it names and writes only the
//! editor's own state (`annotations` through `elab::Annotations::apply`, `doc` only through
//! edit requests).

use std::path::PathBuf;

use schemify_doc::{DocHandle, ObjId, Op, TableId};
#[cfg(not(target_arch = "wasm32"))]
use schemify_doc::{EditRequest, Mode, Transaction};
use schemify_elab::{AnnotationRequest, Request, Severity};
use schemify_expr::Value;
use schemify_format::FileKind;
#[cfg(not(target_arch = "wasm32"))]
use schemify_format::RecordKind;
use schemify_netlist::NameMap;
use schemify_ui::records;
use schemify_ui::{Kind, StyleProp};

#[cfg(not(target_arch = "wasm32"))]
use super::push_req;
use super::{Editor, Panel, TEXT_TAB_LINES};
use crate::checks::{self, SOURCE_CHECKS, SOURCE_OP};
use crate::commands::Cmd;
use crate::docs;
use crate::geometry::Pt;
use crate::platform;
use crate::scene::{ObjKey, UNIT};
use crate::session::{TEXT_WINDOW, TextView, View};
use crate::spines;

/// Bytes of a netlist a text tab shows (the file has all of it).
const TEXT_TAB_BYTES: usize = 256 << 10;
/// Hit tolerance for a pin under the pointer, in screen pixels.
const PIN_PX: f64 = 8.0;
/// Benches "New bench from this component" tries before giving up on a free file name.
const BENCH_NAMES_MAX: u32 = 100;

/// Whether `cmd` makes sense on a read-only text tab (navigation, panels, the run).
pub(super) fn text_tab_cmd(cmd: Cmd) -> bool {
    matches!(
        cmd,
        Cmd::TabSelect
            | Cmd::TabNext
            | Cmd::TabPrev
            | Cmd::TabNew
            | Cmd::NewSchematic
            | Cmd::NewSymbol
            | Cmd::Close
            | Cmd::Quit
            | Cmd::Open
            | Cmd::Waves
            | Cmd::ChecksToggle
            | Cmd::ResultsToggle
            | Cmd::ResultsPick
            | Cmd::ResultsWaves
            | Cmd::ResultsHistory
            | Cmd::ResultsSort
            | Cmd::ResultsFilter
            | Cmd::SimCancel
            | Cmd::SetDialect
            | Cmd::ShowNetlist
            | Cmd::OpAnnotate
            | Cmd::SendToViewer
            | Cmd::SendHighlighted
            | Cmd::Redraw
            | Cmd::ToggleTheme
            | Cmd::DialogClose
            | Cmd::ChooseFilter
            | Cmd::ChoosePick
            | Cmd::TextFind
    )
}

/// Borrows the session's `${NAME}` environment pairs in the shape elab and run take.
fn env_refs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

/// The ngspice spelling of an emitted name: ngspice lowercases every name it writes.
fn spice_case(name: &str) -> String {
    name.to_ascii_lowercase()
}

impl Editor {
    /// The hierarchy prefix of the active level: `/x1/x2` (empty at the top), the instance
    /// path elab and the name map use.
    pub(super) fn level_prefix(&self) -> String {
        let Some(tab) = self.session.tabs.get(self.session.active) else {
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
        let tab = self.session.tabs.get(self.session.active)?;
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
                dialect: schemify_elab::Dialect::Ngspice,
            };
            let host = &mut self.host;
            let mut funcs = |name: &str, args: &[Value<'_>]| host.call_fn(name, args);
            let map = spines::elaborate_all(&mut self.session, top, &req, &mut funcs)
                .map(|e| schemify_netlist::emit(&e, schemify_elab::Dialect::Ngspice).name_map)
                .ok()?;
            self.hier = Some((top, self.generation, map));
            self.decor_dirty = true;
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
            self.decor_dirty = true;
            self.session.status = "highlights cleared".into();
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
                self.session.status = "highlight: the cell is not elaborated".into();
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
            self.session.status = "highlight: no net under the pointer or in the selection".into();
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
        self.decor_dirty = true;
        let verb = if cmd == Cmd::Highlight {
            "highlighted"
        } else {
            "unhighlighted"
        };
        self.session.status = format!("{verb}: {}", shown.join(" "));
        debug_assert!(self.highlight.windows(2).all(|w| w[0] < w[1]));
    }

    /// `text.find`: the lines of the text tab's file (all of it, not only the part the tab
    /// shows; the tab's text when there is no file) containing `query`, ASCII
    /// case-insensitively, numbered, at most a tab's worth shown; an empty query shows the
    /// text again.
    pub(super) fn find_in_text(&mut self, query: &str) {
        use std::fmt::Write as _;
        let q = query.trim().to_owned();
        let active = self.session.active;
        let Some(v) = self
            .session
            .tabs
            .get_mut(active)
            .and_then(|t| t.text.as_mut())
        else {
            self.session.status = "find: no text tab".into();
            return;
        };
        v.find.clone_from(&q);
        if q.is_empty() {
            v.found = None;
            return;
        }
        let full = platform::read(&v.path)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_else(|| v.body.clone());
        let needle = q.to_ascii_lowercase();
        let (mut lines, mut count) = (String::new(), 0u32);
        for (i, l) in full.lines().enumerate() {
            if l.to_ascii_lowercase().contains(&needle) {
                count += 1;
                if count as usize <= TEXT_TAB_LINES {
                    let _ = writeln!(lines, "{:>6}: {l}", i + 1);
                }
            }
        }
        v.found = Some((count, lines));
        self.session.status = format!("find {q:?}: {count} line(s)");
    }

    /// `n` (`whole`: the tab's top cell) / `Shift+N` (the cell at this level): elaborate,
    /// gate on errors (D13: the blocking lines go to the checks panel), emit in the selected
    /// dialect, write `.schemify/netlist/<cell>.<ext>`, and show it in a text tab when "show
    /// netlist" is on.
    pub(super) fn netlist(&mut self, whole: bool) {
        let Some(tab) = self.session.tabs.get(self.session.active) else {
            return;
        };
        let h = if whole {
            tab.stack[0].doc
        } else {
            tab.stack.last().expect("tabs are never empty").doc
        };
        let Some(stem) = self
            .session
            .path(h)
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
        else {
            return;
        };
        let dialect = self.dialect;
        let env = env_refs(&self.env);
        let req = Request {
            config: None,
            corner: None,
            overrides: &[],
            env: &env,
            dialect,
        };
        let host = &mut self.host;
        let mut funcs = |name: &str, args: &[Value<'_>]| host.call_fn(name, args);
        let e = match spines::elaborate_all(&mut self.session, h, &req, &mut funcs) {
            Ok(e) => e,
            Err(err) => {
                self.session.status = format!("netlist: {err}");
                return;
            }
        };
        if e.violations.blocking() {
            self.blocked = e
                .violations
                .rows
                .iter()
                .filter(|v| v.code.severity() == Severity::Error)
                .map(|v| spines::violation_line(&e.names, v))
                .collect();
            self.panel = Panel::Checks;
            self.session.status = format!(
                "netlist blocked: {} error(s) (see the checks panel)",
                self.blocked.len()
            );
            return;
        }
        self.blocked.clear();
        // Options → Flat netlist: the hierarchy expanded (SPICE dialects only).
        let spice = matches!(
            dialect,
            schemify_elab::Dialect::Ngspice
                | schemify_elab::Dialect::Xyce
                | schemify_elab::Dialect::Cdl
        );
        let text = if self.opts.flat && spice {
            schemify_netlist::emit_flat(&e, dialect)
        } else {
            schemify_netlist::emit(&e, dialect).text
        };
        let path = spines::netlist_file(&self.session.root, &stem, dialect);
        if let Err(err) = platform::write_atomic(&path, text.as_bytes()) {
            self.session.status = format!("netlist: {err}");
            return;
        }
        let lines = text.lines().count();
        self.session.status = format!(
            "netlist ({}) written: {} ({lines} lines)",
            spines::dialect_word(dialect),
            path.display()
        );
        if self.show_netlist {
            let mut body: String = text
                .lines()
                .take(TEXT_TAB_LINES)
                .collect::<Vec<_>>()
                .join("\n");
            if body.len() > TEXT_TAB_BYTES {
                let mut cut = TEXT_TAB_BYTES;
                while !body.is_char_boundary(cut) {
                    cut -= 1;
                }
                body.truncate(cut);
            }
            if body.len() < text.len().saturating_sub(1) {
                body.push_str(&format!(
                    "\n… (truncated; the whole netlist is in {})",
                    path.display()
                ));
            }
            let title = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            self.session.open_text_tab(TextView {
                title: format!("{title} (netlist)"),
                path,
                body,
                find: String::new(),
                found: None,
            });
            self.shell.invalidate();
        }
    }

    /// `s` / Optimize: only a `.shbench` runs (D11b; a component gets the message and the
    /// New-bench command). Starts the run without blocking; a refused gate lists its blocking
    /// violations in the checks panel.
    pub(super) fn simulate(&mut self, h: DocHandle, optimize: bool, arg: &str) {
        let kind = self
            .session
            .project
            .doc(h)
            .map(schemify_doc::Document::kind);
        if kind != Some(FileKind::Shbench) {
            self.session.status = "simulate: only a .shbench runs (D11b); use Simulation → New \
                                   bench from this component"
                .into();
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // OP annotation needs device quantities and currents saved (run spec §7a).
            let probe = if self.op_on {
                self.hier_map().map(crate::sim::probe_lines)
            } else {
                None
            };
            let probe = probe.unwrap_or_default();
            // The netlist dialect picks the simulator: espice runs ESPice, any other ngspice.
            self.sim.dialect = match self.dialect {
                schemify_elab::Dialect::Espice => schemify_elab::Dialect::Espice,
                _ => schemify_elab::Dialect::Ngspice,
            };
            let env = env_refs(&self.env);
            let host = &mut self.host;
            let mut funcs = |name: &str, args: &[Value<'_>]| host.call_fn(name, args);
            let what = optimize.then_some(arg.trim());
            match self
                .sim
                .start(&mut self.session, h, what, &probe, &env, &mut funcs)
            {
                Ok(msg) => {
                    self.blocked.clear();
                    self.session.status = msg;
                    self.panel = Panel::Results;
                    self.results_for = Some(h);
                    self.op_job = None;
                }
                Err(e) => {
                    if let Some(rest) = e.strip_prefix("run blocked: ") {
                        self.blocked = rest.split("; ").map(str::to_owned).collect();
                        self.panel = Panel::Checks;
                    }
                    self.session.status = e;
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (optimize, arg);
            self.session.status = "simulate: no simulator on the web (D16: runner plugins)".into();
        }
    }

    /// "New bench from this component" (D11b): writes `<stem>_tb.shbench` beside the
    /// component — one instance at the origin, a label on every pin naming its net after the
    /// pin, and an `.op` code block — and opens it in a new tab.
    pub(super) fn new_bench(&mut self, h: DocHandle) {
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        if doc.kind() == FileKind::Shbench {
            self.session.status = "this is already a bench".into();
            return;
        }
        let Some(path) = self.session.path(h).map(std::path::Path::to_path_buf) else {
            return;
        };
        let stem = path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let dir = path
            .parent()
            .map_or_else(PathBuf::new, std::path::Path::to_path_buf);
        let Some(file) = (0..BENCH_NAMES_MAX)
            .map(|k| {
                let name = if k == 0 {
                    format!("{stem}_tb.shbench")
                } else {
                    format!("{stem}_tb{}.shbench", k + 1)
                };
                dir.join(name)
            })
            .find(|p| !platform::exists(p))
        else {
            self.session.status = format!("{stem}_tb*.shbench: no free name");
            return;
        };
        let prefix = doc.header_value("prefix").unwrap_or("X");
        let scratch = schemify_doc::Document::new(FileKind::Shbench);
        let mut ids = crate::edit::Ids::new(&mut self.rng);
        let coord = |m: i32| {
            let mut v = String::new();
            schemify_format::format_coord(m, &mut v);
            v
        };
        let (id, _) = ids.draw(&scratch);
        let mut text = format!(
            "schemify 1 shbench\n\ninst {id} ./{stem}\n  at 0 0 R0\n  name {}\n\n",
            docs::quote(&format!("{prefix}1"))
        );
        let pins = doc.pins();
        let s = doc.strings();
        for i in 0..pins.name.len() {
            let (id, _) = ids.draw(&scratch);
            text.push_str(&format!(
                "label {id} {} {} R0 {}\n",
                coord(pins.x[i]),
                coord(pins.y[i]),
                docs::quote(s.get(pins.name[i]))
            ));
        }
        text.push_str("\ncode op spice\n  | .op\n");
        let bytes = match schemify_format::parse(text.as_bytes()) {
            Ok(d) => schemify_format::write(&d),
            Err(e) => {
                self.session.status = format!("new bench: {:?}", e.diagnostics.first());
                return;
            }
        };
        if let Err(e) = platform::write_atomic(&file, bytes.as_bytes()) {
            self.session.status = format!("new bench: {e}");
            return;
        }
        match self.session.open_path(&file) {
            Ok(b) => {
                self.session.open_tab(b, View::Schematic);
                self.scene_dirty = true;
                self.session.status = format!(
                    "new bench {}: add sources and analyses, then s",
                    file.display()
                );
            }
            Err(e) => self.session.status = e,
        }
    }

    /// A click on a checks-panel row: select its object and zoom to it.
    pub(super) fn checks_pick(&mut self, h: DocHandle, node: &str) {
        let Some(i) = node
            .rsplit('.')
            .next()
            .and_then(|n| n.parse::<usize>().ok())
        else {
            return;
        };
        let key = {
            let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
                return;
            };
            let Some(v) = cell.violations.rows.get(i) else {
                return;
            };
            self.session.status = checks::line(&cell.names, v);
            if v.object == ObjId::NONE {
                return;
            }
            doc.find_id(v.object).map(|loc| ObjKey {
                table: loc.table,
                key: v.object.0,
            })
        };
        let Some(key) = key.filter(|k| self.scene.find(*k).is_some()) else {
            return;
        };
        self.set_selection(vec![key]);
        let o = self.scene.find(key).expect("filtered");
        let b = self.scene.objs.bbox(o as usize);
        let pad = 60 * UNIT;
        self.fit([b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]);
    }

    /// Results panel commands: pick a job (`results.row.<j>`, `results.best.<j>`), open its
    /// raw file in the waves dock, load an earlier run (`results.hist.<i>`), sort by a header
    /// column (`results.head.<col>`) or filter the rows (`arg`: the filter field's text).
    pub(super) fn results_cmd(&mut self, cmd: Cmd, node: &str, arg: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let index = node.rsplit('.').next().and_then(|n| n.parse::<u32>().ok());
            match cmd {
                Cmd::ResultsSort => {
                    use crate::sim::Col;
                    let col = node.strip_prefix("results.head.").and_then(|c| match c {
                        "job" => Some(Col::Job),
                        "key" => Some(Col::Key),
                        "state" => Some(Col::State),
                        _ => {
                            let k = c.get(1..)?.parse::<u16>().ok()?;
                            match c.get(..1)? {
                                "o" => Some(Col::Output(k)),
                                "s" => Some(Col::Spec(k)),
                                _ => None,
                            }
                        }
                    });
                    if let Some(col) = col {
                        self.sim.sort_by(col);
                    }
                }
                Cmd::ResultsFilter => self.sim.set_filter(arg),
                Cmd::ResultsPick => {
                    if let Some(j) = index {
                        self.sim.select(j);
                        self.op_job = None;
                        if self.waves.is_some() {
                            self.open_job_waves();
                        }
                        self.session.status = format!("job {j} selected");
                    }
                }
                Cmd::ResultsWaves => self.open_job_waves(),
                _ => {
                    let (Some(i), Some(b)) = (index, self.sim.bench()) else {
                        return;
                    };
                    if let Some(dir) = self.sim.history.get(i as usize).cloned() {
                        match self.sim.load_dir(&self.session, b, &dir) {
                            Ok(()) => self.session.status = self.sim.status.clone(),
                            Err(e) => self.session.status = e,
                        }
                        self.op_job = None;
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (cmd, node, arg);
    }

    /// Opens the waves dock on the selected job's raw file.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_job_waves(&mut self) {
        let j = self.sim.selected;
        match self.sim.job_raw(j) {
            Some(p) => self.open_waves(p),
            None => self.session.status = format!("job {j} has no raw file (not finished?)"),
        }
    }

    /// A run finished: an open waves dock re-reads the selected job's raw file, keeping its
    /// traces and panes (spec waves §9). Nothing happens when the dock is closed.
    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_job_waves(&mut self) {
        let (Some(w), Some(path)) = (self.waves.as_mut(), self.sim.job_raw(self.sim.selected))
        else {
            return;
        };
        let title = path.display().to_string();
        match platform::read(&path).and_then(|b| w.reload(&title, &b)) {
            Ok(s) | Err(s) => self.session.status = s,
        }
    }

    /// `Alt+G`: adds `v(<net>)` to the waves dock for every highlighted net (keys the name map
    /// resolved; level paths are skipped), opening the dock on the selected job when closed.
    pub(super) fn send_to_viewer(&mut self) {
        if self.highlight.is_empty() {
            self.session.status = "no highlighted nets (k highlights)".into();
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.waves.is_none() {
            self.open_job_waves();
        }
        let Some(w) = self.waves.as_mut() else {
            self.session.status = "send to viewer: no waves open (simulate first)".into();
            return;
        };
        let mut sent = Vec::new();
        let mut failed = 0;
        for key in self.highlight.iter().filter(|k| !k.starts_with('/')) {
            let src = format!("v({key})");
            match w.add_trace(&src) {
                Ok(()) => sent.push(src),
                Err(_) => failed += 1, // ground and nets the simulator did not save
            }
        }
        self.session.status = format!(
            "sent to viewer: {} ({failed} not in the data)",
            sent.join(" ")
        );
        assert!(sent.len() + failed <= self.highlight.len());
    }

    /// Cross-probe (D23): a click on the canvas with the waves dock open adds `v(<net>)` for
    /// a net (its emitted name through the name map, lowercase for ngspice) and `i(<dev>)` for
    /// a pin of a branch device (`v`, `e`, `h`, `l`: ngspice keeps their currents).
    pub(super) fn cross_probe(&mut self, h: DocHandle, hit: Option<u32>, at: Pt) {
        let Some(o) = hit else { return };
        let key = self.scene.objs.key[o as usize];
        let tol = (PIN_PX / self.camera().scale) as i64;
        let found = {
            let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
                return;
            };
            checks::component_of(doc, cell, key, at, tol).map(|(c, pin)| {
                let inst = pin.map(|(row, k)| {
                    let name = doc.strings().get(doc.instances().name[row as usize]);
                    let f = cell.ifaces.get(cell.inst_iface[row as usize] as usize);
                    // `k` counts the ordinary (not inherited) pins, as `pins_of` does.
                    let pin = f.and_then(|f| {
                        (0..f.pin_name.len())
                            .filter(|&p| f.pin_inherit[p] == schemify_elab::Name::NONE)
                            .nth(k as usize)
                            .map(|p| cell.names.get(f.pin_name[p]).to_owned())
                    });
                    (name.to_owned(), pin.unwrap_or_default())
                });
                (checks::net_name(cell, c).to_owned(), inst)
            })
        };
        let Some((net, inst)) = found else { return };
        let volts = format!("v({})", self.net_key(&net));
        let src = match inst {
            Some((name, pin)) => {
                let path = format!("{}/{name}", self.level_prefix());
                let Some(emitted) = self.hier_map().and_then(|m| m.inst_name(&path)) else {
                    self.session.status = format!("{path}: not in the netlist");
                    return;
                };
                crate::waves::current_probe(&emitted, &pin)
            }
            None => volts.clone(),
        };
        let Some(w) = self.waves.as_mut() else { return };
        // A device current the raw file lacks (a run without OP annotation saves no
        // `savecurrents`): probe the pin's net instead.
        let same = volts == src;
        let added = match w.add_trace(&src) {
            Ok(()) => Ok(src),
            Err(e) if !same => w.add_trace(&volts).map(|()| volts).map_err(|_| e),
            Err(e) => Err(e),
        };
        self.session.status = match added {
            Ok(src) => format!("cross-probe: {src}"),
            Err(e) => format!("cross-probe: {e}"),
        };
    }

    /// The `checks` source of the annotation overlay from the active level's violations: when
    /// the cell state's last check pass continues the pass posted here, only its changes are
    /// patched in (an edit that leaves tens of thousands of `W004` rows alone touches none
    /// of their marks); otherwise (another document, a rebuilt state) everything is re-posted.
    pub(super) fn post_checks(&mut self) {
        let active = self.active().map(|a| a.0);
        let doc = active.and_then(|h| self.session.project.doc(h));
        let Some((c, d)) = self.cell.as_ref().zip(doc) else {
            if self.checks_key.take().is_some() {
                self.annotations.replace(AnnotationRequest {
                    source: SOURCE_CHECKS.to_owned(),
                    marks: Vec::new(),
                });
                self.marker_targets = None;
            }
            return;
        };
        if self.checks_key == Some((active, c.pass)) {
            return;
        }
        let continues = c.prev_pass != 0 && self.checks_key == Some((active, c.prev_pass));
        let delta = continues
            .then(|| {
                let gone = checks::violation_marks_of(&c.changes.0, c, d)?;
                let new = checks::violation_marks_of(&c.changes.1, c, d)?;
                Some((gone, new))
            })
            .flatten();
        self.checks_key = Some((active, c.pass));
        // The markers follow: the targets of the marks that changed, or everything.
        match delta {
            Some((gone, new)) if gone.is_empty() && new.is_empty() => {}
            Some((gone, new)) => {
                if let Some(t) = self.marker_targets.as_mut() {
                    t.extend(gone.iter().chain(&new).map(|m| m.target.clone()));
                }
                self.annotations.patch(SOURCE_CHECKS, gone, new);
            }
            None => {
                self.annotations.replace(checks::violation_request(c, d));
                self.marker_targets = None;
            }
        }
    }

    /// The run step of the frame spine: loads a bench's saved results when it is first
    /// shown, serves plugin run requests, polls the run (prepare + submit a few jobs,
    /// `Runner::poll`, optimizer ask/tell), delivers `RunDone`, stages the optimizer's best
    /// point, marks results stale after edits, and refreshes plot slots and the OP overlay.
    /// Returns whether anything visible changed.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn step_sim(&mut self, reqs: &mut Vec<EditRequest>) -> bool {
        let mut changed = false;
        let active = self.active().map(|a| a.0);
        let bench = active.filter(|&h| {
            self.session
                .project
                .doc(h)
                .map(schemify_doc::Document::kind)
                == Some(FileKind::Shbench)
        });
        if let Some(b) = bench
            && self.results_for != Some(b)
        {
            self.results_for = Some(b);
            changed |= self.sim.load_latest(&self.session, b);
        }
        let env = env_refs(&self.env);
        let host = &mut self.host;
        let mut funcs = |name: &str, args: &[Value<'_>]| host.call_fn(name, args);
        self.sim.serve_queue(&mut self.session, &env, &mut funcs);
        let polled = self.sim.poll(&self.session.project, &env, &mut funcs);
        if self.sim.tables().is_some() {
            self.sim
                .check_stale(&mut self.session, &env, &mut funcs, self.generation);
        }
        drop(env);
        for &(p, j) in &polled.run_done {
            self.host.run_done(p, j);
        }
        if polled.changed {
            changed = true;
            self.session.status = self.sim.status.clone();
            self.op_job = None;
            self.sim.plots.clear();
        }
        if polled.done {
            self.refresh_job_waves();
        }
        for best in polled.best {
            self.stage_best(reqs, best);
        }
        self.sim.refresh_table();
        // Plot slots of the shown page.
        let exprs = match active.and_then(|h| self.session.project.doc(h)) {
            Some(doc) if self.docs_shown() => docs::plot_exprs(doc),
            _ => Vec::new(),
        };
        let related = !exprs.is_empty() && active.is_some_and(|h| self.related_to_bench(h));
        let before = self.sim.plots.len();
        if related && !exprs.is_empty() {
            self.sim.refresh_plots(&exprs);
        } else {
            self.sim.plots.clear();
        }
        changed |= before != self.sim.plots.len();
        // OP back-annotation of the selected job (when the tab shows the bench's hierarchy).
        let top = self
            .session
            .tabs
            .get(self.session.active)
            .map(|t| t.stack[0].doc);
        let key = (self.sim.selected, self.generation);
        let wanted = self.op_on && top.is_some() && top == self.sim.bench();
        // Only this posts `op` marks: while none are shown, an unwanted overlay has nothing to
        // clear (no scan of every mark per frame).
        let stale = if wanted {
            self.op_job != Some(key)
        } else {
            self.op_shown
        };
        if stale {
            let had = self.op_shown;
            let req = if wanted {
                match self.hier_map().cloned() {
                    Some(map) => self.sim.op_marks(&map),
                    None => AnnotationRequest {
                        source: SOURCE_OP.to_owned(),
                        marks: Vec::new(),
                    },
                }
            } else {
                AnnotationRequest {
                    source: SOURCE_OP.to_owned(),
                    marks: Vec::new(),
                }
            };
            if had || !req.marks.is_empty() {
                self.op_shown = !req.marks.is_empty();
                self.annotations.replace(req);
                self.decor_dirty = true;
                changed = true;
            }
            if wanted {
                self.op_job = Some(key);
            }
        }
        debug_assert!(self.op_shown || !self.annotations.has_source(SOURCE_OP));
        changed
    }

    /// Whether the docs page of `h` should show the current run's results: `h` is its bench,
    /// the bench instantiates `h`'s cell (by file stem, also when results were reloaded and
    /// the cells are closed), or `h` is a cell anywhere below it. The walk goes through open
    /// documents (a run opens every cell it reaches), each visited once.
    ///
    /// Cost: O(instances of the open documents of the hierarchy); called only while the
    /// shown page has plot fences.
    #[cfg(not(target_arch = "wasm32"))]
    fn related_to_bench(&self, h: DocHandle) -> bool {
        let Some(b) = self.sim.bench() else {
            return false;
        };
        if b == h {
            return true;
        }
        let stem = self.session.name(h);
        let stem = stem.rsplit_once('.').map_or(stem.as_str(), |(s, _)| s);
        let p = &self.session.project;
        let direct = p.doc(b).is_some_and(|d| {
            let s = d.strings();
            d.instances().cell.iter().any(|&c| {
                let r = s.get(c);
                r.rsplit([':', '/']).next() == Some(stem)
            })
        });
        if direct {
            return true;
        }
        let open: Vec<DocHandle> = p.handles().collect();
        let (mut seen, mut todo) = (vec![b], vec![b]);
        while let Some(d) = todo.pop() {
            if d == h {
                return true;
            }
            let Some(doc) = p.doc(d) else { continue };
            let s = doc.strings();
            let mut cells = doc.instances().cell.clone();
            cells.sort_unstable_by_key(|c| c.0);
            cells.dedup();
            for c in cells {
                let Some(child) = p.resolve(d, s.get(c)).ok().and_then(|r| p.interface(r)) else {
                    continue;
                };
                let found = open
                    .iter()
                    .copied()
                    .find(|&o| p.doc(o).is_some_and(|x| std::ptr::eq(x, child)));
                if let Some(o) = found.filter(|o| !seen.contains(o)) {
                    seen.push(o);
                    todo.push(o);
                }
            }
        }
        debug_assert!(seen.len() <= open.len() + 1);
        false
    }

    /// Stages the optimizer's best point on its bench (D18): one `set_field var` per varied
    /// var (plus dropping a stale D20 cache), reviewed on the docs page, then Accept/Reject.
    #[cfg(not(target_arch = "wasm32"))]
    fn stage_best(&mut self, reqs: &mut Vec<EditRequest>, best: crate::sim::Best) {
        let ops: Vec<Op> = {
            let Some(doc) = self.session.project.doc(best.bench) else {
                return;
            };
            let b = doc.doc_blocks();
            let s = doc.strings();
            best.values
                .iter()
                .flat_map(|(name, value)| {
                    let row = (0..b.kind.len())
                        .find(|&r| b.kind[r] == RecordKind::Var && s.get(b.fields[r][0]) == name);
                    row.map_or_else(Vec::new, |r| docs::var_ops(doc, r as u32, value))
                })
                .collect()
        };
        if ops.is_empty() {
            self.session.status = format!("{}: nothing to stage", best.label);
            return;
        }
        let tx = Transaction {
            label: best.label.clone(),
            mode: Mode::Stage,
            ops,
        };
        push_req(reqs, best.bench, schemify_doc::Edit::Tx(tx));
        self.apply(reqs);
        let mut msg = format!("{}: staged", best.label);
        for (n, v) in &best.values {
            msg.push_str(&format!(" {n}={v}"));
        }
        msg.push_str(" — review on the docs page, then Accept or Reject");
        self.session.status = msg;
        if let Some(t) = self.session.tabs.get_mut(self.session.active) {
            t.docs = true;
        }
    }

    /// Keeps the staged edit under review in step with the active document: its preview
    /// (design ⊕ stage, for the docs page) and the objects it changes (canvas diff).
    pub(super) fn track_stage(&mut self) {
        let Some((h, _)) = self.active() else { return };
        let stages = self.session.project.stages(h);
        let n = stages.len();
        let pick = self.stage_pick.min(n.saturating_sub(1));
        self.stage_pick = pick;
        let key = stages.get(pick).map(|st| (h, u32::from(st.id)));
        if key == self.diff_key {
            return;
        }
        self.diff_key = key;
        self.diff = checks::Diff::default();
        self.staged = None;
        self.decor_dirty = true;
        let Some(st) = stages.get(pick) else { return };
        let Some(doc) = self.session.project.doc(h) else {
            return;
        };
        let s = doc.strings();
        for op in &st.ops {
            let (kind, key, deleted) = match op {
                Op::Delete(t) => (t.kind, t.key.as_str(), true),
                Op::Put { target: t, .. }
                | Op::SetField { target: t, .. }
                | Op::SetAttr { target: t, .. }
                | Op::RemoveAttr { target: t, .. }
                | Op::Reorder { target: t, .. } => (t.kind, t.key.as_str(), false),
                Op::Insert { rec, .. } => {
                    self.diff.inserted.push(rec.clone());
                    continue;
                }
            };
            let table = match TableId::of(kind) {
                Some(t) => t,
                None => continue,
            };
            if let Some(id) = ObjId::find(key, s).filter(|&id| doc.find_id(id).is_some()) {
                let k = ObjKey { table, key: id.0 };
                if deleted {
                    self.diff.deleted.push(k);
                } else {
                    self.diff.changed.push(k);
                }
            }
        }
        let checks::Diff {
            changed, deleted, ..
        } = &mut self.diff;
        deleted.sort_unstable();
        deleted.dedup();
        changed.sort_unstable();
        changed.dedup();
        changed.retain(|k| deleted.binary_search(k).is_err());
        if let Ok(preview) = self.session.project.preview(h, st.id) {
            // With several stages the banner says which one is under review.
            let label = match n {
                1 => st.label.clone(),
                _ => format!("({}/{n}) {}", pick + 1, st.label),
            };
            self.staged = Some((label, preview));
        }
    }

    /// Places the violation markers where the scene or the `checks` marks changed since
    /// they were last placed (`checks::place_markers`: the objects the scene's last update
    /// touched and the targets of the patched marks, or everything after a scene build, a
    /// missed scene generation or a replaced overlay). Returns whether they changed.
    pub(super) fn build_markers(&mut self) -> bool {
        let Some(doc) = self.active().and_then(|(h, _)| self.session.project.doc(h)) else {
            return false;
        };
        let g = self.scene.generation;
        let targets = self.marker_targets.replace(Vec::new());
        let seen = self.markers.generation();
        let ids: Option<Vec<u32>> = match (targets, self.scene.touched()) {
            (Some(t), _) if seen == Some(g) && t.is_empty() => return false,
            (Some(t), _) if seen == Some(g) => Some(ids_of(&t, doc)),
            (Some(t), Some(keys)) if seen == Some(g.wrapping_sub(1)) => {
                let mut ids = ids_of(&t, doc);
                ids.extend(keys.iter().map(|k| k.key));
                ids.sort_unstable();
                ids.dedup();
                Some(ids)
            }
            _ => None,
        };
        checks::place_markers(
            &mut self.markers,
            &self.scene,
            doc,
            &self.annotations,
            ids.as_deref(),
        );
        self.canvas_gen[4] += 1;
        assert_eq!(self.markers.generation(), Some(g));
        true
    }

    /// Rebuilds the decor primitives (highlight, OP annotations, staged diff) when the
    /// overlay, the highlight, the name map or the staged diff changed, or the scene did and
    /// the decor draws something on it. Returns whether it rebuilt.
    pub(super) fn build_decor(&mut self, scene_stale: bool) -> bool {
        // What the decor draws follows the scene only while it has something to draw.
        let draws = !self.decor.is_empty()
            || !self.highlight.is_empty()
            || self.annotations.len() > self.annotations.count_of(SOURCE_CHECKS)
            || self.diff != checks::Diff::default();
        if !(self.decor_dirty || scene_stale && draws) {
            return false;
        }
        self.decor_dirty = false;
        let Some((h, _)) = self.active() else {
            return false;
        };
        // Components with a highlighted bit (a bus by any of its bits).
        let comps: Vec<u32> = match (self.highlight.is_empty(), self.cell.as_ref()) {
            (false, Some(cell)) => {
                let bits: Vec<(u32, String)> = (0..cell.nets.net_name.len() as u32)
                    .flat_map(|c| checks::bit_names(cell, c).map(move |b| (c, b.to_owned())))
                    .collect();
                let mut out: Vec<u32> = Vec::new();
                for (c, n) in &bits {
                    if out.last() == Some(c) {
                        continue;
                    }
                    let key = self.net_key(n);
                    if self.highlight.binary_search(&key).is_ok() {
                        out.push(*c);
                    }
                }
                out
            }
            _ => Vec::new(),
        };
        let prefix = self.level_prefix();
        let has_op = self.annotations.has_source(SOURCE_OP);
        // Net keys for annotation lookup: computed before borrowing the scene and the doc.
        let net_keys: Vec<(String, String)> = match (has_op, self.cell.as_ref()) {
            (true, Some(cell)) => {
                let names: Vec<String> = (0..cell.nets.net_name.len() as u32)
                    .map(|c| checks::net_name(cell, c).to_owned())
                    .collect();
                names
                    .into_iter()
                    .map(|n| (n.clone(), self.net_key(&n)))
                    .collect()
            }
            _ => Vec::new(),
        };
        let Some(doc) = self.session.project.doc(h) else {
            return false;
        };
        self.hl = match self.cell.as_ref() {
            Some(cell) if cell.inst_iface.len() == doc.instances().id.len() => {
                checks::members(doc, cell, &comps)
            }
            _ => checks::Members::default(),
        };
        let cell = self
            .cell
            .as_ref()
            .filter(|c| c.inst_iface.len() == doc.instances().id.len());
        checks::decor(
            &mut self.decor,
            &self.scene,
            doc,
            cell,
            &self.annotations,
            &self.hl,
            |n| net_keys.iter().find(|k| k.0 == n).map(|k| k.1.clone()),
            |inst| format!("{prefix}/{inst}"),
            &self.diff,
        );
        self.canvas_gen[1] += 1;
        true
    }

    /// Appends the text tab's `shown` text to `t` as one fenced-code `textview.body` prose
    /// node under `textview`. A long text is windowed by the tab's scroll offset: only
    /// [`TEXT_WINDOW`] lines (or two screens, if more) are in the node, with spacers above and
    /// below sized in whole code rows ([`schemify_ui::prose::code_metrics`]), so the frame
    /// lays out and paints a screenful, not 16k lines.
    ///
    /// The visible rows are the ones the unwindowed node would show only if every line above
    /// the window is exactly one row: printable ASCII, no fence, and narrower than the wrap
    /// width the last layout had (less any shrink of the window since). The window therefore
    /// never starts past the first line that is not, nor before the tab has been laid out once.
    /// Lines below the window are counted as one row each; the scroll extent is exact once
    /// the window reaches the end.
    fn contribute_text_body(&self, t: &mut String, shown: &str) {
        use std::fmt::Write as _;
        let sh = &self.shell;
        let node = |t: &mut String, body: &str| {
            let fenced = format!("```\n{body}\n```");
            let _ = writeln!(
                t,
                "node textview.body prose\n  parent textview\n  value {}\n  style text-view-body",
                docs::quote(&fenced)
            );
        };
        let style = sh
            .strings
            .find("text-view-body")
            .map_or(schemify_ui::NO_STYLE, |s| sh.tables.styles.row(s));
        let font = sh
            .tables
            .styles
            .get(style, Kind::Prose, StyleProp::Font)
            .max(8);
        let Some(m) = u16::try_from(font)
            .ok()
            .and_then(|px| schemify_ui::prose::code_metrics(&sh.text, px))
        else {
            return node(t, shown);
        };
        // The lines the prose parser sees between the fences.
        let lines: Vec<&str> = shown
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();
        let n = lines.len() as u32;
        assert!(n >= 1);
        let row = |id: &str| {
            let i = sh.tables.nodes.find(sh.strings.find(id)?)?;
            sh.layout.row_of(i).map(|r| sh.layout.rect()[r])
        };
        let scroll = sh
            .strings
            .find("textview")
            .map_or(0.0, |s| sh.ui.scroll_of(s));
        let len = TEXT_WINDOW.max(2 * (self.viewport.1 / m.line).ceil() as u32);
        let (mut start, end) = records::window(scroll, m.line, n, len);
        // Wrap width: last frame's, less the window's shrink since (the tab never shrinks more).
        let shrink = sh
            .layout
            .rect()
            .first()
            .map_or(0.0, |r| r.w - self.viewport.0)
            .max(0.0);
        let wrap = row("textview.body").map_or(0.0, |r| r.w - 2.0 * m.pad - shrink);
        let cols = (wrap / m.advance - 0.01).floor().max(0.0) as usize;
        let one_row = |l: &str| {
            l.len() <= cols
                && l.bytes().all(|b| (b' '..=b'~').contains(&b))
                && !l.trim_start().starts_with("```")
        };
        let clean = lines[..start as usize]
            .iter()
            .take_while(|l| one_row(l))
            .count() as u32;
        start = start.min(clean);
        if start == 0 && end == n {
            return node(t, shown);
        }
        let px = m.line as u32;
        records::spacer(t, "textview", "textview.gap-top", start * px, 0);
        node(t, &lines[start as usize..end as usize].join("\n"));
        records::spacer(t, "textview", "textview.gap-bottom", (n - end) * px, 0);
        debug_assert!(start < end);
    }

    /// Projects the bottom panel, the text tab and the integration state (menu checks) into
    /// the state contribution `t`.
    pub(super) fn contribute_integration(&self, t: &mut String) {
        use std::fmt::Write as _;
        match self.panel {
            Panel::None => {
                let _ = writeln!(t, "remove bottom");
            }
            Panel::Checks => {
                let cell = self.cell.as_ref();
                checks::contribute(t, cell, &self.blocked);
            }
            Panel::Results => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let list = self.shell.strings.find("results.list");
                    let scroll = list.map_or(0.0, |s| self.shell.ui.scroll_of(s));
                    self.sim.contribute(t, scroll);
                }
            }
        }
        // Several staged edits: step through them on the docs page's banner.
        if self.staged.is_some()
            && self.docs_shown()
            && self
                .active()
                .is_some_and(|(h, _)| self.session.project.stages(h).len() > 1)
        {
            for (id, label, cmd) in [
                ("docs.staged.prev", "<", "edit.show_staged.prev"),
                ("docs.staged.next", ">", "edit.show_staged.next"),
            ] {
                let _ = writeln!(
                    t,
                    "node {id} button\n  parent docs.staged\n  label {label}\n  command {cmd}"
                );
            }
        }
        if let Some(v) = self.session.text_view() {
            let _ = writeln!(t, "remove canvas");
            // The pane is a column, so the scroll's content height never reaches the `work`
            // row and the tab scrolls instead of growing to its text (as `docs.pane` does).
            let _ = writeln!(
                t,
                "style text-pane\n  grow 1\n  dir column\nnode textview.pane panel\n  parent work\n  style text-pane\nnode textview scroll\n  parent textview.pane\n  style text-view"
            );
            let _ = writeln!(
                t,
                "node textview.find text_input\n  parent textview\n  value {}\n  command text.find\n  tooltip {}",
                docs::quote(&v.find),
                docs::quote("Find (Enter): the file's lines containing the text, numbered")
            );
            let shown = match &v.found {
                Some((count, lines)) => {
                    let _ = writeln!(
                        t,
                        "node textview.found label\n  parent textview\n  label {}",
                        docs::quote(&format!("{count} line(s) contain {:?}", v.find))
                    );
                    lines
                }
                None => &v.body,
            };
            self.contribute_text_body(t, shown);
        }
        for (word, d, _) in spines::DIALECTS {
            let _ = writeln!(
                t,
                "set sim.dialect.{word} checked {}",
                u8::from(d == self.dialect)
            );
        }
        let _ = writeln!(
            t,
            "set simulation.show_netlist_after_netlist_command checked {}",
            u8::from(self.show_netlist)
        );
        let _ = writeln!(t, "set waves.op_annotate checked {}", u8::from(self.op_on));
    }
}

/// The object ids of mark targets `targets` (sorted, unique; a path or name target has none).
fn ids_of(targets: &[String], doc: &schemify_doc::Document) -> Vec<u32> {
    let s = doc.strings();
    let mut ids: Vec<u32> = targets
        .iter()
        .filter_map(|t| ObjId::find(t, s))
        .map(|id| id.0)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert!(ids.len() <= targets.len());
    ids
}
