//! The native subprocess runner (D15, run spec §5): one directory per job under the
//! run directory, the simulator spawned from the user config's command line (D7: never
//! from a document), at most `workers` at a time, polled by the frame spine.
//!
//! No threads and no callbacks: children write `stdout.log`/`stderr.log` in their job
//! directory; [`Runner::poll`] reaps exited children, kills those past the timeout,
//! tails stdout for progress and starts queued jobs. Sources (OpenVAF, Icarus) compile
//! as child processes too, polled the same way: a job holds its worker slot while its
//! sources compile, one compiler per stale output serves every job that needs it, and
//! the outputs are cached by mtime in a shared build directory and copied into each job
//! directory.

use crate::jobs::{Jobs, State};
use crate::plan::Plan;
use crate::prepare::{Origin, Prepared, Source, Tool};
use crate::results::Results;
use schemify_doc::Digest;
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Netlist file name inside a job directory.
pub const NETLIST: &str = "netlist.cir";
/// Raw file name inside a job directory.
pub const RAW: &str = "out.raw";
/// Simulator stdout (tailed for progress).
pub const STDOUT: &str = "stdout.log";
/// Simulator stderr.
pub const STDERR: &str = "stderr.log";
/// Results file name inside a run directory.
pub const RESULTS: &str = "results";
/// Most workers one runner starts at once.
pub const WORKERS_MAX: u16 = 1024;

/// A simulator command line from the user config (`sim <name>` with `path`, `args`;
/// format spec §4). `{netlist}` and `{raw}` in `args` become the job's file names; the
/// working directory is the job directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimCommand {
    /// Program, looked up on `PATH` when it has no directory part.
    pub path: String,
    /// Arguments.
    pub args: Vec<String>,
}

impl SimCommand {
    /// `ngspice -b -r {raw} {netlist}` (D15: batch mode, raw file out).
    pub fn ngspice() -> SimCommand {
        SimCommand {
            path: "ngspice".into(),
            args: ["-b", "-r", "{raw}", "{netlist}"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// External programs a runner may start, all from the user config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    /// The simulator.
    pub sim: SimCommand,
    /// OpenVAF (`<src> -o <out>`).
    pub openvaf: String,
    /// Icarus Verilog (`-o <out> <src>`).
    pub iverilog: String,
}

impl Default for Tools {
    fn default() -> Tools {
        Tools {
            sim: SimCommand::ngspice(),
            openvaf: "openvaf".into(),
            iverilog: "iverilog".into(),
        }
    }
}

/// Where and how a runner works.
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    /// Programs.
    pub tools: Tools,
    /// Run directory: job `j` runs in `<dir>/<j>`.
    pub dir: PathBuf,
    /// Compiled-source cache shared by runs.
    pub cache: PathBuf,
    /// On-disk directory of each library row (the `lib` of [`Origin::File`] and [`Prepared::base`]).
    pub lib_dirs: Vec<PathBuf>,
    /// Simulators running at once, `1..=WORKERS_MAX`.
    pub workers: u16,
    /// Wall-clock limit per job; the child is killed after it.
    pub timeout: Duration,
}

impl RunnerConfig {
    /// Defaults for a project at `root`: run directory
    /// `.schemify/runs/<first 16 hex digits of design>`, cache `.schemify/build`, the
    /// project directory as library row 0, one worker per available core, one hour per
    /// job.
    pub fn new(root: &Path, design: &Digest) -> RunnerConfig {
        let hex = schemify_doc::hex(design);
        let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
        let c = RunnerConfig {
            tools: Tools::default(),
            dir: root.join(".schemify/runs").join(&hex[..16]),
            cache: root.join(".schemify/build"),
            lib_dirs: vec![root.to_path_buf()],
            workers: workers.clamp(1, usize::from(WORKERS_MAX)) as u16,
            timeout: Duration::from_secs(3600),
        };
        assert!(c.workers >= 1);
        c
    }
}

/// A job with its netlist written: queued for a worker slot, then holding one while its
/// sources compile.
struct Queued {
    job: u32,
    sources: Vec<Source>,
}

/// One compiler child writing `out` into the build cache.
struct Compiling {
    out: String,
    proc: Proc,
    /// `` `openvaf` on <src> ``, for messages.
    what: String,
}

struct Running {
    job: u32,
    proc: Proc,
    /// Bytes of stdout already scanned.
    log_pos: u64,
    /// Last non-empty stdout line.
    last: String,
}

/// Jobs waiting for and holding worker slots, and the compilers they wait on. Dropping a
/// runner kills its children.
pub struct Runner {
    cfg: RunnerConfig,
    queue: VecDeque<Queued>,
    /// Jobs holding a worker slot while their sources compile.
    waiting: Vec<Queued>,
    /// Compiler children, at most one per cache output.
    compiling: Vec<Compiling>,
    /// Cache outputs whose compile failed this run, with the message every job needing
    /// one fails with.
    failed: Vec<(String, String)>,
    running: Vec<Running>,
}

impl Runner {
    /// Creates an idle runner.
    ///
    /// # Panics
    ///
    /// Panics if `cfg.workers` is outside `1..=WORKERS_MAX`.
    pub fn new(cfg: RunnerConfig) -> Runner {
        assert!(
            (1..=WORKERS_MAX).contains(&cfg.workers),
            "workers {}",
            cfg.workers
        );
        assert!(!cfg.dir.as_os_str().is_empty());
        Runner {
            cfg,
            queue: VecDeque::new(),
            waiting: Vec::new(),
            compiling: Vec::new(),
            failed: Vec::new(),
            running: Vec::new(),
        }
    }

    /// Returns the configuration.
    pub fn config(&self) -> &RunnerConfig {
        &self.cfg
    }

    /// Returns job `j`'s directory.
    pub fn job_dir(&self, j: u32) -> PathBuf {
        self.cfg.dir.join(j.to_string())
    }

    /// Writes job `j`'s netlist into its directory (removing an old raw file; relative
    /// corner paths made absolute against the bench's directory, [`Prepared::resolved`])
    /// and queues it. A write failure fails the job.
    pub fn submit(&mut self, jobs: &mut Jobs, results: &mut Results, j: u32, p: &Prepared) {
        assert_eq!(jobs.state(j), State::Pending, "submit job {j} once");
        assert!(self.queue.len() < crate::JOBS_MAX as usize);
        let dir = self.job_dir(j);
        let base = self.cfg.lib_dirs.get(usize::from(p.base.0));
        let text = match base {
            Some(lib) if !p.relative.is_empty() => p.resolved(&lib.join(&p.base.1)),
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
    pub fn busy(&self) -> bool {
        !self.queue.is_empty()
            || !self.waiting.is_empty()
            || !self.running.is_empty()
            || !self.compiling.is_empty()
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
    pub fn poll(
        &mut self,
        plan: &Plan,
        jobs: &mut Jobs,
        results: &mut Results,
        finished: &mut Vec<u32>,
    ) {
        let before = finished.len();
        self.reap_compiles();
        let mut i = 0;
        while i < self.running.len() {
            let dir = self.job_dir(self.running[i].job);
            let r = &mut self.running[i];
            tail(&dir, r);
            let Some(outcome) = r.proc.reap(self.cfg.timeout, "simulator") else {
                i += 1;
                continue;
            };
            let r = self.running.remove(i);
            self.finish(plan, jobs, results, r.job, outcome);
            finished.push(r.job);
        }
        // Each pass admits at least one queued job, or ends: a start that fails frees its
        // slot for the next queued job in the same poll.
        let workers = usize::from(self.cfg.workers);
        loop {
            while self.waiting.len() + self.running.len() < workers {
                let Some(q) = self.queue.pop_front() else {
                    break;
                };
                for s in &q.sources {
                    self.ensure_compiled(s);
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
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(self.queue.is_empty() && self.running.is_empty() && self.waiting.is_empty());
    }

    /// Cancels job `j` if it is queued, compiling or running (a running simulator is
    /// killed; a compiler other jobs may share keeps running).
    pub fn cancel(&mut self, jobs: &mut Jobs, j: u32) {
        if let Some(i) = self.queue.iter().position(|q| q.job == j) {
            self.queue.remove(i);
            jobs.set_state(j, State::Cancelled);
        } else if let Some(i) = self.waiting.iter().position(|w| w.job == j) {
            self.waiting.remove(i);
            jobs.set_state(j, State::Cancelled);
        } else if let Some(i) = self.running.iter().position(|r| r.job == j) {
            self.running.remove(i).proc.kill();
            jobs.set_state(j, State::Cancelled);
        }
        assert!(!self.queue.iter().any(|q| q.job == j) && !self.running.iter().any(|r| r.job == j));
        assert!(!self.waiting.iter().any(|w| w.job == j));
    }

    /// Cancels every queued, compiling and running job and kills every compiler.
    pub fn cancel_all(&mut self, jobs: &mut Jobs) {
        let all: Vec<u32> = self
            .queue
            .iter()
            .chain(&self.waiting)
            .map(|q| q.job)
            .chain(self.running.iter().map(|r| r.job))
            .collect();
        for j in all {
            self.cancel(jobs, j);
        }
        for mut c in self.compiling.drain(..) {
            c.proc.kill();
        }
        assert!(!self.busy());
    }

    /// Writes the results file (`<dir>/results`, [`crate::write_results`]) atomically.
    ///
    /// # Errors
    ///
    /// The file system's.
    pub fn save(&self, jobs: &Jobs, results: &Results) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.cfg.dir)?;
        let path = self.cfg.dir.join(RESULTS);
        let tmp = self.cfg.dir.join(".results.tmp");
        fs::write(&tmp, crate::write_results(jobs, results))?;
        fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Starts the compiler of `s` unless its output is fresh, already compiling, or
    /// failed this run; a compiler that cannot start fails the output.
    fn ensure_compiled(&mut self, s: &Source) {
        if self.failed.iter().any(|(o, _)| *o == s.out)
            || self.compiling.iter().any(|c| c.out == s.out)
        {
            return;
        }
        match spawn_compile(&self.cfg, s) {
            Ok(None) => {}
            Ok(Some(c)) => self.compiling.push(c),
            Err(msg) => self.failed.push((s.out.clone(), msg)),
        }
        assert!(self.compiling.len() <= crate::JOBS_MAX as usize);
    }

    /// Reaps exited compilers; one that failed, wrote nothing or ran past the timeout
    /// fails its output.
    fn reap_compiles(&mut self) {
        let mut i = 0;
        while i < self.compiling.len() {
            let Some(outcome) = self.compiling[i].proc.reap(self.cfg.timeout, "compiler") else {
                i += 1;
                continue;
            };
            let c = self.compiling.remove(i);
            let wrote = self.cfg.cache.join(&c.out).exists();
            let why = match outcome {
                Ok(status) if status.success() && wrote => continue,
                Ok(status) => {
                    let log = self.cfg.cache.join(format!("{}.log", c.out));
                    let last = last_line(&log).unwrap_or_else(|| "no output".into());
                    format!("{status}: {last}")
                }
                Err(msg) => msg,
            };
            self.failed
                .push((c.out, format!("{} failed ({why})", c.what)));
        }
        assert!(
            self.compiling
                .iter()
                .all(|c| !self.failed.iter().any(|f| f.0 == c.out))
        );
    }

    /// Starts the simulator of every waiting job whose sources are compiled; fails those
    /// with a failed source.
    fn start_ready(&mut self, jobs: &mut Jobs, results: &mut Results, finished: &mut Vec<u32>) {
        let mut i = 0;
        while i < self.waiting.len() {
            let w = &self.waiting[i];
            let failed = w
                .sources
                .iter()
                .find_map(|s| self.failed.iter().find(|f| f.0 == s.out))
                .map(|f| f.1.clone());
            let compiling = w
                .sources
                .iter()
                .any(|s| self.compiling.iter().any(|c| c.out == s.out));
            if failed.is_none() && compiling {
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

    /// Copies the compiled sources into the job directory and spawns the simulator.
    fn start(&mut self, q: &Queued) -> Result<(), String> {
        let dir = self.job_dir(q.job);
        for s in &q.sources {
            fs::copy(self.cfg.cache.join(&s.out), dir.join(&s.out))
                .map_err(|e| format!("copy {}: {e}", s.out))?;
        }
        let log = |name: &str| {
            File::create(dir.join(name)).map_err(|e| format!("cannot create {name}: {e}"))
        };
        let (out, err) = (log(STDOUT)?, log(STDERR)?);
        let sim = &self.cfg.tools.sim;
        let args = sim
            .args
            .iter()
            .map(|a| a.replace("{netlist}", NETLIST).replace("{raw}", RAW));
        let proc = Proc::spawn(
            Command::new(&sim.path).args(args).current_dir(&dir),
            out,
            err,
        )
        .map_err(|e| spawn_error(&sim.path, "simulator", "the user config `sim` entry", &e))?;
        self.running.push(Running {
            job: q.job,
            proc,
            log_pos: 0,
            last: String::new(),
        });
        Ok(())
    }

    fn finish(
        &self,
        plan: &Plan,
        jobs: &mut Jobs,
        results: &mut Results,
        j: u32,
        outcome: Result<ExitStatus, String>,
    ) {
        let dir = self.job_dir(j);
        let status = match outcome {
            Ok(s) => s,
            Err(msg) => return results.fail(jobs, j, msg),
        };
        if !status.success() {
            let why = last_line(&dir.join(STDERR)).unwrap_or_default();
            let sim = &self.cfg.tools.sim.path;
            return results.fail(
                jobs,
                j,
                format!(
                    "`{sim}` exited with {status}: {why} (see {})",
                    dir.display()
                ),
            );
        }
        // Benches whose `.control` block `write`s its own raw file (xschem style) leave
        // the `-r` file unwritten: fall back to the raw files the job wrote, by name.
        let raw = fs::read(dir.join(RAW)).or_else(|e| {
            let mut own: Vec<_> = fs::read_dir(&dir)
                .map(|d| d.flatten().map(|f| f.path()).collect())
                .unwrap_or_default();
            own.retain(|p: &PathBuf| p.extension().is_some_and(|x| x == "raw"));
            own.sort();
            own.first().map_or(Err(e), fs::read)
        });
        match raw {
            Ok(raw) => results.record(plan, jobs, j, &raw),
            Err(e) => {
                let why = last_line(&dir.join(STDERR)).unwrap_or_default();
                results.fail(
                    jobs,
                    j,
                    format!("no raw file ({e}): {why} (see {})", dir.display()),
                );
            }
        }
        assert!(jobs.state(j).finished());
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        for r in &mut self.running {
            r.proc.kill();
        }
        for c in &mut self.compiling {
            c.proc.kill();
        }
    }
}

/// Starts compiling `s` into the build cache, or returns `None` when the cached output is
/// at least as new as the source. A `body` source is first written to the cache as
/// `<stem>.va|.v` (only when its text changed, so the mtime check holds across runs).
/// The compiler's output goes to `<cache>/<out>.log`.
fn spawn_compile(cfg: &RunnerConfig, s: &Source) -> Result<Option<Compiling>, String> {
    let cache = |e: io::Error| format!("cache {}: {e}", cfg.cache.display());
    let src = match &s.origin {
        Origin::File { lib, path } => cfg
            .lib_dirs
            .get(usize::from(*lib))
            .ok_or_else(|| format!("no directory for library row {lib}"))?
            .join(path),
        Origin::Body(text) => {
            fs::create_dir_all(&cfg.cache).map_err(cache)?;
            let stem = s.out.rsplit_once('.').map_or(s.out.as_str(), |(st, _)| st);
            let ext = match s.tool {
                Tool::Openvaf => "va",
                Tool::Iverilog => "v",
            };
            let p = cfg.cache.join(format!("{stem}.{ext}"));
            if fs::read(&p).ok().as_deref() != Some(text.as_bytes()) {
                fs::write(&p, text).map_err(cache)?;
            }
            p
        }
    };
    let cached = cfg.cache.join(&s.out);
    let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified());
    let src_time = mtime(&src).map_err(|e| format!("source {}: {e}", src.display()))?;
    if mtime(&cached).is_ok_and(|t| t >= src_time) {
        return Ok(None);
    }
    fs::create_dir_all(&cfg.cache).map_err(cache)?;
    // A stale output must not pass for this compile's result if the compiler writes none.
    match fs::remove_file(&cached) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(cache(e)),
        _ => {}
    }
    let (prog, what) = match s.tool {
        Tool::Openvaf => (&cfg.tools.openvaf, "Verilog-A → OSDI"),
        Tool::Iverilog => (&cfg.tools.iverilog, "Verilog → VVP"),
    };
    let mut cmd = Command::new(prog);
    match s.tool {
        Tool::Openvaf => cmd.arg(&src).arg("-o").arg(&cached),
        Tool::Iverilog => cmd.arg("-o").arg(&cached).arg(&src),
    };
    let log = File::create(cfg.cache.join(format!("{}.log", s.out))).map_err(cache)?;
    let log2 = log.try_clone().map_err(cache)?;
    let proc = Proc::spawn(&mut cmd, log, log2).map_err(|e| {
        let need = format!("needed to compile {} ({what})", src.display());
        format!(
            "{}; {need}",
            spawn_error(prog, "compiler", "the user config", &e)
        )
    })?;
    let c = Compiling {
        out: s.out.clone(),
        proc,
        what: format!("`{prog}` on {}", src.display()),
    };
    assert!(!c.out.is_empty());
    Ok(Some(c))
}

/// A child process and its start time: the crate's one subprocess boundary. Simulators and
/// compilers are spawned, reaped and killed only through it.
struct Proc {
    child: Child,
    started: Instant,
}

impl Proc {
    /// Spawns `cmd` with stdin closed and stdout/stderr into `out`/`err`.
    fn spawn(cmd: &mut Command, out: File, err: File) -> io::Result<Proc> {
        let child = cmd.stdin(Stdio::null()).stdout(out).stderr(err).spawn()?;
        Ok(Proc {
            child,
            started: Instant::now(),
        })
    }

    /// Returns `None` while the child runs within `timeout`, else its outcome: the exit
    /// status, or why it was killed (timeout, or `try_wait` failed). Never blocks on a
    /// live child.
    fn reap(&mut self, timeout: Duration, what: &str) -> Option<Result<ExitStatus, String>> {
        let outcome = match self.child.try_wait() {
            Ok(Some(status)) => Ok(status),
            Ok(None) if self.started.elapsed() <= timeout => return None,
            Ok(None) => Err(format!("timed out after {timeout:?}")),
            Err(e) => Err(format!("cannot wait for the {what}: {e}")),
        };
        if outcome.is_err() {
            self.kill();
        }
        Some(outcome)
    }

    /// Kills the child and reaps it (no zombie); errors mean it already exited.
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_error(prog: &str, what: &str, config: &str, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::NotFound {
        format!("{what} `{prog}` not found on PATH (install it or set its path in {config})")
    } else {
        format!("cannot start {what} `{prog}`: {e}")
    }
}

/// Scans new complete stdout lines (at most 64 KiB per call) and keeps the last one.
fn tail(dir: &Path, r: &mut Running) {
    const CHUNK: u64 = 1 << 16;
    let Ok(mut f) = File::open(dir.join(STDOUT)) else {
        return;
    };
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(r.log_pos)).is_err() || f.take(CHUNK).read_to_end(&mut buf).is_err() {
        return;
    }
    let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
        if buf.len() as u64 == CHUNK {
            r.log_pos += CHUNK; // one line longer than a chunk: skip it
        }
        return;
    };
    r.log_pos += end as u64 + 1;
    if let Some(line) = buf[..end]
        .split(|&b| b == b'\n')
        .rev()
        .find(|l| !l.trim_ascii().is_empty())
    {
        r.last = String::from_utf8_lossy(line.trim_ascii()).into_owned();
    }
}

fn last_line(path: &Path) -> Option<String> {
    let text = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&text);
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_owned())
}
