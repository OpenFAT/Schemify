//! The native subprocess runner (D15, run spec §5): one directory per job under the
//! run directory, the simulator spawned from the user config's command line (D7: never
//! from a document), at most `workers` at a time, polled by the frame spine.
//!
//! No threads and no callbacks: children write `stdout.log`/`stderr.log` in their job
//! directory; [`Runner::poll`] reaps exited children, kills those past the timeout,
//! tails stdout for progress and starts queued jobs. Compiled sources (OpenVAF,
//! Icarus) are cached by mtime in a shared build directory and copied into each job
//! directory.

use crate::jobs::{Jobs, State};
use crate::plan::Plan;
use crate::prepare::{Prepared, Source, Tool};
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
    /// On-disk directory of each library row ([`Source::lib`]).
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

struct Queued {
    job: u32,
    sources: Vec<Source>,
}

struct Running {
    job: u32,
    child: Child,
    started: Instant,
    /// Bytes of stdout already scanned.
    log_pos: u64,
    /// Last non-empty stdout line.
    last: String,
}

/// Jobs waiting for and holding worker slots. Dropping a runner kills its children.
pub struct Runner {
    cfg: RunnerConfig,
    queue: VecDeque<Queued>,
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

    /// Writes job `j`'s netlist into its directory (removing an old raw file) and queues
    /// it. A write failure fails the job.
    pub fn submit(&mut self, jobs: &mut Jobs, results: &mut Results, j: u32, p: &Prepared) {
        assert_eq!(jobs.state(j), State::Pending, "submit job {j} once");
        assert!(self.queue.len() < crate::JOBS_MAX as usize);
        let dir = self.job_dir(j);
        let written = fs::create_dir_all(&dir).and_then(|()| {
            match fs::remove_file(dir.join(RAW)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            fs::write(dir.join(NETLIST), &p.netlist)
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

    /// Returns whether any job is queued or running.
    pub fn busy(&self) -> bool {
        !self.queue.is_empty() || !self.running.is_empty()
    }

    /// Iterates `(job, last stdout line)` of the running jobs.
    pub fn progress(&self) -> impl Iterator<Item = (u32, &str)> + '_ {
        self.running.iter().map(|r| (r.job, r.last.as_str()))
    }

    /// Advances every job one step without blocking: reaps exited simulators (their raw
    /// file goes through [`Results::record`]), kills those past the timeout, tails
    /// stdout, and starts queued jobs into free worker slots. Appends to `finished` every
    /// job that reached `Done` or `Failed` in this call.
    ///
    /// Cost: one `try_wait` per running child; compiling a changed Verilog-A or Verilog
    /// source blocks this call while the compiler runs.
    pub fn poll(
        &mut self,
        plan: &Plan,
        jobs: &mut Jobs,
        results: &mut Results,
        finished: &mut Vec<u32>,
    ) {
        let before = finished.len();
        let mut i = 0;
        while i < self.running.len() {
            let dir = self.job_dir(self.running[i].job);
            let r = &mut self.running[i];
            tail(&dir, r);
            let outcome = match r.child.try_wait() {
                Ok(Some(status)) => Ok(status),
                Ok(None) if r.started.elapsed() <= self.cfg.timeout => {
                    i += 1;
                    continue;
                }
                Ok(None) => Err(format!("timed out after {:?}", self.cfg.timeout)),
                Err(e) => Err(format!("cannot wait for the simulator: {e}")),
            };
            let mut r = self.running.remove(i);
            if outcome.is_err() {
                let _ = r.child.kill();
                let _ = r.child.wait();
            }
            self.finish(plan, jobs, results, r.job, outcome);
            finished.push(r.job);
        }
        while self.running.len() < usize::from(self.cfg.workers) {
            let Some(q) = self.queue.pop_front() else {
                break;
            };
            if let Err(msg) = self.start(&q) {
                results.fail(jobs, q.job, msg);
                finished.push(q.job);
                continue;
            }
            jobs.set_state(q.job, State::Running);
        }
        assert!(self.running.len() <= usize::from(self.cfg.workers));
        assert!(finished[before..].iter().all(|&j| jobs.state(j).finished()));
    }

    /// Polls until no job is queued or running (the CLI and tests; the app polls from
    /// its frame spine instead). Terminates: every child is killed at the timeout.
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
        assert!(self.queue.is_empty() && self.running.is_empty());
    }

    /// Cancels job `j` if it is queued or running (a running simulator is killed).
    pub fn cancel(&mut self, jobs: &mut Jobs, j: u32) {
        if let Some(i) = self.queue.iter().position(|q| q.job == j) {
            self.queue.remove(i);
            jobs.set_state(j, State::Cancelled);
        } else if let Some(i) = self.running.iter().position(|r| r.job == j) {
            let mut r = self.running.remove(i);
            let _ = r.child.kill();
            let _ = r.child.wait();
            jobs.set_state(j, State::Cancelled);
        }
        assert!(!self.queue.iter().any(|q| q.job == j) && !self.running.iter().any(|r| r.job == j));
    }

    /// Cancels every queued and running job.
    pub fn cancel_all(&mut self, jobs: &mut Jobs) {
        let all: Vec<u32> = self
            .queue
            .iter()
            .map(|q| q.job)
            .chain(self.running.iter().map(|r| r.job))
            .collect();
        for j in all {
            self.cancel(jobs, j);
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

    fn start(&mut self, q: &Queued) -> Result<(), String> {
        let dir = self.job_dir(q.job);
        for s in &q.sources {
            compile(&self.cfg, s, &dir)?;
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
        let child = Command::new(&sim.path)
            .args(args)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|e| spawn_error(&sim.path, "simulator", "the user config `sim` entry", &e))?;
        self.running.push(Running {
            job: q.job,
            child,
            started: Instant::now(),
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
            let _ = r.child.kill();
            let _ = r.child.wait();
        }
    }
}

/// Compiles `s` into the cache unless the cached output is at least as new as the
/// source, then copies it into the job directory.
fn compile(cfg: &RunnerConfig, s: &Source, job_dir: &Path) -> Result<(), String> {
    let lib = cfg
        .lib_dirs
        .get(usize::from(s.lib))
        .ok_or_else(|| format!("no directory for library row {}", s.lib))?;
    let src = lib.join(&s.path);
    let cached = cfg.cache.join(&s.out);
    let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified());
    let src_time = mtime(&src).map_err(|e| format!("source {}: {e}", src.display()))?;
    let fresh = mtime(&cached).is_ok_and(|t| t >= src_time);
    if !fresh {
        fs::create_dir_all(&cfg.cache)
            .map_err(|e| format!("cache {}: {e}", cfg.cache.display()))?;
        let (prog, what) = match s.tool {
            Tool::Openvaf => (&cfg.tools.openvaf, "Verilog-A → OSDI"),
            Tool::Iverilog => (&cfg.tools.iverilog, "Verilog → VVP"),
        };
        let mut cmd = Command::new(prog);
        match s.tool {
            Tool::Openvaf => cmd.arg(&src).arg("-o").arg(&cached),
            Tool::Iverilog => cmd.arg("-o").arg(&cached).arg(&src),
        };
        let out = cmd.stdin(Stdio::null()).output().map_err(|e| {
            let need = format!("needed to compile {} ({what})", src.display());
            format!(
                "{}; {need}",
                spawn_error(prog, "compiler", "the user config", &e)
            )
        })?;
        if !out.status.success() || !cached.exists() {
            let err = String::from_utf8_lossy(&out.stderr);
            let why = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no output");
            return Err(format!(
                "`{prog}` failed on {} ({}): {why}",
                src.display(),
                out.status
            ));
        }
    }
    fs::copy(&cached, job_dir.join(&s.out)).map_err(|e| format!("copy {}: {e}", s.out))?;
    assert!(job_dir.join(&s.out).exists());
    Ok(())
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
