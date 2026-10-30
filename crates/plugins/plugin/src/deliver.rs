//! The frame-spine transform: the plugin table, encoded events and the snapshots in →
//! request queues, contribution sets, the composition and health out, one event of one
//! plugin at a time, on the caller's thread or on the plugin's worker thread. Cites
//! plugin-abi §2 (one event at a time, fuel, quarantine), §7 (output intake), §8 (rejection
//! echo), D8 (plugin order, last wins).
//!
//! Contents:
//! - `pub fn deliver(plugins: &mut Plugins, events: &PluginEvents, snapshots: &mut Snapshots,
//!   queues: &mut RequestQueues)`
//! - `pub(crate) enum Echo`, `pub(crate) enum Join`
//! - crate-internal `Plugins::deliver_one`, `Plugins::poll_job`

use std::mem;

use schemify_abi::{Code, Event};
use wasmi::Store;

use crate::host::Pending;
use crate::runtime::{self, Context, Failure, Room};
use crate::{
    DEFERRED_MAX, EVENTS_MAX, IO_MAX, LOGS_MAX, PENDING_MAX, PluginEvents, Plugins, RequestQueues,
    Snapshots, intake, worker,
};

/// Whether the rejections an event causes go back to its plugin as `Event::Rejected` (§8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Echo {
    /// Echo them: every event but a `Rejected`.
    Rejections,
    /// Record them only: a `Rejected` event, so a rejection is never echoed twice.
    Silent,
}

/// How [`Plugins::poll_job`] treats a worker event that has not finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Join {
    /// Block until it finishes.
    Wait,
    /// Leave it running.
    IfFinished,
}

/// The frame-spine transform: takes in the events that finished on workers, delivers queued host
/// events (`Load`, `Rejected`) and the events that waited for a worker, then `events` in order
/// (broadcasts to every plugin in composition order), takes each plugin's outputs into `queues`,
/// quarantines plugins that aborted [`QUARANTINE_AFTER`](crate::QUARANTINE_AFTER) calls in a row,
/// and recomposes contributions if any set changed.
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
        plugins.poll_job(row, Join::IfFinished, snapshots, queues);
        // Host events first, then the events that waited for the worker, in arrival order;
        // whatever meets a new worker event waits again, in the same order.
        let mut queue = mem::take(&mut plugins.pending[row]);
        plugins.given[row] += queue.len() as u64; // deferred ones were counted already
        queue.append(&mut plugins.deferred[row]);
        for pending in queue {
            plugins.deliver_one(row, &pending.bytes, pending.echo, snapshots, queues);
        }
        // Only rejections of these events queued new host events, at most `PENDING_MAX`.
        debug_assert!(plugins.pending[row].len() <= PENDING_MAX);
        debug_assert!(plugins.finished[row] <= plugins.given[row]);
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
            plugins.deliver_one(row, bytes, Echo::Rejections, snapshots, queues);
        }
    }
    plugins.quarantine();
    plugins.recompose();
    debug_assert_eq!(plugins.composed_epoch, plugins.contributions.epoch());
    plugins.assert_rows();
}

impl Plugins {
    /// Runs one event through one plugin and takes its outputs in. A fault (fuel, trap, bad
    /// output buffer) discards every output and staged request of the event. A plugin on a
    /// worker gets the event on its thread; a busy one gets it after its running event.
    pub(crate) fn deliver_one(
        &mut self,
        row: usize,
        event: &[u8],
        echo: Echo,
        snapshots: &mut Snapshots,
        queues: &mut RequestQueues,
    ) {
        debug_assert!(row < self.ids.len());
        if self.jobs[row].is_some() {
            self.defer(row, event, echo);
            return;
        }
        if self.worker[row] {
            self.start_job(row, event, echo, snapshots, queues);
            return;
        }
        let mut output = mem::take(&mut self.scratch);
        let (exports, fuel, room) = (&self.exports[row], self.fuel[row], Room::left(queues));
        let store = &mut self.stores[row];
        let outcome = runtime::run_lent(store, exports, event, fuel, snapshots, room, &mut output);
        debug_assert!(!store.data().live, "run_lent handed the snapshots back");
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

    /// Takes in row `row`'s worker event once it finished; [`Join::Wait`] blocks until it does.
    ///
    /// # Panics
    ///
    /// Re-raises a panic of the worker (a host bug: no guest input panics the host).
    pub(crate) fn poll_job(
        &mut self,
        row: usize,
        join: Join,
        snapshots: &Snapshots,
        queues: &mut RequestQueues,
    ) {
        let wait = join == Join::Wait;
        let Some(job) = self.jobs[row].take_if(|job| wait || job.is_finished()) else {
            return;
        };
        let done = worker::join(job);
        debug_assert!(
            self.stores[row].data().memory.is_none(),
            "replacing the stand-in"
        );
        self.stores[row] = done.store;
        self.jobs_done = self.jobs_done.wrapping_add(1);
        let ran = done.outcome.map(|()| &done.output[..]);
        self.take_in(row, ran, &done.event, done.echo, snapshots, queues);
        debug_assert!(self.jobs[row].is_none());
    }
}

// ---- private ----

impl Plugins {
    /// Takes in what one event of row `row` produced (`ran`: its output, or why it
    /// aborted): faults, then (unless it aborted) its staged requests and output records;
    /// rejections are echoed per §8.
    fn take_in(
        &mut self,
        row: usize,
        ran: Result<&[u8], Code>,
        event: &[u8],
        echo: Echo,
        snapshots: &Snapshots,
        queues: &mut RequestQueues,
    ) {
        let id = self.ids[row];
        self.finished[row] += 1;
        debug_assert!(
            self.finished[row] <= self.given[row],
            "every event taken in was handed over by `deliver` first"
        );
        let context = self.stores[row].data_mut();
        assert!(!context.live, "taken in after the call returned");
        let staged = mem::take(&mut context.staged);
        for Failure { code, detail } in context.faults.drain(..) {
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
        intake::intake(
            id,
            output,
            snapshots,
            &mut self.contributions,
            queues,
            &mut rejections,
        );
        for rejection in rejections {
            let record = String::from_utf8_lossy(&rejection.record);
            let detail = format!("rejected: {record}");
            self.health.fault(id, rejection.code, detail);
            if echo == Echo::Silent {
                continue;
            }
            if self.pending[row].len() >= PENDING_MAX {
                let detail = "Rejected event dropped".to_owned();
                self.health.fault(id, Code::Space, detail);
                continue;
            }
            let mut bytes = Vec::new();
            let rejected = Event::Rejected {
                code: rejection.code,
                record: &rejection.record,
            };
            rejected.encode(&mut bytes).expect("Rejected encodes");
            let echo = Echo::Silent;
            self.pending[row].push_back(Pending { bytes, echo });
        }
        // A worker reserved its room when it started; the spine drains the queues every
        // frame, so at most one more event's worth can sit on top.
        debug_assert!(queues.io.len() <= 2 * IO_MAX);
        debug_assert!(queues.logs.len() <= 2 * LOGS_MAX);
    }

    /// Queues an event behind row `row`'s running worker event.
    fn defer(&mut self, row: usize, event: &[u8], echo: Echo) {
        assert!(self.jobs[row].is_some(), "only a busy plugin defers");
        if self.deferred[row].len() >= DEFERRED_MAX {
            let detail = "event dropped: too many wait for its worker".to_owned();
            self.health.fault(self.ids[row], Code::Space, detail);
            self.finished[row] += 1;
            debug_assert!(self.finished[row] <= self.given[row]);
            return;
        }
        let bytes = event.to_vec();
        self.deferred[row].push_back(Pending { bytes, echo });
        debug_assert!(self.deferred[row].len() <= DEFERRED_MAX);
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
        echo: Echo,
        snapshots: &Snapshots,
        queues: &RequestQueues,
    ) {
        assert!(
            self.jobs[row].is_none(),
            "one event of a plugin at a time (§2)"
        );
        assert!(self.worker[row]);
        let grants = self.stores[row].data().grants.clone();
        let idle = Store::new(&self.engine, Context::new(self.ids[row], grants));
        let store = mem::replace(&mut self.stores[row], idle);
        debug_assert!(
            store.data().memory.is_some(),
            "the instance goes to the worker"
        );
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
        debug_assert!(
            self.stores[row].data().memory.is_none(),
            "the stand-in is idle"
        );
    }
}
