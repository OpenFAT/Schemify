//! wasmi glue: engine and linker, per-plugin stores with a memory limiter, fuel per call,
//! the §3 imports, and the two guest entry points (`schemify_on_event`, `schemify_call`).
//!
//! Every import validates its arguments against the plugin's own memory and returns a
//! negated [`Code`]; no guest input can make the host panic.

use schemify_abi::{Code, IMPORT_MODULE, Level, MEMORY_BYTES_MAX, Table, exports, text, unpack};
use wasmi::{
    Caller, Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
    TrapCode, TypedFunc,
};

use crate::intake::truncate;
use crate::{
    EvalRequest, Grants, IO_BYTES_MAX, IoOp, IoRequest, LOG_BYTES_MAX, LogEntry, OUTPUT_BYTES_MAX,
    PluginId, Snapshots, authorize,
};

/// Longest expression an `eval` import accepts.
const EXPR_BYTES_MAX: usize = 64 << 10;
/// Longest path, URL or argv line a capability import accepts.
const TARGET_BYTES_MAX: usize = 8 << 10;

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

impl Staged {
    pub(crate) fn is_empty(&self) -> bool {
        self.io.is_empty() && self.evals.is_empty() && self.logs.is_empty()
    }
}

/// Per-plugin store data.
pub(crate) struct Ctx {
    limits: StoreLimits,
    plugin: PluginId,
    pub grants: Grants,
    memory: Option<Memory>,
    /// The caller's tables while `live`; a placeholder otherwise.
    pub snapshots: Snapshots,
    /// True only inside `schemify_on_event`; every import is denied otherwise (start
    /// function, `schemify_alloc` outside an event, pure `schemify_call`).
    pub live: bool,
    pub room: Room,
    pub staged: Staged,
    /// Contract violations seen during the call; reported whatever the call's outcome.
    pub faults: Vec<(Code, String)>,
    next_handle: u32,
}

impl Ctx {
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

    /// Issues the next handle; handles are positive `i32`s, unique per plugin.
    fn issue(&mut self) -> Result<u32, Code> {
        if self.next_handle > i32::MAX as u32 {
            return Err(Code::Space);
        }
        let h = self.next_handle;
        self.next_handle += 1;
        Ok(h)
    }
}

/// Typed exports of one instance.
pub(crate) struct Exports {
    alloc: TypedFunc<u32, u32>,
    on_event: TypedFunc<(u32, u32), u64>,
    call: Option<TypedFunc<(u32, u32, u32), u64>>,
}

pub(crate) fn engine() -> Engine {
    let mut config = Config::default();
    config.consume_fuel(true);
    Engine::new(&config)
}

pub(crate) fn linker(engine: &Engine) -> Linker<Ctx> {
    let mut l = Linker::new(engine);
    let m = IMPORT_MODULE;
    let err = "import names are unique";
    l.func_wrap(m, "table_len", table_len).expect(err);
    l.func_wrap(m, "table_epoch", table_epoch).expect(err);
    l.func_wrap(
        m,
        "read_column",
        |mut c: Caller<'_, Ctx>, t: u32, col: u32, s: u32, n: u32, d: u32, dl: u32| {
            let r = read_column(&mut c, t, col, s, n, d, dl);
            charge(&mut c, r.max(0) as usize);
            r
        },
    )
    .expect(err);
    l.func_wrap(
        m,
        "read_strings",
        |mut c: Caller<'_, Ctx>, t: u32, col: u32, s: u32, n: u32, d: u32, dl: u32| {
            let r = read_strings(&mut c, t, col, s, n, d, dl);
            charge(&mut c, r.max(0) as usize);
            r
        },
    )
    .expect(err);
    l.func_wrap(
        m,
        "read_record",
        |mut c: Caller<'_, Ctx>, doc: u32, id: u32, d: u32, dl: u32| {
            let r = read_record(&mut c, doc, id, d, dl);
            charge(&mut c, r.max(0) as usize);
            r
        },
    )
    .expect(err);
    l.func_wrap(
        m,
        "eval",
        |mut c: Caller<'_, Ctx>, doc: u32, p: u32, pl: u32| {
            charge(&mut c, pl as usize);
            eval(&mut c, doc, p, pl)
        },
    )
    .expect(err);
    l.func_wrap(
        m,
        "log",
        |mut c: Caller<'_, Ctx>, level: u32, p: u32, pl: u32| {
            charge(&mut c, pl as usize);
            log(&mut c, level, p, pl)
        },
    )
    .expect(err);
    l.func_wrap(m, "fs_read", |mut c: Caller<'_, Ctx>, p: u32, pl: u32| {
        request(&mut c, IoOp::FsRead, (p, pl), (0, 0))
    })
    .expect(err);
    l.func_wrap(
        m,
        "fs_write",
        |mut c: Caller<'_, Ctx>, p: u32, pl: u32, d: u32, dl: u32| {
            request(&mut c, IoOp::FsWrite, (p, pl), (d, dl))
        },
    )
    .expect(err);
    l.func_wrap(
        m,
        "spawn",
        |mut c: Caller<'_, Ctx>, a: u32, al: u32, s: u32, sl: u32| {
            request(&mut c, IoOp::Spawn, (a, al), (s, sl))
        },
    )
    .expect(err);
    l.func_wrap(
        m,
        "fetch",
        |mut c: Caller<'_, Ctx>, u: u32, ul: u32, b: u32, bl: u32| {
            request(&mut c, IoOp::Fetch, (u, ul), (b, bl))
        },
    )
    .expect(err);
    l.func_wrap(m, "clipboard_get", |mut c: Caller<'_, Ctx>| {
        request(&mut c, IoOp::ClipboardGet, (0, 0), (0, 0))
    })
    .expect(err);
    l.func_wrap(
        m,
        "clipboard_set",
        |mut c: Caller<'_, Ctx>, p: u32, pl: u32| {
            request(&mut c, IoOp::ClipboardSet, (0, 0), (p, pl))
        },
    )
    .expect(err);
    l
}

/// Compiles, links and starts `wasm`, then checks exports and the ABI version.
pub(crate) fn instantiate(
    engine: &Engine,
    linker: &Linker<Ctx>,
    wasm: &[u8],
    ctx: Ctx,
    fuel: u64,
) -> Result<(Store<Ctx>, Exports), (Code, String)> {
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
    let missing =
        |name: &str, e: &dyn std::fmt::Display| (Code::Module, format!("export {name}: {e}"));
    let memory = instance
        .get_memory(&store, exports::MEMORY)
        .ok_or_else(|| missing(exports::MEMORY, &"absent"))?;
    let abi = instance
        .get_typed_func::<(), u32>(&store, exports::ABI)
        .map_err(|e| missing(exports::ABI, &e))?;
    let alloc = instance
        .get_typed_func(&store, exports::ALLOC)
        .map_err(|e| missing(exports::ALLOC, &e))?;
    let on_event = instance
        .get_typed_func(&store, exports::ON_EVENT)
        .map_err(|e| missing(exports::ON_EVENT, &e))?;
    let call = match instance.get_export(&store, exports::CALL) {
        None => None,
        Some(_) => Some(
            instance
                .get_typed_func(&store, exports::CALL)
                .map_err(|e| missing(exports::CALL, &e))?,
        ),
    };
    store.data_mut().memory = Some(memory);
    let version = abi
        .call(&mut store, ())
        .map_err(|e| (trap(&e), e.to_string()))?;
    if version != schemify_abi::ABI_VERSION {
        return Err((
            Code::Version,
            format!("abi {version}, host {}", schemify_abi::ABI_VERSION),
        ));
    }
    debug_assert!(!store.data().live);
    Ok((
        store,
        Exports {
            alloc,
            on_event,
            call,
        },
    ))
}

/// Runs one event: allocate, write, call, copy the output into `out`.
///
/// # Errors
///
/// `E_FUEL` / `E_TRAP` from the guest; `E_SPACE` for a 0 from `schemify_alloc` or an output
/// over [`OUTPUT_BYTES_MAX`]; `E_RANGE` for a buffer outside guest memory.
pub(crate) fn run_event(
    store: &mut Store<Ctx>,
    exports: &Exports,
    input: &[u8],
    fuel: u64,
    out: &mut Vec<u8>,
) -> Result<(), Code> {
    assert!(store.data().live && store.data().snapshots.is_lent());
    out.clear();
    store.set_fuel(fuel).expect("fuel metering is enabled");
    let (ptr, len) = write_input(store, exports.alloc, input)?;
    let packed = exports
        .on_event
        .call(&mut *store, (ptr, len))
        .map_err(|e| trap(&e))?;
    let (out_ptr, out_len) = unpack(packed);
    match (out_ptr, out_len) {
        (0, 0) => Ok(()),
        (0, _) => Err(Code::Range),
        _ => read_output(store, out_ptr, out_len, out),
    }
}

/// Runs one pure function call. The outer error is a guest fault (`E_FUEL`, `E_TRAP`,
/// `E_RANGE`, `E_SPACE`); the inner one is the code the function itself returned.
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

fn trap(e: &wasmi::Error) -> Code {
    e.as_trap_code().map_or(Code::Trap, trap_code)
}

fn trap_code(t: TrapCode) -> Code {
    if t == TrapCode::OutOfFuel {
        Code::Fuel
    } else {
        Code::Trap
    }
}

/// Bytes an import may move per unit of fuel.
const BYTES_PER_FUEL: usize = 8;

/// Charges the guest for host work done on its behalf (1 fuel per [`BYTES_PER_FUEL`] bytes
/// moved), so a loop of large reads exhausts the budget like a loop of instructions. The
/// guest traps with `E_FUEL` at its next fuel check once the budget hits zero.
fn charge(caller: &mut Caller<'_, Ctx>, bytes: usize) {
    let cost = (bytes / BYTES_PER_FUEL) as u64;
    if cost == 0 {
        return;
    }
    let fuel = caller.get_fuel().expect("fuel metering is enabled");
    caller
        .set_fuel(fuel.saturating_sub(cost))
        .expect("fuel metering is enabled");
}

/// Splits the caller into its linear memory and store data; `None` before the memory is
/// resolved (inside the start function).
fn split<'a>(caller: &'a mut Caller<'_, Ctx>) -> Option<(&'a mut [u8], &'a mut Ctx)> {
    let memory = caller.data().memory?;
    Some(memory.data_and_store_mut(caller))
}

/// Returns `len` bytes of guest memory at `ptr`; any range for `len == 0`.
fn guest(mem: &[u8], ptr: u32, len: u32) -> Option<&[u8]> {
    let start = ptr as usize;
    if len == 0 {
        return Some(&[]);
    }
    mem.get(start..start.checked_add(len as usize)?)
}

fn guest_mut(mem: &mut [u8], ptr: u32, len: usize) -> Option<&mut [u8]> {
    let start = ptr as usize;
    mem.get_mut(start..start.checked_add(len)?)
}

/// Copies `src` to `dst`; on `E_SPACE` writes the needed size into the first 4 bytes of
/// `dst` when `dst_len >= 4`.
fn copy_out(mem: &mut [u8], dst: u32, dst_len: u32, src: &[u8]) -> i32 {
    if src.len() > dst_len as usize {
        return need_more(mem, dst, dst_len, src.len());
    }
    match guest_mut(mem, dst, src.len()) {
        None => Code::Range.neg(),
        Some(window) => {
            window.copy_from_slice(src);
            i32::try_from(src.len()).expect("snapshot slices stay under 2 GiB")
        }
    }
}

fn need_more(mem: &mut [u8], dst: u32, dst_len: u32, need: usize) -> i32 {
    if dst_len >= 4 {
        let need = u32::try_from(need).unwrap_or(u32::MAX).to_le_bytes();
        match guest_mut(mem, dst, 4) {
            Some(window) => window.copy_from_slice(&need),
            None => return Code::Range.neg(),
        }
    }
    Code::Space.neg()
}

fn table_len(caller: Caller<'_, Ctx>, table: u32) -> i32 {
    let ctx = caller.data();
    if !ctx.live {
        return Code::Denied.neg();
    }
    match Table::from_raw(table) {
        None => Code::Table.neg(),
        Some(t) => i32::try_from(ctx.snapshots.rows(t)).expect("rows <= ROWS_MAX"),
    }
}

/// The epoch masked to 31 bits so it stays a non-negative `i32`; it still changes on
/// every change (until it wraps).
fn table_epoch(caller: Caller<'_, Ctx>, table: u32) -> i32 {
    let ctx = caller.data();
    if !ctx.live {
        return Code::Denied.neg();
    }
    match Table::from_raw(table) {
        None => Code::Table.neg(),
        Some(t) => (ctx.snapshots.epoch(t) & 0x7fff_ffff) as i32,
    }
}

fn read_column(
    caller: &mut Caller<'_, Ctx>,
    table: u32,
    column: u32,
    start: u32,
    count: u32,
    dst: u32,
    dst_len: u32,
) -> i32 {
    let Some((mem, ctx)) = split(caller) else {
        return Code::Denied.neg();
    };
    if !ctx.live {
        return Code::Denied.neg();
    }
    match ctx.snapshots.column_bytes(table, column, start, count) {
        Err(code) => code.neg(),
        Ok(src) => copy_out(mem, dst, dst_len, src),
    }
}

fn read_strings(
    caller: &mut Caller<'_, Ctx>,
    table: u32,
    column: u32,
    start: u32,
    count: u32,
    dst: u32,
    dst_len: u32,
) -> i32 {
    let Some((mem, ctx)) = split(caller) else {
        return Code::Denied.neg();
    };
    if !ctx.live {
        return Code::Denied.neg();
    }
    let need = match ctx.snapshots.strings_len(table, column, start, count) {
        Err(code) => return code.neg(),
        Ok(n) => n,
    };
    if need > dst_len as usize {
        return need_more(mem, dst, dst_len, need);
    }
    let Some(window) = guest_mut(mem, dst, need) else {
        return Code::Range.neg();
    };
    ctx.snapshots
        .write_strings(table, column, start, count, window);
    i32::try_from(need).expect("string slices stay under 2 GiB")
}

fn read_record(caller: &mut Caller<'_, Ctx>, doc: u32, id: u32, dst: u32, dst_len: u32) -> i32 {
    let Some((mem, ctx)) = split(caller) else {
        return Code::Denied.neg();
    };
    if !ctx.live {
        return Code::Denied.neg();
    }
    match ctx.snapshots.record(doc, id) {
        None => Code::Gone.neg(),
        Some(text) => copy_out(mem, dst, dst_len, text),
    }
}

fn eval(caller: &mut Caller<'_, Ctx>, doc: u32, ptr: u32, len: u32) -> i32 {
    let Some((mem, ctx)) = split(caller) else {
        return Code::Denied.neg();
    };
    if !ctx.live {
        return Code::Denied.neg();
    }
    if len as usize > EXPR_BYTES_MAX {
        return Code::Space.neg();
    }
    let Some(bytes) = guest(mem, ptr, len) else {
        return Code::Range.neg();
    };
    let Ok(expr) = std::str::from_utf8(bytes) else {
        return Code::Parse.neg();
    };
    if !ctx.snapshots.has_doc(doc) {
        return Code::Gone.neg();
    }
    if ctx.room.evals == 0 {
        return Code::Space.neg();
    }
    let handle = match ctx.issue() {
        Ok(h) => h,
        Err(code) => return code.neg(),
    };
    ctx.room.evals -= 1;
    ctx.staged.evals.push(EvalRequest {
        plugin: ctx.plugin,
        handle,
        doc,
        expr: expr.to_owned(),
    });
    i32::try_from(handle).expect("handles are positive i32")
}

fn log(caller: &mut Caller<'_, Ctx>, level: u32, ptr: u32, len: u32) {
    let Some((mem, ctx)) = split(caller) else {
        return;
    };
    if !ctx.live {
        return;
    }
    let level = usize::try_from(level)
        .ok()
        .and_then(|l| Level::ALL.get(l).copied());
    let (Some(level), Some(bytes)) = (level, guest(mem, ptr, len)) else {
        ctx.faults
            .push((Code::Range, "log: bad level or buffer".to_owned()));
        return;
    };
    if ctx.room.logs == 0 {
        ctx.faults.push((Code::Space, "log: queue full".to_owned()));
        return;
    }
    ctx.room.logs -= 1;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(LOG_BYTES_MAX + 3)]);
    let text = truncate(&text, LOG_BYTES_MAX).to_owned();
    ctx.staged.logs.push(LogEntry {
        plugin: ctx.plugin,
        level,
        text,
    });
}

/// The capability imports (§5): validate, authorize, stage an [`IoRequest`], return its
/// handle. Denials are also recorded as faults.
fn request(caller: &mut Caller<'_, Ctx>, op: IoOp, target: (u32, u32), data: (u32, u32)) -> i32 {
    charge(caller, target.1 as usize + data.1 as usize);
    let Some((mem, ctx)) = split(caller) else {
        return Code::Denied.neg();
    };
    if !ctx.live {
        ctx.faults
            .push((Code::Denied, format!("{op:?} outside an event handler")));
        return Code::Denied.neg();
    }
    if target.1 as usize > TARGET_BYTES_MAX || data.1 as usize > IO_BYTES_MAX {
        return Code::Space.neg();
    }
    let (Some(target), Some(data)) = (guest(mem, target.0, target.1), guest(mem, data.0, data.1))
    else {
        return Code::Range.neg();
    };
    let Ok(target) = std::str::from_utf8(target) else {
        return Code::Parse.neg();
    };
    let args: Vec<String> = match op {
        IoOp::ClipboardGet | IoOp::ClipboardSet => Vec::new(),
        IoOp::Spawn => match text::values(target)
            .map(|v| v.map(|v| v.into_owned()))
            .collect()
        {
            Ok(argv) => argv,
            Err(code) => return code.neg(),
        },
        IoOp::FsRead | IoOp::FsWrite | IoOp::Fetch => vec![target.to_owned()],
    };
    if let Err(code) = authorize(&ctx.grants, op, &args) {
        ctx.faults.push((code, format!("{op:?} {args:?}")));
        return code.neg();
    }
    if ctx.room.io == 0 {
        return Code::Space.neg();
    }
    let handle = match ctx.issue() {
        Ok(h) => h,
        Err(code) => return code.neg(),
    };
    ctx.room.io -= 1;
    ctx.staged.io.push(IoRequest {
        plugin: ctx.plugin,
        handle,
        op,
        args,
        data: data.to_vec(),
    });
    i32::try_from(handle).expect("handles are positive i32")
}
