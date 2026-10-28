//! The results side of a run in the editor: a checks-panel pick, the results table's
//! commands (sort, filter, row pick, history), and the selected job's waves.
//!
//! Contents:
//! - `Editor::checks_pick`
//! - `Editor::results_cmd`
//! - `Editor::open_job_waves`
//! - `Editor::refresh_job_waves`

use schemify_doc::{DocHandle, ObjId};

use crate::commands::Cmd;
use crate::scene::{ObjKey, UNIT};
use crate::{checks, platform};

use super::Editor;

impl Editor {
    /// A click on a checks-panel row: select its object and zoom to it.
    pub(super) fn checks_pick(&mut self, h: DocHandle, node: &str) {
        let Some(i) = node
            .rsplit('.')
            .next()
            .and_then(|n| n.parse::<usize>().ok())
        else {
            return;
        };
        let (line, key) = {
            let (Some(cell), Some(doc)) = (self.cell.as_ref(), self.session.project.doc(h)) else {
                return;
            };
            let Some(v) = cell.violations.rows.get(i) else {
                return;
            };
            let key = (v.object != ObjId::NONE)
                .then(|| doc.find_id(v.object))
                .flatten()
                .map(|loc| ObjKey {
                    table: loc.table,
                    key: v.object.0,
                });
            (checks::line(&cell.names, v), key)
        };
        self.session.say(line);
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
                        self.session.say(format!("job {j} selected"));
                    }
                }
                Cmd::ResultsWaves => self.open_job_waves(),
                _ => {
                    let (Some(i), Some(b)) = (index, self.sim.bench()) else {
                        return;
                    };
                    if let Some(dir) = self.sim.history.get(i as usize).cloned() {
                        match self.sim.load_dir(&self.session, b, &dir) {
                            Ok(()) => self.session.say(self.sim.status.clone()),
                            Err(e) => self.session.say(e),
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
    pub(super) fn open_job_waves(&mut self) {
        let j = self.sim.selected;
        match self.sim.job_raw(j) {
            Some(p) => self.open_waves(p),
            None => self
                .session
                .say(format!("job {j} has no raw file (not finished?)")),
        }
    }
    /// A run finished: an open waves dock re-reads the selected job's raw file, keeping its
    /// traces and panes (spec waves §9). Nothing happens when the dock is closed.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn refresh_job_waves(&mut self) {
        let (Some(w), Some(path)) = (self.waves.as_mut(), self.sim.job_raw(self.sim.selected))
        else {
            return;
        };
        let title = path.display().to_string();
        match platform::read(&path).and_then(|b| w.reload(&title, &b)) {
            Ok(s) | Err(s) => self.session.say(s),
        }
    }
}
