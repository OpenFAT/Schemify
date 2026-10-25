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
pub const DIALECTS: [(&str, Dialect, &str); 8] = [
    ("ngspice", Dialect::Ngspice, "spice"),
    ("espice", Dialect::Espice, "sp"),
    ("xyce", Dialect::Xyce, "cir"),
    ("ltspice", Dialect::Ltspice, "net"),
    ("hspice", Dialect::Hspice, "hsp"),
    ("cdl", Dialect::Cdl, "cdl"),
    ("spectre", Dialect::Spectre, "scs"),
    ("verilog", Dialect::Verilog, "v"),
];

/// The dialect a CLI word names (`ngspice espice xyce ltspice hspice cdl spectre verilog`).
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

/// The name a run's `bench` sidecar records for the bench at `path`: `lib:dir/stem` for the
/// library cell whose file is `path` (`stem` alone in the unnamed library), `None` when no
/// cell row's file is `path` (relative paths are taken from the working directory).
/// `lib_dir` gives a library row's directory. One function for
/// the editor and `schemify run`, so a run either one saved shows in the other's history.
pub fn bench_name<'a>(
    project: &Project,
    lib_dir: impl Fn(u8) -> Option<&'a Path>,
    path: &Path,
) -> Option<String> {
    // Library directories hang off the absolute project root (`libs::project_root`).
    let path = crate::libs::absolute(path);
    let cells = project.cells();
    let (row, rel) = (0..cells.lib.len()).find_map(|r| {
        let dir = lib_dir(cells.lib[r])?;
        let rel = crate::libs::cell_relpath(project, r);
        (dir.join(&rel) == path).then_some((r, rel))
    })?;
    let lib = &project.libs().name[usize::from(cells.lib[row])];
    let stem = rel.rsplit_once('.').map_or(rel.as_str(), |(s, _)| s);
    debug_assert!(!stem.is_empty());
    Some(if lib.is_empty() {
        stem.to_owned()
    } else {
        format!("{lib}:{stem}")
    })
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
    let abs = libs::absolute(file);
    let loaded = libs::load(&root, seed, Some(&abs)).map_err(OpenError::Io)?;
    let mut project = loaded.project;
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
    // The index read the file already (a cell file of the library); else read it now.
    let name = project.libs().name[row].clone();
    let opened = match loaded.kept {
        Some((_, bytes)) => project.open_indexed(&name, &rel, bytes, None),
        None => {
            let bytes = crate::platform::read(file).map_err(OpenError::Io)?;
            project.open(&name, &rel, &bytes, None)
        }
    };
    let top = opened
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

/// The simulators Schemify starts (run spec §5), in detection order.
pub use schemify_run::SIMULATORS;

/// The user config's program row of simulator `d` (one of [`SIMULATORS`]).
#[cfg(not(target_arch = "wasm32"))]
fn kind(d: Dialect) -> schemify_config::SimKind {
    use schemify_config::SimKind;
    // By word, not by position: `SIMULATORS` is in detection order, the config's rows in
    // its own.
    assert!(SIMULATORS.contains(&d), "{} is not a simulator", d.as_str());
    let k = SimKind::SIMULATORS
        .into_iter()
        .find(|k| k.word() == d.as_str())
        .unwrap_or_else(|| panic!("the config has no row for {}", d.as_str()));
    assert_eq!(k.word(), d.as_str());
    k
}

/// The dialect a request netlists and simulates for (run spec §5): `explicit`
/// (`--dialect`, the Netlist dialect menu), else the simulator the PDK pins (its tech
/// map's `simulator`, pdk spec §6), else the user config's `schemify.opt.simulator`, else
/// the first of [`SIMULATORS`] installed, else ESPice (the default simulator, user decision
/// 2026-10-25).
///
/// # Errors
///
/// The tech map pins a word that names no simulator.
pub fn sim_dialect(
    explicit: Option<Dialect>,
    project: &Project,
    user: &schemify_config::Table,
) -> Result<Dialect, String> {
    if let Some(d) = explicit {
        return Ok(d);
    }
    if let Some((_, d)) = pin(project)? {
        return Ok(d);
    }
    Ok(match user.opts.simulator {
        Some(k) => SIMULATORS
            .into_iter()
            .find(|d| d.as_str() == k.word())
            .unwrap_or(Dialect::Espice),
        None => detect(user).unwrap_or(Dialect::Espice),
    })
}

/// The simulator the project's PDK pins: (PDK name, dialect); `None` without a tech map
/// or without its `simulator`.
///
/// # Errors
///
/// The pinned word names neither one of [`SIMULATORS`] nor `hspice`.
pub fn pin(project: &Project) -> Result<Option<(&str, Dialect)>, String> {
    let Some(map) = project.techmap() else {
        return Ok(None);
    };
    let Some(word) = map.simulator.as_deref() else {
        return Ok(None);
    };
    match dialect_of(word) {
        Some(d) if SIMULATORS.contains(&d) || d == Dialect::Hspice => {
            Ok(Some((map.pdk.as_str(), d)))
        }
        _ => Err(format!(
            "{} pins simulator `{word}`, which is not a simulator \
             (ngspice espice xyce ltspice spectre hspice)",
            map.pdk
        )),
    }
}

/// The simulator a run netlisted for `chosen` starts (run spec §5): `chosen` when it is
/// one of [`SIMULATORS`]; any other dialect runs ngspice unless the PDK pins a
/// simulator, which never falls back.
///
/// # Errors
///
/// The PDK pins a simulator Schemify cannot start (HSPICE), or (native) one whose
/// configured or default program is not found.
pub fn run_dialect(
    chosen: Dialect,
    project: &Project,
    user: &schemify_config::Table,
) -> Result<Dialect, String> {
    let pin = pin(project)?;
    let run = match (chosen, pin) {
        (d, _) if SIMULATORS.contains(&d) => d,
        (_, None) => Dialect::Ngspice,
        (d, Some((pdk, _))) => {
            return Err(format!(
                "{pdk} pins simulator {}, which Schemify cannot start \
                 (ngspice espice xyce ltspice spectre)",
                d.as_str()
            ));
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((pdk, p)) = pin
        && p == run
        && !installed(run, user)
    {
        return Err(format!(
            "{pdk} pins simulator {}, which is not installed (name its program with schemify.sim)",
            run.as_str()
        ));
    }
    #[cfg(target_arch = "wasm32")]
    let _ = user;
    assert!(SIMULATORS.contains(&run));
    Ok(run)
}

/// The first of [`SIMULATORS`] whose program (the config row's `path`, else the default)
/// is found; `None` on the web, which starts no simulator.
pub fn detect(user: &schemify_config::Table) -> Option<Dialect> {
    #[cfg(not(target_arch = "wasm32"))]
    return SIMULATORS.into_iter().find(|d| installed(*d, user));
    #[cfg(target_arch = "wasm32")]
    {
        let _ = user;
        None
    }
}

/// Whether simulator `d`'s program exists: a path with a directory part as a file, a bare
/// name in a `PATH` directory (with `.exe` on Windows). Runs no program.
#[cfg(not(target_arch = "wasm32"))]
fn installed(d: Dialect, user: &schemify_config::Table) -> bool {
    let prog = tools(user).sim(d).path.clone();
    let p = Path::new(&prog);
    if p.components().count() > 1 {
        return p.is_file();
    }
    let exe = if cfg!(windows) {
        format!("{prog}.exe")
    } else {
        prog
    };
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(&exe).is_file()))
}

/// The external programs of run spec §5 from the user config's `schemify.sim` rows; the
/// defaults for rows it does not set (this fixes "no user-config reader": documents never
/// name a program, D7). A row without `args` keeps the default arguments.
#[cfg(not(target_arch = "wasm32"))]
pub fn tools(user: &schemify_config::Table) -> schemify_run::Tools {
    use schemify_config::SimKind;
    let mut t = schemify_run::Tools::default();
    for (k, d) in SIMULATORS.into_iter().enumerate() {
        if let Some(s) = user.sim(kind(d)) {
            let default =
                std::mem::replace(&mut t.sims[k], schemify_run::SimCommand::default_for(d));
            t.sims[k] = schemify_run::SimCommand {
                path: s.path.clone(),
                args: if s.args.is_empty() {
                    default.args
                } else {
                    s.args.clone()
                },
                env: s.env.clone(),
            };
        }
    }
    if let Some(s) = user.sim(SimKind::Openvaf) {
        t.openvaf.clone_from(&s.path);
    }
    if let Some(s) = user.sim(SimKind::Iverilog) {
        t.iverilog.clone_from(&s.path);
    }
    assert!(t.sims.iter().all(|s| !s.path.is_empty()));
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
    /// Panics if `dialect` is not one of [`SIMULATORS`] (the simulators the runner starts).
    pub fn start_run<C: Cells, F: Funcs>(
        c: &mut C,
        top: DocHandle,
        select: Option<&Select<'_>>,
        env: &[(&str, &str)],
        dialect: Dialect,
        funcs: &mut F,
        cfg: impl FnOnce(&Digest) -> RunnerConfig,
    ) -> Result<Run, String> {
        assert!(SIMULATORS.contains(&dialect));
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
        config.dialect = dialect;
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
                        // Probe lines are ngspice's (`.save @m.x1.mm2[id]`).
                        let ngspice = matches!(self.dialect, Dialect::Ngspice | Dialect::Espice);
                        if !self.extra.is_empty() && ngspice && p.netlist.ends_with(".end\n") {
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
    /// §5; `crates/model/run` neither reads nor writes it).
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
        assert_eq!(dialect_of("vacask"), None);
        assert_eq!(dialect_of(""), None);
        // The simulators are netlist dialects; each dialect has its own netlist file.
        assert!(
            SIMULATORS
                .iter()
                .all(|s| DIALECTS.iter().any(|d| d.1 == *s))
        );
        let exts: std::collections::BTreeSet<&str> = DIALECTS.iter().map(|d| d.2).collect();
        assert_eq!(exts.len(), DIALECTS.len(), "one netlist file per dialect");
    }

    /// ESPice is detected first and is the last fallback (user decision 2026-10-25); every
    /// simulator still finds its own config row, by word, whatever the detection order.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn espice_leads_and_every_simulator_has_its_config_row() {
        assert_eq!(SIMULATORS[0], Dialect::Espice, "espice is detected first");
        for d in SIMULATORS {
            assert_eq!(
                kind(d).word(),
                d.as_str(),
                "{} maps to its own row",
                d.as_str()
            );
        }
        let user = schemify_config::Table::default();
        let t = tools(&user);
        for d in SIMULATORS {
            assert_eq!(t.sim(d).path, schemify_run::SimCommand::default_for(d).path);
        }
    }
}
