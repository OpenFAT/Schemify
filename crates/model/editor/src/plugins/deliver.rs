//! One delivery: events to plugins, then their request queues drained to their owners (edits,
//! runs, I/O, evals, logs). File I/O completes within the delivery ([`run_io`]); `eval`
//! requests are answered by [`eval_in`].
//!
//! Contents:
//! - `Host::deliver`
//! - `pub fn eval_in`
//! - `pub fn fn_result`

use std::path::{Path, PathBuf};

use schemify_doc::{DocHandle, Project};
use schemify_elab::{CellState, Funcs, Val};
use schemify_expr::{Code as XCode, Error as XError, Mode, Program, Scratch, Span, Value};
use schemify_plugin::IoOp;
use schemify_plugin::abi::{Code, EditMode, Event, Level};

use crate::platform;

use super::{Child, Delivered, Host, PluginEdit, ROUNDS_MAX, Timer, WAITING_MAX};

impl Host {
    /// The spine's `plugin::deliver` plus queue draining, at most `ROUNDS_MAX` (4) rounds:
    /// file I/O completes within the frame; `spawn` starts a child whose `IoDone` comes
    /// through [`Host::poll`]; timers are armed against `now_ms`; run requests go out in
    /// [`Delivered::runs`].
    ///
    /// # Panics
    ///
    /// Only on a broken internal invariant (a waiting table past [`WAITING_MAX`], or more
    /// cell states built than the rounds' evals).
    pub fn deliver(&mut self, project: &Project, root: &Path, now_ms: u64) -> Delivered {
        if !self.user_ready {
            self.user_ready = true;
            if self
                .user
                .table()
                .hooks
                .iter()
                .any(|h| h.0 == schemify_config::Event::Load)
            {
                let keys = self.user.table().keymaps.clone();
                let out = self.user.fire(schemify_config::Event::Load, &[]);
                self.absorb(out, &keys);
            }
        }
        let mut out = Delivered {
            state_changed: std::mem::take(&mut self.commanded),
            commands: std::mem::take(&mut self.user_cmds),
            logs: std::mem::take(&mut self.user_logs),
            ..Delivered::default()
        };
        let jobs_done = self.plugins.jobs_done();
        // `eval` answers reuse one cell state per document within this delivery.
        let mut states: Vec<(DocHandle, Option<CellState>)> = Vec::new();
        for _ in 0..ROUNDS_MAX {
            if !self.has_work() {
                break;
            }
            schemify_plugin::deliver(
                &mut self.plugins,
                &self.events,
                &mut self.snapshots,
                &mut self.queues,
            );
            self.events.clear();
            for e in self.queues.edits.drain(..) {
                let Some(h) = super::handle_of(project, e.doc) else {
                    out.logs
                        .push(format!("{}: edit for a closed document", e.plugin));
                    continue;
                };
                let stage = e.mode == EditMode::Stage;
                out.edits.push(PluginEdit {
                    doc: h,
                    label: e.label,
                    ops: e.ops,
                    stage,
                });
            }
            for l in self.queues.logs.drain(..) {
                out.failed |= l.level == Level::Error;
                let name = self.plugins.name(l.plugin).unwrap_or("plugin").to_owned();
                out.logs.push(format!("{name}: {}", l.text));
            }
            for t in self.queues.timers.drain(..) {
                if self.timers.len() < WAITING_MAX {
                    self.timers.push(Timer {
                        due_ms: now_ms + u64::from(t.ms),
                        plugin: t.plugin,
                        id: t.id,
                    });
                } else {
                    out.logs.push(format!("{}: too many timers", t.plugin));
                }
            }
            out.runs.append(&mut self.queues.runs);
            let evals: Vec<_> = self.queues.evals.drain(..).collect();
            for e in evals {
                let (status, text) = self.eval_request(project, &mut states, e.doc, &e.expr);
                let ev = Event::IoDone {
                    handle: e.handle,
                    status,
                    bytes: text.as_bytes(),
                };
                let _ = self.events.push(Some(e.plugin), &ev);
            }
            let io: Vec<_> = self.queues.io.drain(..).collect();
            out.state_changed |= !io.is_empty();
            for r in io {
                if r.op == IoOp::Spawn && self.children.len() < WAITING_MAX {
                    let child = platform::spawn_start(root, r.args, r.data);
                    self.children.push(Child {
                        plugin: r.plugin,
                        handle: r.handle,
                        process: child,
                    });
                    continue;
                }
                let (status, bytes) = if r.op == IoOp::Spawn {
                    let space = -(Code::Space as i32);
                    (space, b"too many child processes".to_vec())
                } else {
                    run_io(root, r.op, &r.args, &r.data)
                };
                // pdk spec §3.1: a plugin that rewrote the manifest or lockfile gets the
                // project reloaded (once per delivery) before its `IoDone` arrives.
                let manifest = matches!(
                    r.args.first().map(String::as_str),
                    Some("schemify.project" | "schemify.lock")
                );
                let reload = ("project.reload".to_owned(), String::new());
                if r.op == IoOp::FsWrite
                    && status == 0
                    && manifest
                    && !out.commands.contains(&reload)
                {
                    out.commands.push(reload);
                }
                let ev = Event::IoDone {
                    handle: r.handle,
                    status,
                    bytes: &bytes,
                };
                let _ = self.events.push(Some(r.plugin), &ev);
            }
        }
        // A worker event that finished may have loaded what a pure function reads (D20).
        out.state_changed |= self.plugins.jobs_done() != jobs_done;
        // Aborted calls (out of fuel, traps) and quarantines reach the status line, so a
        // plugin never fails silently (plugin-abi §2); the rest stay in the health report.
        let health = self.plugins.health();
        let fresh = (health.recorded() - self.faults_seen).min(health.faults().len() as u64);
        let skip = health.faults().len() - fresh as usize;
        for f in health.faults().iter().skip(skip) {
            let quarantined = f.code == Code::Gone && f.detail.contains(" quarantined: ");
            if matches!(f.code, Code::Fuel | Code::Trap) || quarantined {
                out.failed = true;
                out.logs.push(f.detail.clone());
            }
        }
        self.faults_seen = health.recorded();
        assert!(states.len() <= ROUNDS_MAX as usize * schemify_plugin::EVALS_MAX);
        assert!(self.timers.len() <= WAITING_MAX);
        assert!(self.children.len() <= WAITING_MAX);
        out
    }

    /// Answers one plugin `eval` (plugin-abi §3) for document id `doc`: [`eval_in`] over its
    /// cell state, built once per delivery (`states`) with this host's plugin functions.
    /// Returns the `IoDone` status and bytes: `0` and the value, or `-E_GONE` for a closed
    /// document, `-E_PARSE` and the expression error.
    fn eval_request(
        &mut self,
        project: &Project,
        states: &mut Vec<(DocHandle, Option<CellState>)>,
        doc: u32,
        expr: &str,
    ) -> (i32, String) {
        let Some(h) = super::handle_of(project, doc) else {
            return (Code::Gone.neg(), "document closed".to_owned());
        };
        let i = match states.iter().position(|s| s.0 == h) {
            Some(i) => i,
            None => {
                let mut funcs = |name: &str, args: &[Value<'_>]| self.call_fn(name, args);
                states.push((h, CellState::build(project, h, &mut funcs)));
                states.len() - 1
            }
        };
        let Some(st) = &states[i].1 else {
            return (Code::Gone.neg(), "document closed".to_owned());
        };
        assert!(
            states.iter().filter(|s| s.0 == h).count() == 1,
            "one state per document"
        );
        let mut funcs = |name: &str, args: &[Value<'_>]| self.call_fn(name, args);
        match eval_in(st, expr, &mut funcs) {
            Ok(text) => (0, text),
            Err(e) => (
                Code::Parse.neg(),
                format!("{}: {}", e.code.as_str(), e.message),
            ),
        }
    }
}

/// Evaluates a plugin's `eval` expression (plugin-abi §3) in the cell scope of `st` (expr
/// spec §4: the cell's own params, then its vars, at their default-context values), calling
/// plugin functions through `funcs`. Returns the value as one line-record value, the form
/// `schemify_call` results take: `int`/`num` in canonical number text (expr spec §7), `bool`
/// as `true`/`false`, `str` quoted, `cnum` as `re+imi`.
///
/// # Errors
///
/// The compile or evaluation error of `expr`; `X004` also for a name whose own value failed
/// in the cell (its error is the cell's violation, not this expression's).
///
/// # Panics
///
/// Never: the asserts check the slots of the compiled program.
pub fn eval_in<F: Funcs>(st: &CellState, expr: &str, funcs: &mut F) -> Result<String, XError> {
    let prog = Program::compile(expr, Mode::Scalar)?;
    let g = &st.graph;
    let own = st.ifaces.first().map_or(&[][..], |f| &f.param_name[..]);
    let params = own.len().min(g.params as usize);
    let mut inputs: Vec<Value<'_>> = Vec::with_capacity(prog.idents().len());
    for name in prog.idents() {
        let node = (0..params)
            .find(|&p| st.names.get(own[p]) == name)
            .or_else(|| {
                let vars = g.var_name.iter().position(|&v| st.names.get(v) == name)?;
                Some(g.params as usize + vars)
            });
        let Some(value) = node.and_then(|n| g.vals.get(n)).and_then(Val::value) else {
            let msg = format!("`{name}` has no value in this cell");
            return Err(XError::new(XCode::X004, Span::default(), msg));
        };
        inputs.push(value);
    }
    let mut call = |slot: u32, args: &[Value<'_>]| -> Result<F::Out, XError> {
        let name = prog
            .plugin_fns()
            .nth(slot as usize)
            .expect("slot of this program")
            .to_string();
        funcs.call(&name, args).unwrap_or_else(|| {
            let msg = format!("plugin function `{name}` is not loaded");
            Err(XError::new(XCode::X006, Span::default(), msg))
        })
    };
    let mut scratch = Scratch::default();
    let value = prog.eval_with(&inputs, &mut call, &mut scratch)?;
    let mut out = String::new();
    match value {
        Value::Str(s) => out.push_str(&crate::docs::quote(s)),
        other => Val::from(other).write_text(&mut out),
    }
    assert!(
        !out.is_empty() || matches!(value, Value::Str(_)),
        "values have text"
    );
    Ok(out)
}

/// A plugin function's result text (plugin ABI §2 `schemify_call`, expr §5): `true`/`false`,
/// a number in canonical text, or one quoted string (format §2 escapes). `None` for anything
/// else, including trailing values.
pub fn fn_result(t: &str) -> Option<Val> {
    match t {
        "true" => Some(Val::Bool(true)),
        "false" => Some(Val::Bool(false)),
        t if t.starts_with('"') => {
            let mut v = schemify_plugin::abi::text::values(t);
            let s = v.need().ok()?;
            v.end().ok()?;
            Some(Val::Str(s.as_ref().into()))
        }
        t => schemify_expr::parse_num(t).ok().map(Val::Num),
    }
}

// ---- private ----

/// Executes an authorized I/O request natively (project-relative paths were checked by the
/// host). Returns the `IoDone` status and bytes.
fn run_io(root: &Path, op: IoOp, args: &[String], data: &[u8]) -> (i32, Vec<u8>) {
    let file_op = matches!(
        op,
        IoOp::FsRead | IoOp::FsList | IoOp::FsWrite | IoOp::FsRemove
    );
    assert!(
        !file_op || !args.is_empty(),
        "the host checked a file request names its path"
    );
    let denied = -(Code::Denied as i32);
    let gone = -(Code::Gone as i32);
    // A read path: project-relative, or `${NAME}/…` under the variable's directory (the
    // host authorized the `read:<NAME>` grant, plugin-abi §5).
    let read_path = |p: &str| -> Result<PathBuf, String> {
        match schemify_plugin::env_path(p) {
            None => Ok(root.join(p)),
            Some((name, rel)) => {
                let dir = platform::env_var(name).filter(|d| !d.is_empty());
                let dir = dir.ok_or_else(|| format!("${{{name}}}: not set"))?;
                Ok(if rel.is_empty() {
                    PathBuf::from(dir)
                } else {
                    Path::new(&dir).join(rel)
                })
            }
        }
    };
    match op {
        IoOp::FsRead => match read_path(&args[0]).and_then(|p| platform::read(&p)) {
            Ok(b) => (0, b),
            Err(e) => (gone, e.into_bytes()),
        },
        IoOp::FsList => match read_path(&args[0]) {
            Err(e) => (gone, e.into_bytes()),
            Ok(dir) if !platform::is_dir(&dir) => {
                (gone, format!("{}: not a directory", args[0]).into_bytes())
            }
            Ok(dir) => {
                let mut text = String::new();
                for f in platform::list_files(&dir, &[]) {
                    let rel = f.strip_prefix(&dir).expect("listed under dir");
                    text.push_str(&rel.to_string_lossy().replace('\\', "/"));
                    text.push('\n');
                }
                (0, text.into_bytes())
            }
        },
        IoOp::FsWrite => match platform::write_atomic(&root.join(&args[0]), data) {
            Ok(()) => (0, Vec::new()),
            Err(e) => (denied, e.into_bytes()),
        },
        IoOp::FsRemove => {
            let path = root.join(&args[0]);
            if !platform::exists(&path) {
                let gone = -(Code::Gone as i32);
                return (gone, format!("{}: nothing to remove", args[0]).into_bytes());
            }
            match platform::remove_tree(&path) {
                Ok(()) => (0, Vec::new()),
                Err(e) => (denied, e.into_bytes()),
            }
        }
        IoOp::Spawn => unreachable!("spawn runs through platform::spawn_start"),
        // ponytail: fetch and clipboard are M3; answer "denied".
        IoOp::Fetch | IoOp::ClipboardGet | IoOp::ClipboardSet => {
            (denied, b"not supported yet".to_vec())
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod io_tests {
    use super::*;

    /// `fs_remove` (plugin ABI §5) removes a whole tree or one file and answers `E_GONE` when
    /// nothing is there; the host has already confined the path to `.schemify/`.
    #[test]
    fn fs_remove_removes_trees_and_files() {
        let root = std::env::temp_dir().join(format!("schemify-fsremove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tree = root.join(".schemify/pdks/sky130/v1");
        std::fs::create_dir_all(tree.join("lib/sky130_fd_pr")).unwrap();
        std::fs::write(tree.join("lib/sky130_fd_pr/a.shprim"), "x").unwrap();
        std::fs::write(tree.join("STATE"), "ok\n").unwrap();
        std::fs::write(root.join("schemify.project"), "schemify 1 project\n").unwrap();
        let rm = |p: &str| run_io(&root, IoOp::FsRemove, &[p.to_owned()], b"").0;
        assert_eq!(rm(".schemify/pdks/sky130/v1/STATE"), 0);
        assert!(!tree.join("STATE").exists() && tree.join("lib").exists());
        assert_eq!(rm(".schemify/pdks/sky130/v1"), 0);
        assert!(!tree.exists() && root.join(".schemify/pdks/sky130").exists());
        assert_eq!(
            rm(".schemify/pdks/sky130/v1"),
            Code::Gone.neg(),
            "nothing to remove"
        );
        assert!(root.join("schemify.project").exists(), "never touched");
    }

    /// `fs_read` and `fs_list` of `${NAME}/…` paths resolve under the variable's directory
    /// (plugin-abi §5, `read:<NAME>`); an unset variable or a missing directory is `E_GONE`;
    /// project paths still resolve under the root.
    #[test]
    fn env_paths_read_and_list_under_the_variable() {
        let gone = -(Code::Gone as i32);
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (status, bytes) = run_io(
            Path::new("/nowhere"),
            IoOp::FsRead,
            &["${CARGO_MANIFEST_DIR}/Cargo.toml".to_owned()],
            &[],
        );
        assert_eq!(status, 0);
        assert_eq!(bytes, std::fs::read(crate_dir.join("Cargo.toml")).unwrap());
        let (status, bytes) = run_io(
            Path::new("/nowhere"),
            IoOp::FsList,
            &["${CARGO_MANIFEST_DIR}/src".to_owned()],
            &[],
        );
        assert_eq!(status, 0);
        let list = String::from_utf8(bytes).unwrap();
        assert!(list.lines().any(|l| l == "plugins.rs"), "{list}");
        assert!(list.lines().any(|l| l == "editor/gestures.rs"), "{list}");
        // Path order: each directory's entries by name, depth first (`editor/…` before
        // `editor.rs`), as `platform::list_files` walks.
        let mut sorted: Vec<&Path> = list.lines().map(Path::new).collect();
        sorted.sort_unstable();
        assert_eq!(sorted, list.lines().map(Path::new).collect::<Vec<_>>());
        for missing in [
            "${SCHEMIFY_TEST_UNSET_VAR}/x",
            "${CARGO_MANIFEST_DIR}/no_such_dir",
        ] {
            let (status, _) = run_io(crate_dir, IoOp::FsList, &[missing.to_owned()], &[]);
            assert_eq!(status, gone, "{missing}");
        }
        let (status, _) = run_io(crate_dir, IoOp::FsRead, &["Cargo.toml".to_owned()], &[]);
        assert_eq!(status, 0, "project-relative reads are unchanged");
    }
}

#[cfg(test)]
mod eval_tests {
    use super::*;
    use schemify_elab::no_funcs;

    /// Oracle: a plugin's `eval` of cell names gives the values elab computed for them
    /// (`b = a + p = 4`), combined by the expression; names without a value and missing
    /// plugin functions are declared errors, never a panic.
    #[test]
    fn eval_reads_the_cell_scope() {
        let cell = "schemify 1 shcomp\n\npin P inout 0 0\n\nparam p 3\n\nimpl sch schematic\n\nport 0p0001 0 0 R0 P\n\nvar a 1\nvar b a+p\nvar bad 1/0\n";
        let mut project = Project::new(None, None, 1).unwrap();
        project
            .index_library("", "", &[("c.shcomp", cell.as_bytes())])
            .unwrap();
        let h = project
            .open("", "c.shcomp", cell.as_bytes(), None)
            .unwrap()
            .handle;
        let st = CellState::build(&project, h, &mut no_funcs).unwrap();
        let ev = |e: &str| eval_in(&st, e, &mut no_funcs);
        assert_eq!(ev("b"), Ok("4".to_owned()));
        assert_eq!(ev("b*2 + p"), Ok("11".to_owned()));
        assert_eq!(ev("b > a"), Ok("true".to_owned()));
        assert_eq!(ev("str(b) + \"V\""), Ok("\"4V\"".to_owned()));
        assert_eq!(ev("nope + 1").unwrap_err().code, XCode::X004);
        assert_eq!(
            ev("bad").unwrap_err().code,
            XCode::X004,
            "its error is the cell's"
        );
        assert_eq!(ev("gmid.width(1, 2, 3)").unwrap_err().code, XCode::X006);
        assert!(ev("1 +").is_err());
    }
}
