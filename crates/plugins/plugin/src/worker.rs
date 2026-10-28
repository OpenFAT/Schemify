//! Worker events (`Plugins::set_worker`): a plugin's store, its event and a copy of the
//! snapshots in → a thread that runs the event and hands the store back with the outcome
//! and output. Cites plugin-abi §2 (one event of a plugin at a time).
//!
//! Contents:
//! - `pub(crate) struct Done`
//! - `pub(crate) fn spawn(store, exports, fuel, event, echo, snapshots, room) -> JoinHandle<Done>`
//! - `pub(crate) fn join(job: JoinHandle<Done>) -> Done`

use std::thread::JoinHandle;

use schemify_abi::Code;
use wasmi::Store;

use crate::Snapshots;
use crate::runtime::{self, Ctx, Exports, Room};

/// What a worker hands back: the plugin's store (idle again) and the event's outcome and
/// output, taken in by the next `deliver` after it finished.
pub(crate) struct Done {
    pub store: Store<Ctx>,
    pub outcome: Result<(), Code>,
    pub output: Vec<u8>,
    pub event: Vec<u8>,
    /// Whether rejections of this event are echoed (false for a `Rejected` event).
    pub echo: bool,
}

/// Runs `event` on a new thread over `store`, which owns `snapshots` for the event and may
/// fill `room` through its imports.
///
/// # Panics
///
/// Panics if the store is live or has staged requests, `snapshots` is a placeholder, or the
/// OS refuses a thread (`std::thread::spawn`).
pub(crate) fn spawn(
    mut store: Store<Ctx>,
    exports: Exports,
    fuel: u64,
    event: Vec<u8>,
    echo: bool,
    snapshots: Snapshots,
    room: Room,
) -> JoinHandle<Done> {
    let ctx = store.data_mut();
    assert!(!ctx.live);
    assert!(ctx.staged.is_empty());
    assert!(snapshots.is_lent());
    ctx.snapshots = snapshots;
    ctx.live = true;
    ctx.room = room;
    std::thread::spawn(move || {
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
    })
}

/// Waits for `job`.
///
/// # Panics
///
/// Re-raises a panic of the worker (a host bug: no guest input panics the host).
pub(crate) fn join(job: JoinHandle<Done>) -> Done {
    let done = job
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    debug_assert!(!done.store.data().live);
    done
}
