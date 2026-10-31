//! Remote jobs (run spec §5): a runner whose [`RunnerConfig::host`] is set runs every job
//! through `ssh`. The `ssh` here is a test double named by [`Tools::ssh`]: it logs its argv
//! and runs the remote command with `sh -c` (as sshd runs `$SHELL -c <command>`, here with
//! the "server's" `TMPDIR`), so the "server" is this machine.
//!
//! Oracles: the same jobs run locally give byte-identical results files and raw files (the
//! fake simulator's output came back byte for byte); the server's scratch directories are
//! gone afterwards, also after a failure; a stand-in ESPice that fails unless the job's
//! Verilog-A sources sit in its working directory in their layout passes; arguments with
//! spaces and shell metacharacters reach the simulator verbatim; ssh never prompts and the
//! destination follows `--`; a host without the simulator and a host that asks for a
//! password fail the job naming the host and the command.

mod common;

use schemify_doc::Digest;
use schemify_elab::Dialect;
use schemify_run::{
    Jobs, NO_SAMPLE, Plan, RunnerConfig, Select, SimCommand, State, expand, write_results,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Reads `.param R=` from the netlist `$1` and writes a two-point ASCII raw file into `$2`
/// with v(out) going from 0 to R over 1 s (runner.rs's fake simulator, as a program file).
const FAKE: &str = r#"#!/bin/sh
R=$(sed -n 's/^\.param R=//p' "$1")
printf 'Title: fake\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 2\nNo. Points: 2\nVariables:\n 0 time time\n 1 v(out) voltage\nValues:\n0 0\n 0\n1 1\n %s\n' "$R" > "$2"
"#;

/// A fake simulator that writes one line per argument after the first two to stdout (which a
/// remote job streams into `stderr.log`), then [`FAKE`]'s raw file.
const ECHO_ARGS: &str = r#"#!/bin/sh
n="$1"; r="$2"; shift 2
for a in "$@"; do printf '<%s>\n' "$a"; done
printf 'Title: fake\nPlotname: Operating Point\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n 0 v(out) voltage\nValues:\n0 1\n' > "$r"
"#;

/// A Verilog-A cell whose source includes a file in a subdirectory.
const VSRC: &str = "schemify 1 shprim\n\npin A inout 0 0\npin B inout 0 20\n\nimpl va veriloga\n  model vsrc\n  file va/vsrc.va\n";
const VSRC_BENCH: &str = "schemify 1 shbench\n\ninst 0aaaaa ./vsrc\n  at 0 0 R0\n  name N1\n\n\
label 0bbbba 0 0 R0 out\nlabel 0bbbbb 0 20 R0 0\n\nvar R 1\n";
/// A stand-in ESPice that fails unless the netlist, the copied source and its include sit
/// in its working directory in their layout.
const NEEDS_SOURCES: &str = r#"#!/bin/sh
for f in "$1" vsrc.va sub/k.inc; do [ -f "$f" ] || { echo "missing $f" >&2; exit 5; }; done
printf 'Title: fake\nPlotname: Operating Point\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n 0 v(out) voltage\nValues:\n0 1\n' > "$2"
"#;

/// The "server": a directory with the fake simulators, a scratch `TMPDIR`, and the ssh
/// double (with its argv log).
struct Server {
    root: PathBuf,
}

// ---- private ----

impl Server {
    fn new(name: &str) -> Server {
        Server::with_shell(name, "sh")
    }

    /// A server whose login shell is `shell` (sshd runs `$SHELL -c <command>`).
    fn with_shell(name: &str, shell: &str) -> Server {
        let root = common::tmp(&format!("remote-{name}"));
        fs::create_dir_all(root.join("tmp")).expect("the test temp dir is writable");
        fs::create_dir_all(root.join("bin")).expect("the test temp dir is writable");
        let s = Server { root };
        // sshd joins the words after the destination and runs them with the login shell.
        let shim = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\n\
             while [ $# -gt 0 ]; do case \"$1\" in --) shift; break ;; -o|-p|-F|-i|-l|-J) shift 2 ;; -*) shift ;; *) break ;; esac; done\n\
             host=\"$1\"; shift\n\
             case \"$host\" in *nopass*) echo \"$host: Permission denied (publickey,password).\" >&2; exit 255 ;; esac\n\
             TMPDIR='{tmp}' exec {shell} -c \"$*\"\n",
            log = s.root.join("ssh.log").display(),
            tmp = s.root.join("tmp").display(),
        );
        s.program("ssh", &shim);
        s.program("fake", FAKE);
        s.program("echo-args", ECHO_ARGS);
        s.program("needs-sources", NEEDS_SOURCES);
        s
    }

    fn program(&self, name: &str, text: &str) {
        let p = self.root.join("bin").join(name);
        fs::write(&p, text).expect("the server's bin is writable");
        let ok = Command::new("chmod")
            .arg("+x")
            .arg(&p)
            .status()
            .expect("chmod on PATH");
        assert!(ok.success());
    }

    fn bin(&self, name: &str) -> String {
        self.root.join("bin").join(name).display().to_string()
    }

    fn ssh_log(&self) -> Vec<String> {
        let t = fs::read_to_string(self.root.join("ssh.log")).unwrap_or_default();
        t.lines().map(str::to_owned).collect()
    }

    /// Entries left in the server's scratch directory (each remote job's `mktemp -d`).
    fn scratch(&self) -> Vec<String> {
        let d = fs::read_dir(self.root.join("tmp")).expect("the server's scratch exists");
        d.flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// A runner configuration for `host` whose every simulator is `sim` (args after it).
    fn cfg(&self, dir: &Path, d: &Digest, host: &str, sim: &str, args: &[&str]) -> RunnerConfig {
        let mut c = RunnerConfig::new(dir, d);
        c.tools.set_sims(&SimCommand {
            path: self.bin(sim),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            env: Vec::new(),
        });
        c.tools.ssh = self.bin("ssh");
        c.host = Some(host.to_owned());
        c
    }
}

fn all_jobs(plan: &Plan) -> Jobs {
    let select = Select {
        corners: &["ff", "tt"],
        sweeps: &["rsw"],
        montecarlo: Some("mc"),
        ..Select::default()
    };
    expand(plan, &select).expect("the rc bench expands")
}

#[test]
fn remote_jobs_equal_local_jobs() {
    let (p, top) = common::rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let server = Server::new("equal");
    let local_dir = common::tmp("remote-equal-local");
    let (jobs_l, results_l, runner_l) = common::run(&p, top, &plan, all_jobs(&plan), |d| {
        let mut c = RunnerConfig::new(&local_dir, d);
        c.tools.set_sims(&SimCommand {
            path: server.bin("fake"),
            args: vec!["{netlist}".into(), "{raw}".into()],
            env: Vec::new(),
        });
        c
    });
    let dir = common::tmp("remote-equal-project");
    let (jobs, results, runner) = common::run(&p, top, &plan, all_jobs(&plan), |d| {
        server.cfg(&dir, d, "pdk@server", "fake", &["{netlist}", "{raw}"])
    });
    assert_eq!(jobs.len(), 24);
    assert!(
        jobs.states().iter().all(|s| *s == State::Done),
        "{:?}",
        results.errors().collect::<Vec<_>>()
    );
    assert_eq!(
        write_results(&jobs, &results),
        write_results(&jobs_l, &results_l)
    );
    // One ssh per job, never prompting, the destination after `--`, the job as one word.
    let log = server.ssh_log();
    assert_eq!(log.len(), 24, "{log:?}");
    for line in &log {
        assert!(
            line.starts_with("-T -x -a -o BatchMode=yes -o ConnectTimeout=10 ")
                && line.contains(" -o ClearAllForwardings=yes -- pdk@server sh -c '"),
            "{line}"
        );
    }
    for j in 0..jobs.len() {
        let (remote, local) = (runner.job_dir(j), runner_l.job_dir(j));
        let raw = |d: &Path| fs::read(d.join("out.raw")).unwrap();
        assert_eq!(
            raw(&remote),
            raw(&local),
            "job {j}: the raw file came back as written"
        );
        let command = fs::read_to_string(remote.join("command.log")).unwrap();
        assert_eq!(
            command,
            format!(
                "ssh pdk@server {} netlist.cir out.raw\n",
                server.bin("fake")
            )
        );
        assert!(remote.join("stdout.log").is_file() && remote.join("stderr.log").is_file());
        assert!(
            !remote.join("upload.tar").exists(),
            "the upload is not kept"
        );
    }
    assert!(server.scratch().is_empty(), "{:?}", server.scratch());
}

/// Gate (issue Schemify-PDK-Manager#1): a host without the configured simulator fails the
/// job, naming the host and the command; its scratch directory is removed all the same. A
/// host that asks for a password fails with ssh's own message.
#[test]
fn failures_name_the_host_and_the_command() {
    let (p, top) = common::rc();
    let plan = Plan::read(p.doc(top).unwrap()).unwrap();
    let server = Server::new("fail");
    let one = || {
        let mut j = Jobs::new();
        j.push(None, None, NO_SAMPLE, &[]).unwrap();
        j
    };
    let dir = common::tmp("remote-fail-project");
    let (jobs, results, _) = common::run(&p, top, &plan, one(), |d| {
        let mut c = server.cfg(
            &dir,
            d,
            "server.example.edu",
            "fake",
            &["{netlist}", "{raw}"],
        );
        c.tools.set_sims(&SimCommand {
            path: "no-such-espice".into(),
            args: vec!["-b".into(), "{netlist}".into()],
            env: Vec::new(),
        });
        c
    });
    assert_eq!(jobs.states(), [State::Failed]);
    let msg = results.errors().next().unwrap().message.to_owned();
    assert!(
        msg.starts_with("`no-such-espice` on `server.example.edu` exited with exit status: 127: "),
        "{msg}"
    );
    assert!(
        msg.contains("no-such-espice") && msg.contains("not found"),
        "{msg}"
    );
    assert!(server.scratch().is_empty(), "{:?}", server.scratch());

    let (jobs, results, _) = common::run(&p, top, &plan, one(), |d| {
        server.cfg(
            &dir,
            d,
            "nopass.example.edu",
            "fake",
            &["{netlist}", "{raw}"],
        )
    });
    assert_eq!(jobs.states(), [State::Failed]);
    let msg = results.errors().next().unwrap().message.to_owned();
    assert!(
        msg.starts_with("ssh to `nopass.example.edu` failed (exit status: 255): ")
            && msg.contains("Permission denied (publickey,password)."),
        "{msg}"
    );
}

/// The job directory goes up whole: an espice job's copied Verilog-A source and its
/// include, in their layout; arguments with spaces, quotes, `$`, backslashes and a
/// semicolon reach the simulator as single words, and its output streams back into
/// `stderr.log`.
#[test]
fn the_job_directory_and_arguments_arrive_intact() {
    job_directory_and_arguments(Server::new("upload"));
}

/// The same under a csh login shell (many EDA servers'): the job is one `sh -c` word, so
/// csh hands it to sh untouched. Skipped without `tcsh` on PATH.
#[test]
fn a_csh_login_shell_runs_the_same_job() {
    if !common::have("tcsh", "--version") {
        return;
    }
    job_directory_and_arguments(Server::with_shell("upload-csh", "tcsh"));
}

fn job_directory_and_arguments(server: Server) {
    let (p, top) = common::project(&[("vsrc.shprim", VSRC), ("tb.shbench", VSRC_BENCH)]);
    let plan = Plan::read(p.doc(top).expect("the bench is open")).expect("the bench plan reads");
    let dir = server.root.join("project");
    let va = dir.join("va");
    fs::create_dir_all(va.join("sub")).expect("the project dir is writable");
    fs::write(
        va.join("vsrc.va"),
        "`include \"sub/k.inc\"\nmodule vsrc(p, n); endmodule\n",
    )
    .expect("the project dir is writable");
    fs::write(va.join("sub/k.inc"), "`define K 1\n").expect("the project dir is writable");
    let one = || {
        let mut j = Jobs::new();
        j.push(None, None, NO_SAMPLE, &[]).expect("one job fits");
        j
    };
    let (jobs, results, _) =
        common::run_dialect(&p, top, &plan, one(), &[], Dialect::Espice, |d| {
            let mut c = server.cfg(&dir, d, "server", "needs-sources", &["{netlist}", "{raw}"]);
            c.dir = dir.join("runs");
            c.cache = dir.join("cache");
            c
        });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.states(), [State::Done], "{errs:?}");

    let words = ["a b", "it's", "$HOME", "x\"y\\z", "a;b", "!bang", "`id`"];
    let mut args = vec!["{netlist}", "{raw}"];
    args.extend(words);
    let (p, top) = common::rc();
    let plan = Plan::read(p.doc(top).expect("the bench is open")).expect("the bench plan reads");
    let (jobs, results, runner) = common::run(&p, top, &plan, one(), |d| {
        let mut c = server.cfg(&dir, d, "server", "echo-args", &args);
        c.dir = dir.join("runs-args");
        c
    });
    let errs: Vec<_> = results.errors().collect();
    assert_eq!(jobs.states(), [State::Done], "{errs:?}");
    let got = fs::read_to_string(runner.job_dir(0).join("stderr.log"))
        .expect("a done job has its stderr.log");
    let want: String = words.iter().map(|w| format!("<{w}>\n")).collect();
    assert_eq!(got, want);
    assert!(server.scratch().is_empty());
}
