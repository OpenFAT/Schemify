//! Worker events (`Plugins::set_worker`): a plugin's store, its event and a copy of the
//! snapshots in → a thread that runs the event and hands the store back with the outcome
//! and output. Cites plugin-abi §2 (one event of a plugin at a time).
//!
//! Contents:
//! - `pub(crate) struct Done`
//! - `pub(crate) fn spawn(store, exports, fuel, event, echo, snapshots, room) -> JoinHandle<Done>`
//! - `pub(crate) fn join(job: JoinHandle<Done>) -> Done`

use std::panic;
use std::thread::{self, JoinHandle};

use schemify_abi::Code;
use wasmi::Store;

use crate::Snapshots;
use crate::deliver::Echo;
use crate::runtime::{self, Context, Exports, Room};

/// What a worker hands back: the plugin's store (idle again) and the event's outcome and
/// output, taken in by the next `deliver` after it finished.
pub(crate) struct Done {
    pub store: Store<Context>,
    pub outcome: Result<(), Code>,
    pub output: Vec<u8>,
    pub event: Vec<u8>,
    /// Whether rejections of this event are echoed.
    pub echo: Echo,
}

/// Runs `event` on a new thread over `store`, which owns `snapshots` for the event and may
/// fill `room` through its imports.
///
/// # Panics
///
/// Panics if the store is live or has staged requests, `snapshots` is a placeholder, or the
/// OS refuses a thread (`std::thread::spawn`).
pub(crate) fn spawn(
    mut store: Store<Context>,
    exports: Exports,
    fuel: u64,
    event: Vec<u8>,
    echo: Echo,
    snapshots: Snapshots,
    room: Room,
) -> JoinHandle<Done> {
    assert!(fuel > 0);
    let context = store.data_mut();
    assert!(!context.live);
    assert!(context.staged.is_empty());
    assert!(
        snapshots.is_lent(),
        "the worker reads its own copy of the caller's snapshots"
    );
    context.snapshots = snapshots;
    context.live = true;
    context.room = room;
    thread::spawn(move || {
        let mut output = Vec::new();
        let outcome = runtime::run_event(&mut store, &exports, &event, fuel, &mut output);
        let context = store.data_mut();
        context.live = false;
        context.snapshots = Snapshots::placeholder();
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
        .unwrap_or_else(|payload| panic::resume_unwind(payload));
    debug_assert!(!done.store.data().live);
    debug_assert!(
        !done.store.data().snapshots.is_lent(),
        "the worker dropped its snapshot copy"
    );
    done
}
