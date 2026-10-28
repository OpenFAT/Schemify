//! The run in the editor: start, poll, results, plots, OP annotation, staged edits.
//!
//! Contents:
//! - `Editor::simulate`
//! - `Editor::new_bench`
//! - `Editor::step_sim`
//! - `Editor::track_stage`

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, Document, ObjId, Op, TableId};
#[cfg(not(target_arch = "wasm32"))]
use schemify_doc::{Edit, EditRequest, Mode, Transaction};
#[cfg(not(target_arch = "wasm32"))]
use schemify_elab::AnnotationRequest;
#[cfg(not(target_arch = "wasm32"))]
use schemify_expr::Value;
use schemify_format::FileKind;
#[cfg(not(target_arch = "wasm32"))]
use schemify_format::RecordKind;

use crate::scene::ObjKey;
use crate::session::View;
use crate::{checks, docs, platform};

use super::Editor;
#[cfg(not(target_arch = "wasm32"))]
use super::{Panel, apply, highlight};
#[cfg(not(target_arch = "wasm32"))]
use crate::checks::SOURCE_OP;
use crate::edit::Ids;
#[cfg(not(target_arch = "wasm32"))]
use crate::sim::Best;
#[cfg(not(target_arch = "wasm32"))]
use crate::spines;

/// Benches "New bench from this component" tries before giving up on a free file name.
const BENCH_NAMES_MAX: u32 = 100;

impl Editor {
    /// `s` / Optimize: only a `.shbench` runs (D11b; a component gets the message and the
    /// New-bench command). Starts the run without blocking; a refused gate lists its blocking
    /// violations in the checks panel.
    pub(super) fn simulate(&mut self, h: DocHandle, optimize: bool, arg: &str) {
        let kind = self.session.project.doc(h).map(Document::kind);
        if kind != Some(FileKind::Shbench) {
            self.session.say(
                "simulate: only a .shbench runs (D11b); use Simulation → New \
                                   bench from this component",
            );
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
            // The netlist dialect picks the simulator: espice runs ESPice, any other ngspice,
            // unless the PDK pins one (run spec §5).
            match spines::run_dialect(self.dialect, &self.session.project, self.host.user()) {
                Ok(d) => self.sim.dialect = d,
                Err(e) => {
                    self.session.say(format!("simulate: {e}"));
                    return;
                }
            }
            let env = highlight::env_refs(&self.env);
            let host = &mut self.host;
            let mut funcs = |name: &str, args: &[Value<'_>]| host.call_fn(name, args);
            let what = optimize.then_some(arg.trim());
            match self
                .sim
                .start(&mut self.session, h, what, &probe, &env, &mut funcs)
            {
                Ok(msg) => {
                    self.blocked.clear();
                    self.session.say(msg);
                    self.panel = Panel::Results;
                    self.results_for = Some(h);
                    self.op_job = None;
                }
                Err(e) => {
                    if let Some(rest) = e.strip_prefix("run blocked: ") {
                        self.blocked = rest.split("; ").map(str::to_owned).collect();
                        self.panel = Panel::Checks;
                    }
                    self.session.say(e);
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (optimize, arg);
            self.session
                .say("simulate: no simulator on the web (D16: runner plugins)");
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
            self.session.say("this is already a bench");
            return;
        }
        let Some(path) = self.session.path(h).map(Path::to_path_buf) else {
            return;
        };
        let stem = path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
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
            self.session
                .say(format!("{stem}_tb*.shbench: no free name"));
            return;
        };
        let prefix = doc.header_value("prefix").unwrap_or("X");
        let scratch = Document::new(FileKind::Shbench);
        let mut ids = Ids::new(&mut self.rng);
        let coord = crate::geometry::coord;
        let id = ids.draw(&scratch).text;
        let mut text = format!(
            "schemify 1 shbench\n\ninst {id} ./{stem}\n  at 0 0 R0\n  name {}\n\n",
            docs::quote(&format!("{prefix}1"))
        );
        let pins = doc.pins();
        let s = doc.strings();
        for i in 0..pins.name.len() {
            let id = ids.draw(&scratch).text;
            let _ = writeln!(
                text,
                "label {id} {} {} R0 {}",
                coord(pins.x[i]),
                coord(pins.y[i]),
                docs::quote(s.get(pins.name[i]))
            );
        }
        text.push_str("\ncode op spice\n  | .op\n");
        let bytes = match schemify_format::parse(text.as_bytes()) {
            Ok(d) => schemify_format::write(&d),
            Err(e) => {
                self.session
                    .say(format!("new bench: {:?}", e.diagnostics.first()));
                return;
            }
        };
        if let Err(e) = platform::write_atomic(&file, bytes.as_bytes()) {
            self.session.say(format!("new bench: {e}"));
            return;
        }
        match self.session.open_path(&file) {
            Ok(b) => {
                self.session.open_tab(b, View::Schematic);
                self.session.say(format!(
                    "new bench {}: add sources and analyses, then s",
                    file.display()
                ));
            }
            Err(e) => self.session.say(e),
        }
    }
}

impl Editor {
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
            self.session.project.doc(h).map(Document::kind) == Some(FileKind::Shbench)
        });
        if let Some(b) = bench
            && self.results_for != Some(b)
        {
            self.results_for = Some(b);
            changed |= self.sim.load_latest(&self.session, b);
        }
        let env = highlight::env_refs(&self.env);
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
            self.session.say(self.sim.status.clone());
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
            .tabs()
            .get(self.session.active())
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
                self.op_gen = self.op_gen.wrapping_add(1);
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
    fn stage_best(&mut self, reqs: &mut Vec<EditRequest>, best: Best) {
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
            self.session
                .say(format!("{}: nothing to stage", best.label));
            return;
        }
        let tx = Transaction {
            label: best.label.clone(),
            mode: Mode::Stage,
            ops,
        };
        apply::push_req(reqs, best.bench, Edit::Tx(tx));
        self.apply(reqs);
        let mut msg = format!("{}: staged", best.label);
        for (n, v) in &best.values {
            let _ = write!(msg, " {n}={v}");
        }
        msg.push_str(" — review on the docs page, then Accept or Reject");
        self.session.say(msg);
        if let Some(t) = self.session.tab_view_mut() {
            *t.docs = true;
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
}
