//! The native subprocess runner (D15, run spec §5): [`Prepared`] jobs in, one directory
//! per job under the run directory, simulators spawned from the user config's command
//! line (D7: never from a document), at most `workers` at a time, raw files out into
//! [`Results::record`].
//!
//! No threads and no callbacks: children write `stdout.log`/`stderr.log` in their job
//! directory; [`Runner::poll`] reaps exited children, kills those past the timeout, tails
//! stdout for progress and starts queued jobs. A job holds its worker slot while its
//! sources compile (the build cache, `cache.rs`).
//!
//! Contents:
//! - `pub const NETLIST`, `RAW`, `STDOUT`, `STDERR`, `COMMAND`, `RESULTS: &str`,
//!   `pub const WORKERS_MAX: u16`
//! - `pub struct SimCommand` (`default_for`), `pub struct Tools` (`set_sims`, `sim`),
//!   `pub struct RunnerConfig` (`new`)
//! - `pub struct Runner`: `new`, `config`, `job_dir`, `submit`, `busy`, `progress`,
//!   `poll`, `wait`, `cancel`, `cancel_all`, `save`

use crate::SIMULATORS;
use crate::cache::Cache;
use crate::jobs::{Jobs, State};
use crate::plan::Plan;
use crate::prepare::{Prepared, Source};
use crate::process::{self, Process};
use crate::results::Results;
use schemify_doc::Digest;
use schemify_elab::Dialect;
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, SeekFrom};
use std::iter;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::Duration;

/// Netlist file name inside a job directory.
pub const NETLIST: &str = "netlist.cir";
/// Raw file name inside a job directory.
pub const RAW: &str = "out.raw";
/// Simulator stdout (tailed for progress).
pub const STDOUT: &str = "stdout.log";
/// Simulator stderr.
pub const STDERR: &str = "stderr.log";
/// The simulator command line the job ran, one line (for the run log view).
pub const COMMAND: &str = "command.log";
/// Results file name inside a run directory.
pub const RESULTS: &str = "results";
/// Most workers one runner starts at once.
pub const WORKERS_MAX: u16 = 1024;

/// A simulator command line from the user config (`sim <name>` with `path`, `args`,
/// `env`; config spec §3). `{netlist}` and `{raw}` in `args` become the job's file names;
/// the working directory is the job directory; the process environment is inherited with
/// `env` set on top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimCommand {
    /// Program, looked up on `PATH` when it has no directory part.
    pub path: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Variables set for the simulator (ESPice's `ZIG`, the Zig its `.hdl` compiles with).
    pub env: Vec<(String, String)>,
}

/// External programs a runner may start, all from the user config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    /// Each simulator's command line, in [`SIMULATORS`] order; a runner starts the one
    /// of its [`RunnerConfig::dialect`].
    pub sims: [SimCommand; SIMULATORS.len()],
    /// OpenVAF (`<src> -o <out>`).
    pub openvaf: String,
    /// Icarus Verilog (`-o <out> <src>`).
    pub iverilog: String,
}

/// Where and how a runner works.
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    /// Programs.
    pub tools: Tools,
    /// The simulator every job runs in (one of [`SIMULATORS`]; its netlists are in this
    /// dialect).
    pub dialect: Dialect,
    /// Run directory: job `j` runs in `<dir>/<j>`.
    pub dir: PathBuf,
    /// Compiled-source cache shared by runs.
    pub cache: PathBuf,
    /// On-disk directory of each library row (the `lib` of `Origin::File` and
    /// [`Prepared::base`]); row 0 is the project directory.
    pub lib_dirs: Vec<PathBuf>,
    /// Simulators running at once, `1..=WORKERS_MAX`.
    pub workers: u16,
    /// Wall-clock limit per job and per compile; the child is killed after it.
    pub timeout: Duration,
}

/// Jobs waiting for and holding worker slots, and the compilers they wait on. Dropping a
/// runner kills its children.
pub struct Runner {
    config: RunnerConfig,
    queue: VecDeque<Queued>,
    /// Jobs holding a worker slot while their sources compile.
    waiting: Vec<Queued>,
    running: Vec<Running>,
    cache: Cache,
}

/// A job with its netlist written: queued for a worker slot, then holding one while its
/// sources compile.
struct Queued {
    job: u32,
    sources: Vec<Source>,
}

struct Running {
    job: u32,
    process: Process,
    /// Bytes of stdout already scanned.
    log_pos: u64,
    /// Last non-empty stdout line.
    last: String,
}

impl SimCommand {
    /// Returns the default command line of simulator `d` (run spec §5): batch mode, the
    /// job's netlist in, a raw file out the reader loads.
    ///
    /// - ngspice `ngspice -b -r {raw} {netlist}` (D15);
    /// - ESPice `espice -b --tokenizer=ngspice -r {raw} {netlist}` (the ngspice deck
    ///   syntax the espice dialect writes; ESPice's default `--format` is ngspice binary);
    /// - Xyce `Xyce -r {raw} {netlist}` (a binary raw file);
    /// - LTspice `ltspice -b {netlist}` (it writes `netlist.raw` beside the deck);
    /// - Spectre `spectre -format nutbin -raw {raw} {netlist}` (Nutmeg binary, not PSF).
    ///
    /// # Panics
    ///
    /// Panics if `d` is not one of [`SIMULATORS`].
    #[must_use]
    pub fn default_for(d: Dialect) -> SimCommand {
        assert!(SIMULATORS.contains(&d), "{} is not a simulator", d.as_str());
        let (path, args): (&str, &[&str]) = match d {
            Dialect::Ngspice => ("ngspice", &["-b", "-r", "{raw}", "{netlist}"]),
            Dialect::Espice => (
                "espice",
                &["-b", "--tokenizer=ngspice", "-r", "{raw}", "{netlist}"],
            ),
            Dialect::Xyce => ("Xyce", &["-r", "{raw}", "{netlist}"]),
            Dialect::Ltspice => ("ltspice", &["-b", "{netlist}"]),
            Dialect::Spectre => (
                "spectre",
                &["-format", "nutbin", "-raw", "{raw}", "{netlist}"],
            ),
            _ => unreachable!("checked above"),
        };
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        assert!(
            args.iter().any(|a| a == "{netlist}"),
            "the job's netlist goes in"
        );
        SimCommand {
            path: path.to_owned(),
            args,
            env: Vec::new(),
        }
    }
}

impl Tools {
    /// Gives every simulator the command line `cmd` (a test harness's fake simulator, the
    /// editor's `--sim`).
    pub fn set_sims(&mut self, cmd: &SimCommand) {
        for s in &mut self.sims {
            s.clone_from(cmd);
        }
    }

    /// Returns the command line of simulator `d`.
    ///
    /// # Panics
    ///
    /// Panics if `d` is not one of [`SIMULATORS`].
    #[must_use]
    pub fn sim(&self, d: Dialect) -> &SimCommand {
        let Some(k) = SIMULATORS.iter().position(|s| *s == d) else {
            panic!("{} is not a simulator", d.as_str());
        };
        &self.sims[k]
    }
}

impl Default for Tools {
    fn default() -> Tools {
        Tools {
            sims: SIMULATORS.map(SimCommand::default_for),
            openvaf: "openvaf".to_owned(),
            iverilog: "iverilog".to_owned(),
        }
    }
}

impl RunnerConfig {
    /// Returns the defaults for a project at `root`: run directory
    /// `.schemify/runs/<first 16 hex digits of design>`, cache `.schemify/build`, the
    /// project directory as library row 0, ngspice, one worker per available core, one
    /// hour per job.
    #[must_use]
    pub fn new(root: &Path, design: &Digest) -> RunnerConfig {
        let hex = schemify_doc::hex(design);
        debug_assert_eq!(hex.len(), 64, "two hex digits per byte");
        let cores = thread::available_parallelism().map_or(1, |n| n.get());
        let c = RunnerConfig {
            tools: Tools::default(),
            dialect: Dialect::Ngspice,
            dir: root.join(".schemify/runs").join(&hex[..16]),
            cache: root.join(".schemify/build"),
            lib_dirs: vec![root.to_path_buf()],
            workers: cores.clamp(1, usize::from(WORKERS_MAX)) as u16,
            timeout: Duration::from_secs(3600),
        };
        // Provable from the clamp: a postcondition, not a documented panic.
        debug_assert!((1..=WORKERS_MAX).contains(&c.workers));
        c
    }
}

impl Runner {
    /// Creates an idle runner.
    ///
    /// # Panics
    ///
    /// Panics if `config.workers` is outside `1..=WORKERS_MAX` or `config.dir` is empty.
    #[must_use]
    pub fn new(config: RunnerConfig) -> Runner {
        assert!(
            (1..=WORKERS_MAX).contains(&config.workers),
            "workers {}",
            config.workers
        );
        assert!(!config.dir.as_os_str().is_empty());
        Runner {
            config,
            queue: VecDeque::new(),
            waiting: Vec::new(),
            running: Vec::new(),
            cache: Cache::default(),
        }
    }

    /// Returns the configuration.
    #[must_use]
    pub fn config(&self) -> &RunnerConfig {
        &self.config
    }

    /// Returns job `j`'s directory, `<dir>/<j>`.
    #[must_use]
    pub fn job_dir(&self, j: u32) -> PathBuf {
        self.config.dir.join(j.to_string())
    }

    /// Writes job `j`'s netlist into its directory (removing an old raw file; relative
    /// corner paths made absolute against the bench's directory, or the project root for
    /// a path from a project variable, [`Prepared::resolved`]) and queues it. A write
    /// failure fails the job.
    ///
    /// # Panics
    ///
    /// Panics if job `j` is not pending (submitted once), or `p` holds no netlist.
    pub fn submit(&mut self, jobs: &mut Jobs, results: &mut Results, j: u32, p: &Prepared) {
        assert_eq!(jobs.state(j), State::Pending, "submit job {j} once");
        assert!(
            !p.netlist.is_empty(),
            "`prepare` never emits an empty netlist"
        );
        assert!(self.queue.len() < crate::JOBS_MAX as usize);
        let dir = self.job_dir(j);
        let text = match self.config.lib_dirs.get(usize::from(p.base.0)) {
            Some(lib) if !p.relative.is_empty() => {
                p.resolved(&lib.join(&p.base.1), &self.config.lib_dirs[0])
            }
            _ => p.netlist.clone(),
        };
        let written = fs::create_dir_all(&dir).and_then(|()| {
            match fs::remove_file(dir.join(RAW)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            fs::write(dir.join(NETLIST), &text)
        });
        if let Err(e) = written {
            return results.fail(jobs, j, format!("cannot write {}: {e}", dir.display()));
        }
        self.queue.push_back(Queued {
            job: j,
            sources: p.sources.clone(),
        });
        jobs.set_state(j, State::Queued);
    }

    /// Returns whether any job is queued, compiling or running, or a compiler runs.
    #[must_use]
    pub fn busy(&self) -> bool {
        !self.queue.is_empty()
            || !self.waiting.is_empty()
            || !self.running.is_empty()
            || self.cache.busy()
    }

    /// Iterates `(job, progress)` of the jobs holding a worker slot: the last stdout line
    /// of a running simulator, or that the job waits for its sources to compile.
    pub fn progress(&self) -> impl Iterator<Item = (u32, &str)> + '_ {
        let running = self.running.iter().map(|r| (r.job, r.last.as_str()));
        running.chain(self.waiting.iter().map(|w| (w.job, "compiling sources")))
    }

    /// Advances every job one step without blocking: reaps exited compilers and
    /// simulators (a simulator's raw file goes through [`Results::record`]), kills those
    /// past the timeout, tails stdout, admits queued jobs into free worker slots (starting
    /// the compiles their sources need: one compiler per stale cache output, shared by
    /// every job that needs it) and starts the simulator of every job whose sources are
    /// ready. Appends to `finished` every job that reached `Done` or `Failed` in this call.
    ///
    /// Cost: one `try_wait` per child and a stat of each source of an admitted job;
    /// compilers and simulators are child processes, so nothing here waits for them.
    ///
    /// # Panics
    ///
    /// Panics if `jobs` is not the table the jobs were submitted from (a job this runner
    /// holds is missing or changed state behind it).
    pub fn poll(
        &mut self,
        plan: &Plan,
        jobs: &mut Jobs,
        results: &mut Results,
        finished: &mut Vec<u32>,
    ) {
        let before = finished.len();
        self.cache.reap(&self.config);
        let mut i = 0;
        while i < self.running.len() {
            let dir = self.job_dir(self.running[i].job);
            let r = &mut self.running[i];
            tail(&dir, r);
            let Some(outcome) = r.process.reap(self.config.timeout, "simulator") else {
                i += 1;
                continue;
            };
            let r = self.running.remove(i);
            self.finish(plan, jobs, results, r.job, outcome);
            finished.push(r.job);
        }
        // Each pass admits at least one queued job, or ends: a start that fails frees its
        // slot for the next queued job in the same poll.
        let workers = usize::from(self.config.workers);
        let passes_max = self.queue.len() + 1;
        let mut passes = 0;
        loop {
            passes += 1;
            assert!(
                passes <= passes_max,
                "each pass admits a queued job or ends"
            );
            while self.waiting.len() + self.running.len() < workers {
                let Some(q) = self.queue.pop_front() else {
                    break;
                };
                for s in &q.sources {
                    self.cache.ensure(&self.config, s);
                }
                self.waiting.push(q);
            }
            self.start_ready(jobs, results, finished);
            if self.queue.is_empty() || self.waiting.len() + self.running.len() >= workers {
                break;
            }
        }
        assert!(self.running.len() + self.waiting.len() <= workers);
        assert!(finished[before..].iter().all(|&j| jobs.state(j).finished()));
    }

    /// Polls until no job is queued, compiling or running (the CLI and tests; the app polls
    /// from its frame spine instead). Terminates: every child is killed at the timeout.
    ///
    /// # Panics
    ///
    /// As [`Runner::poll`].
    pub fn wait(
        &mut self,
        plan: &Plan,
        jobs: &mut Jobs,
        results: &mut Results,
        finished: &mut Vec<u32>,
    ) {
        // ponytail: 5 ms sleep-polling; a blocking wait on the oldest child (or a
        // pidfd/kqueue wakeup) when idle CPU during long sims matters.
        loop {
            self.poll(plan, jobs, results, finished);
            if !self.busy() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert!(self.queue.is_empty());
        assert!(self.running.is_empty());
        assert!(self.waiting.is_empty());
    }

    /// Cancels job `j` if it is queued, compiling or running (a running simulator is
    /// killed; a compiler other jobs may share keeps running); any other job is left as
    /// it is.
    ///
    /// # Panics
    ///
    /// Panics if `j` is held by this runner but finished in `jobs` (another table).
    pub fn cancel(&mut self, jobs: &mut Jobs, j: u32) {
        let found = if let Some(i) = self.queue.iter().position(|q| q.job == j) {
            self.queue.remove(i).is_some()
        } else if let Some(i) = self.waiting.iter().position(|w| w.job == j) {
            self.waiting.remove(i);
            true
        } else if let Some(i) = self.running.iter().position(|r| r.job == j) {
            self.running.remove(i); // dropping the Process kills the simulator
            true
        } else {
            false
        };
        if found {
            jobs.set_state(j, State::Cancelled);
        }
        assert!(!self.queue.iter().chain(&self.waiting).any(|q| q.job == j));
        assert!(!self.running.iter().any(|r| r.job == j));
    }

    /// Cancels every queued, compiling and running job and kills every compiler.
    ///
    /// # Panics
    ///
    /// As [`Runner::cancel`].
    pub fn cancel_all(&mut self, jobs: &mut Jobs) {
        let queued = self.queue.iter().chain(&self.waiting).map(|q| q.job);
        let all: Vec<u32> = queued.chain(self.running.iter().map(|r| r.job)).collect();
        for j in all {
            self.cancel(jobs, j);
        }
        self.cache.kill_all();
        assert!(!self.busy());
    }

    /// Writes the results file (`<dir>/results`, [`crate::write_results`]) atomically
    /// (a temporary file renamed over it) and returns its path.
    ///
    /// # Errors
    ///
    /// The file system's.
    pub fn save(&self, jobs: &Jobs, results: &Results) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.config.dir)?;
        let path = self.config.dir.join(RESULTS);
        let tmp = self.config.dir.join(".results.tmp");
        fs::write(&tmp, crate::write_results(jobs, results))?;
        fs::rename(&tmp, &path)?;
        Ok(path)
    }
}

// ---- private ----

impl Runner {
    /// Starts the simulator of every waiting job whose sources are compiled; fails those
    /// with a failed source.
    fn start_ready(&mut self, jobs: &mut Jobs, results: &mut Results, finished: &mut Vec<u32>) {
        let mut i = 0;
        while i < self.waiting.len() {
            let w = &self.waiting[i];
            let failed = self.cache.failure(&w.sources).map(str::to_owned);
            if failed.is_none() && self.cache.compiling(&w.sources) {
                i += 1;
                continue;
            }
            let w = self.waiting.remove(i);
            let started = match failed {
                Some(msg) => Err(msg),
                None => self.start(&w),
            };
            match started {
                Ok(()) => jobs.set_state(w.job, State::Running),
                Err(msg) => {
                    results.fail(jobs, w.job, msg);
                    finished.push(w.job);
                }
            }
        }
        assert!(
            self.waiting
                .iter()
                .all(|w| jobs.state(w.job) == State::Queued)
        );
    }

    /// Copies the compiled sources into the job directory, writes `command.log` and spawns
    /// the simulator.
    fn start(&mut self, q: &Queued) -> Result<(), String> {
        assert!(
            self.running.len() < usize::from(self.config.workers),
            "the job left its worker slot in `waiting`"
        );
        let dir = self.job_dir(q.job);
        self.cache.place(&self.config, &q.sources, &dir)?;
        let log = |name: &str| {
            File::create(dir.join(name)).map_err(|e| format!("cannot create {name}: {e}"))
        };
        let (out, err) = (log(STDOUT)?, log(STDERR)?);
        let sim = self.config.tools.sim(self.config.dialect);
        let args: Vec<String> = sim
            .args
            .iter()
            .map(|a| a.replace("{netlist}", NETLIST).replace("{raw}", RAW))
            .collect();
        let words: Vec<String> = iter::once(sim.path.clone())
            .chain(args.iter().cloned())
            .collect();
        let line = words.join(" ");
        fs::write(dir.join(COMMAND), format!("{line}\n"))
            .map_err(|e| format!("cannot create {COMMAND}: {e}"))?;
        let mut cmd = Command::new(&sim.path);
        cmd.args(args)
            .envs(sim.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&dir);
        let process = Process::spawn(&mut cmd, out, err).map_err(|e| {
            process::spawn_error(&sim.path, "simulator", "the user config `sim` entry", &e)
        })?;
        let before = self.running.len();
        self.running.push(Running {
            job: q.job,
            process,
            log_pos: 0,
            last: String::new(),
        });
        assert_eq!(self.running.len(), before + 1);
        Ok(())
    }

    /// Records the outcome of job `j`'s simulator: its raw file's results, or the failure.
    fn finish(
        &self,
        plan: &Plan,
        jobs: &mut Jobs,
        results: &mut Results,
        j: u32,
        outcome: Result<ExitStatus, String>,
    ) {
        assert_eq!(jobs.state(j), State::Running, "only a running job finishes");
        let dir = self.job_dir(j);
        let status = match outcome {
            Ok(s) => s,
            Err(msg) => return results.fail(jobs, j, msg),
        };
        let why = || process::last_line(&dir.join(STDERR)).unwrap_or_default();
        if !status.success() {
            let sim = &self.config.tools.sim(self.config.dialect).path;
            let msg = format!(
                "`{sim}` exited with {status}: {} (see {})",
                why(),
                dir.display()
            );
            return results.fail(jobs, j, msg);
        }
        match read_raw(&dir) {
            Ok(raw) => results.record(plan, jobs, j, &raw),
            Err(e) => {
                let msg = format!("no raw file ({e}): {} (see {})", why(), dir.display());
                results.fail(jobs, j, msg);
            }
        }
        assert!(jobs.state(j).finished());
    }
}

/// A job's raw data: `{raw}`, or without it (a bench whose `.control` block `write`s its
/// own, as xschem's do; LTspice's `netlist.raw`; VACASK's one file per analysis) every
/// `.raw` file the job wrote, by name, one after the other: the reader takes a file of
/// several plots. The error is `{raw}`'s when there is none.
fn read_raw(dir: &Path) -> io::Result<Vec<u8>> {
    fs::read(dir.join(RAW)).or_else(|e| {
        let mut own: Vec<PathBuf> = fs::read_dir(dir)
            .map(|d| d.flatten().map(|f| f.path()).collect())
            .unwrap_or_default();
        own.retain(|p| p.extension().is_some_and(|x| x == "raw"));
        own.sort();
        if own.is_empty() {
            return Err(e);
        }
        let mut all = Vec::new();
        for p in &own {
            all.extend(fs::read(p)?);
        }
        Ok(all)
    })
}

/// Scans new complete stdout lines (at most 64 KiB per call) and keeps the last one.
fn tail(dir: &Path, r: &mut Running) {
    use std::io::{Read, Seek};

    const CHUNK: u64 = 1 << 16;
    let Ok(mut f) = File::open(dir.join(STDOUT)) else {
        return;
    };
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(r.log_pos)).is_err() || f.take(CHUNK).read_to_end(&mut buf).is_err() {
        return;
    }
    assert!(buf.len() as u64 <= CHUNK, "read through `take`");
    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
        if buf.len() as u64 == CHUNK {
            r.log_pos += CHUNK; // one line longer than a chunk: skip it
        }
        return;
    };
    r.log_pos += end as u64 + 1;
    let lines = buf[..end].split(|&b| b == b'\n').rev();
    if let Some(line) = lines.into_iter().find(|l| !l.trim_ascii().is_empty()) {
        r.last = String::from_utf8_lossy(line.trim_ascii()).into_owned();
    }
}
