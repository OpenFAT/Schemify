//! Plugin host glue for the frame spine: loads the bundled default UI (D6) and the
//! manifest's local plugins in configured order (D8), fills snapshot tables from `doc`,
//! `elab` and `run`, runs `plugin::deliver`, and drains the request queues to their owners
//! (edits to `doc`, run requests to the editor's run queue, I/O to `platform`, logs to the
//! status line).
//!
//! I/O is inverted and never blocks the frame (plugin-abi §5): file reads and writes complete
//! within the delivery, a `spawn` runs on a worker ([`platform::spawn_start`]) and its
//! `IoDone` arrives in a later frame through [`Host::poll`]; timers fire the same way. While
//! either waits, [`Host::wake_ms`] asks the window for a tick.

use std::path::Path;

use schemify_doc::{DocHandle, Project};
use schemify_elab::CellState;
use schemify_expr::{Code as XCode, Error as XError, Span, Value};
use schemify_plugin::abi::{Event, FUEL_DEFAULT, Kind, Table, col};
use schemify_plugin::{
    IoOp, PluginEvents, PluginId, Plugins, RequestQueues, RunRequest, Snapshots, deliver,
};

use crate::platform::{self, Spawned};

/// Hex digits of a plugin content hash in a D20 `cached <value> <hash>` attribute: the
/// first 8 bytes of the module's sha256 (change detection, not authentication).
pub const HASH_HEX: usize = 16;

/// The bundled default UI plugin, built by `plugins/build.sh`.
pub const DEFAULT_UI_WASM: &[u8] = include_bytes!("../assets/default_ui.wasm");

/// Delivery rounds per frame: a plugin's I/O completion is answered in the same frame, up to
/// this many request → completion hops.
const ROUNDS_MAX: u32 = 4;
/// Child processes and timers waiting at once (the plugin crate's per-frame queue
/// capacities, plugin-abi §10); a request past it completes at once with `E_SPACE`.
pub const WAITING_MAX: usize = 256;
/// Tick period while a child process runs (its completion is polled, not signalled).
const IO_POLL_MS: u32 = 20;

/// Results rows a run exposes to plugins (the `Results` snapshot): `(job, output, value
/// text, pass)`; `pass` is 1 pass, 2 fail, 0 unknown or no spec.
pub type ResultRows = Vec<(u32, String, String, u8)>;

/// Loaded plugins, their per-frame event table, snapshots and request queues.
pub struct Host {
    /// The plugin table (owner: `plugin`).
    pub plugins: Plugins,
    /// Events queued for the next delivery.
    pub events: PluginEvents,
    snapshots: Snapshots,
    /// What the document tables in `snapshots` were filled from (see [`Host::snapshot`]).
    snapshot_key: Option<u64>,
    queues: RequestQueues,
    /// Epoch of the last composition the app consumed.
    pub seen_epoch: Option<u32>,
    /// Content hash ([`HASH_HEX`] hex digits) of each loaded plugin's module, by id.
    hashes: Vec<(PluginId, String)>,
    args: String,
    result: String,
    /// A command event waits for delivery (see [`Delivered::state_changed`]).
    commanded: bool,
    /// Child processes started for `spawn` requests: (plugin, handle, child).
    children: Vec<(PluginId, u32, Spawned)>,
    /// Pending timers: (due, in the editor's monotonic ms; plugin; timer id).
    timers: Vec<(u64, PluginId, u32)>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("plugins", &self.plugins.ids().len())
            .finish()
    }
}

/// What a delivery produced for the rest of the spine.
#[derive(Debug, Default)]
pub struct Delivered {
    /// Plugin edit transactions: (document, label, op text, stage?).
    pub edits: Vec<(DocHandle, String, String, bool)>,
    /// Log lines for the status line.
    pub logs: Vec<String>,
    /// A plugin ran a command or got an I/O completion: its pure functions may answer
    /// differently now (a LUT loaded), so derived values must be recomputed.
    pub state_changed: bool,
    /// Plugin `run` requests (plugin-abi §7) for the editor's run queue.
    pub runs: Vec<RunRequest>,
}

impl Host {
    /// Loads the bundled default UI (unless `no_default_ui`) and then every manifest plugin
    /// with a `path` source (a `.wasm` file, or a directory holding `<name>.wasm`), granting
    /// what the manifest declares. Load failures are returned as messages and recorded in
    /// the plugin health report.
    pub fn new(project: &Project, root: &Path, no_default_ui: bool) -> (Host, Vec<String>) {
        let mut plugins = Plugins::new();
        let mut notes = Vec::new();
        let mut hashes = Vec::new();
        if !no_default_ui {
            match plugins.load("default-ui", DEFAULT_UI_WASM, &[], FUEL_DEFAULT) {
                Ok(id) => hashes.push((id, content_hash(DEFAULT_UI_WASM))),
                Err(c) => notes.push(format!("default-ui: {c:?}")),
            }
        }
        for rec in project.plugins() {
            let name = rec.fields.first().map_or("", String::as_str);
            let (Some("path"), Some(path)) =
                (rec.fields.get(1).map(String::as_str), rec.fields.get(2))
            else {
                notes.push(format!("plugin {name}: only path sources load in M2"));
                continue;
            };
            let mut file = root.join(path);
            if platform::is_dir(&file) {
                file = file.join(format!("{name}.wasm"));
            }
            let grants: Vec<&str> = rec
                .attrs
                .iter()
                .filter(|a| a.key == "grant")
                .filter_map(|a| a.values.first().map(String::as_str))
                .collect();
            let fuel = match manifest_fuel(rec.attrs.iter().find(|a| a.key == "fuel")) {
                Ok(f) => f,
                Err(e) => {
                    notes.push(format!("plugin {name}: {e}; using the default fuel"));
                    FUEL_DEFAULT
                }
            };
            match platform::read(&file) {
                Ok(wasm) => match plugins.load(name, &wasm, &grants, fuel) {
                    Ok(id) => hashes.push((id, content_hash(&wasm))),
                    Err(c) => notes.push(format!("plugin {name}: {c:?}")),
                },
                Err(e) => notes.push(format!("plugin {name}: {e}")),
            }
        }
        let host = Host {
            plugins,
            events: PluginEvents::new(),
            snapshots: Snapshots::new(),
            snapshot_key: None,
            queues: RequestQueues::default(),
            seen_epoch: None,
            hashes,
            args: String::new(),
            result: String::new(),
            commanded: false,
            children: Vec::new(),
            timers: Vec::new(),
        };
        assert_eq!(host.hashes.len(), host.plugins.ids().len());
        (host, notes)
    }

    /// The D20 function registry (`elab::Funcs`): calls `ns.f` through the composed
    /// `functions` contributions and `schemify_call`. `None` when no loaded plugin exports
    /// `name` (elab then falls back to the record's `cached` value, unverified).
    ///
    /// Args travel as line-record values (strings quoted, numbers canonical); a number or
    /// `true`/`false` comes back. Failures map to expression errors: wrong arity `X008`, a
    /// complex or string argument or a non-numeric result `X003`, a plugin error code `X010`.
    pub fn call_fn(
        &mut self,
        name: &str,
        args: &[Value<'_>],
    ) -> Option<Result<Value<'static>, XError>> {
        let f = &self.plugins.composed().functions;
        let row = f.find(name)?;
        let (plugin, fn_id, arity) = (f.plugin[row], f.fn_id[row], f.arity[row]);
        let err = |code, msg: String| Some(Err(XError::new(code, Span::default(), msg)));
        if args.len() != usize::from(arity) {
            return err(
                XCode::X008,
                format!("{name} takes {arity} arguments, got {}", args.len()),
            );
        }
        self.args.clear();
        for (k, a) in args.iter().enumerate() {
            if k > 0 {
                self.args.push(' ');
            }
            match *a {
                Value::Num(x) => schemify_expr::write_num(&mut self.args, x),
                Value::Int(i) => schemify_expr::write_int(&mut self.args, i),
                Value::Bool(b) => self.args.push_str(if b { "true" } else { "false" }),
                Value::Str(s) => self.args.push_str(&crate::docs::quote(s)),
                Value::Cnum { .. } => {
                    return err(
                        XCode::X003,
                        format!("{name}: complex arguments unsupported"),
                    );
                }
            }
        }
        match self
            .plugins
            .call(plugin, fn_id, &self.args, &mut self.result)
        {
            Ok(()) => Some(match self.result.trim() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                t => schemify_expr::parse_num(t).map(Value::Num).map_err(|_| {
                    // ponytail: string results need an owned `Value`; numbers cover gm/Id.
                    XError::new(
                        XCode::X003,
                        Span::default(),
                        format!("{name} returned non-numeric `{t}`"),
                    )
                }),
            }),
            Err(code) => err(XCode::X010, format!("{name}: plugin reports {code:?}")),
        }
    }

    /// D20 cache hash of an expression calling `fns` (sorted, distinct `ns.f` names): the
    /// content hash of the one plugin serving them, or of their hashes joined when several
    /// do. `None` when any function is not loaded.
    pub fn fn_hash(&self, fns: &[String]) -> Option<String> {
        let f = &self.plugins.composed().functions;
        let mut owners: Vec<PluginId> = Vec::with_capacity(fns.len());
        for name in fns {
            owners.push(f.plugin[f.find(name)?]);
        }
        owners.sort_unstable();
        owners.dedup();
        let hashes: Vec<&str> = owners
            .iter()
            .filter_map(|id| self.hashes.iter().find(|(h, _)| h == id))
            .map(|(_, h)| h.as_str())
            .collect();
        match hashes.as_slice() {
            [] => None,
            [one] => Some((*one).to_owned()),
            many => Some(content_hash(many.join("").as_bytes())),
        }
    }

    /// Queues an event (`None` = broadcast). A full table drops it with a note.
    pub fn push(&mut self, target: Option<PluginId>, ev: &Event<'_>) -> Result<(), String> {
        self.commanded |= matches!(ev, Event::Command { .. });
        self.events
            .push(target, ev)
            .map_err(|c| format!("plugin event dropped: {c:?}"))
    }

    /// Returns whether a delivery has work (queued events or host-originated ones).
    pub fn has_work(&self) -> bool {
        !self.events.is_empty() || self.plugins.has_pending()
    }

    /// Queues the completions that arrived since the last call: `IoDone` for every child
    /// process that exited, `Timer` for every timer due at `now_ms`. Returns whether any
    /// event was queued. Cost: one channel poll per running child.
    pub fn poll(&mut self, now_ms: u64) -> bool {
        let before = self.events.len();
        let mut i = 0;
        while i < self.children.len() {
            let Some((status, bytes)) = self.children[i].2.try_finish() else {
                i += 1;
                continue;
            };
            let (plugin, handle, _) = self.children.swap_remove(i);
            let ev = Event::IoDone {
                handle,
                status,
                bytes: &bytes,
            };
            let _ = self.events.push(Some(plugin), &ev);
            self.commanded = true; // a completion may change what a pure function answers
        }
        let mut k = 0;
        while k < self.timers.len() {
            if self.timers[k].0 > now_ms {
                k += 1;
                continue;
            }
            let (_, plugin, id) = self.timers.swap_remove(k);
            let _ = self.events.push(Some(plugin), &Event::Timer { id });
        }
        assert!(self.children.len() <= WAITING_MAX && self.timers.len() <= WAITING_MAX);
        self.events.len() > before
    }

    /// Milliseconds until the next completion should be looked for: 20 ms while a
    /// child runs, else the nearest timer; `None` when nothing waits (idle CPU 0%, D39).
    pub fn wake_ms(&self, now_ms: u64) -> Option<u32> {
        let timer = self
            .timers
            .iter()
            .map(|t| t.0.saturating_sub(now_ms).min(u64::from(u32::MAX)) as u32)
            .min();
        let io = (!self.children.is_empty()).then_some(IO_POLL_MS);
        match (timer, io) {
            (Some(t), Some(i)) => Some(t.min(i)),
            (t, i) => t.or(i),
        }
    }

    /// Queues `RunDone { job }` for `plugin` (its `run` request's job finished or failed).
    pub fn run_done(&mut self, plugin: PluginId, job: u32) {
        let _ = self.events.push(Some(plugin), &Event::RunDone { job });
    }

    /// Refreshes the snapshot tables plugins read for the active document `h` (Docs,
    /// Instances, Wires, Labels, Selection), its derived state `cell` (net ids of wires and
    /// labels, Nets, Violations; `u32::MAX` nets and empty tables without it) and the last
    /// run's `results` rows. `key` names everything but `selection` (document epochs, the
    /// check pass, the results): with the previous call's key only the Selection table is
    /// refilled. Cost: one copy of those columns, or of the selection.
    pub fn snapshot(
        &mut self,
        project: &Project,
        h: Option<DocHandle>,
        selection: &[u32],
        cell: Option<&CellState>,
        results: &ResultRows,
        key: u64,
    ) {
        let s = &mut self.snapshots;
        let id = h.map_or(0, doc_id);
        s.fill::<u32>(
            Table::Selection,
            col::selection::DOC,
            &vec![id; selection.len()],
        );
        s.fill::<u32>(Table::Selection, col::selection::ID, selection);
        if self.snapshot_key.replace(key) == Some(key) {
            debug_assert_eq!(s.check(), Ok(()));
            return; // a selection change only (a click): the document tables stand
        }
        let handles: Vec<DocHandle> = project.handles().collect();
        let ids: Vec<u32> = handles.iter().map(|&d| doc_id(d)).collect();
        s.fill::<u32>(Table::Docs, col::docs::HANDLE, &ids);
        let kinds: Vec<u8> = handles
            .iter()
            .map(|&d| project.doc(d).map_or(0, |x| x.kind() as u8))
            .collect();
        s.fill::<u8>(Table::Docs, col::docs::KIND, &kinds);
        let dirty: Vec<u8> = handles
            .iter()
            .map(|&d| u8::from(project.is_dirty(d)))
            .collect();
        s.fill::<u8>(Table::Docs, col::docs::DIRTY, &dirty);
        let empty: Vec<&str> = vec![""; handles.len()];
        s.fill_strings(Table::Docs, col::docs::PATH, &empty);
        s.fill_strings(Table::Docs, col::docs::CELL, &empty);
        let Some(doc) = h.and_then(|h| project.doc(h)) else {
            return;
        };
        let id = doc_id(h.expect("doc implies handle"));
        let st = doc.strings();
        let inst = doc.instances();
        let n = inst.id.len();
        s.fill::<u32>(Table::Instances, col::instances::DOC, &vec![id; n]);
        let raw: Vec<u32> = inst.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Instances, col::instances::ID, &raw);
        s.fill::<i32>(Table::Instances, col::instances::X, &inst.x);
        s.fill::<i32>(Table::Instances, col::instances::Y, &inst.y);
        let o: Vec<u8> = inst.orient.iter().map(|&o| o as u8).collect();
        s.fill::<u8>(Table::Instances, col::instances::ORIENT, &o);
        let cells: Vec<&str> = inst.cell.iter().map(|&c| st.get(c)).collect();
        s.fill_strings(Table::Instances, col::instances::CELL, &cells);
        let names: Vec<&str> = inst.name.iter().map(|&c| st.get(c)).collect();
        s.fill_strings(Table::Instances, col::instances::NAME, &names);
        let w = doc.wires();
        let n = w.id.len();
        s.fill::<u32>(Table::Wires, col::wires::DOC, &vec![id; n]);
        let raw: Vec<u32> = w.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Wires, col::wires::ID, &raw);
        s.fill::<i32>(Table::Wires, col::wires::X1, &w.x1);
        s.fill::<i32>(Table::Wires, col::wires::Y1, &w.y1);
        s.fill::<i32>(Table::Wires, col::wires::X2, &w.x2);
        s.fill::<i32>(Table::Wires, col::wires::Y2, &w.y2);
        // Net ids are elab's component rows; u32::MAX = not elaborated.
        let nets_of = |col: &[u32], n: usize| match cell {
            Some(_) if col.len() == n => col.to_vec(),
            _ => vec![u32::MAX; n],
        };
        s.fill::<u32>(
            Table::Wires,
            col::wires::NET,
            &nets_of(cell.map_or(&[][..], |c| &c.nets.wire_net), n),
        );
        let l = doc.labels();
        let n = l.id.len();
        s.fill::<u32>(Table::Labels, col::labels::DOC, &vec![id; n]);
        let raw: Vec<u32> = l.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Labels, col::labels::ID, &raw);
        s.fill::<i32>(Table::Labels, col::labels::X, &l.x);
        s.fill::<i32>(Table::Labels, col::labels::Y, &l.y);
        s.fill::<u32>(
            Table::Labels,
            col::labels::NET,
            &nets_of(cell.map_or(&[][..], |c| &c.nets.label_net), n),
        );
        let texts: Vec<&str> = l.text.iter().map(|&t| st.get(t)).collect();
        s.fill_strings(Table::Labels, col::labels::TEXT, &texts);
        let empty = CellState::default();
        let c = cell.unwrap_or(&empty);
        let nets = &c.nets;
        let names: Vec<&str> = nets.net_name.iter().map(|&n| c.names.get(n)).collect();
        s.fill_strings(Table::Nets, col::nets::NAME, &names);
        s.fill::<u16>(Table::Nets, col::nets::WIDTH, &nets.net_width);
        let global: Vec<u8> = nets.net_global.iter().map(|&g| u8::from(g)).collect();
        s.fill::<u8>(Table::Nets, col::nets::GLOBAL, &global);
        s.fill::<u32>(Table::Nets, col::nets::PIN_COUNT, &nets.net_pins);
        let v = &c.violations.rows;
        let codes: Vec<&str> = v.iter().map(|r| r.code.as_str()).collect();
        s.fill_strings(Table::Violations, col::violations::CODE, &codes);
        let sev: Vec<u8> = v.iter().map(|r| r.code.severity() as u8).collect();
        s.fill::<u8>(Table::Violations, col::violations::SEVERITY, &sev);
        let obj: Vec<u32> = v.iter().map(|r| r.object.0).collect();
        s.fill::<u32>(Table::Violations, col::violations::OBJECT, &obj);
        let none = schemify_elab::Name::NONE;
        let param: Vec<&str> = v
            .iter()
            .map(|r| {
                if r.param == none {
                    ""
                } else {
                    c.names.get(r.param)
                }
            })
            .collect();
        s.fill_strings(Table::Violations, col::violations::PARAM, &param);
        let jobs: Vec<u32> = results.iter().map(|r| r.0).collect();
        s.fill::<u32>(Table::Results, col::results::JOB, &jobs);
        let outs: Vec<&str> = results.iter().map(|r| r.1.as_str()).collect();
        s.fill_strings(Table::Results, col::results::OUTPUT, &outs);
        let vals: Vec<&str> = results.iter().map(|r| r.2.as_str()).collect();
        s.fill_strings(Table::Results, col::results::VALUE, &vals);
        let pass: Vec<u8> = results.iter().map(|r| r.3).collect();
        s.fill::<u8>(Table::Results, col::results::PASS, &pass);
        debug_assert_eq!(s.check(), Ok(()));
    }

    /// The spine's `plugin::deliver` plus queue draining, at most `ROUNDS_MAX` (4) rounds:
    /// file I/O completes within the frame; `spawn` starts a child whose `IoDone` comes
    /// through [`Host::poll`]; timers are armed against `now_ms`; run requests go out in
    /// [`Delivered::runs`].
    pub fn deliver(&mut self, project: &Project, root: &Path, now_ms: u64) -> Delivered {
        let mut out = Delivered {
            state_changed: std::mem::take(&mut self.commanded),
            ..Delivered::default()
        };
        for _ in 0..ROUNDS_MAX {
            if !self.has_work() {
                break;
            }
            deliver(
                &mut self.plugins,
                &self.events,
                &mut self.snapshots,
                &mut self.queues,
            );
            self.events.clear();
            for e in self.queues.edits.drain(..) {
                let Some(h) = handle_of(project, e.doc) else {
                    out.logs
                        .push(format!("{}: edit for a closed document", e.plugin));
                    continue;
                };
                let stage = e.mode == schemify_plugin::abi::EditMode::Stage;
                out.edits.push((h, e.label, e.ops, stage));
            }
            for l in self.queues.logs.drain(..) {
                let name = self.plugins.name(l.plugin).unwrap_or("plugin").to_owned();
                out.logs.push(format!("{name}: {}", l.text));
            }
            for t in self.queues.timers.drain(..) {
                if self.timers.len() < WAITING_MAX {
                    self.timers.push((now_ms + u64::from(t.ms), t.plugin, t.id));
                } else {
                    out.logs.push(format!("{}: too many timers", t.plugin));
                }
            }
            out.runs.append(&mut self.queues.runs);
            let evals: Vec<_> = self.queues.evals.drain(..).collect();
            for e in evals {
                // elab integration point: no evaluator yet.
                let gone = -(schemify_plugin::abi::Code::Gone as i32);
                let ev = Event::IoDone {
                    handle: e.handle,
                    status: gone,
                    bytes: b"",
                };
                let _ = self.events.push(Some(e.plugin), &ev);
            }
            let io: Vec<_> = self.queues.io.drain(..).collect();
            out.state_changed |= !io.is_empty();
            for r in io {
                if r.op == IoOp::Spawn && self.children.len() < WAITING_MAX {
                    let child = platform::spawn_start(root, r.args, r.data);
                    self.children.push((r.plugin, r.handle, child));
                    continue;
                }
                let (status, bytes) = if r.op == IoOp::Spawn {
                    let space = -(schemify_plugin::abi::Code::Space as i32);
                    (space, b"too many child processes".to_vec())
                } else {
                    run_io(root, r.op, &r.args, &r.data)
                };
                let ev = Event::IoDone {
                    handle: r.handle,
                    status,
                    bytes: &bytes,
                };
                let _ = self.events.push(Some(r.plugin), &ev);
            }
        }
        out
    }

    /// Returns `(plugin name, ui set + keymap set)` per plugin in load order, when the
    /// composition changed since the last call.
    pub fn ui_sets(&mut self) -> Option<Vec<(String, String)>> {
        let epoch = self.plugins.composed_epoch();
        if self.seen_epoch == Some(epoch) {
            return None;
        }
        self.seen_epoch = Some(epoch);
        let c = self.plugins.contributions();
        let sets = self
            .plugins
            .ids()
            .iter()
            .map(|&id| {
                let mut text = c.get(id, Kind::Ui, "").unwrap_or("").to_owned();
                text.push('\n');
                text.push_str(c.get(id, Kind::Keymap, "").unwrap_or(""));
                (self.plugins.name(id).unwrap_or("?").to_owned(), text)
            })
            .collect();
        Some(sets)
    }
}

/// Plugin-visible document id: slot index in the low 16 bits, generation in the high 16
/// (`doc::DOCS_MAX` is 2^16), so a handle kept across a close is detected as gone.
pub fn doc_id(h: DocHandle) -> u32 {
    assert!(h.index < 1 << 16);
    (h.generation << 16) | h.index
}

/// The live handle a plugin's document id names.
pub fn handle_of(project: &Project, id: u32) -> Option<DocHandle> {
    project.handles().find(|&h| doc_id(h) == id)
}

/// Executes an authorized I/O request natively (project-relative paths were checked by the
/// host). Returns the `IoDone` status and bytes.
fn run_io(root: &Path, op: IoOp, args: &[String], data: &[u8]) -> (i32, Vec<u8>) {
    let denied = -(schemify_plugin::abi::Code::Denied as i32);
    match op {
        IoOp::FsRead => match platform::read(&root.join(&args[0])) {
            Ok(b) => (0, b),
            Err(e) => (-(schemify_plugin::abi::Code::Gone as i32), e.into_bytes()),
        },
        IoOp::FsWrite => match platform::write_atomic(&root.join(&args[0]), data) {
            Ok(()) => (0, Vec::new()),
            Err(e) => (denied, e.into_bytes()),
        },
        IoOp::Spawn => unreachable!("spawn runs through platform::spawn_start"),
        // ponytail: fetch and clipboard are M3; answer "denied".
        IoOp::Fetch | IoOp::ClipboardGet | IoOp::ClipboardSet => {
            (denied, b"not supported yet".to_vec())
        }
    }
}

/// `HASH_HEX` lowercase hex digits of the sha256 of `bytes`.
fn content_hash(bytes: &[u8]) -> String {
    let mut h = schemify_doc::hex(&schemify_doc::sha256(bytes));
    h.truncate(HASH_HEX);
    debug_assert_eq!(h.len(), HASH_HEX);
    h
}

/// Bounds on a manifest `fuel` override (plugin ABI §2). The ceiling keeps one event from
/// freezing the editor indefinitely: wasmi runs on the frame thread.
// ponytail: events run synchronously on the frame; a plugin that needs 10^10+ fuel (large
// cktimg imports) still blocks the UI for seconds. Upgrade: run long events on a worker.
const FUEL_MIN: u64 = 1_000_000;
const FUEL_MAX: u64 = 100_000_000_000;

/// The per-plugin fuel budget from a manifest `fuel <n>` attribute, or the default when
/// absent.
///
/// # Errors
///
/// The value is not a whole number in `[FUEL_MIN, FUEL_MAX]`; the caller reports it and
/// falls back to the default rather than trusting a project file's number.
fn manifest_fuel(attr: Option<&schemify_doc::Attr>) -> Result<u64, String> {
    let Some(attr) = attr else {
        return Ok(FUEL_DEFAULT);
    };
    let text = attr.values.first().map_or("", String::as_str);
    let fuel: u64 = text
        .parse()
        .map_err(|_| format!("fuel `{text}` is not a whole number"))?;
    if !(FUEL_MIN..=FUEL_MAX).contains(&fuel) {
        return Err(format!("fuel {fuel} is outside [{FUEL_MIN}, {FUEL_MAX}]"));
    }
    assert!(fuel > 0);
    Ok(fuel)
}

#[cfg(test)]
mod fuel_tests {
    use super::*;

    fn attr(v: &str) -> schemify_doc::Attr {
        schemify_doc::Attr {
            key: "fuel".into(),
            values: vec![v.into()],
        }
    }

    #[test]
    fn manifest_fuel_defaults_parses_and_rejects() {
        assert_eq!(manifest_fuel(None), Ok(FUEL_DEFAULT));
        assert_eq!(
            manifest_fuel(Some(&attr("50000000000"))),
            Ok(50_000_000_000)
        );
        assert_eq!(manifest_fuel(Some(&attr("1000000"))), Ok(FUEL_MIN));
        assert!(manifest_fuel(Some(&attr("999999"))).is_err());
        assert!(manifest_fuel(Some(&attr("100000000001"))).is_err());
        assert!(manifest_fuel(Some(&attr("1e9"))).is_err());
        assert!(manifest_fuel(Some(&attr("-5"))).is_err());
        assert!(manifest_fuel(Some(&attr(""))).is_err());
    }
}
