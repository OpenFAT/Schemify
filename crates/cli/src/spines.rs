//! The design spines shared by the headless CLI (`schemify netlist|check|run`) and the editor
//! (`n`, `s`, Optimize): elaboration that opens the cells elab asks for, the violation report
//! line, the dialect words and netlist file names, and the run spine up to the point where the
//! CLI waits and the editor polls (docs/dev/architecture.md § Spines, `netlist` and `run`).
//!
//! Both callers own their project differently (the CLI a bare `doc::Project`, the editor a
//! `Session` (`schemify_app::session`) with journals), so opening a cell is the one step behind a
//! trait, [`Cells`], resolved at compile time.
//!
//! Data design (the run): one [`Run`] per Simulate/Optimize. Jobs ≤ `run::JOBS_MAX` (2^20),
//! outputs/specs/vars ≤ `run::TABLE_MAX` each; the `finished` buffer holds the jobs one poll
//! finished (≤ workers + jobs started that poll). Lifetime: until the next run replaces it.

use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, Project};
use schemify_elab::{Dialect, Elab, Funcs, Name, Names, Request, Violation};

/// How a caller opens the cell rows elaboration asks for.
pub trait Cells {
    /// The project elaboration reads.
    fn project(&self) -> &Project;
    /// Opens indexed cell `row` (with its journal, for the editor).
    ///
    /// # Errors
    ///
    /// The file cannot be read or does not parse.
    fn open_row(&mut self, row: u32) -> Result<(), String>;
}

/// Elaborates `top`, opening every cell elab asks for and elaborating again, at most once per
/// indexed cell plus one (each round opens at least one new cell).
///
/// # Errors
///
/// A cell cannot be opened, or elaboration keeps asking (a bug in the opener).
pub fn elaborate_all<C: Cells, F: Funcs>(
    c: &mut C,
    top: DocHandle,
    req: &Request<'_>,
    funcs: &mut F,
) -> Result<Elab, String> {
    assert!(c.project().doc(top).is_some(), "elaborate an open document");
    let rounds = c.project().cells().lib.len() + 1;
    for _ in 0..rounds {
        let e = schemify_elab::elaborate(c.project(), top, req, funcs);
        if e.needs_open.is_empty() {
            return Ok(e);
        }
        for &row in &e.needs_open {
            c.open_row(row)?;
        }
    }
    Err("elaboration kept asking for more cells".into())
}

/// One violation as the CLI prints it and the editor lists it:
/// `E005 lib:cell /X1 W: detail` (path and param only when set). `names` is the arena the
/// violation's names live in (`Elab::names` or `CellState::names`).
pub fn violation_line(names: &Names, v: &Violation) -> String {
    let name = |n: Name| (n != Name::NONE).then(|| names.get(n));
    let mut line = format!("{} {}", v.code.as_str(), name(v.cell).unwrap_or_default());
    for extra in [name(v.path), name(v.param)].into_iter().flatten() {
        line.push(' ');
        line.push_str(extra);
    }
    line.push_str(": ");
    line.push_str(&v.detail);
    line
}

/// The core dialects (D22): CLI word, dialect, netlist file extension.
pub const DIALECTS: [(&str, Dialect, &str); 6] = [
    ("ngspice", Dialect::Ngspice, "spice"),
    ("espice", Dialect::Espice, "sp"),
    ("xyce", Dialect::Xyce, "cir"),
    ("cdl", Dialect::Cdl, "cdl"),
    ("spectre", Dialect::Spectre, "scs"),
    ("verilog", Dialect::Verilog, "v"),
];

/// The dialect a CLI word names (`ngspice espice xyce cdl spectre verilog`).
pub fn dialect_of(word: &str) -> Option<Dialect> {
    DIALECTS.iter().find(|d| d.0 == word).map(|d| d.1)
}

/// The CLI word of `d`.
pub fn dialect_word(d: Dialect) -> &'static str {
    DIALECTS
        .iter()
        .find(|x| x.1 == d)
        .map_or("ngspice", |x| x.0)
}

/// Where the editor writes the netlist of cell `stem` in dialect `d`:
/// `<root>/.schemify/netlist/<stem>.<ext>` (git-ignored with the rest of `.schemify/`).
pub fn netlist_file(root: &Path, stem: &str, d: Dialect) -> PathBuf {
    assert!(
        !stem.is_empty() && !stem.contains(['/', '\\']),
        "a file stem"
    );
    let ext = DIALECTS.iter().find(|x| x.1 == d).map_or("spice", |x| x.2);
    root.join(".schemify")
        .join("netlist")
        .join(format!("{stem}.{ext}"))
}

/// The process environment for `${NAME}` expansion (netlist spec §7.1) and the design hash
/// (run spec §4), sorted by name so the hash does not depend on the OS's listing order.
pub fn env_pairs() -> Vec<(String, String)> {
    #[cfg(not(target_arch = "wasm32"))]
    let mut v: Vec<(String, String)> = std::env::vars().collect();
    #[cfg(target_arch = "wasm32")]
    let mut v: Vec<(String, String)> = Vec::new();
    v.sort_unstable();
    v
}

/// A project loaded from disk with one cell file open: the CLI's `load` spine (the editor's
/// equivalent is its session).
#[derive(Debug)]
pub struct Opened {
    /// Design data.
    pub project: Project,
    /// Directory per library row (`libs::Loaded::lib_dir`).
    pub lib_dir: Vec<Option<PathBuf>>,
    /// The opened file.
    pub top: DocHandle,
}

impl Cells for Opened {
    fn project(&self) -> &Project {
        &self.project
    }

    fn open_row(&mut self, row: u32) -> Result<(), String> {
        let row = row as usize;
        let lib = self.project.cells().lib[row];
        let rel = crate::libs::cell_relpath(&self.project, row);
        let (_, bytes, _) = crate::libs::cell_bytes(&self.lib_dir, &self.project, lib, &rel)
            .map_err(|e| format!("{rel}: {e}"))?;
        let name = self.project.libs().name[usize::from(lib)].clone();
        self.project
            .open(&name, &rel, &bytes, None)
            .map(|_| ())
            .map_err(|e| format!("{name}:{rel}: cannot open: {e:?}"))
    }
}

/// Why [`open_file`] failed: the CLI exits 2 on `Io`, 1 on `Design`.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The project or file cannot be read, or the file is outside every library.
    Io(String),
    /// The file does not parse or is refused by `doc`.
    Design(String),
}

/// Loads the project around `file` (nearest `schemify.project`) and opens `file` in the
/// library whose directory holds it (deepest wins). Returns the load notes (unfetched
/// libraries, index findings) beside the result.
///
/// # Errors
///
/// See [`OpenError`].
pub fn open_file(file: &Path, seed: u64) -> Result<(Opened, Vec<String>), OpenError> {
    use crate::libs;
    let root = libs::project_root(file);
    let loaded = libs::load(&root, seed).map_err(OpenError::Io)?;
    let mut project = loaded.project;
    let abs = libs::absolute(file);
    let mut best: Option<(usize, String, usize)> = None;
    for (row, dir) in loaded.lib_dir.iter().enumerate() {
        let Some(dir) = dir else { continue };
        let dir = libs::absolute(dir);
        if let Some(rel) = libs::rel_path(&dir, &abs) {
            let depth = dir.components().count();
            if best.as_ref().is_none_or(|b| depth > b.2) {
                best = Some((row, rel, depth));
            }
        }
    }
    let Some((row, rel, _)) = best else {
        return Err(OpenError::Io(format!(
            "outside every library of the project at {}",
            root.display()
        )));
    };
    let bytes = crate::platform::read(file).map_err(OpenError::Io)?;
    let name = project.libs().name[row].clone();
    let top = project
        .open(&name, &rel, &bytes, None)
        .map_err(|e| OpenError::Design(format!("cannot open: {e:?}")))?
        .handle;
    assert!(project.doc(top).is_some());
    let opened = Opened {
        project,
        lib_dir: loaded.lib_dir,
        top,
    };
    Ok((opened, loaded.notes))
}

/// The simulator dialect the user config selects (`schemify.opt.simulator`, config spec
/// §3): what Simulate and `schemify run` netlist for unless told otherwise.
pub fn sim_dialect(user: &schemify_config::Table) -> Dialect {
    match user.opts.simulator {
        schemify_config::SimKind::Espice => Dialect::Espice,
        _ => Dialect::Ngspice,
    }
}

/// The external programs of run spec §5 from the user config's `schemify.sim` rows; the
/// defaults for rows it does not set (this fixes "no user-config reader": documents never
/// name a program, D7). A row without `args` keeps the default arguments.
#[cfg(not(target_arch = "wasm32"))]
pub fn tools(user: &schemify_config::Table) -> schemify_run::Tools {
    use schemify_config::SimKind;
    let mut t = schemify_run::Tools::default();
    let command =
        |s: &schemify_config::Sim, default: schemify_run::SimCommand| schemify_run::SimCommand {
            path: s.path.clone(),
            args: if s.args.is_empty() {
                default.args
            } else {
                s.args.clone()
            },
            env: s.env.clone(),
        };
    if let Some(s) = user.sim(SimKind::Ngspice) {
        t.sim = command(s, schemify_run::SimCommand::ngspice());
    }
    if let Some(s) = user.sim(SimKind::Espice) {
        t.espice = command(s, schemify_run::SimCommand::espice());
    }
    if let Some(s) = user.sim(SimKind::Openvaf) {
        t.openvaf.clone_from(&s.path);
    }
    if let Some(s) = user.sim(SimKind::Iverilog) {
        t.iverilog.clone_from(&s.path);
    }
    assert!(!t.sim.path.is_empty() && !t.espice.path.is_empty());
    t
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{BENCH, Run, start_run};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use schemify_doc::Digest;
    use schemify_run::{Jobs, Plan, Results, Runner, RunnerConfig, Select};

    /// One run in flight or finished: the `run` spine's tables (owner: `run`) plus the
    /// runner. The CLI submits every job and waits; the editor submits a few per frame
    /// ([`Run::submit_next`]) and polls.
    pub struct Run {
        /// The bench's plan.
        pub plan: Plan,
        /// Jobs (owner: `run`).
        pub jobs: Jobs,
        /// Results (owner: `run`).
        pub results: Results,
        /// The native runner (its children die with it).
        pub runner: Runner,
        /// The design hash the run was gated and tagged with.
        pub digest: Digest,
        /// The next job index not yet prepared and submitted.
        pub next: u32,
        /// Jobs the last poll finished (cleared by the caller).
        pub finished: Vec<u32>,
        /// Lines every job's ngspice netlist gets before its `.end` (the editor's
        /// `.save`/`.options` for OP annotation and cross-probe, run spec §7a); empty: none.
        pub extra: String,
        /// The dialect every job is netlisted in ([`Dialect::Ngspice`] or
        /// [`Dialect::Espice`]); the design hash covers it.
        pub dialect: Dialect,
    }

    impl std::fmt::Debug for Run {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Run")
                .field("jobs", &self.jobs.len())
                .field("next", &self.next)
                .finish()
        }
    }

    /// The run spine up to submission (run spec §1–§4): opens every cell each selected
    /// config reaches, reads the plan, expands `select` (`None`: no jobs, for an optimizer
    /// or plugin requests that push their own), gates and hashes the design for `dialect`,
    /// and creates the results and the runner (`cfg` builds its configuration from the
    /// digest; an espice run starts `Tools::espice`). Jobs are not prepared yet.
    ///
    /// Cost: one [`elaborate_all`] per selected config (at least one), then the gate's.
    ///
    /// # Errors
    ///
    /// A cell cannot be opened, the plan or selection is refused, or the gate blocks (one
    /// message, blocking violations joined by `; `).
    ///
    /// # Panics
    ///
    /// Panics if `dialect` is neither ngspice nor espice (the simulators the runner starts).
    pub fn start_run<C: Cells, F: Funcs>(
        c: &mut C,
        top: DocHandle,
        select: Option<&Select<'_>>,
        env: &[(&str, &str)],
        dialect: Dialect,
        funcs: &mut F,
        cfg: impl FnOnce(&Digest) -> RunnerConfig,
    ) -> Result<Run, String> {
        assert!(matches!(dialect, Dialect::Ngspice | Dialect::Espice));
        // Open every cell each selected config reaches (configs select different impls,
        // so different cells); no selection or no config: the bench default. Bounded by
        // the selection, which names each bench config at most once per entry.
        let configs: Vec<Option<&str>> = match select.map(|s| s.configs) {
            Some(cs) if !cs.is_empty() => cs.iter().map(|c| Some(*c)).collect(),
            _ => vec![None],
        };
        for config in configs {
            let probe = Request {
                config,
                corner: None,
                overrides: &[],
                env,
                dialect,
            };
            elaborate_all(c, top, &probe, funcs)?;
        }
        let project = c.project();
        let bench = project.doc(top).expect("checked by elaborate_all");
        let plan = Plan::read(bench).map_err(|e| format!("bench: {e:?}"))?;
        let jobs = match select {
            Some(s) => schemify_run::expand(&plan, s).map_err(|e| format!("bench: {e:?}"))?,
            None => Jobs::new(),
        };
        let digest = schemify_run::design(project, top, &jobs, env, dialect, funcs).map_err(
            |b| match b {
                schemify_run::Blocked::NeedsOpen(rows) => format!("cells not open: rows {rows:?}"),
                schemify_run::Blocked::Violations(v) => format!("run blocked: {}", v.join("; ")),
            },
        )?;
        let results = Results::new(&plan, digest);
        let mut config = cfg(&digest);
        if dialect == Dialect::Espice {
            config.tools.sim = config.tools.espice.clone();
        }
        let runner = Runner::new(config);
        let run = Run {
            plan,
            jobs,
            results,
            runner,
            digest,
            next: 0,
            finished: Vec::new(),
            extra: String::new(),
            dialect,
        };
        assert!(run.next <= run.jobs.len());
        Ok(run)
    }

    impl Run {
        /// Prepares (elaborate with the job's assignments → emit) and submits at most `max`
        /// jobs from [`Run::next`]; a job that cannot be netlisted fails and is reported in
        /// [`Run::finished`]. Returns how many jobs were handled.
        ///
        /// Cost: one elaboration and one emission per job.
        pub fn submit_next<F: Funcs>(
            &mut self,
            project: &Project,
            top: DocHandle,
            env: &[(&str, &str)],
            funcs: &mut F,
            max: u32,
        ) -> u32 {
            let start = self.next;
            while self.next < self.jobs.len() && self.next - start < max {
                let j = self.next;
                self.next += 1;
                if self.jobs.state(j) != schemify_run::State::Pending {
                    continue; // cancelled before it was prepared
                }
                let prepared = schemify_run::prepare(
                    project,
                    top,
                    &mut self.jobs,
                    j,
                    env,
                    self.dialect,
                    funcs,
                    &mut self.results,
                );
                match prepared {
                    Some(mut p) => {
                        if !self.extra.is_empty() && p.netlist.ends_with(".end\n") {
                            let at = p.netlist.len() - ".end\n".len();
                            p.netlist.insert_str(at, &self.extra);
                        }
                        self.runner.submit(&mut self.jobs, &mut self.results, j, &p);
                    }
                    None => self.finished.push(j),
                }
                if self.jobs.state(j) == schemify_run::State::Failed && !self.finished.contains(&j)
                {
                    self.finished.push(j); // the runner could not write its directory
                }
            }
            assert!(self.next <= self.jobs.len());
            self.next - start
        }

        /// Advances the runner without blocking (`Runner::poll`), appending finished jobs to
        /// [`Run::finished`].
        pub fn poll(&mut self) {
            self.runner.poll(
                &self.plan,
                &mut self.jobs,
                &mut self.results,
                &mut self.finished,
            );
        }

        /// Whether every job is submitted and none is queued or running.
        pub fn idle(&self) -> bool {
            self.next >= self.jobs.len() && !self.runner.busy()
        }

        /// Cancels every queued and running job and every job not yet submitted.
        pub fn cancel(&mut self) {
            self.runner.cancel_all(&mut self.jobs);
            for j in self.next..self.jobs.len() {
                self.jobs.cancel_pending(j);
            }
            self.next = self.jobs.len();
            assert!(self.idle());
        }

        /// Writes the results file and, beside it, `bench` (the bench's display name, so
        /// the editor finds a bench's history among the run directories). Returns the run
        /// directory.
        ///
        /// # Errors
        ///
        /// The file system's, as text.
        pub fn save(&self, bench: &str) -> Result<PathBuf, String> {
            let path = self
                .runner
                .save(&self.jobs, &self.results)
                .map_err(|e| e.to_string())?;
            let dir = path
                .parent()
                .expect("results live in the run dir")
                .to_path_buf();
            std::fs::write(dir.join(BENCH), format!("{bench}\n")).map_err(|e| e.to_string())?;
            Ok(dir)
        }
    }

    /// The sidecar beside a run's `results` naming its bench (the app's addition to run spec
    /// §5; `crates/run` neither reads nor writes it).
    pub const BENCH: &str = "bench";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_words_round_trip_and_files_land_in_dot_schemify() {
        for (w, d, ext) in DIALECTS {
            assert_eq!(dialect_of(w), Some(d));
            assert_eq!(dialect_word(d), w);
            let f = netlist_file(Path::new("/p"), "top", d);
            assert_eq!(
                f,
                Path::new("/p/.schemify/netlist").join(format!("top.{ext}"))
            );
        }
        assert_eq!(dialect_of("hspice"), None);
        assert_eq!(dialect_of(""), None);
    }
}
