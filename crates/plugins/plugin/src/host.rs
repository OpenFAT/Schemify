//! The plugin table: modules, grants and fuel budgets in → loaded plugins, their stores and
//! everything derived from them (contributions, composition, health) out; pure calls and
//! unloads run here, events run through [`deliver`](crate::deliver). Owns every store; one
//! event of one plugin runs at a time on the caller's thread, or on a worker thread for a
//! plugin [`Plugins::set_worker`] moved there. Cites plugin-abi §2 (load, quarantine, fuel),
//! §6 (pure calls), D8 (plugin order, last wins).
//!
//! Contents:
//! - `pub struct Plugins`: `new`, `load`, `unload`, `set_worker`, `running`, `busy`,
//!   `event_counts`, `jobs_done`, `call`, `ids`, `name`, `contributions`, `composed`,
//!   `composed_epoch`, `health`, `has_pending`; crate-internal `row`, `assert_rows`,
//!   `strike`, `quarantine`, `recompose`
//! - `pub(crate) struct Pending`

use std::collections::VecDeque;
use std::fmt::{self, Debug, Formatter};
use std::thread::JoinHandle;

use schemify_abi::{Code, Event};
use wasmi::{Engine, Linker, Store};

use crate::deliver::{Echo, Join};
use crate::runtime::{self, Context, Exports, Failure};
use crate::worker::Done;
use crate::{
    BROADCAST, CALL_ARGS_BYTES_MAX, Composed, Contributions, Grants, Health, OUTPUT_BYTES_MAX,
    PLUGINS_MAX, PluginId, QUARANTINE_AFTER, RequestQueues, Snapshots, compose, imports,
};

/// The plugin table plus everything derived from plugins: contributions, composition,
/// health. One row per loaded plugin, in load order, which is the configured order (D8:
/// last wins); capacity [`PLUGINS_MAX`] rows. Every row column has one entry per row.
pub struct Plugins {
    pub(crate) engine: Engine,
    linker: Linker<Context>,
    pub(crate) ids: Vec<PluginId>,
    pub(crate) names: Vec<Box<str>>,
    /// Fuel budget per event and per `call`.
    pub(crate) fuel: Vec<u64>,
    /// The instance; while the row's event runs on a worker, an idle stand-in.
    pub(crate) stores: Vec<Store<Context>>,
    pub(crate) exports: Vec<Exports>,
    /// Host-originated events (`Load`, `Rejected`) for the next delivery, ≤
    /// [`PENDING_MAX`](crate::PENDING_MAX).
    pub(crate) pending: Vec<VecDeque<Pending>>,
    /// Events that arrived while the row's event ran on a worker, in arrival order,
    /// ≤ [`DEFERRED_MAX`](crate::DEFERRED_MAX).
    pub(crate) deferred: Vec<VecDeque<Pending>>,
    /// Aborted calls in a row (quarantine at [`QUARANTINE_AFTER`]) and the last abort's code.
    pub(crate) strikes: Vec<u8>,
    last_abort: Vec<Code>,
    /// Rows whose events run on a worker thread.
    pub(crate) worker: Vec<bool>,
    /// The row's event running on a worker.
    pub(crate) jobs: Vec<Option<JoinHandle<Done>>>,
    /// Events handed to [`deliver`](crate::deliver) for the row, and events finished (taken
    /// in, or dropped while waiting), so a caller can tell when an event it queued has run.
    pub(crate) given: Vec<u64>,
    pub(crate) finished: Vec<u64>,
    pub(crate) jobs_done: u32,
    next_id: u32,
    pub(crate) contributions: Contributions,
    composed: Composed,
    pub(crate) composed_epoch: u32,
    pub(crate) health: Health,
    /// Output buffer reused by every synchronous event, ≤ [`crate::OUTPUT_BYTES_MAX`].
    pub(crate) scratch: Vec<u8>,
}

/// An event waiting for a delivery.
#[derive(Debug)]
pub(crate) struct Pending {
    pub(crate) bytes: Vec<u8>,
    pub(crate) echo: Echo,
}

impl Default for Plugins {
    fn default() -> Plugins {
        Plugins::new()
    }
}

impl Debug for Plugins {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plugins")
            .field("ids", &self.ids)
            .field("names", &self.names)
            .finish_non_exhaustive()
    }
}

impl Plugins {
    /// Returns an empty table with a fuel-metering wasmi engine and the §3 imports linked.
    #[must_use]
    pub fn new() -> Plugins {
        let engine = runtime::engine();
        let linker = imports::linker(&engine);
        let plugins = Plugins {
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
        };
        plugins.assert_rows();
        debug_assert_eq!(
            plugins.composed_epoch,
            plugins.contributions.epoch(),
            "nothing to compose yet"
        );
        plugins
    }

    /// Validates, links and instantiates a module, checks its ABI version, and queues `Event::Load`
    /// for the next [`deliver`](crate::deliver). The new plugin composes after every plugin already
    /// loaded. `fuel` is the budget per event and per `call` ([`crate::abi::FUEL_DEFAULT`] unless
    /// the user config overrides it).
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
        assert!(self.next_id < BROADCAST, "plugin ids exhausted");
        let id = PluginId(self.next_id);
        self.next_id += 1;
        debug_assert!(self.row(id).is_none(), "ids are never reused");
        let loaded = if self.ids.len() >= PLUGINS_MAX {
            Err(Failure {
                code: Code::Space,
                detail: "plugin table full".to_owned(),
            })
        } else {
            Grants::parse(grants)
                .map_err(|code| Failure {
                    code,
                    detail: format!("bad grant in {grants:?}"),
                })
                .and_then(|granted| {
                    let context = Context::new(id, granted);
                    runtime::instantiate(&self.engine, &self.linker, wasm, context, fuel)
                })
        };
        let (store, exports) = match loaded {
            Ok(loaded) => loaded,
            Err(Failure { code, detail }) => {
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
            echo: Echo::Rejections,
        }]));
        self.deferred.push(VecDeque::new());
        self.strikes.push(0);
        self.last_abort.push(Code::Trap);
        self.worker.push(false);
        self.jobs.push(None);
        self.given.push(0);
        self.finished.push(0);
        debug_assert!(self.ids.len() <= PLUGINS_MAX);
        debug_assert_eq!(self.ids.last(), Some(&id), "the new plugin composes last");
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
        self.poll_job(row, Join::Wait, snapshots, queues);
        debug_assert!(self.jobs[row].is_none(), "waited for its worker");
        self.worker[row] = false; // `Unload` runs here, on the caller's thread
        let mut unload = Vec::new();
        Event::Unload.encode(&mut unload).expect("Unload encodes");
        self.given[row] += 1; // keeps `finished <= given` for the row's last event
        self.deliver_one(row, &unload, Echo::Silent, snapshots, queues);
        self.remove_row(row);
        debug_assert!(self.row(id).is_none());
        Ok(())
    }

    /// Runs `id`'s events on a worker thread from its next event on, so a long event (a large
    /// import under a fuel override far above [`crate::abi::FUEL_DEFAULT`]) never blocks the
    /// caller's frame. Each such event completes in a later [`deliver`](crate::deliver): its
    /// outputs are taken in there, before the plugin's next event, and are the same as if it had
    /// run on the caller's thread (it reads a copy of the snapshots its event was delivered with).
    /// Events for the plugin that arrive meanwhile wait in order (at most
    /// [`DEFERRED_MAX`](crate::DEFERRED_MAX); more are dropped with an `E_SPACE` fault) and
    /// [`Plugins::call`] answers `E_DENIED`. Composition stays in plugin order (D8) whenever the
    /// outputs arrive. On wasm32 (no threads) this does nothing: events stay on the caller's
    /// thread.
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

    /// Returns how many plugins have an event running on a worker; [`deliver`](crate::deliver)
    /// takes each in once it finished, so the caller keeps delivering (on a timer) while this is
    /// non-zero.
    #[must_use]
    pub fn running(&self) -> usize {
        debug_assert_eq!(self.jobs.len(), self.ids.len());
        self.jobs.iter().filter(|job| job.is_some()).count()
    }

    /// Returns whether `id` has an event running on a worker (its pure functions are
    /// unavailable until it finishes).
    #[must_use]
    pub fn busy(&self, id: PluginId) -> bool {
        let busy = self.row(id).is_some_and(|row| self.jobs[row].is_some());
        debug_assert!(
            !busy
                || self
                    .row(id)
                    .is_some_and(|row| self.stores[row].data().memory.is_none()),
            "a busy plugin's instance is on its worker, an idle stand-in here"
        );
        busy
    }

    /// Returns `(given, finished)` for `id`: how many events [`deliver`](crate::deliver) has handed
    /// it (host events and queued ones, broadcasts included) and how many of those finished
    /// (completed, aborted, or dropped while waiting for its worker). An event queued before a
    /// delivery has finished once `finished` reaches the `given` read right after that delivery.
    /// `None` if `id` is not loaded.
    #[must_use]
    pub fn event_counts(&self, id: PluginId) -> Option<(u64, u64)> {
        let row = self.row(id)?;
        debug_assert!(self.finished[row] <= self.given[row]);
        Some((self.given[row], self.finished[row]))
    }

    /// Returns a counter that advances (wrapping) whenever [`deliver`](crate::deliver) or
    /// [`Plugins::unload`] takes in an event that ran on a worker.
    #[must_use]
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
        debug_assert!(!store.data().live);
        debug_assert!(self.scratch.len() <= OUTPUT_BYTES_MAX);
        for Failure { code, detail } in store.data_mut().faults.drain(..) {
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
        debug_assert_eq!(result.len(), self.scratch.len());
        Ok(())
    }

    /// Returns loaded plugin ids in composition order.
    #[must_use]
    pub fn ids(&self) -> &[PluginId] {
        &self.ids
    }

    /// Returns the name a plugin was loaded under.
    #[must_use]
    pub fn name(&self, id: PluginId) -> Option<&str> {
        self.row(id).map(|row| &*self.names[row])
    }

    /// Returns every plugin's current contribution sets.
    #[must_use]
    pub fn contributions(&self) -> &Contributions {
        &self.contributions
    }

    /// Returns the composition of all contributions (D8). Recomputed by [`deliver`](crate::deliver)
    /// and [`Plugins::unload`] when [`Plugins::composed_epoch`] moves.
    #[must_use]
    pub fn composed(&self) -> &Composed {
        &self.composed
    }

    /// Returns a value that changes whenever [`Plugins::composed`] changes.
    #[must_use]
    pub fn composed_epoch(&self) -> u32 {
        self.composed_epoch
    }

    /// Returns the health report.
    #[must_use]
    pub fn health(&self) -> &Health {
        &self.health
    }

    /// Returns whether host-originated events (`Load`, `Rejected`) or events deferred behind a
    /// worker wait for the next [`deliver`](crate::deliver); the app calls `deliver` when this or
    /// its own event table is non-empty.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        debug_assert_eq!(self.pending.len(), self.deferred.len());
        self.pending
            .iter()
            .chain(&self.deferred)
            .any(|queue| !queue.is_empty())
    }
}

impl Plugins {
    pub(crate) fn row(&self, id: PluginId) -> Option<usize> {
        self.ids.iter().position(|&i| i == id)
    }

    /// Every row column has one entry per row.
    pub(crate) fn assert_rows(&self) {
        let rows = self.ids.len();
        debug_assert!(rows <= PLUGINS_MAX);
        debug_assert_eq!(rows, self.names.len());
        debug_assert_eq!(rows, self.fuel.len());
        debug_assert_eq!(rows, self.stores.len());
        debug_assert_eq!(rows, self.exports.len());
        debug_assert_eq!(rows, self.pending.len());
        debug_assert_eq!(rows, self.deferred.len());
        debug_assert_eq!(rows, self.strikes.len());
        debug_assert_eq!(rows, self.last_abort.len());
        debug_assert_eq!(rows, self.worker.len());
        debug_assert_eq!(rows, self.jobs.len());
        debug_assert_eq!(rows, self.given.len());
        debug_assert_eq!(rows, self.finished.len());
        // Ids are issued in increasing order and rows keep load order, so the id column is
        // strictly increasing and below the next id to issue.
        debug_assert!(self.ids.windows(2).all(|pair| pair[0] < pair[1]));
        debug_assert!(self.ids.last().is_none_or(|last| last.0 < self.next_id));
    }

    /// Counts one aborted call of row `row`.
    pub(crate) fn strike(&mut self, row: usize, code: Code) {
        debug_assert!(
            matches!(code, Code::Fuel | Code::Trap | Code::Range | Code::Space),
            "only a guest abort strikes, not {code:?}"
        );
        self.strikes[row] = self.strikes[row].saturating_add(1);
        self.last_abort[row] = code;
        debug_assert!(self.strikes[row] > 0);
    }

    /// Drops every plugin with [`QUARANTINE_AFTER`] aborted calls in a row, recording why.
    pub(crate) fn quarantine(&mut self) {
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

    /// Recomposes when the contribution sets changed since the last composition.
    pub(crate) fn recompose(&mut self) {
        if self.contributions.epoch() == self.composed_epoch {
            return;
        }
        let (composed, overrides) = compose::compose(&self.contributions, &self.ids);
        self.composed = composed;
        self.health.overrides = overrides;
        self.composed_epoch = self.contributions.epoch();
        debug_assert!(
            self.health
                .overrides
                .iter()
                .all(|replaced| self.ids.contains(&replaced.winner)),
            "only loaded plugins win"
        );
        debug_assert!(
            self.composed
                .records
                .iter()
                .all(|r| self.ids.contains(&r.owner))
        );
    }
}

// ---- private ----

impl Plugins {
    /// Drops row `row` and its contributions, and recomposes. A worker still running for it
    /// is detached; its result is dropped when it ends.
    fn remove_row(&mut self, row: usize) {
        let rows = self.ids.len();
        assert!(row < rows, "row {row} of {rows}");
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
        debug_assert_eq!(self.ids.len(), rows - 1);
        self.assert_rows();
    }
}
