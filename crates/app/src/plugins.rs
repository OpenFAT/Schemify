//! Plugin host glue for the frame spine: loads the bundled default UI (D6) and the
//! manifest's local plugins in configured order (D8), fills snapshot tables from `doc`,
//! `elab` and `run`, runs `plugin::deliver`, and drains the request queues to their owners
//! (edits to `doc`, run requests to the editor's run queue, I/O to `platform`, logs to the
//! status line). User packages (D35, config spec §5) load between the default UI and the
//! project's plugins; lazy ones wait for their command or event. The user config's Lua
//! keymaps and hooks run here too, so their requests join the plugins' (config spec §6).
//!
//! I/O is inverted and never blocks the frame (plugin-abi §5): file reads and writes complete
//! within the delivery, a `spawn` runs on a worker ([`platform::spawn_start`]) and its
//! `IoDone` arrives in a later frame through [`Host::poll`]; timers fire the same way. While
//! either waits, [`Host::wake_ms`] asks the window for a tick. A plugin granted more fuel than
//! the default (a manifest or package `fuel` override) runs its events on a worker thread
//! (`Plugins::set_worker`), so a long import never freezes the editor; its outputs land in a
//! later frame. `eval` requests are answered within the delivery by [`eval_in`].

use std::path::Path;

use schemify_doc::{DocHandle, ObjId, Project, Sym};
use schemify_elab::{CellState, Funcs, Val};
use schemify_expr::{Code as XCode, Error as XError, Mode, Program, Scratch, Span, Value};
use schemify_plugin::abi::{Code, Event, FUEL_DEFAULT, Kind, Table, col};
use schemify_plugin::{
    IoOp, PluginEvents, PluginId, Plugins, RequestQueues, RunRequest, Snapshots, deliver,
};

use crate::platform::{self, Spawned};
use schemify_cli::pack::{self, Installed};
use schemify_config::{Config, Request, Startup};

/// Hex digits of a plugin content hash in a D20 `cached <value> <hash>` attribute: the
/// first 8 bytes of the module's sha256 (change detection, not authentication).
pub const HASH_HEX: usize = 16;

/// The bundled default UI plugin, built by `ui/build.sh`.
pub const DEFAULT_UI_WASM: &[u8] = include_bytes!("../assets/default_ui.wasm");

/// Delivery rounds per frame: a plugin's I/O completion is answered in the same frame, up to
/// this many request → completion hops.
const ROUNDS_MAX: u32 = 4;
/// Child processes and timers waiting at once (the plugin crate's per-frame queue
/// capacities, plugin-abi §10); a request past it completes at once with `E_SPACE`.
pub const WAITING_MAX: usize = 256;
/// Tick period while a child process runs (its completion is polled, not signalled).
const IO_POLL_MS: u32 = 20;

/// Snapshot groups [`Host::snapshot`] refills independently: Instances; Wires and Labels
/// without their net column; Nets with Violations; Results; the Wires and the Labels net
/// columns (a check pass renumbers nets without moving a wire).
const GROUPS: usize = 7;

/// Fills the snapshot groups flagged `stale` ([`GROUPS`]) from document `doc` (snapshot id
/// `id`), its state `cell` and the run's `results`; see [`Host::snapshot`].
fn fill_groups(
    s: &mut Snapshots,
    doc: &schemify_doc::Document,
    id: u32,
    cell: Option<&CellState>,
    results: &ResultRows,
    stale: [bool; GROUPS],
) {
    let st = doc.strings();
    let (inst, w, l) = (doc.instances(), doc.wires(), doc.labels());
    // Net ids are elab's component rows; u32::MAX = not elaborated.
    let nets_of = |col: &[u32], n: usize| match cell {
        Some(_) if col.len() == n => col.to_vec(),
        _ => vec![u32::MAX; n],
    };
    if stale[0] {
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
    }
    if stale[1] {
        let n = w.id.len();
        s.fill::<u32>(Table::Wires, col::wires::DOC, &vec![id; n]);
        let raw: Vec<u32> = w.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Wires, col::wires::ID, &raw);
        s.fill::<i32>(Table::Wires, col::wires::X1, &w.x1);
        s.fill::<i32>(Table::Wires, col::wires::Y1, &w.y1);
        s.fill::<i32>(Table::Wires, col::wires::X2, &w.x2);
        s.fill::<i32>(Table::Wires, col::wires::Y2, &w.y2);
    }
    if stale[5] {
        let n = w.id.len();
        s.fill::<u32>(
            Table::Wires,
            col::wires::NET,
            &nets_of(cell.map_or(&[][..], |c| &c.nets.wire_net), n),
        );
    }
    if stale[2] {
        let n = l.id.len();
        s.fill::<u32>(Table::Labels, col::labels::DOC, &vec![id; n]);
        let raw: Vec<u32> = l.id.iter().map(|i| i.0).collect();
        s.fill::<u32>(Table::Labels, col::labels::ID, &raw);
        s.fill::<i32>(Table::Labels, col::labels::X, &l.x);
        s.fill::<i32>(Table::Labels, col::labels::Y, &l.y);
        let texts: Vec<&str> = l.text.iter().map(|&t| st.get(t)).collect();
        s.fill_strings(Table::Labels, col::labels::TEXT, &texts);
    }
    if stale[6] {
        let n = l.id.len();
        s.fill::<u32>(
            Table::Labels,
            col::labels::NET,
            &nets_of(cell.map_or(&[][..], |c| &c.nets.label_net), n),
        );
    }
    if stale[3] {
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
    }
    if stale[4] {
        let jobs: Vec<u32> = results.iter().map(|r| r.0).collect();
        s.fill::<u32>(Table::Results, col::results::JOB, &jobs);
        let outs: Vec<&str> = results.iter().map(|r| r.1.as_str()).collect();
        s.fill_strings(Table::Results, col::results::OUTPUT, &outs);
        let vals: Vec<&str> = results.iter().map(|r| r.2.as_str()).collect();
        s.fill_strings(Table::Results, col::results::VALUE, &vals);
        let pass: Vec<u8> = results.iter().map(|r| r.3).collect();
        s.fill::<u8>(Table::Results, col::results::PASS, &pass);
    }
    debug_assert_eq!(s.check(), Ok(()));
}

/// A snapshot's debug text without its table epochs (they count fills, not content).
#[cfg(debug_assertions)]
fn without_epochs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("epoch: ") {
        out.push_str(&rest[..i]);
        rest = rest[i + 7..].trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

/// Folds `values` into fingerprint `h` (a multiply-xorshift per value: ~0.3 ns each, no
/// allocation; a fingerprint, not a cryptographic hash).
fn fold(mut h: u64, values: impl Iterator<Item = u32>) -> u64 {
    let mut n = 0u64;
    for v in values {
        h = (h ^ u64::from(v)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        h ^= h >> 29;
        n += 1;
    }
    (h ^ n).wrapping_mul(0xbf58_476d_1ce4_e5b9)
}

fn obj_ids(v: &[ObjId]) -> impl Iterator<Item = u32> + '_ {
    v.iter().map(|i| i.0)
}

fn sym_ids(v: &[Sym]) -> impl Iterator<Item = u32> + '_ {
    v.iter().map(|i| i.0)
}

fn as_u32(v: &[i32]) -> impl Iterator<Item = u32> + '_ {
    v.iter().map(|&x| x as u32)
}

/// The std hash of `v`.
fn hash_of(v: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

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
    /// Fingerprints of what each group of tables was filled from ([`GROUPS`], see
    /// [`Host::snapshot`]).
    group_keys: [Option<u64>; GROUPS],
    /// The document fingerprints (Instances, Wires, Labels) and what they were taken at: the
    /// snapshot id, edit epoch, string count and row counts of the document.
    doc_keys: Option<([u64; 5], [u64; 3])>,
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
    /// The user config (owner: `schemify_config`); its Lua runs only through this host.
    user: Config,
    /// User packages waiting for their first command or event (config spec §5 `lazy`).
    lazy: Vec<Installed>,
    /// Commands the user config queued, for the next delivery.
    user_cmds: Vec<(String, String)>,
    /// User config notifications and errors, for the next delivery.
    user_logs: Vec<String>,
    /// The user's `load` hooks ran.
    user_ready: bool,
    /// Recent editor events as their plugin-abi §6 wire text (`event doc_changed 3 7`),
    /// oldest first, at most [`NOTES_MAX`]: the command stream's notifications (D27).
    notes: std::collections::VecDeque<String>,
    /// Editor events recorded so far (the sequence number of the newest note).
    notes_seq: u64,
    /// Health faults already surfaced ([`schemify_plugin::Health::recorded`]).
    faults_seen: u64,
}

/// Editor events kept for the command stream ([`Host::notes_since`]); a client that falls
/// further behind misses the oldest.
pub const NOTES_MAX: usize = 256;

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
    /// Commands the user config queued (`schemify.cmd`): (name, args), to dispatch in order.
    pub commands: Vec<(String, String)>,
}

impl Host {
    /// Runs the user config `startup` names, then loads, in composition order (config spec
    /// §6): the bundled default UI (unless `no_default_ui`), the user's eager packages, and
    /// every manifest plugin with a `path` source (a `.wasm` file, or a directory holding
    /// `<name>.wasm`), granting what the manifest declares. Load failures and config errors
    /// are returned as messages; plugin failures are also in the health report.
    pub fn new(
        project: &Project,
        root: &Path,
        no_default_ui: bool,
        startup: &Startup,
    ) -> (Host, Vec<String>) {
        let mut plugins = Plugins::new();
        let mut notes = Vec::new();
        let mut hashes = Vec::new();
        // Through the platform: on the web `init.lua` lives in OPFS `/config` (web spec §5).
        let (user, startup_out) = Config::load_with(startup, |p| {
            platform::read(p)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
        });
        if !no_default_ui {
            match plugins.load("default-ui", DEFAULT_UI_WASM, &[], FUEL_DEFAULT) {
                Ok(id) => hashes.push((id, content_hash(DEFAULT_UI_WASM))),
                Err(c) => notes.push(format!("default-ui: {c:?}")),
            }
        }
        let mut lazy = Vec::new();
        if !user.table().packs.is_empty() {
            match pack::Paths::of(&user) {
                Ok(paths) => {
                    let (found, pack_notes) = pack::installed(user.table(), &paths);
                    notes.extend(pack_notes);
                    for p in found {
                        if p.lazy_cmd.is_empty() && p.lazy_event.is_empty() {
                            load_installed(&mut plugins, &mut hashes, &mut notes, &p);
                        } else {
                            lazy.push(p);
                        }
                    }
                }
                Err(e) => notes.push(format!("user packages: {e}")),
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
                    Ok(id) => {
                        let _ = plugins.set_worker(id, fuel > FUEL_DEFAULT);
                        hashes.push((id, content_hash(&wasm)));
                    }
                    Err(c) => notes.push(format!("plugin {name}: {c:?}")),
                },
                Err(e) => notes.push(format!("plugin {name}: {e}")),
            }
        }
        let faults_seen = plugins.health().recorded();
        let host = Host {
            plugins,
            events: PluginEvents::new(),
            snapshots: Snapshots::new(),
            snapshot_key: None,
            group_keys: [None; GROUPS],
            doc_keys: None,
            queues: RequestQueues::default(),
            seen_epoch: None,
            hashes,
            args: String::new(),
            result: String::new(),
            commanded: false,
            children: Vec::new(),
            timers: Vec::new(),
            user,
            lazy,
            user_cmds: Vec::new(),
            user_logs: Vec::new(),
            user_ready: false,
            notes: std::collections::VecDeque::new(),
            notes_seq: 0,
            // Load faults are in the startup notes already.
            faults_seen,
        };
        let mut host = host;
        // Startup errors go out with the load notes (all of them reach the status line).
        for r in startup_out {
            match r {
                Request::Command { name, args } => host.user_cmds.push((name, args)),
                Request::Notify { text, .. } => notes.push(text),
            }
        }
        assert_eq!(host.hashes.len(), host.plugins.ids().len());
        (host, notes)
    }

    /// The user-config table (options, keymaps, sims, packages, hooks).
    pub fn user(&self) -> &schemify_config::Table {
        self.user.table()
    }

    /// The user config itself: its root locates the package lockfile (`pack::Paths::of`).
    pub fn config(&self) -> &Config {
        &self.user
    }

    /// Runs a command nobody in the command table owns, if the user config or a lazy package
    /// does: a Lua keymap function (`user.lua.<id>`) runs now; a lazy package listing `name`
    /// loads and gets the command as `Event::Command`. Returns whether it was handled.
    pub fn user_command(&mut self, name: &str, args: &str) -> bool {
        if let Some(f) = schemify_config::fn_of_command(name) {
            let keys = self.user.table().keymaps.clone();
            let out = self.user.call(f);
            self.absorb(out, &keys);
            return true;
        }
        let Some(i) = self
            .lazy
            .iter()
            .position(|p| p.lazy_cmd.iter().any(|c| c == name))
        else {
            return false;
        };
        let p = self.lazy.remove(i);
        let mut notes = Vec::new();
        match load_installed(&mut self.plugins, &mut self.hashes, &mut notes, &p) {
            Some(id) => {
                let ev = Event::Command { name, args };
                if let Err(e) = self.push(Some(id), &ev) {
                    notes.push(e);
                }
            }
            None => notes.push(format!("{name}: its package did not load")),
        }
        self.user_logs.extend(notes);
        assert!(self.lazy.len() < schemify_config::PACKS_MAX);
        true
    }

    /// Queues the requests of a user-config call; recomposes the UI when the call changed the
    /// keymaps (`keys` is the table before it; a function may call `schemify.keymap.set`).
    fn absorb(&mut self, out: Vec<Request>, keys: &schemify_config::Keymaps) {
        for r in out {
            match r {
                Request::Command { name, args } => self.user_cmds.push((name, args)),
                Request::Notify { text, .. } => self.user_logs.push(text),
            }
        }
        if *keys != self.user.table().keymaps {
            self.seen_epoch = None;
        }
    }

    /// The editor events recorded after sequence number `seq` (a value [`Host::notes_seq`]
    /// returned), oldest first, as plugin-abi §6 wire text without the line end; only the
    /// last [`NOTES_MAX`] are kept.
    pub fn notes_since(&self, seq: u64) -> impl Iterator<Item = &str> {
        let new = self
            .notes_seq
            .saturating_sub(seq)
            .min(self.notes.len() as u64) as usize;
        self.notes
            .iter()
            .skip(self.notes.len() - new)
            .map(String::as_str)
    }

    /// The sequence number of the newest recorded editor event (0 before the first).
    pub fn notes_seq(&self) -> u64 {
        self.notes_seq
    }

    /// The user side of an editor event: records it for the command stream, loads the lazy
    /// packages that wait for it, then runs the user's hooks.
    fn user_event(&mut self, ev: &Event<'_>) {
        let (event, fields): (schemify_config::Event, Vec<(&str, i64)>) = match *ev {
            Event::DocOpened { doc } => {
                (schemify_config::Event::DocOpened, vec![("doc", doc.into())])
            }
            Event::DocClosed { doc } => {
                (schemify_config::Event::DocClosed, vec![("doc", doc.into())])
            }
            Event::DocChanged { doc, epoch } => (
                schemify_config::Event::DocChanged,
                vec![("doc", doc.into()), ("epoch", epoch.into())],
            ),
            Event::SelectionChanged => (schemify_config::Event::SelectionChanged, Vec::new()),
            Event::Saved { doc } => (schemify_config::Event::Saved, vec![("doc", doc.into())]),
            Event::RunDone { job } => (schemify_config::Event::RunDone, vec![("job", job.into())]),
            _ => return,
        };
        let mut wire = Vec::new();
        if ev.encode(&mut wire).is_ok() {
            if self.notes.len() == NOTES_MAX {
                self.notes.pop_front();
            }
            let line = String::from_utf8_lossy(&wire);
            self.notes.push_back(line.trim_end().to_owned());
            self.notes_seq += 1;
        }
        debug_assert!(self.notes.len() <= NOTES_MAX);
        self.wake_lazy(event);
        if self.user.table().hooks.iter().any(|h| h.0 == event) {
            let keys = self.user.table().keymaps.clone();
            let out = self.user.fire(event, &fields);
            self.absorb(out, &keys);
        }
    }

    /// Loads every lazy package waiting for `event` (its `Load` is delivered before the
    /// event that woke it, plugin-abi §6).
    fn wake_lazy(&mut self, event: schemify_config::Event) {
        let mut i = 0;
        while i < self.lazy.len() {
            if !self.lazy[i].lazy_event.contains(&event) {
                i += 1;
                continue;
            }
            let p = self.lazy.remove(i);
            let mut notes = Vec::new();
            load_installed(&mut self.plugins, &mut self.hashes, &mut notes, &p);
            self.user_logs.extend(notes);
        }
    }

    /// Whether a user package is still waiting to load (tests, status).
    pub fn is_lazy(&self, name: &str) -> bool {
        self.lazy.iter().any(|p| p.name == name)
    }

    /// The D20 function registry (`elab::Funcs`): calls `ns.f` through the composed
    /// `functions` contributions and `schemify_call`. `None` when no loaded plugin exports
    /// `name`, or its plugin's event is running on a worker (elab then falls back to the
    /// record's `cached` value, unverified, until the next recompute).
    ///
    /// Args travel as line-record values (strings quoted, numbers canonical); a number,
    /// `true`/`false` or a quoted string comes back ([`fn_result`]). Failures map to
    /// expression errors: wrong arity `X008`, a complex argument or a result that is none of
    /// those `X003`, a plugin error code `X010`.
    pub fn call_fn(&mut self, name: &str, args: &[Value<'_>]) -> Option<Result<Val, XError>> {
        let f = &self.plugins.composed().functions;
        let row = f.find(name)?;
        let (plugin, fn_id, arity) = (f.plugin[row], f.fn_id[row], f.arity[row]);
        if self.plugins.busy(plugin) {
            // Its event runs on a worker: the record's `cached` value stands in (D20).
            return None;
        }
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
            Ok(()) => Some(fn_result(self.result.trim()).ok_or_else(|| {
                let t = self.result.trim();
                XError::new(
                    XCode::X003,
                    Span::default(),
                    format!("{name} returned `{t}`: not a number, bool or quoted string"),
                )
            })),
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
        if target.is_none() {
            self.user_event(ev);
        }
        self.events
            .push(target, ev)
            .map_err(|c| format!("plugin event dropped: {c:?}"))
    }

    /// Returns whether a delivery has work (queued events or host-originated ones, user
    /// config requests, an event running on a worker to take in).
    pub fn has_work(&self) -> bool {
        !self.events.is_empty()
            || self.plugins.has_pending()
            || self.plugins.running() > 0
            || !self.user_ready
            || !self.user_cmds.is_empty()
            || !self.user_logs.is_empty()
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
    /// child or a plugin's worker event runs, else the nearest timer; `None` when nothing
    /// waits (idle CPU 0%, D39).
    pub fn wake_ms(&self, now_ms: u64) -> Option<u32> {
        let timer = self
            .timers
            .iter()
            .map(|t| t.0.saturating_sub(now_ms).min(u64::from(u32::MAX)) as u32)
            .min();
        let busy = !self.children.is_empty() || self.plugins.running() > 0;
        let io = busy.then_some(IO_POLL_MS);
        match (timer, io) {
            (Some(t), Some(i)) => Some(t.min(i)),
            (t, i) => t.or(i),
        }
    }

    /// Queues `RunDone { job }` for `plugin` (its `run` request's job finished or failed).
    pub fn run_done(&mut self, plugin: PluginId, job: u32) {
        let _ = self.events.push(Some(plugin), &Event::RunDone { job });
        self.user_event(&Event::RunDone { job });
    }

    /// Refreshes the snapshot tables plugins read for the active document `h` (Docs,
    /// Instances, Wires, Labels, Selection), its derived state `cell` (net ids of wires and
    /// labels, Nets, Violations; `u32::MAX` nets and empty tables without it) and the last
    /// run's `results` rows. `key` names everything but `selection` (document epochs, the
    /// check pass, the results): with the previous call's key only the Selection table is
    /// refilled. Otherwise each group of tables ([`GROUPS`]) is refilled only when its source
    /// changed: a fingerprint of the document's columns, the check pass, the rows. `epoch` is
    /// the document's edit epoch (`Session::epoch`, moved by every change of its tables): while
    /// it stands, the document fingerprints are reused, so a pass change (the frame after an
    /// edit) refills the net columns and the Nets group only. Cost: a hash pass over the
    /// instance, wire and label columns when `epoch` moved, plus one copy of the groups that
    /// changed (or of the selection).
    #[allow(clippy::too_many_arguments)]
    pub fn snapshot(
        &mut self,
        project: &Project,
        h: Option<DocHandle>,
        epoch: u32,
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
        let (w, l) = (doc.wires(), doc.labels());
        let pass = cell.map(|c| c.pass);
        // A group is refilled when its fingerprint moved (always on the first call).
        // ponytail: names are fingerprinted by symbol, with the string count: a reload whose
        // interner gave the same symbols to other strings at the same count would keep stale
        // names. Key on a per-document reload counter if that ever shows.
        // ponytail: the epoch stands for the document's columns (with its string and row counts
        // as a guard); a document that changed without moving its epoch would keep stale rows.
        let at = [
            u64::from(id),
            u64::from(epoch),
            st.len() as u64,
            inst.id.len() as u64,
            (w.id.len() as u64) << 32 | l.id.len() as u64,
        ];
        let doc_keys = match self.doc_keys {
            Some((was, keys)) if was == at => keys,
            _ => {
                let seed = hash_of((id, st.len()));
                let inst_key = {
                    let f = fold(
                        fold(fold(seed, obj_ids(&inst.id)), as_u32(&inst.x)),
                        as_u32(&inst.y),
                    );
                    let f = fold(fold(f, sym_ids(&inst.cell)), sym_ids(&inst.name));
                    fold(f, inst.orient.iter().map(|&o| o as u32))
                };
                let wire_key = {
                    let f = fold(fold(seed, obj_ids(&w.id)), as_u32(&w.x1));
                    fold(fold(fold(f, as_u32(&w.y1)), as_u32(&w.x2)), as_u32(&w.y2))
                };
                let label_key = {
                    let f = fold(fold(seed, obj_ids(&l.id)), as_u32(&l.x));
                    fold(fold(f, as_u32(&l.y)), sym_ids(&l.text))
                };
                [inst_key, wire_key, label_key]
            }
        };
        self.doc_keys = Some((at, doc_keys));
        let [inst_key, wire_key, label_key] = doc_keys;
        let keys = [
            inst_key,
            wire_key,
            label_key,
            hash_of((id, pass)),
            hash_of(results),
            hash_of((wire_key, pass)),
            hash_of((label_key, pass)),
        ];
        let mut stale = [false; GROUPS];
        for (g, k) in keys.into_iter().enumerate() {
            stale[g] = self.group_keys[g].replace(k) != Some(k);
        }
        fill_groups(&mut self.snapshots, doc, id, cell, results, stale);
        // Oracle (debug builds, small documents): the refilled groups equal a full refill.
        #[cfg(debug_assertions)]
        if inst.id.len() + w.id.len() + l.id.len() < 20_000 {
            let mut full = self.snapshots.clone();
            fill_groups(&mut full, doc, id, cell, results, [true; GROUPS]);
            debug_assert_eq!(
                without_epochs(&format!("{:?}", self.snapshots)),
                without_epochs(&format!("{full:?}")),
                "incremental snapshot == full refill"
            );
        }
        debug_assert_eq!(self.snapshots.check(), Ok(()));
    }

    /// The spine's `plugin::deliver` plus queue draining, at most `ROUNDS_MAX` (4) rounds:
    /// file I/O completes within the frame; `spawn` starts a child whose `IoDone` comes
    /// through [`Host::poll`]; timers are armed against `now_ms`; run requests go out in
    /// [`Delivered::runs`].
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
                    self.children.push((r.plugin, r.handle, child));
                    continue;
                }
                let (status, bytes) = if r.op == IoOp::Spawn {
                    let space = -(schemify_plugin::abi::Code::Space as i32);
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
                out.logs.push(f.detail.clone());
            }
        }
        self.faults_seen = health.recorded();
        assert!(states.len() <= ROUNDS_MAX as usize * schemify_plugin::EVALS_MAX);
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
        let Some(h) = handle_of(project, doc) else {
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
        let mut funcs = |name: &str, args: &[Value<'_>]| self.call_fn(name, args);
        match eval_in(st, expr, &mut funcs) {
            Ok(text) => (0, text),
            Err(e) => (Code::Parse.neg(), format!("{}: {}", e.code.as_str(), e.msg)),
        }
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
            .collect::<Vec<_>>();
        // The user's own keymaps go after every plugin (config spec §6).
        let mut sets = sets;
        let keys = self.user.table().keymaps.records();
        if !keys.is_empty() {
            sets.push(("user".to_owned(), keys));
        }
        Some(sets)
    }
}

/// Loads one installed user package with its effective grants and fuel; returns its id, or
/// `None` with a note.
fn load_installed(
    plugins: &mut Plugins,
    hashes: &mut Vec<(PluginId, String)>,
    notes: &mut Vec<String>,
    p: &Installed,
) -> Option<PluginId> {
    let wasm = match platform::read(&p.wasm) {
        Ok(w) => w,
        Err(e) => {
            notes.push(format!("plugin {}: {e}", p.name));
            return None;
        }
    };
    let grants: Vec<&str> = p.grants.iter().map(String::as_str).collect();
    let fuel = p.fuel.unwrap_or(FUEL_DEFAULT).clamp(FUEL_MIN, FUEL_MAX);
    match plugins.load(&p.name, &wasm, &grants, fuel) {
        Ok(id) => {
            let _ = plugins.set_worker(id, fuel > FUEL_DEFAULT);
            hashes.push((id, content_hash(&wasm)));
            Some(id)
        }
        Err(c) => {
            notes.push(format!("plugin {}: {c:?}", p.name));
            None
        }
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
        let (ns, f) = prog
            .plugin_fns()
            .nth(slot as usize)
            .expect("slot of this program");
        let name = format!("{ns}.{f}");
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
        other => Val::from_value(other).write_text(&mut out),
    }
    assert!(
        !out.is_empty() || matches!(value, Value::Str(_)),
        "values have text"
    );
    Ok(out)
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
        IoOp::FsRemove => {
            let path = root.join(&args[0]);
            if !platform::exists(&path) {
                let gone = -(schemify_plugin::abi::Code::Gone as i32);
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

/// `HASH_HEX` lowercase hex digits of the sha256 of `bytes`.
fn content_hash(bytes: &[u8]) -> String {
    let mut h = schemify_doc::hex(&schemify_doc::sha256(bytes));
    h.truncate(HASH_HEX);
    debug_assert_eq!(h.len(), HASH_HEX);
    h
}

/// Bounds on a manifest `fuel` override (plugin ABI §2). A plugin above the default runs its
/// events on a worker thread; the ceiling still bounds how long one event may run there.
// ponytail: on the web (no threads) such events still run on the frame thread and block it;
// slice them with wasmi's resumable calls (`call_resumable`, out-of-fuel) if the web needs it.
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

#[cfg(test)]
mod fuel_tests {
    use super::*;

    fn attr(v: &str) -> schemify_doc::Attr {
        schemify_doc::Attr {
            key: "fuel".into(),
            values: vec![v.into()],
        }
    }

    /// Plugin results: numbers, bools and quoted strings (escapes undone); a bare word, two
    /// values or an unterminated quote are not results.
    #[test]
    fn fn_results_are_numbers_bools_or_quoted_strings() {
        assert_eq!(fn_result("2.5e-6"), Some(Val::Num(2.5e-6)));
        assert_eq!(fn_result("true"), Some(Val::Bool(true)));
        assert_eq!(
            fn_result("\"nfet_01v8\""),
            Some(Val::Str("nfet_01v8".into()))
        );
        assert_eq!(fn_result(r#""a \"b\"""#), Some(Val::Str("a \"b\"".into())));
        assert_eq!(
            fn_result("\"12\""),
            Some(Val::Str("12".into())),
            "quoted: a string"
        );
        for bad in ["nfet", "\"a\" \"b\"", "\"open", "1 2", ""] {
            assert_eq!(fn_result(bad), None, "{bad:?}");
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
}
