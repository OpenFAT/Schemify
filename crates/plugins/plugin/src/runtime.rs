//! wasmi glue (D3): module bytes in → a started store and its typed exports; an event or a
//! pure call in → the guest's output bytes, under a fuel budget and a memory limiter. Cites
//! plugin-abi §2 (exports, ABI version, fuel, buffer protocol) and §6 (pure calls).
//!
//! Contents:
//! - `pub(crate) struct Room`, `Room::left(queues: &RequestQueues) -> Room`
//! - `pub(crate) struct Staged`, `Staged::is_empty(&self) -> bool`
//! - `pub(crate) struct Ctx`, `Ctx::new(plugin: PluginId, grants: Grants) -> Ctx`
//! - `pub(crate) struct Exports`
//! - `pub(crate) fn engine() -> Engine`
//! - `pub(crate) fn instantiate(engine, linker, wasm, ctx, fuel) -> Result<(Store<Ctx>, Exports), (Code, String)>`
//! - `pub(crate) fn run_event(store, exports, input, fuel, out) -> Result<(), Code>`
//! - `pub(crate) fn run_lent(store, exports, input, fuel, snapshots, room, out) -> Result<(), Code>`
//! - `pub(crate) fn run_call(store, exports, fn_id, args, fuel, out) -> Result<Result<(), Code>, Code>`
//! - `pub(crate) fn trap(e: &wasmi::Error) -> Code`

use schemify_abi::{ABI_VERSION, Code, MEMORY_BYTES_MAX, exports, unpack};
use wasmi::{
    Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TrapCode,
    TypedFunc,
};

use crate::{
    EVALS_MAX, EvalRequest, Grants, IO_MAX, IoRequest, LOGS_MAX, LogEntry, OUTPUT_BYTES_MAX,
    PluginId, RequestQueues, Snapshots,
};

/// Remaining queue capacity for the current call; imports refuse with `E_SPACE` at zero,
/// so a handle is only ever returned for a request that will be queued.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Room {
    pub io: usize,
    pub evals: usize,
    pub logs: usize,
}

/// Requests made through imports during the current call; committed to the queues only if
/// the call returns normally.
#[derive(Debug, Default)]
pub(crate) struct Staged {
    pub io: Vec<IoRequest>,
    pub evals: Vec<EvalRequest>,
    pub logs: Vec<LogEntry>,
}

/// Per-plugin store data. Invariant: `live` only inside `schemify_on_event`, and then
/// `snapshots` is the caller's full table set; a placeholder otherwise.
pub(crate) struct Ctx {
    limits: StoreLimits,
    pub plugin: PluginId,
    pub grants: Grants,
    /// The instance's exported memory; `None` until instantiation resolved it.
    pub memory: Option<Memory>,
    pub snapshots: Snapshots,
    /// True only inside `schemify_on_event`; every import is denied otherwise (start
    /// function, `schemify_alloc` outside an event, pure `schemify_call`).
    pub live: bool,
    pub room: Room,
    pub staged: Staged,
    /// Contract violations seen during the call; reported whatever the call's outcome.
    pub faults: Vec<(Code, String)>,
    /// The next I/O or eval handle; handles are positive `i32`s, unique per plugin.
    pub next_handle: u32,
}

/// Typed exports of one instance (handles into its store: cheap to copy, valid only with it).
#[derive(Clone)]
pub(crate) struct Exports {
    alloc: TypedFunc<u32, u32>,
    on_event: TypedFunc<(u32, u32), u64>,
    call: Option<TypedFunc<(u32, u32, u32), u64>>,
}

impl Room {
    /// The room `queues` leave below each queue's capacity.
    pub(crate) fn left(queues: &RequestQueues) -> Room {
        Room {
            io: IO_MAX.saturating_sub(queues.io.len()),
            evals: EVALS_MAX.saturating_sub(queues.evals.len()),
            logs: LOGS_MAX.saturating_sub(queues.logs.len()),
        }
    }
}

impl Staged {
    pub(crate) fn is_empty(&self) -> bool {
        self.io.is_empty() && self.evals.is_empty() && self.logs.is_empty()
    }
}

impl Ctx {
    /// An idle context: no memory, nothing lent, nothing staged; the limiter caps memory at
    /// [`MEMORY_BYTES_MAX`] and the instance count at one.
    pub(crate) fn new(plugin: PluginId, grants: Grants) -> Ctx {
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_BYTES_MAX)
            .memories(1)
            .instances(1)
            .tables(4)
            .table_elements(1 << 20)
            .build();
        Ctx {
            limits,
            plugin,
            grants,
            memory: None,
            snapshots: Snapshots::placeholder(),
            live: false,
            room: Room::default(),
            staged: Staged::default(),
            faults: Vec::new(),
            next_handle: 1,
        }
    }
}

/// A wasmi engine with fuel metering on.
pub(crate) fn engine() -> Engine {
    let mut config = Config::default();
    config.consume_fuel(true);
    Engine::new(&config)
}

/// Compiles, links and starts `wasm` with `fuel` for the start function, then checks the
/// exports and the ABI version.
///
/// # Errors
///
/// `E_MODULE` for invalid wasm, unknown imports, a missing or mistyped export; `E_FUEL` /
/// `E_TRAP` if the start function or `schemify_abi` fails; `E_VERSION` for an ABI
/// mismatch. Each with its detail text.
pub(crate) fn instantiate(
    engine: &Engine,
    linker: &Linker<Ctx>,
    wasm: &[u8],
    ctx: Ctx,
    fuel: u64,
) -> Result<(Store<Ctx>, Exports), (Code, String)> {
    assert!(fuel > 0);
    assert!(!ctx.live && ctx.memory.is_none());
    let module = Module::new(engine, wasm).map_err(|e| (Code::Module, e.to_string()))?;
    let mut store = Store::new(engine, ctx);
    store.limiter(|ctx| &mut ctx.limits);
    store.set_fuel(fuel).expect("fuel metering is enabled");
    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(|e| {
            (
                e.as_trap_code().map_or(Code::Module, trap_code),
                e.to_string(),
            )
        })?;
    let memory = instance
        .get_memory(&store, exports::MEMORY)
        .ok_or_else(|| missing(exports::MEMORY, "absent"))?;
    let abi = instance
        .get_typed_func::<(), u32>(&store, exports::ABI)
        .map_err(|e| missing(exports::ABI, e))?;
    let alloc = instance
        .get_typed_func(&store, exports::ALLOC)
        .map_err(|e| missing(exports::ALLOC, e))?;
    let on_event = instance
        .get_typed_func(&store, exports::ON_EVENT)
        .map_err(|e| missing(exports::ON_EVENT, e))?;
    // `schemify_call` is optional; present, it must have its type.
    let call = instance
        .get_export(&store, exports::CALL)
        .map(|_| instance.get_typed_func(&store, exports::CALL))
        .transpose()
        .map_err(|e| missing(exports::CALL, e))?;
    store.data_mut().memory = Some(memory);
    let version = abi
        .call(&mut store, ())
        .map_err(|e| (trap(&e), e.to_string()))?;
    if version != ABI_VERSION {
        return Err((Code::Version, format!("abi {version}, host {ABI_VERSION}")));
    }
    debug_assert!(!store.data().live);
    let exports = Exports {
        alloc,
        on_event,
        call,
    };
    Ok((store, exports))
}

/// Runs one event: allocate, write `input`, call, copy the output into `out`.
///
/// # Errors
///
/// `E_FUEL` / `E_TRAP` from the guest; `E_SPACE` for a 0 from `schemify_alloc` or an output
/// over [`OUTPUT_BYTES_MAX`]; `E_RANGE` for a buffer outside guest memory.
///
/// # Panics
///
/// Panics unless the store is live with the caller's snapshots lent.
pub(crate) fn run_event(
    store: &mut Store<Ctx>,
    exports: &Exports,
    input: &[u8],
    fuel: u64,
    out: &mut Vec<u8>,
) -> Result<(), Code> {
    assert!(store.data().live);
    assert!(store.data().snapshots.is_lent());
    out.clear();
    store.set_fuel(fuel).expect("fuel metering is enabled");
    let (ptr, len) = write_input(store, exports.alloc, input)?;
    let packed = exports
        .on_event
        .call(&mut *store, (ptr, len))
        .map_err(|e| trap(&e))?;
    match unpack(packed) {
        (0, 0) => Ok(()),
        (0, _) => Err(Code::Range),
        (out_ptr, out_len) => read_output(store, out_ptr, out_len, out),
    }
}

/// [`run_event`] with the caller's `snapshots` lent to the store for the call (handed back
/// unchanged) and `room` as the queue room its imports may fill.
///
/// # Panics
///
/// Panics if the store is live or has staged requests, or `snapshots` is a placeholder.
pub(crate) fn run_lent(
    store: &mut Store<Ctx>,
    exports: &Exports,
    input: &[u8],
    fuel: u64,
    snapshots: &mut Snapshots,
    room: Room,
    out: &mut Vec<u8>,
) -> Result<(), Code> {
    let ctx = store.data_mut();
    assert!(!ctx.live);
    assert!(ctx.staged.is_empty());
    assert!(snapshots.is_lent());
    std::mem::swap(&mut ctx.snapshots, snapshots);
    ctx.live = true;
    ctx.room = room;
    let outcome = run_event(store, exports, input, fuel, out);
    let ctx = store.data_mut();
    ctx.live = false;
    std::mem::swap(&mut ctx.snapshots, snapshots);
    debug_assert!(snapshots.is_lent());
    outcome
}

/// Runs one pure function call with every import denied. The outer error is a guest fault
/// (`E_FUEL`, `E_TRAP`, `E_RANGE`, `E_SPACE`); the inner one is the code the function itself
/// returned (`E_RANGE` without a `schemify_call` export).
///
/// # Panics
///
/// Panics if the store is live (an event is running on it).
pub(crate) fn run_call(
    store: &mut Store<Ctx>,
    exports: &Exports,
    fn_id: u32,
    args: &[u8],
    fuel: u64,
    out: &mut Vec<u8>,
) -> Result<Result<(), Code>, Code> {
    assert!(!store.data().live);
    out.clear();
    let Some(call) = exports.call else {
        return Ok(Err(Code::Range));
    };
    store.set_fuel(fuel).expect("fuel metering is enabled");
    let (ptr, len) = write_input(store, exports.alloc, args)?;
    let packed = call
        .call(&mut *store, (fn_id, ptr, len))
        .map_err(|e| trap(&e))?;
    match unpack(packed) {
        (0, code) => Ok(Err(Code::from_raw(code).unwrap_or(Code::Range))),
        (ptr, len) => read_output(store, ptr, len, out).map(Ok),
    }
}

/// The code of a guest failure: `E_FUEL` for fuel exhaustion, `E_TRAP` otherwise.
pub(crate) fn trap(e: &wasmi::Error) -> Code {
    e.as_trap_code().map_or(Code::Trap, trap_code)
}

// ---- private ----

/// The load error for export `name` (absent or mistyped).
fn missing(name: &str, e: impl std::fmt::Display) -> (Code, String) {
    (Code::Module, format!("export {name}: {e}"))
}

/// Copies `input` into a buffer from the guest's `schemify_alloc`; returns its (ptr, len).
fn write_input(
    store: &mut Store<Ctx>,
    alloc: TypedFunc<u32, u32>,
    input: &[u8],
) -> Result<(u32, u32), Code> {
    let len = u32::try_from(input.len()).map_err(|_| Code::Space)?;
    let ptr = alloc.call(&mut *store, len).map_err(|e| trap(&e))?;
    if ptr == 0 {
        return Err(Code::Space);
    }
    let memory = store.data().memory.expect("resolved at load");
    memory
        .write(&mut *store, ptr as usize, input)
        .map_err(|_| Code::Range)?;
    Ok((ptr, len))
}

/// Copies the guest's output buffer into `out`.
fn read_output(store: &Store<Ctx>, ptr: u32, len: u32, out: &mut Vec<u8>) -> Result<(), Code> {
    if len as usize > OUTPUT_BYTES_MAX {
        return Err(Code::Space);
    }
    let memory = store.data().memory.expect("resolved at load");
    out.resize(len as usize, 0);
    memory
        .read(store, ptr as usize, out)
        .map_err(|_| Code::Range)
}

fn trap_code(t: TrapCode) -> Code {
    if t == TrapCode::OutOfFuel {
        Code::Fuel
    } else {
        Code::Trap
    }
}
