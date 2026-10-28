//! Plugin host (WS6): wasm modules, events and snapshots in → request tables, contribution
//! composition and a health report out. Loads modules with wasmi (D3), enforces
//! capabilities (D7), composes contributions in plugin order, last wins (D8), and turns
//! plugin outputs into request tables (D3a). Cites `docs/spec/plugin-abi.md` §1–§8.
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
//! Contents:
//! - capacities: `pub const PLUGINS_MAX`, `EVENTS_MAX`, `EVENT_BYTES_MAX`, `OUTPUT_BYTES_MAX`,
//!   `CONTRIBUTION_BYTES_MAX`, `CONTRIBUTIONS_MAX`, `PENDING_MAX`, `FAULTS_MAX`, `EDITS_MAX`,
//!   `RUNS_MAX`, `TIMERS_MAX`, `LOGS_MAX`, `IO_MAX`, `EVALS_MAX`, `LOG_BYTES_MAX`,
//!   `IO_BYTES_MAX`, `TIMER_MS_MAX`, `CALL_ARGS_BYTES_MAX`, `REJECT_ECHO_BYTES`,
//!   `QUARANTINE_AFTER`, `DEFERRED_MAX`
//! - `pub struct PluginId(pub u32)`
//! - `pub struct PluginEvents`: `new`, `push`, `len`, `is_empty`, `clear`
//! - re-exports: `Plugins`, `deliver` (host), `RequestQueues` and its rows (requests),
//!   `Fault`, `Health` (health), `Grants`, `authorize`, `env_path` (grants), `Contributions`
//!   (contributions), `Rejection`, `intake` (intake), `Composed`, `ComposedRecord`,
//!   `Functions`, `Override`, `compose` (compose), `Snapshots`, `ROWS_MAX` (snapshot),
//!   `abi` (`schemify_abi`)
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
mod contributions;
mod grants;
mod health;
mod host;
mod imports;
mod intake;
mod requests;
mod runtime;
mod snapshot;
mod worker;

use schemify_abi::{Code, Event};

pub use compose::{Composed, ComposedRecord, Functions, Override, compose};
pub use contributions::Contributions;
pub use grants::{Grants, authorize, env_path};
pub use health::{Fault, Health};
pub use host::{Plugins, deliver};
pub use intake::{Rejection, intake};
pub use requests::{
    EditRequest, EvalRequest, IoOp, IoRequest, LogEntry, RequestQueues, RunRequest, TimerRequest,
};
pub use schemify_abi as abi;
pub use snapshot::{ROWS_MAX, Snapshots};

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

/// Events for one [`deliver`] call, already encoded (one encoding per event, shared by
/// every recipient of a broadcast). Columns: target (`BROADCAST` = every plugin) and the
/// exclusive end of the event's bytes; capacity [`EVENTS_MAX`] rows.
#[derive(Clone, Debug, Default)]
pub struct PluginEvents {
    target: Vec<u32>,
    ends: Vec<u32>,
    bytes: Vec<u8>,
}

/// The target column's "every plugin"; never assigned as an id.
const BROADCAST: u32 = u32::MAX;

impl std::fmt::Display for PluginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin#{}", self.0)
    }
}

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
        debug_assert_eq!(self.target.len(), self.ends.len());
        if self.target.len() >= EVENTS_MAX {
            return Err(Code::Space);
        }
        if target == Some(PluginId(BROADCAST)) {
            return Err(Code::Gone);
        }
        let before = self.bytes.len();
        event.encode(&mut self.bytes)?;
        let end = match u32::try_from(self.bytes.len()) {
            Ok(end) if self.bytes.len() - before <= EVENT_BYTES_MAX => end,
            _ => {
                self.bytes.truncate(before);
                return Err(Code::Space);
            }
        };
        self.target.push(target.map_or(BROADCAST, |id| id.0));
        self.ends.push(end);
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
}

// ---- private ----

impl PluginEvents {
    /// Event `i`: its target (`None` = broadcast) and its encoded bytes.
    fn get(&self, i: usize) -> (Option<PluginId>, &[u8]) {
        assert!(i < self.target.len());
        let start = snapshot::row_start(&self.ends, i);
        let target = (self.target[i] != BROADCAST).then_some(PluginId(self.target[i]));
        (target, &self.bytes[start..self.ends[i] as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: each stored event is exactly `Event::encode`'s bytes, with its target; a
    /// refused push leaves the table as it was.
    #[test]
    fn events_keep_their_encoding_and_refuse_bad_pushes() {
        let mut events = PluginEvents::new();
        let timer = Event::Timer { id: 7 };
        events.push(None, &Event::Load).unwrap();
        events.push(Some(PluginId(3)), &timer).unwrap();
        let bad_name = Event::Command {
            name: "a b",
            args: "",
        };
        assert_eq!(events.push(None, &bad_name), Err(Code::Parse));
        let never = Some(PluginId(BROADCAST));
        assert_eq!(events.push(never, &Event::Load), Err(Code::Gone));
        assert_eq!(events.len(), 2);
        let encoded = |e: &Event<'_>| {
            let mut bytes = Vec::new();
            e.encode(&mut bytes).unwrap();
            bytes
        };
        assert_eq!(events.get(0), (None, &encoded(&Event::Load)[..]));
        assert_eq!(events.get(1), (Some(PluginId(3)), &encoded(&timer)[..]));

        while events.len() < EVENTS_MAX {
            events.push(None, &Event::Load).unwrap();
        }
        assert_eq!(events.push(None, &Event::Load), Err(Code::Space));
        assert_eq!(events.len(), EVENTS_MAX);
        events.clear();
        assert!(events.is_empty());
    }
}
