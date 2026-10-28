//! The §3 imports: a guest's call (raw `u32` arguments over its own linear memory) in →
//! a negated [`Code`] or a byte count / handle out, plus staged requests and faults in the
//! store's [`Context`]. Every import validates its arguments against the plugin's own memory;
//! no guest input can make the host panic. Host work is charged as fuel. Cites plugin-abi
//! §3 (imports, the `E_SPACE` size protocol), §4 (snapshot reads), §5 (capabilities).
//!
//! Contents:
//! - `pub(crate) fn linker(engine: &Engine) -> Linker<Context>`

use schemify_abi::{Code, IMPORT_MODULE, Level, Table};
use wasmi::{Caller, Engine, Linker};

use crate::runtime::{Context, Failure};
use crate::{
    EVALS_MAX, EvalRequest, IO_BYTES_MAX, IO_MAX, IoOp, IoRequest, LOG_BYTES_MAX, LOGS_MAX,
    LogEntry, contributions, grants,
};

/// Longest expression an `eval` import accepts.
const EXPR_BYTES_MAX: usize = 64 << 10;
/// Longest path, URL or argv line a capability import accepts.
const TARGET_BYTES_MAX: usize = 8 << 10;
/// Bytes an import may move per unit of fuel.
const BYTES_PER_FUEL: usize = 8;

/// A guest buffer named by an import's (pointer, length) arguments.
#[derive(Clone, Copy, Debug)]
struct GuestSlice {
    ptr: u32,
    len: u32,
}

/// A linker with every §3 import of [`IMPORT_MODULE`]; anything else fails to link.
pub(crate) fn linker(engine: &Engine) -> Linker<Context> {
    let mut linker = Linker::new(engine);
    let module = IMPORT_MODULE;
    let unique = "import names are unique";
    let none = GuestSlice { ptr: 0, len: 0 };
    let slice = |ptr, len| GuestSlice { ptr, len };
    linker
        .func_wrap(module, "table_len", table_len)
        .expect(unique);
    linker
        .func_wrap(module, "table_epoch", table_epoch)
        .expect(unique);
    linker
        .func_wrap(module, "read_column", read_column)
        .expect(unique);
    linker
        .func_wrap(module, "read_strings", read_strings)
        .expect(unique);
    linker.func_wrap(module, "eval", eval).expect(unique);
    linker.func_wrap(module, "log", log).expect(unique);
    linker
        .func_wrap(
            module,
            "fs_read",
            move |caller: Caller<'_, Context>, path, path_len| {
                request(caller, IoOp::FsRead, slice(path, path_len), none)
            },
        )
        .expect(unique);
    linker
        .func_wrap(
            module,
            "fs_list",
            move |caller: Caller<'_, Context>, path, path_len| {
                request(caller, IoOp::FsList, slice(path, path_len), none)
            },
        )
        .expect(unique);
    linker
        .func_wrap(
            module,
            "fs_write",
            move |caller: Caller<'_, Context>, path, path_len, data, data_len| {
                let data = slice(data, data_len);
                request(caller, IoOp::FsWrite, slice(path, path_len), data)
            },
        )
        .expect(unique);
    linker
        .func_wrap(
            module,
            "fs_remove",
            move |caller: Caller<'_, Context>, path, path_len| {
                request(caller, IoOp::FsRemove, slice(path, path_len), none)
            },
        )
        .expect(unique);
    linker
        .func_wrap(
            module,
            "spawn",
            move |caller: Caller<'_, Context>, argv, argv_len, stdin, stdin_len| {
                let stdin = slice(stdin, stdin_len);
                request(caller, IoOp::Spawn, slice(argv, argv_len), stdin)
            },
        )
        .expect(unique);
    linker
        .func_wrap(
            module,
            "fetch",
            move |caller: Caller<'_, Context>, url, url_len, body, body_len| {
                let body = slice(body, body_len);
                request(caller, IoOp::Fetch, slice(url, url_len), body)
            },
        )
        .expect(unique);
    linker
        .func_wrap(
            module,
            "clipboard_get",
            move |caller: Caller<'_, Context>| request(caller, IoOp::ClipboardGet, none, none),
        )
        .expect(unique);
    linker
        .func_wrap(
            module,
            "clipboard_set",
            move |caller: Caller<'_, Context>, text, text_len| {
                request(caller, IoOp::ClipboardSet, none, slice(text, text_len))
            },
        )
        .expect(unique);
    linker
}

// ---- private ----

fn table_len(caller: Caller<'_, Context>, table: u32) -> i32 {
    let context = caller.data();
    if !context.live {
        return Code::Denied.neg();
    }
    debug_assert!(
        context.snapshots.is_lent(),
        "a live store holds the caller's snapshots"
    );
    match Table::from_raw(table) {
        None => Code::Table.neg(),
        Some(table) => i32::try_from(context.snapshots.rows(table)).expect("rows <= ROWS_MAX"),
    }
}

/// The epoch masked to 31 bits so it stays a non-negative `i32`; it still changes on
/// every change (until it wraps).
fn table_epoch(caller: Caller<'_, Context>, table: u32) -> i32 {
    let context = caller.data();
    if !context.live {
        return Code::Denied.neg();
    }
    debug_assert!(
        context.snapshots.is_lent(),
        "a live store holds the caller's snapshots"
    );
    match Table::from_raw(table) {
        None => Code::Table.neg(),
        Some(table) => (context.snapshots.epoch(table) & 0x7fff_ffff) as i32,
    }
}

/// Like every bulk read, charges for the bytes it copied.
fn read_column(
    mut caller: Caller<'_, Context>,
    table: u32,
    column: u32,
    start: u32,
    count: u32,
    dst: u32,
    dst_len: u32,
) -> i32 {
    let returned = match live(&mut caller) {
        None => Code::Denied.neg(),
        Some((memory, context)) => {
            match context.snapshots.column_bytes(table, column, start, count) {
                Err(code) => code.neg(),
                Ok(src) => copy_out(memory, dst, dst_len, src),
            }
        }
    };
    let copied = usize::try_from(returned).unwrap_or(0);
    debug_assert!(
        copied <= dst_len as usize,
        "a read never copies past `dst_len`"
    );
    charge(&mut caller, copied);
    returned
}

fn read_strings(
    mut caller: Caller<'_, Context>,
    table: u32,
    column: u32,
    start: u32,
    count: u32,
    dst: u32,
    dst_len: u32,
) -> i32 {
    let returned = match live(&mut caller) {
        None => Code::Denied.neg(),
        Some((memory, context)) => {
            match context.snapshots.strings_len(table, column, start, count) {
                Err(code) => code.neg(),
                Ok(need) if need > dst_len as usize => need_more(memory, dst, dst_len, need),
                Ok(need) => match guest_mut(memory, dst, need) {
                    None => Code::Range.neg(),
                    Some(window) => {
                        context
                            .snapshots
                            .write_strings(table, column, start, count, window);
                        i32::try_from(need).expect("string slices stay under 2 GiB")
                    }
                },
            }
        }
    };
    let copied = usize::try_from(returned).unwrap_or(0);
    debug_assert!(
        copied <= dst_len as usize,
        "a read never copies past `dst_len`"
    );
    charge(&mut caller, copied);
    returned
}

fn eval(mut caller: Caller<'_, Context>, doc: u32, ptr: u32, len: u32) -> i32 {
    charge(&mut caller, len as usize);
    let Some((memory, context)) = live(&mut caller) else {
        return Code::Denied.neg();
    };
    if len as usize > EXPR_BYTES_MAX {
        return Code::Space.neg();
    }
    let Some(bytes) = guest(memory, ptr, len) else {
        return Code::Range.neg();
    };
    let Ok(expr) = std::str::from_utf8(bytes) else {
        return Code::Parse.neg();
    };
    if !context.snapshots.has_doc(doc) {
        return Code::Gone.neg();
    }
    if context.room.evals == 0 {
        return Code::Space.neg();
    }
    let handle = match issue(context) {
        Ok(handle) => handle,
        Err(code) => return code.neg(),
    };
    context.room.evals -= 1;
    context.staged.evals.push(EvalRequest {
        plugin: context.plugin,
        handle,
        doc,
        expr: expr.to_owned(),
    });
    // Staged requests use up the room they were given, which never exceeds the capacity.
    debug_assert!(context.staged.evals.len() + context.room.evals <= EVALS_MAX);
    i32::try_from(handle).expect("handles are positive i32")
}

fn log(mut caller: Caller<'_, Context>, level: u32, ptr: u32, len: u32) {
    charge(&mut caller, len as usize);
    let Some((memory, context)) = live(&mut caller) else {
        return;
    };
    let level = usize::try_from(level)
        .ok()
        .and_then(|index| Level::ALL.get(index).copied());
    let (Some(level), Some(bytes)) = (level, guest(memory, ptr, len)) else {
        let detail = "log: bad level or buffer".to_owned();
        context.faults.push(Failure {
            code: Code::Range,
            detail,
        });
        return;
    };
    if context.room.logs == 0 {
        let detail = "log: queue full".to_owned();
        context.faults.push(Failure {
            code: Code::Space,
            detail,
        });
        return;
    }
    context.room.logs -= 1;
    debug_assert_eq!(bytes.len(), len as usize);
    // A lossy decode turns at most 3 cut bytes into one U+FFFD, which `truncate` then cuts.
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(LOG_BYTES_MAX + 3)]);
    let text = contributions::truncate(&text, LOG_BYTES_MAX).to_owned();
    debug_assert!(text.len() <= LOG_BYTES_MAX);
    context.staged.logs.push(LogEntry {
        plugin: context.plugin,
        level,
        text,
    });
    debug_assert!(context.staged.logs.len() + context.room.logs <= LOGS_MAX);
}

/// The capability imports (§5): validate, authorize, stage an [`IoRequest`], return its
/// handle. Denials are also recorded as faults.
fn request(mut caller: Caller<'_, Context>, op: IoOp, target: GuestSlice, data: GuestSlice) -> i32 {
    charge(&mut caller, target.len as usize + data.len as usize);
    let Some((memory, context)) = live(&mut caller) else {
        // Inside the start function (no memory yet) the denial is not recorded.
        let context = caller.data_mut();
        if context.memory.is_some() {
            let detail = format!("{op:?} outside an event handler");
            context.faults.push(Failure {
                code: Code::Denied,
                detail,
            });
        }
        return Code::Denied.neg();
    };
    if target.len as usize > TARGET_BYTES_MAX || data.len as usize > IO_BYTES_MAX {
        return Code::Space.neg();
    }
    let target = guest(memory, target.ptr, target.len);
    let data = guest(memory, data.ptr, data.len);
    let (Some(target), Some(data)) = (target, data) else {
        return Code::Range.neg();
    };
    let Ok(target) = std::str::from_utf8(target) else {
        return Code::Parse.neg();
    };
    let args: Vec<String> = match op {
        IoOp::ClipboardGet | IoOp::ClipboardSet => Vec::new(),
        IoOp::Spawn => match contributions::line_values(target) {
            Ok(argv) => argv,
            Err(code) => return code.neg(),
        },
        IoOp::FsRead | IoOp::FsList | IoOp::FsWrite | IoOp::FsRemove | IoOp::Fetch => {
            vec![target.to_owned()]
        }
    };
    if let Err(code) = grants::authorize(&context.grants, op, &args) {
        let detail = format!("{op:?} {args:?}");
        context.faults.push(Failure { code, detail });
        return code.neg();
    }
    // `authorize` checked the arity: one path or URL, an argv, or nothing.
    debug_assert!(args.len() <= 1 || op == IoOp::Spawn);
    if context.room.io == 0 {
        return Code::Space.neg();
    }
    let handle = match issue(context) {
        Ok(handle) => handle,
        Err(code) => return code.neg(),
    };
    context.room.io -= 1;
    context.staged.io.push(IoRequest {
        plugin: context.plugin,
        handle,
        op,
        args,
        data: data.to_vec(),
    });
    debug_assert!(context.staged.io.len() + context.room.io <= IO_MAX);
    i32::try_from(handle).expect("handles are positive i32")
}

/// Charges the guest for host work done on its behalf (1 fuel per [`BYTES_PER_FUEL`] bytes
/// moved), so a loop of large reads exhausts the budget like a loop of instructions. The
/// guest traps with `E_FUEL` at its next fuel check once the budget hits zero.
fn charge(caller: &mut Caller<'_, Context>, bytes: usize) {
    let cost = (bytes / BYTES_PER_FUEL) as u64;
    if cost == 0 {
        return;
    }
    let fuel = caller.get_fuel().expect("fuel metering is enabled");
    caller
        .set_fuel(fuel.saturating_sub(cost))
        .expect("fuel metering is enabled");
    debug_assert!(caller.get_fuel().is_ok_and(|left| left < fuel || fuel == 0));
}

/// Splits the caller into its linear memory and store data; `None` outside an event handler
/// (start function, `schemify_alloc` outside an event, pure `schemify_call`), where every
/// import is denied.
fn live<'a>(caller: &'a mut Caller<'_, Context>) -> Option<(&'a mut [u8], &'a mut Context)> {
    let context = caller.data();
    if context.live {
        debug_assert!(context.memory.is_some(), "live only after instantiation");
        debug_assert!(
            context.snapshots.is_lent(),
            "a live store holds the caller's snapshots"
        );
    }
    let memory = context.memory.filter(|_| context.live)?;
    Some(memory.data_and_store_mut(caller))
}

/// Returns `len` bytes of guest memory at `ptr`; any range for `len == 0`.
fn guest(memory: &[u8], ptr: u32, len: u32) -> Option<&[u8]> {
    if len == 0 {
        return Some(&[]);
    }
    let start = ptr as usize;
    let window = memory.get(start..start.checked_add(len as usize)?)?;
    debug_assert_eq!(window.len(), len as usize);
    Some(window)
}

fn guest_mut(memory: &mut [u8], ptr: u32, len: usize) -> Option<&mut [u8]> {
    let start = ptr as usize;
    let window = memory.get_mut(start..start.checked_add(len)?)?;
    debug_assert_eq!(window.len(), len);
    Some(window)
}

/// Copies `src` to `dst`; on `E_SPACE` writes the needed size into the first 4 bytes of
/// `dst` when `dst_len >= 4`.
fn copy_out(memory: &mut [u8], dst: u32, dst_len: u32, src: &[u8]) -> i32 {
    if src.len() > dst_len as usize {
        return need_more(memory, dst, dst_len, src.len());
    }
    match guest_mut(memory, dst, src.len()) {
        None => Code::Range.neg(),
        Some(window) => {
            window.copy_from_slice(src);
            i32::try_from(src.len()).expect("snapshot slices stay under 2 GiB")
        }
    }
}

/// The §3 size protocol: `E_SPACE`, with `need` written to `dst` when it holds 4 bytes.
fn need_more(memory: &mut [u8], dst: u32, dst_len: u32, need: usize) -> i32 {
    debug_assert!(
        need > dst_len as usize,
        "only a too-small buffer needs more"
    );
    if dst_len >= 4 {
        let need = u32::try_from(need).unwrap_or(u32::MAX).to_le_bytes();
        match guest_mut(memory, dst, 4) {
            Some(window) => window.copy_from_slice(&need),
            None => return Code::Range.neg(),
        }
    }
    Code::Space.neg()
}

/// Issues the next handle; handles are positive `i32`s, unique per plugin.
fn issue(context: &mut Context) -> Result<u32, Code> {
    debug_assert!(context.next_handle > 0, "handles start at 1");
    if context.next_handle > i32::MAX as u32 {
        return Err(Code::Space);
    }
    let handle = context.next_handle;
    context.next_handle += 1;
    debug_assert!(context.next_handle > handle, "never reused");
    Ok(handle)
}
