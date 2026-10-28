//! The plugin table and the frame-spine transform: modules, grants and events in →
//! request queues, contribution sets, the composition and health out. Owns every store;
//! one event of one plugin runs at a time on the caller's thread, or on a worker thread for
//! a plugin [`Plugins::set_worker`] moved there. Cites plugin-abi §2 (load, quarantine,
//! fuel), §6 (pure calls), §8 (rejection echo), D8 (plugin order, last wins).
//!
//! Contents:
//! - `pub struct Plugins`: `new`, `load`, `unload`, `set_worker`, `running`, `busy`,
//!   `event_counts`, `jobs_done`, `call`, `ids`, `name`, `contributions`, `composed`,
//!   `composed_epoch`, `health`, `has_pending`
//! - `pub fn deliver(plugins: &mut Plugins, events: &PluginEvents, snapshots: &mut Snapshots,
//!   queues: &mut RequestQueues)`

use std::collections::VecDeque;
use std::thread::JoinHandle;

use schemify_abi::{Code, Event};
use wasmi::{Engine, Linker, Store};

use crate::runtime::{self, Ctx, Exports, Room};
use crate::worker::{self, Done};
use crate::{
    CALL_ARGS_BYTES_MAX, Composed, Contributions, DEFERRED_MAX, EVENTS_MAX, Grants, Health, IO_MAX,
    LOGS_MAX, PENDING_MAX, PLUGINS_MAX, PluginEvents, PluginId, QUARANTINE_AFTER, RequestQueues,
    Snapshots, compose, imports, intake,
};

/// The plugin table plus everything derived from plugins: contributions, composition,
/// health. One row per loaded plugin, in load order, which is the configured order (D8:
/// last wins); capacity [`PLUGINS_MAX`] rows. Every row column has one entry per row.
pub struct Plugins {
    engine: Engine,
    linker: Linker<Ctx>,
    ids: Vec<PluginId>,
    names: Vec<Box<str>>,
    /// Fuel budget per event and per `call`.
    fuel: Vec<u64>,
    /// The instance; while the row's event runs on a worker, an idle stand-in.
    stores: Vec<Store<Ctx>>,
    exports: Vec<Exports>,
    /// Host-originated events (`Load`, `Rejected`) for the next delivery, ≤ [`PENDING_MAX`].
    pending: Vec<VecDeque<Pending>>,
    /// Events that arrived while the row's event ran on a worker, in arrival order,
    /// ≤ [`DEFERRED_MAX`].
    deferred: Vec<VecDeque<Pending>>,
    /// Aborted calls in a row (quarantine at [`QUARANTINE_AFTER`]) and the last abort's code.
    strikes: Vec<u8>,
    last_abort: Vec<Code>,
    /// Rows whose events run on a worker thread.
    worker: Vec<bool>,
    /// The row's event running on a worker.
    jobs: Vec<Option<JoinHandle<Done>>>,
    /// Events handed to [`deliver`] for the row, and events finished (taken in, or dropped
    /// while waiting), so a caller can tell when an event it queued has run.
    given: Vec<u64>,
    finished: Vec<u64>,
    jobs_done: u32,
    next_id: u32,
    contributions: Contributions,
    composed: Composed,
    composed_epoch: u32,
    health: Health,
    /// Output buffer reused by every synchronous event, ≤ [`crate::OUTPUT_BYTES_MAX`].
    scratch: Vec<u8>,
}

/// An event waiting for a delivery; `echo` is false for `Rejected`, so rejections caused
/// while handling a rejection are not echoed again.
#[derive(Debug)]
struct Pending {
    bytes: Vec<u8>,
    echo: bool,
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
        let linker = imports::linker(&engine);
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
            last_abort: Vec::new(),
            worker: Vec::new(),
            jobs: Vec::new(),
            given: Vec::new(),
            finished: Vec::new(),
            jobs_done: 0,
            next_id: 1,
            contributions: Contributions::new(),
            composed: Composed::default(),
            composed_epoch: 0,
            health: Health::default(),
            scratch: Vec::new(),
        }
    }

    /// Validates, links and instantiates a module, checks its ABI version, and queues
    /// `Event::Load` for the next [`deliver`]. The new plugin composes after every plugin
    /// already loaded. `fuel` is the budget per event and per `call`
    /// ([`crate::abi::FUEL_DEFAULT`] unless the user config overrides it).
    ///
    /// # Errors
    ///
    /// Each also recorded in [`Health`]: `E_SPACE` at [`PLUGINS_MAX`]; `E_PARSE` for a bad
    /// grant; `E_MODULE` for invalid wasm, unknown imports, a missing or mistyped export;
    /// `E_FUEL` / `E_TRAP` if the start function or `schemify_abi` fails; `E_VERSION` for an
    /// ABI mismatch. A failed load still uses up an id.
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
        assert!(self.next_id < crate::BROADCAST, "plugin ids exhausted");
        let id = PluginId(self.next_id);
        self.next_id += 1;
        let loaded = if self.ids.len() >= PLUGINS_MAX {
            Err((Code::Space, "plugin table full".to_owned()))
        } else {
            Grants::parse(grants)
                .map_err(|code| (code, format!("bad grant in {grants:?}")))
                .and_then(|g| {
                    let ctx = Ctx::new(id, g);
                    runtime::instantiate(&self.engine, &self.linker, wasm, ctx, fuel)
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
        self.strikes.push(0);
        self.last_abort.push(Code::Trap);
        self.worker.push(false);
        self.jobs.push(None);
        self.given.push(0);
        self.finished.push(0);
        debug_assert!(self.ids.len() <= PLUGINS_MAX);
        self.assert_rows();
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
    /// large import under a fuel override far above [`crate::abi::FUEL_DEFAULT`]) never
    /// blocks the caller's frame. Each such event completes in a later [`deliver`]: its
    /// outputs are taken in there, before the plugin's next event, and are the same as if it
    /// had run on the caller's thread (it reads a copy of the snapshots its event was
    /// delivered with). Events for the plugin that arrive meanwhile wait in order (at most
    /// [`DEFERRED_MAX`]; more are dropped with an `E_SPACE` fault) and [`Plugins::call`]
    /// answers `E_DENIED`. Composition stays in plugin order (D8) whenever the outputs
    /// arrive. On wasm32 (no threads) this does nothing: events stay on the caller's thread.
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

    /// Returns `(given, finished)` for `id`: how many events [`deliver`] has handed it
    /// (host events and queued ones, broadcasts included) and how many of those finished
    /// (completed, aborted, or dropped while waiting for its worker). An event queued
    /// before a delivery has finished once `finished` reaches the `given` read right after
    /// that delivery. `None` if `id` is not loaded.
    pub fn event_counts(&self, id: PluginId) -> Option<(u64, u64)> {
        let row = self.row(id)?;
        debug_assert!(self.finished[row] <= self.given[row]);
        Some((self.given[row], self.finished[row]))
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
    /// [`QUARANTINE_AFTER`]: the plugin may be gone after this call). `result` is left as it
    /// was on any error.
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
        let (exports, fuel) = (&self.exports[row], self.fuel[row]);
        let outcome = runtime::run_call(
            store,
            exports,
            fn_id,
            args.as_bytes(),
            fuel,
            &mut self.scratch,
        );
        for (code, detail) in store.data_mut().faults.drain(..) {
            self.health.fault(plugin, code, detail);
        }
        let returned = match outcome {
            Err(code) => {
                let name = &self.names[row];
                let detail = match code {
                    Code::Fuel => format!("{name}: call {fn_id}: out of fuel (budget {fuel})"),
                    _ => format!("{name}: call {fn_id} aborted ({code:?})"),
                };
                self.health.fault(plugin, code, detail);
                self.strike(row, code);
                self.quarantine();
                return Err(code);
            }
            Ok(returned) => returned,
        };
        self.strikes[row] = 0;
        returned?;
        let text = std::str::from_utf8(&self.scratch).map_err(|_| Code::Parse)?;
        result.clear();
        result.push_str(text);
        Ok(())
    }

    /// Returns loaded plugin ids in composition order.
    pub fn ids(&self) -> &[PluginId] {
        &self.ids
    }

    /// Returns the name a plugin was loaded under.
    pub fn name(&self, id: PluginId) -> Option<&str> {
        self.row(id).map(|row| &*self.names[row])
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

    /// Returns whether host-originated events (`Load`, `Rejected`) or events deferred
    /// behind a worker wait for the next [`deliver`]; the app calls `deliver` when this or
    /// its own event table is non-empty.
    pub fn has_pending(&self) -> bool {
        self.pending
            .iter()
            .chain(&self.deferred)
            .any(|p| !p.is_empty())
    }
}

/// The frame-spine transform: takes in the events that finished on workers, delivers
/// queued host events (`Load`, `Rejected`) and the events that waited for a worker, then
/// `events` in order (broadcasts to every plugin in composition order), takes each
/// plugin's outputs into `queues`, quarantines plugins that aborted [`QUARANTINE_AFTER`]
/// calls in a row, and recomposes contributions if any set changed.
///
/// Never fails: every plugin misbehaviour is a declared [`crate::Fault`] in
/// [`Plugins::health`], and an event for an unloaded (or quarantined) plugin is an `E_GONE`
/// fault. `snapshots` is borrowed mutably only to lend it to each plugin's store; it is
/// returned unchanged.
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
        plugins.given[row] += queue.len() as u64; // deferred ones were counted already
        queue.append(&mut plugins.deferred[row]);
        for p in queue {
            plugins.deliver_one(row, &p.bytes, p.echo, snapshots, queues);
        }
    }
    for i in 0..events.len() {
        let (target, bytes) = events.get(i);
        let rows = match target {
            None => 0..plugins.ids.len(),
            Some(id) => match plugins.row(id) {
                Some(row) => row..row + 1,
                None => {
                    let detail = "event for a plugin that is not loaded".to_owned();
                    plugins.health.fault(id, Code::Gone, detail);
                    continue;
                }
            },
        };
        for row in rows {
            plugins.given[row] += 1;
            plugins.deliver_one(row, bytes, true, snapshots, queues);
        }
    }
    plugins.quarantine();
    plugins.recompose();
    debug_assert_eq!(plugins.composed_epoch, plugins.contributions.epoch());
    plugins.assert_rows();
}

// ---- private ----

impl Plugins {
    fn row(&self, id: PluginId) -> Option<usize> {
        self.ids.iter().position(|&i| i == id)
    }

    /// Every row column has one entry per row.
    fn assert_rows(&self) {
        let n = self.ids.len();
        debug_assert!(n == self.names.len() && n == self.fuel.len() && n == self.stores.len());
        debug_assert!(n == self.exports.len() && n == self.pending.len());
        debug_assert!(n == self.deferred.len() && n == self.strikes.len());
        debug_assert!(n == self.last_abort.len() && n == self.worker.len());
        debug_assert!(n == self.jobs.len() && n == self.given.len() && n == self.finished.len());
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
        let mut output = std::mem::take(&mut self.scratch);
        let (exports, fuel, room) = (&self.exports[row], self.fuel[row], Room::left(queues));
        let store = &mut self.stores[row];
        let outcome = runtime::run_lent(store, exports, event, fuel, snapshots, room, &mut output);
        self.take_in(
            row,
            outcome.map(|()| &output[..]),
            event,
            echo,
            snapshots,
            queues,
        );
        self.scratch = output;
    }

    /// Takes in what one event of row `row` produced (`ran`: its output, or why it
    /// aborted): faults, then (unless it aborted) its staged requests and output records;
    /// rejections are echoed per §8.
    fn take_in(
        &mut self,
        row: usize,
        ran: Result<&[u8], Code>,
        event: &[u8],
        echo: bool,
        snapshots: &Snapshots,
        queues: &mut RequestQueues,
    ) {
        let id = self.ids[row];
        self.finished[row] += 1;
        let ctx = self.stores[row].data_mut();
        assert!(!ctx.live, "taken in after the call returned");
        let staged = std::mem::take(&mut ctx.staged);
        for (code, detail) in ctx.faults.drain(..) {
            self.health.fault(id, code, detail);
        }
        let output = match ran {
            Ok(output) => output,
            Err(code) => {
                // The event's first line names it (`event command x.run <args>`), cut to 128
                // bytes.
                let line = event.split(|&b| b == b'\n').next().unwrap_or_default();
                let line = line.strip_prefix(b"event ").unwrap_or(line);
                let kind = String::from_utf8_lossy(&line[..line.len().min(128)]);
                let name = &self.names[row];
                let detail = match code {
                    Code::Fuel => format!(
                        "{name}: {kind}: out of fuel (budget {}), outputs discarded",
                        self.fuel[row]
                    ),
                    _ => format!("{name}: {kind}: aborted ({code:?}), outputs discarded"),
                };
                self.health.fault(id, code, detail);
                self.strike(row, code);
                return;
            }
        };
        self.strikes[row] = 0;
        queues.io.extend(staged.io);
        queues.evals.extend(staged.evals);
        queues.logs.extend(staged.logs);
        let mut rejections = Vec::new();
        intake(
            id,
            output,
            snapshots,
            &mut self.contributions,
            queues,
            &mut rejections,
        );
        for r in rejections {
            let detail = format!("rejected: {}", String::from_utf8_lossy(&r.record));
            self.health.fault(id, r.code, detail);
            if !echo {
                continue;
            }
            if self.pending[row].len() >= PENDING_MAX {
                let detail = "Rejected event dropped".to_owned();
                self.health.fault(id, Code::Space, detail);
                continue;
            }
            let mut bytes = Vec::new();
            let rejected = Event::Rejected {
                code: r.code,
                record: &r.record,
            };
            rejected.encode(&mut bytes).expect("Rejected encodes");
            self.pending[row].push_back(Pending { bytes, echo: false });
        }
        // A worker reserved its room when it started; the spine drains the queues every
        // frame, so at most one more event's worth can sit on top.
        debug_assert!(queues.io.len() <= 2 * IO_MAX);
        debug_assert!(queues.logs.len() <= 2 * LOGS_MAX);
    }

    /// Queues an event behind row `row`'s running worker event.
    fn defer(&mut self, row: usize, event: &[u8], echo: bool) {
        assert!(self.jobs[row].is_some(), "only a busy plugin defers");
        if self.deferred[row].len() >= DEFERRED_MAX {
            let detail = "event dropped: too many wait for its worker".to_owned();
            self.health.fault(self.ids[row], Code::Space, detail);
            self.finished[row] += 1;
            return;
        }
        let bytes = event.to_vec();
        self.deferred[row].push_back(Pending { bytes, echo });
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
        assert!(self.jobs[row].is_none());
        assert!(self.worker[row]);
        let grants = self.stores[row].data().grants.clone();
        let idle = Store::new(&self.engine, Ctx::new(self.ids[row], grants));
        let store = std::mem::replace(&mut self.stores[row], idle);
        let (exports, fuel) = (self.exports[row].clone(), self.fuel[row]);
        let (event, snapshots) = (event.to_vec(), snapshots.clone());
        let job = worker::spawn(
            store,
            exports,
            fuel,
            event,
            echo,
            snapshots,
            Room::left(queues),
        );
        self.jobs[row] = Some(job);
    }

    /// Takes in row `row`'s worker event once it finished (`wait`: blocks until it does).
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
    ) {
        let Some(job) = self.jobs[row].take_if(|job| wait || job.is_finished()) else {
            return;
        };
        let done = worker::join(job);
        self.stores[row] = done.store;
        self.jobs_done = self.jobs_done.wrapping_add(1);
        let ran = done.outcome.map(|()| &done.output[..]);
        self.take_in(row, ran, &done.event, done.echo, snapshots, queues);
        debug_assert!(self.jobs[row].is_none());
    }

    /// Counts one aborted call of row `row`.
    fn strike(&mut self, row: usize, code: Code) {
        self.strikes[row] = self.strikes[row].saturating_add(1);
        self.last_abort[row] = code;
    }

    /// Drops every plugin with [`QUARANTINE_AFTER`] aborted calls in a row, recording why.
    fn quarantine(&mut self) {
        for row in (0..self.ids.len()).rev() {
            let count = self.strikes[row];
            if count < QUARANTINE_AFTER {
                continue;
            }
            let (name, code) = (&self.names[row], self.last_abort[row]);
            let detail =
                format!("{name} quarantined: {count} aborted calls in a row, the last {code:?}");
            self.health.fault(self.ids[row], Code::Gone, detail);
            self.remove_row(row);
        }
        debug_assert!(self.strikes.iter().all(|&s| s < QUARANTINE_AFTER));
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
        self.last_abort.remove(row);
        self.worker.remove(row);
        self.jobs.remove(row);
        self.given.remove(row);
        self.finished.remove(row);
        self.contributions.remove_plugin(id);
        self.recompose();
        debug_assert!(self.row(id).is_none());
        self.assert_rows();
    }

    /// Recomposes when the contribution sets changed since the last composition.
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
