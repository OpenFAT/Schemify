//! Plugin host glue for the frame spine: loads the bundled default UI (D6) and the
//! manifest's local plugins in configured order (D8), fills snapshot tables from `doc`,
//! `elab` and `run`, runs `plugin::deliver`, and drains the request queues to their owners
//! (edits to `doc`, run requests to the editor's run queue, I/O to `platform`, logs to the
//! status line). User packages (D35, config spec §5) load between the default UI and the
//! project's plugins; lazy ones wait for their command or event. The user config's Lua
//! keymaps and hooks run here too, so their requests join the plugins' (config spec §6).
//!
//! I/O is inverted and never blocks the frame (plugin-abi §5): file reads and writes complete
//! within the delivery, a `spawn` runs on a worker ([`platform::spawn_start`](crate::platform::spawn_start)) and its
//! `IoDone` arrives in a later frame through [`Host::poll`]; timers fire the same way. While
//! either waits, [`Host::wake_ms`] asks the window for a tick. A plugin granted more fuel than
//! the default (a manifest or package `fuel` override) runs its events on a worker thread
//! (`Plugins::set_worker`), so a long import never freezes the editor; its outputs land in a
//! later frame. `eval` requests are answered within the delivery by [`eval_in`].
//!
//! Contents:
//! - `pub const HASH_HEX`
//! - `pub const DEFAULT_UI_WASM`
//! - `pub const WAITING_MAX`
//! - `pub type ResultRows`
//! - `pub struct Host`, `Host::push`, `Host::has_work`, `Host::poll`, `Host::wake_ms`,
//!   `Host::run_done`, `Host::ui_sets`
//! - `pub const NOTES_MAX`
//! - `pub struct Note`, `Note::values`
//! - `pub struct Delivered`, `pub struct PluginEdit`
//! - `pub fn doc_id`
//! - `pub fn handle_of`

mod deliver;
mod load;
mod patch;
mod snapshot;
mod user;

use std::collections::VecDeque;
use std::fmt;

use schemify_cli::pack::Installed;
use schemify_config::Config;
use schemify_doc::{DocHandle, Project};
use schemify_plugin::abi::{Event, Kind};
use schemify_plugin::{PluginEvents, PluginId, Plugins, RequestQueues, RunRequest, Snapshots};

use crate::platform::Spawned;

pub use deliver::{eval_in, fn_result};
use snapshot::Filled;

/// Hex digits of a plugin content hash in a D20 `cached <value> <hash>` attribute: the
/// first 8 bytes of the module's sha256 (change detection, not authentication).
pub const HASH_HEX: usize = 16;

/// The bundled default UI plugin, built by `ui/build.sh`.
pub const DEFAULT_UI_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/default_ui.wasm"
));

/// Child processes and timers waiting at once (the plugin crate's per-frame queue
/// capacities, plugin-abi §10); a request past it completes at once with `E_SPACE`.
pub const WAITING_MAX: usize = 256;

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
    /// What each group of tables was filled from, so a fill writes only what changed.
    filled: Filled,
    queues: RequestQueues,
    /// Epoch of the last composition the app consumed.
    pub seen_epoch: Option<u32>,
    /// Content hash ([`HASH_HEX`] hex digits) of each loaded plugin's module, by id.
    hashes: Vec<(PluginId, String)>,
    args: String,
    result: String,
    /// A command event waits for delivery (see [`Delivered::state_changed`]).
    commanded: bool,
    /// Child processes started for `spawn` requests, at most [`WAITING_MAX`].
    children: Vec<Child>,
    /// Pending timers, at most [`WAITING_MAX`].
    timers: Vec<Timer>,
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
    /// Recent editor events, oldest first, at most [`NOTES_MAX`]: the command stream's
    /// notifications (D27).
    notes: VecDeque<Note>,
    /// Editor events recorded so far (the sequence number of the newest note).
    notes_seq: u64,
    /// Health faults already surfaced ([`schemify_plugin::Health::recorded`]).
    faults_seen: u64,
}

/// Editor events kept for the command stream ([`Host::notes_since`]); a client that falls
/// further behind misses the oldest.
pub const NOTES_MAX: usize = 256;

/// One editor event kept for the command stream: its kind and its numbers in plugin-abi §6
/// wire order (`doc_changed` → doc, epoch; `selection_changed` → none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Note {
    /// The event.
    pub event: schemify_config::Event,
    values: [u32; 2],
    len: u8,
}

/// What a delivery produced for the rest of the spine.
#[derive(Debug, Default)]
pub struct Delivered {
    /// Plugin edit transactions, in request order.
    pub edits: Vec<PluginEdit>,
    /// Log lines for the status line.
    pub logs: Vec<String>,
    /// A plugin ran a command or got an I/O completion: its pure functions may answer
    /// differently now (a LUT loaded), so derived values must be recomputed.
    pub state_changed: bool,
    /// Plugin `run` requests (plugin-abi §7) for the editor's run queue.
    pub runs: Vec<RunRequest>,
    /// Commands the user config queued (`schemify.cmd`): (name, args), to dispatch in order.
    pub commands: Vec<(String, String)>,
    /// A plugin logged at `error` level or a call aborted (out of fuel, a trap, a
    /// quarantine): what `schemify cmd` turns into exit status 1.
    pub failed: bool,
}

/// One plugin edit transaction for the editor's request queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginEdit {
    /// The document it edits (live when the delivery ended).
    pub doc: DocHandle,
    /// The undo label.
    pub label: String,
    /// The ops as edit text (edits spec §1), parsed against the document's kind.
    pub ops: String,
    /// Staged for review (plugin-abi `EditMode::Stage`) rather than committed.
    pub stage: bool,
}

/// A child process started for a plugin's `spawn` request.
struct Child {
    plugin: PluginId,
    /// The request's handle, echoed in its `IoDone`.
    handle: u32,
    process: Spawned,
}

/// A plugin timer waiting to fire.
struct Timer {
    /// Due time, in the editor's monotonic ms.
    due_ms: u64,
    plugin: PluginId,
    id: u32,
}

/// Delivery rounds per frame: a plugin's I/O completion is answered in the same frame, up to
/// this many request → completion hops.
const ROUNDS_MAX: u32 = 4;

/// Tick period while a child process runs (its completion is polled, not signalled).
const IO_POLL_MS: u32 = 20;

impl Note {
    /// The event's numbers (document handles, epochs, job ids), at most two.
    #[must_use]
    pub fn values(&self) -> &[u32] {
        debug_assert!(usize::from(self.len) <= self.values.len());
        &self.values[..usize::from(self.len)]
    }
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("plugins", &self.plugins.ids().len())
            .finish()
    }
}

impl Host {
    /// Queues an event (`None` = broadcast).
    ///
    /// # Errors
    ///
    /// The event table is full: the event is dropped, and the error says so.
    ///
    /// # Panics
    ///
    /// Never: the assert checks that a queued event waits.
    pub fn push(&mut self, target: Option<PluginId>, ev: &Event<'_>) -> Result<(), String> {
        self.commanded |= matches!(ev, Event::Command { .. });
        if target.is_none() {
            self.user_event(ev);
        }
        let queued = self
            .events
            .push(target, ev)
            .map_err(|c| format!("plugin event dropped: {c:?}"));
        assert!(
            queued.is_err() || !self.events.is_empty(),
            "a queued event waits"
        );
        queued
    }

    /// Returns whether a delivery has work (queued events or host-originated ones, user
    /// config requests, an event running on a worker to take in).
    #[must_use]
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
    ///
    /// # Panics
    ///
    /// Only on a broken internal invariant (more than [`WAITING_MAX`] children or timers).
    pub fn poll(&mut self, now_ms: u64) -> bool {
        assert!(self.children.len() <= WAITING_MAX);
        assert!(self.timers.len() <= WAITING_MAX);
        let before = self.events.len();
        let mut i = 0;
        while i < self.children.len() {
            let Some((status, bytes)) = self.children[i].process.try_finish() else {
                i += 1;
                continue;
            };
            let child = self.children.swap_remove(i);
            let ev = Event::IoDone {
                handle: child.handle,
                status,
                bytes: &bytes,
            };
            let _ = self.events.push(Some(child.plugin), &ev);
            self.commanded = true; // a completion may change what a pure function answers
        }
        let mut k = 0;
        while k < self.timers.len() {
            if self.timers[k].due_ms > now_ms {
                k += 1;
                continue;
            }
            let timer = self.timers.swap_remove(k);
            let _ = self
                .events
                .push(Some(timer.plugin), &Event::Timer { id: timer.id });
        }
        assert!(
            self.timers.iter().all(|t| t.due_ms > now_ms),
            "every due timer fired"
        );
        assert!(self.events.len() >= before, "events are only queued here");
        self.events.len() > before
    }

    /// Milliseconds until the next completion should be looked for: 20 ms while a
    /// child or a plugin's worker event runs, else the nearest timer; `None` when nothing
    /// waits (idle CPU 0%, D39).
    ///
    /// # Panics
    ///
    /// Never: the wait is clamped to `u32`, and the assert checks that `None` means idle.
    #[must_use]
    pub fn wake_ms(&self, now_ms: u64) -> Option<u32> {
        let timer = self
            .timers
            .iter()
            .map(|t| {
                let wait = t.due_ms.saturating_sub(now_ms).min(u64::from(u32::MAX));
                u32::try_from(wait).expect("clamped to u32")
            })
            .min();
        let busy = !self.children.is_empty() || self.plugins.running() > 0;
        let io = busy.then_some(IO_POLL_MS);
        let wake = match (timer, io) {
            (Some(t), Some(i)) => Some(t.min(i)),
            (t, i) => t.or(i),
        };
        assert_eq!(
            wake.is_none(),
            timer.is_none() && !busy,
            "idle exactly when nothing waits"
        );
        wake
    }

    /// Queues `RunDone { job }` for `plugin` (its `run` request's job finished or failed).
    pub fn run_done(&mut self, plugin: PluginId, job: u32) {
        let _ = self.events.push(Some(plugin), &Event::RunDone { job });
        self.user_event(&Event::RunDone { job });
    }
}

impl Host {
    /// Returns `(plugin name, ui set + keymap set)` per plugin in load order, when the
    /// composition changed since the last call.
    ///
    /// # Panics
    ///
    /// Never: the assert checks one set per plugin.
    pub fn ui_sets(&mut self) -> Option<Vec<(String, String)>> {
        let epoch = self.plugins.composed_epoch();
        if self.seen_epoch == Some(epoch) {
            return None;
        }
        self.seen_epoch = Some(epoch);
        let c = self.plugins.contributions();
        let mut sets: Vec<(String, String)> = self
            .plugins
            .ids()
            .iter()
            .map(|&id| {
                let ui = c.get(id, Kind::Ui, "").unwrap_or("");
                let keymap = c.get(id, Kind::Keymap, "").unwrap_or("");
                let name = self.plugins.name(id).unwrap_or("?").to_owned();
                (name, format!("{ui}\n{keymap}"))
            })
            .collect();
        assert_eq!(
            sets.len(),
            self.plugins.ids().len(),
            "one set per plugin, in load order"
        );
        // The user's own keymaps go after every plugin (config spec §6).
        let keys = self.user.table().keymaps.records();
        if !keys.is_empty() {
            sets.push(("user".to_owned(), keys));
        }
        Some(sets)
    }
}

/// Plugin-visible document id: slot index in the low 16 bits, generation in the high 16
/// (`doc::DOCS_MAX` is 2^16), so a handle kept across a close is detected as gone.
///
/// # Panics
///
/// A handle index past `doc::DOCS_MAX` (2^16).
#[must_use]
pub fn doc_id(h: DocHandle) -> u32 {
    assert!(
        h.index < 1 << 16,
        "doc handle index {} past DOCS_MAX",
        h.index
    );
    let id = (h.generation << 16) | h.index;
    debug_assert_eq!(id & 0xffff, h.index, "the slot is the low 16 bits");
    id
}

/// The live handle a plugin's document id names.
///
/// # Panics
///
/// Never: the assert checks that the handle's slot is the id's low 16 bits.
#[must_use]
pub fn handle_of(project: &Project, id: u32) -> Option<DocHandle> {
    let h = project.handles().find(|&h| doc_id(h) == id);
    assert!(
        h.is_none_or(|h| h.index == id & 0xffff),
        "the slot is the id's low 16 bits"
    );
    h
}
