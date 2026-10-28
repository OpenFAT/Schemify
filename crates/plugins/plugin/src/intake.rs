//! Output intake (§7, §8): one plugin's output bytes → contribution sets, request queues
//! and rejections. Pure over its parameters, so it is fuzzed without any wasm.
//!
//! Contents:
//! - `pub struct Rejection`
//! - `pub fn intake(plugin: PluginId, output: &[u8], snapshots: &Snapshots,
//!   contributions: &mut Contributions, queues: &mut RequestQueues,
//!   rejections: &mut Vec<Rejection>) -> u32`

use schemify_abi::{Code, Output, text};

use crate::contributions::{set_records, truncate};
use crate::{
    CONTRIBUTION_BYTES_MAX, Contributions, EDITS_MAX, EditRequest, LOG_BYTES_MAX, LOGS_MAX,
    LogEntry, PluginId, REJECT_ECHO_BYTES, RUNS_MAX, RequestQueues, RunRequest, Snapshots,
    TIMER_MS_MAX, TIMERS_MAX, TimerRequest,
};

/// One rejected output record (§8). Goes to the health report and, unless it arose while
/// handling a rejection, back to the plugin as `Event::Rejected`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    /// Producing plugin.
    pub plugin: PluginId,
    /// Why.
    pub code: Code,
    /// The record, truncated to [`REJECT_ECHO_BYTES`].
    pub record: Box<[u8]>,
}

/// Takes one plugin's output in: each record is accepted or rejected on its own.
/// Contributions replace the plugin's set; requests are appended to `queues`; rejections
/// are appended to `rejections`. Returns the number of accepted records.
///
/// Rejection codes: `E_PARSE` (invalid UTF-8, malformed or unknown record, record word not
/// legal for the contribution kind), `E_GONE` (`edit` of a document not in
/// `snapshots`' `Docs`), `E_RANGE` (timer over [`TIMER_MS_MAX`]), `E_SPACE` (queue or
/// contribution table full, set over [`CONTRIBUTION_BYTES_MAX`]).
///
/// # Time complexity
///
/// One format lex per `contribute` record, one `Docs` scan per `edit` record.
pub fn intake(
    plugin: PluginId,
    output: &[u8],
    snapshots: &Snapshots,
    contributions: &mut Contributions,
    queues: &mut RequestQueues,
    rejections: &mut Vec<Rejection>,
) -> u32 {
    let rejected_before = rejections.len();
    let mut accepted = 0u32;
    let mut chunks = 0usize;
    for chunk in text::chunks(output) {
        chunks += 1;
        match accept(plugin, chunk, snapshots, contributions, queues) {
            Ok(()) => accepted += 1,
            Err(code) => rejections.push(Rejection {
                plugin,
                code,
                record: chunk[..chunk.len().min(REJECT_ECHO_BYTES)].into(),
            }),
        }
    }
    debug_assert_eq!(
        accepted as usize + rejections.len() - rejected_before,
        chunks
    );
    accepted
}

// ---- private ----

/// Takes one record in, or says why not (nothing changed then).
fn accept(
    plugin: PluginId,
    chunk: &[u8],
    snapshots: &Snapshots,
    contributions: &mut Contributions,
    queues: &mut RequestQueues,
) -> Result<(), Code> {
    let chunk = std::str::from_utf8(chunk).map_err(|_| Code::Parse)?;
    match Output::parse(chunk)? {
        Output::Contribute { kind, scope, body } => {
            if body.len() > CONTRIBUTION_BYTES_MAX {
                return Err(Code::Space);
            }
            set_records(kind, &body)?;
            contributions.set(plugin, kind, &scope, &body)
        }
        Output::Edit {
            doc,
            mode,
            label,
            ops,
        } => {
            if !snapshots.has_doc(doc) {
                return Err(Code::Gone);
            }
            let label = label.into_owned();
            let req = EditRequest {
                plugin,
                doc,
                mode,
                label,
                ops,
            };
            push_bounded(&mut queues.edits, EDITS_MAX, req)
        }
        Output::Run {
            bench,
            config,
            corner,
            vary,
        } => {
            let req = RunRequest {
                plugin,
                bench: bench.into_owned(),
                config: config.into_owned(),
                corner: corner.into_owned(),
                vary: vary
                    .into_iter()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect(),
            };
            push_bounded(&mut queues.runs, RUNS_MAX, req)
        }
        Output::Timer { ms, .. } if ms > TIMER_MS_MAX => Err(Code::Range),
        Output::Timer { id, ms } => push_bounded(
            &mut queues.timers,
            TIMERS_MAX,
            TimerRequest { plugin, id, ms },
        ),
        Output::Log { level, text } => {
            let text = truncate(&text, LOG_BYTES_MAX).to_owned();
            let entry = LogEntry {
                plugin,
                level,
                text,
            };
            push_bounded(&mut queues.logs, LOGS_MAX, entry)
        }
    }
}

/// Appends `item` unless `queue` holds `max` already.
fn push_bounded<T>(queue: &mut Vec<T>, max: usize, item: T) -> Result<(), Code> {
    // A worker's staged logs may already sit past `max` (`Plugins` reserves room per event).
    if queue.len() >= max {
        return Err(Code::Space);
    }
    queue.push(item);
    debug_assert!(queue.len() <= max);
    Ok(())
}
