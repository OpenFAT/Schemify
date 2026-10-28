//! The §3 imports: a guest's call (raw `u32` arguments over its own linear memory) in →
//! a negated [`Code`] or a byte count / handle out, plus staged requests and faults in the
//! store's [`Ctx`]. Every import validates its arguments against the plugin's own memory;
//! no guest input can make the host panic. Host work is charged as fuel. Cites plugin-abi
//! §3 (imports, the `E_SPACE` size protocol), §4 (snapshot reads), §5 (capabilities).
//!
//! Contents:
//! - `pub(crate) fn linker(engine: &Engine) -> Linker<Ctx>`

use schemify_abi::{Code, IMPORT_MODULE, Level, Table};
use wasmi::{Caller, Engine, Linker};

use crate::contributions::{line_values, truncate};
use crate::runtime::Ctx;
use crate::{EvalRequest, IO_BYTES_MAX, IoOp, IoRequest, LOG_BYTES_MAX, LogEntry, authorize};

/// Longest expression an `eval` import accepts.
const EXPR_BYTES_MAX: usize = 64 << 10;
/// Longest path, URL or argv line a capability import accepts.
const TARGET_BYTES_MAX: usize = 8 << 10;
/// Bytes an import may move per unit of fuel.
const BYTES_PER_FUEL: usize = 8;

/// A linker with every §3 import of [`IMPORT_MODULE`]; anything else fails to link.
pub(crate) fn linker(engine: &Engine) -> Linker<Ctx> {
    let mut l = Linker::new(engine);
    let m = IMPORT_MODULE;
    let err = "import names are unique";
    l.func_wrap(m, "table_len", table_len).expect(err);
    l.func_wrap(m, "table_epoch", table_epoch).expect(err);
    l.func_wrap(m, "read_column", read_column).expect(err);
    l.func_wrap(m, "read_strings", read_strings).expect(err);
    l.func_wrap(m, "eval", eval).expect(err);
    l.func_wrap(m, "log", log).expect(err);
    l.func_wrap(m, "fs_read", |c: Caller<'_, Ctx>, p: u32, pl: u32| {
        request(c, IoOp::FsRead, (p, pl), (0, 0))
    })
    .expect(err);
    l.func_wrap(m, "fs_list", |c: Caller<'_, Ctx>, p: u32, pl: u32| {
        request(c, IoOp::FsList, (p, pl), (0, 0))
    })
    .expect(err);
    l.func_wrap(m, "fs_write", |c: Caller<'_, Ctx>, p, pl, d, dl| {
        request(c, IoOp::FsWrite, (p, pl), (d, dl))
    })
    .expect(err);
    l.func_wrap(m, "fs_remove", |c: Caller<'_, Ctx>, p: u32, pl: u32| {
        request(c, IoOp::FsRemove, (p, pl), (0, 0))
    })
    .expect(err);
    l.func_wrap(m, "spawn", |c: Caller<'_, Ctx>, a, al, s, sl| {
        request(c, IoOp::Spawn, (a, al), (s, sl))
    })
    .expect(err);
    l.func_wrap(m, "fetch", |c: Caller<'_, Ctx>, u, ul, b, bl| {
        request(c, IoOp::Fetch, (u, ul), (b, bl))
    })
    .expect(err);
    l.func_wrap(m, "clipboard_get", |c: Caller<'_, Ctx>| {
        request(c, IoOp::ClipboardGet, (0, 0), (0, 0))
    })
    .expect(err);
    l.func_wrap(m, "clipboard_set", |c: Caller<'_, Ctx>, p: u32, pl: u32| {
        request(c, IoOp::ClipboardSet, (0, 0), (p, pl))
    })
    .expect(err);
    l
}

// ---- private ----

fn table_len(caller: Caller<'_, Ctx>, table: u32) -> i32 {
    let ctx = caller.data();
    match Table::from_raw(table) {
        _ if !ctx.live => Code::Denied.neg(),
        None => Code::Table.neg(),
        Some(t) => i32::try_from(ctx.snapshots.rows(t)).expect("rows <= ROWS_MAX"),
    }
}

/// The epoch masked to 31 bits so it stays a non-negative `i32`; it still changes on
/// every change (until it wraps).
fn table_epoch(caller: Caller<'_, Ctx>, table: u32) -> i32 {
    let ctx = caller.data();
    match Table::from_raw(table) {
        _ if !ctx.live => Code::Denied.neg(),
        None => Code::Table.neg(),
        Some(t) => (ctx.snapshots.epoch(t) & 0x7fff_ffff) as i32,
    }
}

/// Like every bulk read, charges for the bytes it copied.
fn read_column(
    mut caller: Caller<'_, Ctx>,
    table: u32,
    column: u32,
    start: u32,
    count: u32,
    dst: u32,
    dst_len: u32,
) -> i32 {
    let r = match live(&mut caller) {
        None => Code::Denied.neg(),
        Some((mem, ctx)) => match ctx.snapshots.column_bytes(table, column, start, count) {
            Err(code) => code.neg(),
            Ok(src) => copy_out(mem, dst, dst_len, src),
        },
    };
    charge(&mut caller, r.max(0) as usize);
    r
}

fn read_strings(
    mut caller: Caller<'_, Ctx>,
    table: u32,
    column: u32,
    start: u32,
    count: u32,
    dst: u32,
    dst_len: u32,
) -> i32 {
    let r = match live(&mut caller) {
        None => Code::Denied.neg(),
        Some((mem, ctx)) => match ctx.snapshots.strings_len(table, column, start, count) {
            Err(code) => code.neg(),
            Ok(need) if need > dst_len as usize => need_more(mem, dst, dst_len, need),
            Ok(need) => match guest_mut(mem, dst, need) {
                None => Code::Range.neg(),
                Some(window) => {
                    ctx.snapshots
                        .write_strings(table, column, start, count, window);
                    i32::try_from(need).expect("string slices stay under 2 GiB")
                }
            },
        },
    };
    charge(&mut caller, r.max(0) as usize);
    r
}

fn eval(mut caller: Caller<'_, Ctx>, doc: u32, ptr: u32, len: u32) -> i32 {
    charge(&mut caller, len as usize);
    let Some((mem, ctx)) = live(&mut caller) else {
        return Code::Denied.neg();
    };
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
    let handle = match issue(ctx) {
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

fn log(mut caller: Caller<'_, Ctx>, level: u32, ptr: u32, len: u32) {
    charge(&mut caller, len as usize);
    let Some((mem, ctx)) = live(&mut caller) else {
        return;
    };
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
    // A lossy decode turns at most 3 cut bytes into one U+FFFD, which `truncate` then cuts.
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
fn request(mut caller: Caller<'_, Ctx>, op: IoOp, target: (u32, u32), data: (u32, u32)) -> i32 {
    charge(&mut caller, target.1 as usize + data.1 as usize);
    let Some((mem, ctx)) = live(&mut caller) else {
        // Inside the start function (no memory yet) the denial is not recorded.
        let ctx = caller.data_mut();
        if ctx.memory.is_some() {
            let detail = format!("{op:?} outside an event handler");
            ctx.faults.push((Code::Denied, detail));
        }
        return Code::Denied.neg();
    };
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
        IoOp::Spawn => match line_values(target) {
            Ok(argv) => argv,
            Err(code) => return code.neg(),
        },
        IoOp::FsRead | IoOp::FsList | IoOp::FsWrite | IoOp::FsRemove | IoOp::Fetch => {
            vec![target.to_owned()]
        }
    };
    if let Err(code) = authorize(&ctx.grants, op, &args) {
        ctx.faults.push((code, format!("{op:?} {args:?}")));
        return code.neg();
    }
    if ctx.room.io == 0 {
        return Code::Space.neg();
    }
    let handle = match issue(ctx) {
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

/// Splits the caller into its linear memory and store data; `None` outside an event handler
/// (start function, `schemify_alloc` outside an event, pure `schemify_call`), where every
/// import is denied.
fn live<'a>(caller: &'a mut Caller<'_, Ctx>) -> Option<(&'a mut [u8], &'a mut Ctx)> {
    let memory = caller.data().memory.filter(|_| caller.data().live)?;
    Some(memory.data_and_store_mut(caller))
}

/// Returns `len` bytes of guest memory at `ptr`; any range for `len == 0`.
fn guest(mem: &[u8], ptr: u32, len: u32) -> Option<&[u8]> {
    if len == 0 {
        return Some(&[]);
    }
    let start = ptr as usize;
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

/// The §3 size protocol: `E_SPACE`, with `need` written to `dst` when it holds 4 bytes.
fn need_more(mem: &mut [u8], dst: u32, dst_len: u32, need: usize) -> i32 {
    debug_assert!(need > dst_len as usize);
    if dst_len >= 4 {
        let need = u32::try_from(need).unwrap_or(u32::MAX).to_le_bytes();
        match guest_mut(mem, dst, 4) {
            Some(window) => window.copy_from_slice(&need),
            None => return Code::Range.neg(),
        }
    }
    Code::Space.neg()
}

/// Issues the next handle; handles are positive `i32`s, unique per plugin.
fn issue(ctx: &mut Ctx) -> Result<u32, Code> {
    if ctx.next_handle > i32::MAX as u32 {
        return Err(Code::Space);
    }
    let h = ctx.next_handle;
    ctx.next_handle += 1;
    debug_assert!(h > 0);
    Ok(h)
}
