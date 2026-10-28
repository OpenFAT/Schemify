//! Request rows (D3a): what plugins ask other owners to do, one queue per owner. Plain
//! data: [`crate::intake`] and the imports append, each owner drains its queue once per
//! frame. Cites plugin-abi §5 (inverted I/O and its capabilities), §7 (output records).
//!
//! Contents:
//! - `pub struct EditRequest`, `pub struct RunRequest`, `pub struct TimerRequest`,
//!   `pub struct LogEntry`, `pub struct IoRequest`, `pub struct EvalRequest`
//! - `pub enum IoOp`, `IoOp::cap(self) -> Cap`
//! - `pub struct RequestQueues`

use schemify_abi::{Cap, EditMode, Level};

use crate::PluginId;

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
    /// Delay, at most [`crate::TIMER_MS_MAX`].
    pub ms: u32,
}

/// A log line for the app's log panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    /// Source plugin.
    pub plugin: PluginId,
    /// Severity.
    pub level: Level,
    /// Message, at most [`crate::LOG_BYTES_MAX`] bytes.
    pub text: String,
}

/// Inverted I/O operations (§5); each maps to one capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IoOp {
    /// Read a file (`read_project`, or `read:<NAME>` for a `${NAME}/…` path,
    /// [`crate::env_path`]). `args = [path]`.
    FsRead,
    /// Write a project-relative file (`write_project`). `args = [path]`, `data` = bytes.
    FsWrite,
    /// Remove a file or directory tree under `.schemify/` (`write_project`). `args = [path]`.
    FsRemove,
    /// Run a binary (`spawn:<argv[0]>`, native only). `args` = argv, `data` = stdin.
    Spawn,
    /// HTTP request (`network`). `args = [url]`, `data` = body.
    Fetch,
    /// Read the clipboard (`clipboard`). No args.
    ClipboardGet,
    /// Write the clipboard (`clipboard`). `data` = text.
    ClipboardSet,
    /// List the files under a directory, as [`FsRead`](IoOp::FsRead) authorizes paths.
    /// `args = [dir]`; the answer is one path relative to `dir` per line, in path order.
    FsList,
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
    /// Payload, at most [`crate::IO_BYTES_MAX`] bytes.
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

/// Requests made by plugins, one table per owner. Only [`crate::deliver`] and
/// [`crate::intake`] append; each owner drains its queue (`Vec::drain`) once per frame.
/// Every queue is bounded by its `*_MAX` constant ([`crate::EDITS_MAX`] and siblings).
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

impl IoOp {
    /// Returns the capability the operation needs.
    pub const fn cap(self) -> Cap {
        match self {
            IoOp::FsRead | IoOp::FsList => Cap::ReadProject,
            IoOp::FsWrite | IoOp::FsRemove => Cap::WriteProject,
            IoOp::Spawn => Cap::Spawn,
            IoOp::Fetch => Cap::Network,
            IoOp::ClipboardGet | IoOp::ClipboardSet => Cap::Clipboard,
        }
    }
}
