//! Plugin host (WS6): loads WASM plugins with wasmi (D3), delivers events, enforces
//! capabilities (D7), composes contributions in plugin order with last-wins semantics and
//! a health report (D8), and turns plugin outputs into request tables (D3a). Spec:
//! `docs/spec/plugin-abi.md`.
//!
//! Frame-spine position: `plugin::deliver(plugins, plugin_events, snapshots) → request_queues`.
//! The crate does no OS I/O and builds for `wasm32-unknown-unknown`: file, process, network
//! and clipboard requests become [`IoRequest`] rows the app executes, and completions come
//! back as `Event::IoDone`.
//!
//! # Tables (owner: this crate; others read, or drain request queues)
//!
//! | Table | Rows | Capacity | Lifetime |
//! |---|---|---|---|
//! | [`Plugins`] (plugins, grants, stores) | loaded plugins; row order = composition order | [`PLUGINS_MAX`] | session |
//! | [`Contributions`] | (plugin, kind, scope) → current set | [`CONTRIBUTIONS_MAX`] | session |
//! | [`Composed`] | surviving records + function registry, derived on change | — | session |
//! | [`RequestQueues`] | requests made this frame | `*_MAX` per queue | frame, drained by owners |
//! | [`Health`] | faults (ring) + current overrides | [`FAULTS_MAX`] | session |
//!
//! # Examples
//!
//! The app spine, per frame (plugins are loaded once at session start):
//!
//! ```no_run
//! use schemify_plugin::{deliver, Plugins, PluginEvents, RequestQueues, Snapshots};
//! use schemify_plugin::abi::{Event, FUEL_DEFAULT};
//!
//! # fn main() -> Result<(), schemify_plugin::abi::Code> {
//! # let wasm: Vec<u8> = Vec::new();
//! let mut plugins = Plugins::new();
//! let hello = plugins.load("hello", &wasm, &["read_project"], FUEL_DEFAULT)?;
//! let (mut events, mut snapshots, mut queues) =
//!     (PluginEvents::new(), Snapshots::new(), RequestQueues::default());
//!
//! events.push(None, &Event::DocChanged { doc: 1, epoch: 2 })?;
//! events.push(Some(hello), &Event::Command { name: "count_nets.run", args: "" })?;
//! deliver(&mut plugins, &events, &mut snapshots, &mut queues);
//! events.clear();
//! for edit in queues.edits.drain(..) { /* doc::apply_edits */ }
//! for io in queues.io.drain(..) { /* app executes; later pushes Event::IoDone to io.plugin */ }
//! # Ok(()) }
//! ```

mod compose;
mod intake;
mod runtime;
mod snapshot;

use std::collections::VecDeque;

use schemify_abi::{Cap, Code, EditMode, Event, Level};
use wasmi::{Engine, Linker, Store};

pub use compose::{Composed, ComposedRecord, Functions, Override, compose};
pub use intake::{Contributions, Rejection, authorize, intake};
pub use schemify_abi as abi;
pub use snapshot::{ROWS_MAX, Snapshots};

use runtime::{Ctx, Exports};

/// Loaded plugins at once (ARCHITECTURE: 2^8; `u32::MAX` is never an id).
pub const PLUGINS_MAX: usize = 255;
/// Events per [`PluginEvents`] table (one frame).
pub const EVENTS_MAX: usize = 1 << 12;
/// Bytes of one encoded event, payload included.
pub const EVENT_BYTES_MAX: usize = 64 << 20;
/// Bytes of one event's output buffer; larger outputs abort the event with `E_SPACE`.
pub const OUTPUT_BYTES_MAX: usize = 16 << 20;
/// Bytes of one contribution set.
pub const CONTRIBUTION_BYTES_MAX: usize = 1 << 20;
/// Rows of [`Contributions`] across all plugins.
pub const CONTRIBUTIONS_MAX: usize = 1 << 12;
/// Host-originated events (`Load`, `Rejected`) queued per plugin between deliveries.
pub const PENDING_MAX: usize = 64;
/// Faults kept in [`Health`]; older ones are dropped and counted.
pub const FAULTS_MAX: usize = 1 << 10;
/// Queue capacities; a record or import that would overflow one is refused with `E_SPACE`.
pub const EDITS_MAX: usize = 1 << 10;
/// See [`EDITS_MAX`].
pub const RUNS_MAX: usize = 1 << 8;
/// See [`EDITS_MAX`].
pub const TIMERS_MAX: usize = 1 << 8;
/// See [`EDITS_MAX`].
pub const LOGS_MAX: usize = 1 << 10;
/// See [`EDITS_MAX`].
pub const IO_MAX: usize = 1 << 8;
/// See [`EDITS_MAX`].
pub const EVALS_MAX: usize = 1 << 8;
/// Bytes of one log message; longer ones are truncated at a character boundary.
pub const LOG_BYTES_MAX: usize = 4 << 10;
/// Bytes of the data argument of one I/O request (file contents, request body, stdin).
pub const IO_BYTES_MAX: usize = 64 << 20;
/// Longest timer delay: one day.
pub const TIMER_MS_MAX: u32 = 86_400_000;
/// Bytes of the args of one `call`.
pub const CALL_ARGS_BYTES_MAX: usize = 64 << 10;
/// Bytes of a rejected record echoed in [`Rejection`] and `Event::Rejected`.
pub const REJECT_ECHO_BYTES: usize = 256;
/// Aborted calls in a row (fuel, trap, bad output buffer; events and pure calls alike) after
/// which a plugin is quarantined: dropped like an unload, without `Unload` (§2).
pub const QUARANTINE_AFTER: u8 = 8;
/// Events waiting, per plugin, behind its event running on a worker ([`Plugins::set_worker`]).
pub const DEFERRED_MAX: usize = 1 << 10;

/// Identifies a loaded plugin. Assigned at load, never reused in a session, so a stale id
/// (an event for an unloaded plugin) is detected, never misdelivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginId(pub u32);

impl std::fmt::Display for PluginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin#{}", self.0)
    }
}

/// Events for one [`deliver`] call, already encoded (one encoding per event, shared by
/// every recipient of a broadcast).
#[derive(Clone, Debug, Default)]
pub struct PluginEvents {
    target: Vec<u32>,
    ends: Vec<u32>,
    bytes: Vec<u8>,
}

const BROADCAST: u32 = u32::MAX;

impl PluginEvents {
    /// Returns an empty table.
    pub fn new() -> PluginEvents {
        PluginEvents::default()
    }

    /// Appends `event` for `target` (`None` = every plugin, in plugin order).
    ///
    /// # Errors
    ///
    /// `E_SPACE` (nothing appended) at [`EVENTS_MAX`] events or when the encoding exceeds
    /// [`EVENT_BYTES_MAX`]; `E_PARSE` when an identifier field is not a bare value; `E_GONE`
    /// for `PluginId(u32::MAX)`, which is never assigned.
    pub fn push(&mut self, target: Option<PluginId>, event: &Event<'_>) -> Result<(), Code> {
        if self.target.len() >= EVENTS_MAX {
            return Err(Code::Space);
        }
        if target == Some(PluginId(BROADCAST)) {
            return Err(Code::Gone);
        }
        let before = self.bytes.len();
        event.encode(&mut self.bytes)?;
        let end = u32::try_from(self.bytes.len()).ok();
        if self.bytes.len() - before > EVENT_BYTES_MAX || end.is_none() {
            self.bytes.truncate(before);
            return Err(Code::Space);
        }
        self.target.push(target.map_or(BROADCAST, |id| id.0));
        self.ends.push(end.expect("checked above"));
        debug_assert_eq!(self.target.len(), self.ends.len());
        Ok(())
    }

    /// Returns the number of events.
    pub fn len(&self) -> usize {
        self.target.len()
    }

    /// Returns whether there are no events.
    pub fn is_empty(&self) -> bool {
        self.target.is_empty()
    }

    /// Drops every event, keeping capacity.
    pub fn clear(&mut self) {
        self.target.clear();
        self.ends.clear();
        self.bytes.clear();
    }

    fn get(&self, i: usize) -> (Option<PluginId>, &[u8]) {
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        let target = (self.target[i] != BROADCAST).then_some(PluginId(self.target[i]));
        (target, &self.bytes[start..self.ends[i] as usize])
    }
}

/// An edit transaction for `doc::apply_edits` (edits spec §1). The host checked only that
/// `doc` was live; op validation belongs to `doc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditRequest {
    /// Requesting plugin.
    pub plugin: PluginId,
    /// Document handle.
    pub doc: u32,
    /// Commit or stage.
    pub mode: EditMode,
    /// Undo-menu label.
    pub label: String,
    /// Op lines (line-record text).
    pub ops: String,
}

/// A simulation job for `run`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRequest {
    /// Requesting plugin; `Event::RunDone` goes back to it.
    pub plugin: PluginId,
    /// Bench as `lib:cell`.
    pub bench: String,
    /// Named config.
    pub config: String,
    /// Corner.
    pub corner: String,
    /// Variable overrides.
    pub vary: Vec<(String, String)>,
}

/// A timer: the app sends `Event::Timer { id }` to `plugin` after `ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerRequest {
    /// Requesting plugin.
    pub plugin: PluginId,
    /// Plugin-chosen id.
    pub id: u32,
    /// Delay, at most [`TIMER_MS_MAX`].
    pub ms: u32,
}

/// A log line for the app's log panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    /// Source plugin.
    pub plugin: PluginId,
    /// Severity.
    pub level: Level,
    /// Message, at most [`LOG_BYTES_MAX`] bytes.
    pub text: String,
}

/// Inverted I/O operations (§5); each maps to one capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IoOp {
    /// Read a project-relative file (`read_project`). `args = [path]`.
    FsRead,
    /// Write a project-relative file (`write_project`). `args = [path]`, `data` = bytes.
    FsWrite,
    /// Run a binary (`spawn:<argv[0]>`, native only). `args` = argv, `data` = stdin.
    Spawn,
    /// HTTP request (`network`). `args = [url]`, `data` = body.
    Fetch,
    /// Read the clipboard (`clipboard`). No args.
    ClipboardGet,
    /// Write the clipboard (`clipboard`). `data` = text.
    ClipboardSet,
}

impl IoOp {
    /// Returns the capability the operation needs.
    pub const fn cap(self) -> Cap {
        match self {
            IoOp::FsRead => Cap::ReadProject,
            IoOp::FsWrite => Cap::WriteProject,
            IoOp::Spawn => Cap::Spawn,
            IoOp::Fetch => Cap::Network,
            IoOp::ClipboardGet | IoOp::ClipboardSet => Cap::Clipboard,
        }
    }
}

/// An authorized I/O request for the app to execute. The app answers with
/// `Event::IoDone { handle, .. }` targeted at `plugin`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IoRequest {
    /// Requesting plugin.
    pub plugin: PluginId,
    /// Handle the import returned to the plugin (unique per plugin, never reused).
    pub handle: u32,
    /// Operation.
    pub op: IoOp,
    /// Path, URL or argv (see [`IoOp`]).
    pub args: Vec<String>,
    /// Payload, at most [`IO_BYTES_MAX`] bytes.
    pub data: Vec<u8>,
}

/// An `eval` request: the app evaluates `expr` in `doc`'s cell scope with `expr`/`elab` and
/// answers with `Event::IoDone { handle, status, bytes: value text }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalRequest {
    /// Requesting plugin.
    pub plugin: PluginId,
    /// Handle returned to the plugin (same space as I/O handles).
    pub handle: u32,
    /// Document handle, live when requested.
    pub doc: u32,
    /// Expression source.
    pub expr: String,
}

/// Requests made by plugins, one table per owner. Only [`deliver`] / [`intake`] append;
/// each owner drains its queue (`Vec::drain`) once per frame. Every queue is bounded by its
/// `*_MAX` constant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestQueues {
    /// For `doc::apply_edits`.
    pub edits: Vec<EditRequest>,
    /// For `run`.
    pub runs: Vec<RunRequest>,
    /// For the app's timer wheel.
    pub timers: Vec<TimerRequest>,
    /// For the log panel.
    pub logs: Vec<LogEntry>,
    /// For the app's platform layer (fs / OPFS, subprocess, fetch, clipboard).
    pub io: Vec<IoRequest>,
    /// For `expr`/`elab` evaluation.
    pub evals: Vec<EvalRequest>,
}

/// One declared error attributed to a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// The plugin (for a failed load: the id it would have had).
    pub plugin: PluginId,
    /// What went wrong.
    pub code: Code,
    /// Human-readable context.
    pub detail: String,
}

/// Health report (D8): recent faults and the overrides of the current composition.
#[derive(Clone, Debug, Default)]
pub struct Health {
    faults: VecDeque<Fault>,
    dropped: u32,
    overrides: Vec<Override>,
}

impl Health {
    /// Returns the most recent faults, oldest first, at most [`FAULTS_MAX`].
    pub fn faults(&self) -> &VecDeque<Fault> {
        &self.faults
    }

    /// Returns how many faults were dropped to keep the ring bounded (saturating).
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// Returns every record of the current composition that a later plugin overrode.
    pub fn overrides(&self) -> &[Override] {
        &self.overrides
    }

    fn fault(&mut self, plugin: PluginId, code: Code, detail: String) {
        if self.faults.len() == FAULTS_MAX {
            self.faults.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.faults.push_back(Fault {
            plugin,
            code,
            detail,
        });
        debug_assert!(self.faults.len() <= FAULTS_MAX);
    }
}

/// Capabilities granted to one plugin (§5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    caps: u8,
    spawn: Vec<Box<str>>,
}

impl Grants {
    /// Parses manifest grants (`read_project`, `spawn:ngspice`, …).
    ///
    /// # Errors
    ///
    /// `E_PARSE` for any grant [`Cap::parse_grant`] rejects.
    pub fn parse(grants: &[&str]) -> Result<Grants, Code> {
        let mut g = Grants::default();
        for grant in grants {
            let (cap, arg) = Cap::parse_grant(grant).ok_or(Code::Parse)?;
            g.caps |= 1 << cap as u8;
            if cap == Cap::Spawn && !g.spawn.iter().any(|b| &**b == arg) {
                g.spawn.push(arg.into());
            }
        }
        debug_assert_eq!(g.has(Cap::Spawn), !g.spawn.is_empty());
        Ok(g)
    }

    /// Returns whether `cap` is granted (for `spawn`: for at least one binary).
    pub fn has(&self, cap: Cap) -> bool {
        self.caps & (1 << cap as u8) != 0
    }

    /// Returns whether spawning exactly `binary` is granted.
    pub fn may_spawn(&self, binary: &str) -> bool {
        self.spawn.iter().any(|b| &**b == binary)
    }
}

/// A host-originated event waiting for the next [`deliver`]; `echo` is false for
/// `Rejected`, so rejections caused while handling a rejection are not echoed again.
#[derive(Debug)]
struct Pending {
    bytes: Vec<u8>,
    echo: bool,
}

/// What a worker hands back ([`Plugins::set_worker`]): the plugin's store and the event's
/// outcome and output, taken in by the next [`deliver`] after it finished.
struct Done {
    store: Store<Ctx>,
    outcome: Result<(), Code>,
    output: Vec<u8>,
    event: Vec<u8>,
    echo: bool,
}

/// The plugin table plus everything derived from plugins: contributions, composition,
/// health. Row order is load order, which is the configured order (D8: last wins).
pub struct Plugins {
    engine: Engine,
    linker: Linker<Ctx>,
    ids: Vec<PluginId>,
    names: Vec<Box<str>>,
    fuel: Vec<u64>,
    stores: Vec<Store<Ctx>>,
    exports: Vec<Exports>,
    pending: Vec<VecDeque<Pending>>,
    /// Events that arrived while the row's event ran on a worker, in arrival order.
    deferred: Vec<VecDeque<Pending>>,
    /// Aborted calls in a row and the last abort's code (quarantine at [`QUARANTINE_AFTER`]).
    strikes: Vec<(u8, Code)>,
    /// Rows whose events run on a worker thread.
    worker: Vec<bool>,
    /// The row's event running on a worker; its store is then away and an idle one stands in.
    jobs: Vec<Option<std::thread::JoinHandle<Done>>>,
    jobs_done: u32,
    next_id: u32,
    contributions: Contributions,
    composed: Composed,
    composed_epoch: u32,
    health: Health,
    scratch: Vec<u8>,
    rejections: Vec<Rejection>,
}

impl Default for Plugins {
    fn default() -> Plugins {
        Plugins::new()
    }
}

impl std::fmt::Debug for Plugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugins")
            .field("ids", &self.ids)
            .field("names", &self.names)
            .finish_non_exhaustive()
    }
}

impl Plugins {
    /// Returns an empty table with a fuel-metering wasmi engine and the §3 imports linked.
    pub fn new() -> Plugins {
        let engine = runtime::engine();
        let linker = runtime::linker(&engine);
        Plugins {
            engine,
            linker,
            ids: Vec::new(),
            names: Vec::new(),
            fuel: Vec::new(),
            stores: Vec::new(),
            exports: Vec::new(),
            pending: Vec::new(),
            deferred: Vec::new(),
            strikes: Vec::new(),
            worker: Vec::new(),
            jobs: Vec::new(),
            jobs_done: 0,
            next_id: 1,
            contributions: Contributions::new(),
            composed: Composed::default(),
            composed_epoch: 0,
            health: Health::default(),
            scratch: Vec::new(),
            rejections: Vec::new(),
        }
    }

    /// Validates, links and instantiates a module, checks its ABI version, and queues
    /// `Event::Load` for the next [`deliver`]. The new plugin composes after every plugin
    /// already loaded. `fuel` is the budget per event and per `call`
    /// ([`abi::FUEL_DEFAULT`] unless the user config overrides it).
    ///
    /// # Errors
    ///
    /// Each also recorded in [`Health`]: `E_SPACE` at [`PLUGINS_MAX`]; `E_PARSE` for a bad
    /// grant; `E_MODULE` for invalid wasm, unknown imports, a missing or mistyped export;
    /// `E_FUEL` / `E_TRAP` if the start function or `schemify_abi` fails; `E_VERSION` for an
    /// ABI mismatch.
    ///
    /// # Panics
    ///
    /// Panics if `fuel == 0` or after `u32::MAX - 1` loads in one session.
    pub fn load(
        &mut self,
        name: &str,
        wasm: &[u8],
        grants: &[&str],
        fuel: u64,
    ) -> Result<PluginId, Code> {
        assert!(fuel > 0, "a zero fuel budget can run nothing");
        assert!(self.next_id < BROADCAST, "plugin ids exhausted");
        let id = PluginId(self.next_id);
        self.next_id += 1;
        let loaded = if self.ids.len() >= PLUGINS_MAX {
            Err((Code::Space, "plugin table full".to_owned()))
        } else {
            Grants::parse(grants)
                .map_err(|code| (code, format!("bad grant in {grants:?}")))
                .and_then(|g| {
                    runtime::instantiate(&self.engine, &self.linker, wasm, Ctx::new(id, g), fuel)
                })
        };
        let (store, exports) = match loaded {
            Ok(loaded) => loaded,
            Err((code, detail)) => {
                self.health
                    .fault(id, code, format!("load {name}: {detail}"));
                return Err(code);
            }
        };
        let mut load = Vec::new();
        Event::Load.encode(&mut load).expect("Load encodes");
        self.ids.push(id);
        self.names.push(name.into());
        self.fuel.push(fuel);
        self.stores.push(store);
        self.exports.push(exports);
        self.pending.push(VecDeque::from([Pending {
            bytes: load,
            echo: true,
        }]));
        self.deferred.push(VecDeque::new());
        self.strikes.push((0, Code::Trap));
        self.worker.push(false);
        self.jobs.push(None);
        debug_assert!(self.ids.len() <= PLUGINS_MAX && self.ids.len() == self.stores.len());
        debug_assert!(self.jobs.len() == self.ids.len() && self.strikes.len() == self.ids.len());
        Ok(id)
    }

    /// Delivers `Event::Unload` (requests it makes are kept, contributions are not), then
    /// drops the plugin, its memory and its contributions, and recomposes. A plugin whose
    /// event runs on a worker is waited for first (its outputs are taken in); events still
    /// waiting for it are dropped.
    ///
    /// # Errors
    ///
    /// `E_GONE` if `id` is not loaded.
    ///
    /// # Panics
    ///
    /// Panics if `snapshots` fails [`Snapshots::check`].
    pub fn unload(
        &mut self,
        id: PluginId,
        snapshots: &mut Snapshots,
        queues: &mut RequestQueues,
    ) -> Result<(), Code> {
        assert_eq!(snapshots.check(), Ok(()), "snapshot columns disagree");
        let row = self.row(id).ok_or(Code::Gone)?;
        self.poll_job(row, true, snapshots, queues);
        self.worker[row] = false; // `Unload` runs here, on the caller's thread
        let mut unload = Vec::new();
        Event::Unload.encode(&mut unload).expect("Unload encodes");
        self.deliver_one(row, &unload, false, snapshots, queues);
        self.remove_row(row);
        debug_assert!(self.row(id).is_none());
        Ok(())
    }

    /// Runs `id`'s events on a worker thread from its next event on, so a long event (a
    /// large import under a fuel override far above [`abi::FUEL_DEFAULT`]) never blocks the
    /// caller's frame. Each such event completes in a later [`deliver`]: its outputs are
    /// taken in there, before the plugin's next event, and are the same as if it had run on
    /// the caller's thread (it reads a copy of the snapshots its event was delivered with).
    /// Events for the plugin that arrive meanwhile wait in order (at most [`DEFERRED_MAX`];
    /// more are dropped with an `E_SPACE` fault) and [`Plugins::call`] answers `E_DENIED`.
    /// Composition stays in plugin order (D8) whenever the outputs arrive. On wasm32 (no
    /// threads) this does nothing: events stay on the caller's thread.
    ///
    /// # Errors
    ///
    /// `E_GONE` if `id` is not loaded.
    pub fn set_worker(&mut self, id: PluginId, on: bool) -> Result<(), Code> {
        let row = self.row(id).ok_or(Code::Gone)?;
        self.worker[row] = on && cfg!(not(target_arch = "wasm32"));
        debug_assert!(!self.worker[row] || on);
        Ok(())
    }

    /// Returns how many plugins have an event running on a worker; [`deliver`] takes each in
    /// once it finished, so the caller keeps delivering (on a timer) while this is non-zero.
    pub fn running(&self) -> usize {
        self.jobs.iter().filter(|j| j.is_some()).count()
    }

    /// Returns whether `id` has an event running on a worker (its pure functions are
    /// unavailable until it finishes).
    pub fn busy(&self, id: PluginId) -> bool {
        self.row(id).is_some_and(|row| self.jobs[row].is_some())
    }

    /// Returns a counter that advances (wrapping) whenever [`deliver`] or
    /// [`Plugins::unload`] takes in an event that ran on a worker.
    pub fn jobs_done(&self) -> u32 {
        self.jobs_done
    }

    /// Calls pure expression function `fn_id` of `plugin` (D20) with `args` (line-record
    /// values text, e.g. `10 1e-6 "x"`) and writes the canonical result text into `result`.
    /// Every import is denied during the call; fuel is the plugin's per-event budget.
    ///
    /// # Errors
    ///
    /// `E_GONE` for an unloaded plugin; `E_DENIED` while its event runs on a worker
    /// ([`Plugins::set_worker`]); `E_SPACE` for args over [`CALL_ARGS_BYTES_MAX`]; whatever
    /// code the function returns; `E_FUEL` / `E_TRAP` / `E_RANGE` / `E_PARSE` when the guest
    /// misbehaves (those are also recorded in [`Health`], and an abort counts toward
    /// [`QUARANTINE_AFTER`]: the plugin may be gone after this call).
    pub fn call(
        &mut self,
        plugin: PluginId,
        fn_id: u32,
        args: &str,
        result: &mut String,
    ) -> Result<(), Code> {
        let row = self.row(plugin).ok_or(Code::Gone)?;
        if self.jobs[row].is_some() {
            return Err(Code::Denied); // its store is on a worker
        }
        if args.len() > CALL_ARGS_BYTES_MAX {
            return Err(Code::Space);
        }
        let store = &mut self.stores[row];
        debug_assert!(!store.data().live);
        let outcome = runtime::run_call(
            store,
            &self.exports[row],
            fn_id,
            args.as_bytes(),
            self.fuel[row],
            &mut self.scratch,
        );
        for (code, detail) in store.data_mut().faults.drain(..) {
            self.health.fault(plugin, code, detail);
        }
        match outcome {
            Err(code) => {
                self.health
                    .fault(plugin, code, format!("call {fn_id} aborted"));
                self.strike(row, code);
                self.quarantine();
                Err(code)
            }
            Ok(Err(code)) => {
                self.strikes[row].0 = 0;
                Err(code)
            }
            Ok(Ok(())) => {
                self.strikes[row].0 = 0;
                let text = std::str::from_utf8(&self.scratch).map_err(|_| Code::Parse)?;
                result.clear();
                result.push_str(text);
                Ok(())
            }
        }
    }

    /// Returns loaded plugin ids in composition order.
    pub fn ids(&self) -> &[PluginId] {
        &self.ids
    }

    /// Returns the name a plugin was loaded under.
    pub fn name(&self, id: PluginId) -> Option<&str> {
        self.row(id).map(|row| &*self.names[row])
    }

    /// Returns the grants of a loaded plugin.
    pub fn grants(&self, id: PluginId) -> Option<&Grants> {
        self.row(id).map(|row| &self.stores[row].data().grants)
    }

    /// Returns every plugin's current contribution sets.
    pub fn contributions(&self) -> &Contributions {
        &self.contributions
    }

    /// Returns the composition of all contributions (D8). Recomputed by [`deliver`] and
    /// [`Plugins::unload`] when [`Plugins::composed_epoch`] moves.
    pub fn composed(&self) -> &Composed {
        &self.composed
    }

    /// Returns a value that changes whenever [`Plugins::composed`] changes.
    pub fn composed_epoch(&self) -> u32 {
        self.composed_epoch
    }

    /// Returns the health report.
    pub fn health(&self) -> &Health {
        &self.health
    }

    /// Returns whether host-originated events (`Load`, `Rejected`) wait for the next
    /// [`deliver`]; the app calls `deliver` when this or its own event table is non-empty.
    pub fn has_pending(&self) -> bool {
        self.pending
            .iter()
            .chain(&self.deferred)
            .any(|p| !p.is_empty())
    }

    fn row(&self, id: PluginId) -> Option<usize> {
        self.ids.iter().position(|&i| i == id)
    }

    /// Runs one event through one plugin and takes its outputs in. A fault (fuel, trap, bad
    /// output buffer) discards every output and staged request of the event. A plugin on a
    /// worker gets the event on its thread; a busy one gets it after its running event.
    fn deliver_one(
        &mut self,
        row: usize,
        event: &[u8],
        echo: bool,
        snapshots: &mut Snapshots,
        queues: &mut RequestQueues,
    ) {
        if self.jobs[row].is_some() {
            self.defer(row, event, echo);
            return;
        }
        if self.worker[row] {
            self.start_job(row, event, echo, snapshots, queues);
            return;
        }
        let store = &mut self.stores[row];
        let ctx = store.data_mut();
        assert!(!ctx.live && ctx.staged.is_empty() && snapshots.is_lent());
        std::mem::swap(&mut ctx.snapshots, snapshots);
        ctx.live = true;
        ctx.room = room(queues);
        let mut output = std::mem::take(&mut self.scratch);
        let outcome = runtime::run_event(
            store,
            &self.exports[row],
            event,
            self.fuel[row],
            &mut output,
        );
        let ctx = store.data_mut();
        ctx.live = false;
        std::mem::swap(&mut ctx.snapshots, snapshots);
        self.take_in(row, outcome, event, &output, echo, snapshots, queues);
        self.scratch = output;
    }

    /// Takes in what one event of row `row` produced: faults, then (unless it aborted) its
    /// staged requests and output records; rejections are echoed per §8.
    #[allow(clippy::too_many_arguments)]
    fn take_in(
        &mut self,
        row: usize,
        outcome: Result<(), Code>,
        event: &[u8],
        output: &[u8],
        echo: bool,
        snapshots: &Snapshots,
        queues: &mut RequestQueues,
    ) {
        let id = self.ids[row];
        let ctx = self.stores[row].data_mut();
        assert!(!ctx.live, "taken in after the call returned");
        let staged = std::mem::take(&mut ctx.staged);
        for (code, detail) in ctx.faults.drain(..) {
            self.health.fault(id, code, detail);
        }
        if let Err(code) = outcome {
            let kind =
                String::from_utf8_lossy(event.split(|&b| b == b'\n').next().unwrap_or_default())
                    .into_owned();
            self.health
                .fault(id, code, format!("{kind}: aborted, outputs discarded"));
            self.strike(row, code);
            return;
        }
        self.strikes[row].0 = 0;
        queues.io.extend(staged.io);
        queues.evals.extend(staged.evals);
        queues.logs.extend(staged.logs);
        intake(
            id,
            output,
            snapshots,
            &mut self.contributions,
            queues,
            &mut self.rejections,
        );
        for r in self.rejections.drain(..) {
            self.health.fault(
                id,
                r.code,
                format!("rejected: {}", String::from_utf8_lossy(&r.record)),
            );
            if !echo {
                continue;
            }
            if self.pending[row].len() >= PENDING_MAX {
                self.health
                    .fault(id, Code::Space, "Rejected event dropped".to_owned());
                continue;
            }
            let mut bytes = Vec::new();
            Event::Rejected {
                code: r.code,
                record: &r.record,
            }
            .encode(&mut bytes)
            .expect("Rejected encodes");
            self.pending[row].push_back(Pending { bytes, echo: false });
        }
        // A worker reserved its room when it started; the spine drains the queues every
        // frame, so at most one more event's worth can sit on top.
        debug_assert!(queues.io.len() <= 2 * IO_MAX && queues.logs.len() <= 2 * LOGS_MAX);
    }

    /// Queues an event behind row `row`'s running worker event.
    fn defer(&mut self, row: usize, event: &[u8], echo: bool) {
        assert!(self.jobs[row].is_some(), "only a busy plugin defers");
        if self.deferred[row].len() >= DEFERRED_MAX {
            let detail = "event dropped: too many wait for its worker".to_owned();
            self.health.fault(self.ids[row], Code::Space, detail);
            return;
        }
        self.deferred[row].push_back(Pending {
            bytes: event.to_vec(),
            echo,
        });
    }

    /// Moves row `row`'s store to a worker thread that runs `event` with a copy of
    /// `snapshots`; an idle store (same grants, no instance) stands in until it returns.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses a thread (`std::thread::spawn`).
    fn start_job(
        &mut self,
        row: usize,
        event: &[u8],
        echo: bool,
        snapshots: &Snapshots,
        queues: &RequestQueues,
    ) {
        assert!(self.jobs[row].is_none() && self.worker[row]);
        let id = self.ids[row];
        let grants = self.stores[row].data().grants.clone();
        let idle = Store::new(&self.engine, Ctx::new(id, grants));
        let mut store = std::mem::replace(&mut self.stores[row], idle);
        let ctx = store.data_mut();
        assert!(!ctx.live && ctx.staged.is_empty() && snapshots.is_lent());
        ctx.snapshots = snapshots.clone();
        ctx.live = true;
        ctx.room = room(queues);
        let exports = self.exports[row].clone();
        let fuel = self.fuel[row];
        let event = event.to_vec();
        self.jobs[row] = Some(std::thread::spawn(move || {
            let mut output = Vec::new();
            let outcome = runtime::run_event(&mut store, &exports, &event, fuel, &mut output);
            let ctx = store.data_mut();
            ctx.live = false;
            ctx.snapshots = Snapshots::placeholder();
            Done {
                store,
                outcome,
                output,
                event,
                echo,
            }
        }));
    }

    /// Takes in row `row`'s worker event once it finished (`wait`: blocks until it does).
    /// Returns whether one was taken in.
    ///
    /// # Panics
    ///
    /// Re-raises a panic of the worker (a host bug: no guest input panics the host).
    fn poll_job(
        &mut self,
        row: usize,
        wait: bool,
        snapshots: &Snapshots,
        queues: &mut RequestQueues,
    ) -> bool {
        match &self.jobs[row] {
            Some(job) if wait || job.is_finished() => {}
            _ => return false,
        }
        let job = self.jobs[row].take().expect("matched above");
        let done = match job.join() {
            Ok(done) => done,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        self.stores[row] = done.store;
        self.jobs_done = self.jobs_done.wrapping_add(1);
        self.take_in(
            row,
            done.outcome,
            &done.event,
            &done.output,
            done.echo,
            snapshots,
            queues,
        );
        debug_assert!(self.jobs[row].is_none());
        true
    }

    /// Counts one aborted call of row `row`.
    fn strike(&mut self, row: usize, code: Code) {
        let s = &mut self.strikes[row];
        s.0 = s.0.saturating_add(1);
        s.1 = code;
    }

    /// Drops every plugin with [`QUARANTINE_AFTER`] aborted calls in a row, recording why.
    fn quarantine(&mut self) {
        for row in (0..self.ids.len()).rev() {
            let (count, code) = self.strikes[row];
            if count < QUARANTINE_AFTER {
                continue;
            }
            let id = self.ids[row];
            let detail = format!(
                "{} quarantined: {count} aborted calls in a row, the last {code:?}",
                self.names[row]
            );
            self.health.fault(id, Code::Gone, detail);
            self.remove_row(row);
        }
        debug_assert!(self.strikes.iter().all(|s| s.0 < QUARANTINE_AFTER));
    }

    /// Drops row `row` and its contributions, and recomposes. A worker still running for it
    /// is detached; its result is dropped when it ends.
    fn remove_row(&mut self, row: usize) {
        let id = self.ids.remove(row);
        self.names.remove(row);
        self.fuel.remove(row);
        self.stores.remove(row);
        self.exports.remove(row);
        self.pending.remove(row);
        self.deferred.remove(row);
        self.strikes.remove(row);
        self.worker.remove(row);
        self.jobs.remove(row);
        self.contributions.remove_plugin(id);
        self.recompose();
        debug_assert!(self.row(id).is_none() && self.jobs.len() == self.ids.len());
    }

    fn recompose(&mut self) {
        if self.contributions.epoch() == self.composed_epoch {
            return;
        }
        let (composed, overrides) = compose(&self.contributions, &self.ids);
        self.composed = composed;
        self.health.overrides = overrides;
        self.composed_epoch = self.contributions.epoch();
    }
}

/// Queue room an event may fill through imports; they refuse with `E_SPACE` at zero.
fn room(queues: &RequestQueues) -> runtime::Room {
    runtime::Room {
        io: IO_MAX.saturating_sub(queues.io.len()),
        evals: EVALS_MAX.saturating_sub(queues.evals.len()),
        logs: LOGS_MAX.saturating_sub(queues.logs.len()),
    }
}

/// The frame-spine transform: takes in the events that finished on workers, delivers
/// queued host events (`Load`, `Rejected`) and the events that waited for a worker, then
/// `events` in order (broadcasts to every plugin in composition order), takes each
/// plugin's outputs into `queues`, quarantines plugins that aborted [`QUARANTINE_AFTER`]
/// calls in a row, and recomposes contributions if any set changed.
///
/// Never fails: every plugin misbehaviour is a declared [`Fault`] in [`Plugins::health`],
/// and an event for an unloaded (or quarantined) plugin is an `E_GONE` fault. `snapshots` is borrowed
/// mutably only to lend it to each plugin's store; it is returned unchanged.
///
/// # Panics
///
/// Panics if `snapshots` fails [`Snapshots::check`] (a caller bug, not a plugin's).
///
/// # Time complexity
///
/// One guest call per (event, recipient); each bounded by the recipient's fuel budget.
pub fn deliver(
    plugins: &mut Plugins,
    events: &PluginEvents,
    snapshots: &mut Snapshots,
    queues: &mut RequestQueues,
) {
    assert_eq!(snapshots.check(), Ok(()), "snapshot columns disagree");
    assert!(events.len() <= EVENTS_MAX);
    for row in 0..plugins.ids.len() {
        plugins.poll_job(row, false, snapshots, queues);
        // Host events first, then the events that waited for the worker, in arrival order;
        // whatever meets a new worker event waits again, in the same order.
        let mut queue = std::mem::take(&mut plugins.pending[row]);
        queue.append(&mut plugins.deferred[row]);
        for p in queue {
            plugins.deliver_one(row, &p.bytes, p.echo, snapshots, queues);
        }
    }
    for i in 0..events.len() {
        let (target, bytes) = events.get(i);
        match target {
            None => {
                for row in 0..plugins.ids.len() {
                    plugins.deliver_one(row, bytes, true, snapshots, queues);
                }
            }
            Some(id) => match plugins.row(id) {
                Some(row) => plugins.deliver_one(row, bytes, true, snapshots, queues),
                None => plugins.health.fault(
                    id,
                    Code::Gone,
                    "event for a plugin that is not loaded".to_owned(),
                ),
            },
        }
    }
    plugins.quarantine();
    plugins.recompose();
    debug_assert!(plugins.composed_epoch == plugins.contributions.epoch());
}
