//! Rust guest SDK (feature `guest`, `wasm32` only): a plugin's event handler and pure
//! functions → the §2 exports via [`export!`]; typed reads over the §3 imports via [`Host`];
//! the input arena behind `schemify_alloc`.
//!
//! Cites: plugin-abi §2 (exports, packed returns, one `schemify_alloc` per call), §3 and §5
//! (imports and their capabilities), D20 (pure `schemify_call`: every import denied).
//!
//! ```ignore
//! use schemify_abi::guest::{self, Host};
//! use schemify_abi::{Event, Out, Table};
//!
//! guest::export!(on_event);
//!
//! fn on_event(ev: Event<'_>, host: &Host, out: &mut Out) {
//!     if let Event::Load = ev {
//!         let nets = host.len(Table::Nets).unwrap_or(0);
//!         out.log(&format!("{nets} nets"));
//!     }
//! }
//! ```
//!
//! This is the crate's only module with unsafe code: the extern import block (`sys`:
//! `table_len`, `table_epoch`, `read_column`, `read_strings`, `eval`, `log`, `fs_read`,
//! `fs_list`, `fs_write`, `fs_remove`, `spawn`, `fetch`, `clipboard_get`, `clipboard_set`)
//! and the calls into it that hand the host a pointer into our own memory.
//!
//! Contents:
//! - `pub struct Host`: `len`, `is_empty`, `epoch`, `column`, `strings`, `eval`, `log`, `fs_read`,
//!   `fs_list`, `fs_write`, `fs_remove`, `spawn`, `fetch`, `clipboard_get`, `clipboard_set`
//! - `export!` (re-exported), `pub fn no_functions(..) -> Result<(), Code>`
//! - hidden, for `export!`: `__alloc`, `__on_event`, `__call`

#![allow(unsafe_code)]

use std::mem;
use std::ops::Range;
use std::sync::{Mutex, MutexGuard};

use crate::text;
use crate::{Code, ColType, Event, Level, Out, Scalar, Table};

pub use crate::export;

mod sys {
    #[link(wasm_import_module = "schemify")]
    unsafe extern "C" {
        pub safe fn table_len(table: u32) -> i32;
        pub safe fn table_epoch(table: u32) -> i32;
        pub fn read_column(
            table: u32,
            column: u32,
            start: u32,
            count: u32,
            dst: u32,
            dst_len: u32,
        ) -> i32;
        pub fn read_strings(
            table: u32,
            column: u32,
            start: u32,
            count: u32,
            dst: u32,
            dst_len: u32,
        ) -> i32;
        pub fn eval(doc: u32, ptr: u32, len: u32) -> i32;
        pub fn log(level: u32, ptr: u32, len: u32);
        pub fn fs_read(path: u32, path_len: u32) -> i32;
        pub fn fs_list(path: u32, path_len: u32) -> i32;
        pub fn fs_write(path: u32, path_len: u32, data: u32, data_len: u32) -> i32;
        pub fn fs_remove(path: u32, path_len: u32) -> i32;
        pub fn spawn(argv: u32, argv_len: u32, stdin: u32, stdin_len: u32) -> i32;
        pub fn fetch(url: u32, url_len: u32, body: u32, body_len: u32) -> i32;
        pub safe fn clipboard_get() -> i32;
        pub fn clipboard_set(ptr: u32, len: u32) -> i32;
    }
}

/// Reads through the host imports. Exists only inside an event handler: the SDK passes
/// `&Host` to the function given to [`export!`]; pure `schemify_call` functions get none,
/// because every import is denied there (D20).
#[derive(Debug)]
pub struct Host {
    _private: (),
}

const HOST: Host = Host { _private: () };

/// Host-written input of the current call. `schemify_alloc` resets it (one input per call),
/// so it behaves as an arena with call lifetime.
// ponytail: a one-slot arena, because the host writes exactly one buffer per call; grow it
// into a chunked bump arena if a future ABI writes several inputs per call.
static INPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Output of the last call; must outlive the export's return until the host copies it.
static OUTPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Emits the §2 exports for a plugin crate (`crate-type = ["cdylib"]`).
///
/// `export!(on_event)` or `export!(on_event, call)` where
/// `on_event: fn(Event<'_>, &Host, &mut Out)` and
/// `call: fn(fn_id: u32, args: &str, result: &mut String) -> Result<(), Code>`.
/// `args` is line-record values text (iterate with [`crate::text::values`]); `result` must
/// receive one canonical value. Use once per crate.
#[macro_export]
macro_rules! export {
    ($on_event:path) => {
        $crate::export!($on_event, $crate::guest::no_functions);
    };
    ($on_event:path, $call:path) => {
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        pub extern "C" fn schemify_abi() -> u32 {
            $crate::ABI_VERSION
        }
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        pub extern "C" fn schemify_alloc(len: u32) -> u32 {
            $crate::guest::__alloc(len)
        }
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        pub extern "C" fn schemify_on_event(ptr: u32, len: u32) -> u64 {
            $crate::guest::__on_event(ptr, len, $on_event)
        }
        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        pub extern "C" fn schemify_call(fn_id: u32, ptr: u32, len: u32) -> u64 {
            $crate::guest::__call(fn_id, ptr, len, $call)
        }
    };
}

impl Host {
    /// Returns the row count of `table`.
    ///
    /// # Errors
    ///
    /// `E_DENIED` outside an event handler.
    pub fn len(&self, table: Table) -> Result<u32, Code> {
        Code::check(sys::table_len(table as u32))
    }

    /// Returns whether `table` has no rows.
    ///
    /// # Errors
    ///
    /// As [`Host::len`].
    pub fn is_empty(&self, table: Table) -> Result<bool, Code> {
        self.len(table).map(|rows| rows == 0)
    }

    /// Returns a value that changes whenever `table` changes.
    ///
    /// # Errors
    ///
    /// `E_DENIED` outside an event handler.
    pub fn epoch(&self, table: Table) -> Result<u32, Code> {
        Code::check(sys::table_epoch(table as u32))
    }

    /// Copies rows `rows` of a fixed-width column. Allocates one `Vec` of `rows.len()`
    /// elements; the host writes straight into it.
    ///
    /// # Errors
    ///
    /// `E_COLUMN` if the column is not of type `T` (checked here, before any call),
    /// `E_RANGE` if `rows` runs past the table.
    pub fn column<T: Scalar>(
        &self,
        table: Table,
        column: u32,
        rows: Range<u32>,
    ) -> Result<Vec<T>, Code> {
        let column_def = table.columns().get(column as usize).ok_or(Code::Column)?;
        if column_def.ty != T::TY || T::TY == ColType::Str {
            return Err(Code::Column);
        }
        let count = rows.end.checked_sub(rows.start).ok_or(Code::Range)?;
        let mut dst = vec![T::default(); count as usize];
        debug_assert_eq!(len32(&dst), count * u32::from(T::TY.width()));
        // SAFETY: `dst` is an initialized, exclusively borrowed buffer of exactly
        // `len32(&dst)` bytes; the host writes at most that many bytes into it, and any bit
        // pattern is a valid `u8`/`u16`/`u32`/`i32` (wasm32 is little-endian, like the wire).
        let ret = unsafe {
            let dst_len = len32(&dst);
            sys::read_column(
                table as u32,
                column,
                rows.start,
                count,
                addr_mut(&mut dst),
                dst_len,
            )
        };
        let written = Code::check(ret)?;
        debug_assert_eq!(written, len32(&dst));
        Ok(dst)
    }

    /// Copies rows `rows` of a string column.
    ///
    /// # Errors
    ///
    /// `E_COLUMN` for a non-string column, `E_RANGE` past the table, `E_PARSE` if the host
    /// sent malformed data.
    pub fn strings(
        &self,
        table: Table,
        column: u32,
        rows: Range<u32>,
    ) -> Result<Vec<String>, Code> {
        let column_def = table.columns().get(column as usize).ok_or(Code::Column)?;
        if column_def.ty != ColType::Str {
            return Err(Code::Column);
        }
        let count = rows.end.checked_sub(rows.start).ok_or(Code::Range)?;
        let bytes = fill_retrying(count as usize * 16, |dst| {
            // SAFETY: `dst` is an initialized, exclusively borrowed byte buffer of
            // `dst.len()` bytes; the host writes at most that many bytes into it.
            unsafe {
                let dst_len = len32(dst);
                sys::read_strings(
                    table as u32,
                    column,
                    rows.start,
                    count,
                    addr_mut(dst),
                    dst_len,
                )
            }
        })?;
        let mut out = Vec::with_capacity(count as usize);
        let mut rest = &bytes[..];
        while let Some((len, tail)) = rest.split_first_chunk::<4>() {
            let len = u32::from_le_bytes(*len) as usize;
            let string = tail.get(..len).ok_or(Code::Parse)?;
            out.push(String::from_utf8(string.to_vec()).map_err(|_| Code::Parse)?);
            rest = &tail[len..];
        }
        if out.len() != count as usize || !rest.is_empty() {
            return Err(Code::Parse);
        }
        debug_assert!(
            out.capacity() >= count as usize,
            "sized once, before the loop"
        );
        Ok(out)
    }

    /// Requests evaluation of `expr` in `doc`'s cell scope. Returns a handle at once; the
    /// value arrives as `Event::IoDone { handle, bytes, .. }` (inverted like all I/O).
    ///
    /// # Errors
    ///
    /// `E_GONE` for an unknown document, `E_SPACE` when the host queue is full.
    pub fn eval(&self, doc: u32, expr: &str) -> Result<u32, Code> {
        // SAFETY: the host only reads `expr.len()` bytes at `expr`'s address.
        Code::check(unsafe { sys::eval(doc, addr(expr.as_bytes()), len32(expr.as_bytes())) })
    }

    /// Logs `text` at `level`. Dropped silently outside an event handler.
    pub fn log(&self, level: Level, text: &str) {
        // SAFETY: the host only reads `text.len()` bytes at `text`'s address.
        unsafe { sys::log(level as u32, addr(text.as_bytes()), len32(text.as_bytes())) }
    }

    /// Requests the bytes of a file: project-relative (capability `read_project`), or
    /// `${NAME}/…` under the directory environment variable `NAME` names (capability
    /// `read:<NAME>`, plugin ABI §5). Completion arrives as `Event::IoDone`.
    ///
    /// # Errors
    ///
    /// `E_DENIED` without the grant or for a path that leaves the project (or the variable's
    /// directory); `E_SPACE` when the host queue is full.
    pub fn fs_read(&self, path: &str) -> Result<u32, Code> {
        // SAFETY: the host only reads the given byte range.
        Code::check(unsafe { sys::fs_read(addr(path.as_bytes()), len32(path.as_bytes())) })
    }

    /// Requests the files under a directory, paths as [`Host::fs_read`] takes them. The
    /// `Event::IoDone` bytes are one path relative to the directory per line, recursive in
    /// path order (each directory's entries by name), hidden entries and `target` left out,
    /// at most 65 536 (`-E_GONE` for a missing directory or an unset variable).
    ///
    /// # Errors
    ///
    /// As [`Host::fs_read`].
    pub fn fs_list(&self, dir: &str) -> Result<u32, Code> {
        // SAFETY: the host only reads the given byte range.
        Code::check(unsafe { sys::fs_list(addr(dir.as_bytes()), len32(dir.as_bytes())) })
    }

    /// Requests writing `data` to a project-relative file (capability `write_project`).
    ///
    /// # Errors
    ///
    /// As [`Host::fs_read`].
    pub fn fs_write(&self, path: &str, data: &[u8]) -> Result<u32, Code> {
        let path = path.as_bytes();
        // SAFETY: the host only reads the given byte ranges.
        Code::check(unsafe { sys::fs_write(addr(path), len32(path), addr(data), len32(data)) })
    }

    /// Requests removing a project-relative file or directory tree (capability
    /// `write_project`, plugin ABI §5). Completion arrives as `Event::IoDone` (status 0, or
    /// `-E_GONE` when nothing was there).
    ///
    /// # Errors
    ///
    /// `E_DENIED` without the grant, for a path outside `.schemify/` (or `.schemify` itself),
    /// or one naming `schemify.project`/`schemify.lock`; `E_SPACE` when the host queue is full.
    pub fn fs_remove(&self, path: &str) -> Result<u32, Code> {
        let path = path.as_bytes();
        // SAFETY: the host only reads the given byte range.
        Code::check(unsafe { sys::fs_remove(addr(path), len32(path)) })
    }

    /// Requests running `argv` (capability `spawn:<argv[0]>`, native only); stdout arrives
    /// in `Event::IoDone` with the exit status.
    ///
    /// # Errors
    ///
    /// `E_DENIED` unless `argv[0]` is exactly a granted binary; `E_PARSE` for empty `argv`.
    pub fn spawn(&self, argv: &[&str], stdin: &[u8]) -> Result<u32, Code> {
        let mut line = String::new();
        for (index, arg) in argv.iter().enumerate() {
            if index > 0 {
                line.push(' ');
            }
            text::push_value(&mut line, arg);
        }
        debug_assert!(
            !line.contains(['\n', '\r', '\0']),
            "argv travels as one line of values"
        );
        let line = line.as_bytes();
        // SAFETY: the host only reads the given byte ranges.
        Code::check(unsafe { sys::spawn(addr(line), len32(line), addr(stdin), len32(stdin)) })
    }

    /// Requests an HTTP fetch of `url` with `body` (capability `network`).
    ///
    /// # Errors
    ///
    /// `E_DENIED` without the grant.
    pub fn fetch(&self, url: &str, body: &[u8]) -> Result<u32, Code> {
        let url = url.as_bytes();
        // SAFETY: the host only reads the given byte ranges.
        Code::check(unsafe { sys::fetch(addr(url), len32(url), addr(body), len32(body)) })
    }

    /// Requests the clipboard text (capability `clipboard`).
    ///
    /// # Errors
    ///
    /// `E_DENIED` without the grant.
    pub fn clipboard_get(&self) -> Result<u32, Code> {
        Code::check(sys::clipboard_get())
    }

    /// Requests setting the clipboard text (capability `clipboard`).
    ///
    /// # Errors
    ///
    /// `E_DENIED` without the grant.
    pub fn clipboard_set(&self, text: &str) -> Result<u32, Code> {
        let text = text.as_bytes();
        // SAFETY: the host only reads the given byte range.
        Code::check(unsafe { sys::clipboard_set(addr(text), len32(text)) })
    }
}

#[doc(hidden)]
#[must_use]
pub fn __alloc(len: u32) -> u32 {
    let mut input = lock(&INPUT);
    input.clear();
    if input.try_reserve_exact(len as usize).is_err() {
        return 0;
    }
    input.resize(len as usize, 0);
    let ptr = input.as_mut_ptr() as u32;
    debug_assert_ne!(ptr, 0, "0 is the out-of-memory answer");
    debug_assert_eq!(input.len(), len as usize);
    ptr
}

#[doc(hidden)]
pub fn __on_event(ptr: u32, len: u32, handler: fn(Event<'_>, &Host, &mut Out)) -> u64 {
    let Some(input) = take_input(ptr, len) else {
        return 0;
    };
    let mut out = Out::new();
    let parsed = Event::parse(&input);
    match parsed {
        Ok(event) => handler(event, &HOST, &mut out),
        Err(code) => out.log_at(Level::Error, &format!("unparsable event ({code})")),
    }
    let packed = publish(out.into_string().into_bytes());
    debug_assert!(
        parsed.is_ok() || packed != 0,
        "an unparsable event is answered with a log"
    );
    packed
}

#[doc(hidden)]
pub fn __call(
    fn_id: u32,
    ptr: u32,
    len: u32,
    call: fn(u32, &str, &mut String) -> Result<(), Code>,
) -> u64 {
    let Some(input) = take_input(ptr, len) else {
        return crate::pack(0, Code::Range as u32);
    };
    let Ok(args) = std::str::from_utf8(&input) else {
        return crate::pack(0, Code::Parse as u32);
    };
    let mut result = String::new();
    match call(fn_id, args, &mut result) {
        Ok(()) if !result.is_empty() => {
            let packed = publish(result.into_bytes());
            debug_assert_ne!(
                crate::unpack(packed).0,
                0,
                "a result must not read as the failure form (pointer 0)"
            );
            packed
        }
        Ok(()) => crate::pack(0, Code::Parse as u32),
        Err(code) => crate::pack(0, code as u32),
    }
}

/// `schemify_call` handler for plugins that export no functions.
///
/// # Errors
///
/// Always `E_RANGE` (no such function id).
pub fn no_functions(_fn_id: u32, _args: &str, _result: &mut String) -> Result<(), Code> {
    Err(Code::Range)
}

// ---- private ----

/// Calls a variable-size read; on `-E_SPACE` the host put the needed size in the first
/// four bytes, so grow once and retry.
fn fill_retrying(guess: usize, mut read: impl FnMut(&mut [u8]) -> i32) -> Result<Vec<u8>, Code> {
    let mut buf = vec![0u8; guess.max(4)];
    for _ in 0..2 {
        match Code::check(read(&mut buf)) {
            Ok(written) => {
                buf.truncate(written as usize);
                return Ok(buf);
            }
            Err(Code::Space) => {
                let Some(&need) = buf.first_chunk::<4>() else {
                    unreachable!("the buffer starts at 4 bytes and only grows");
                };
                let need = u32::from_le_bytes(need) as usize;
                if need <= buf.len() {
                    return Err(Code::Space);
                }
                buf = vec![0u8; need];
                debug_assert!(buf.len() > 4);
            }
            Err(code) => return Err(code),
        }
    }
    Err(Code::Space)
}

/// Takes the input buffer if `(ptr, len)` is what `__alloc` handed out; `None` otherwise.
fn take_input(ptr: u32, len: u32) -> Option<Vec<u8>> {
    let mut input = lock(&INPUT);
    let handed_out = input.as_ptr() as u32 == ptr && input.len() == len as usize;
    let taken = handed_out.then(|| mem::take(&mut *input));
    debug_assert!(
        !handed_out || input.is_empty(),
        "an input is taken at most once"
    );
    taken
}

/// Parks `bytes` as the call's output and returns the packed pointer; 0 for no output.
fn publish(bytes: Vec<u8>) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let mut output = lock(&OUTPUT);
    *output = bytes;
    debug_assert_ne!(output.as_ptr() as u32, 0, "0 means no output");
    crate::pack(output.as_ptr() as u32, len32(&output))
}

/// Locks one of the call buffers.
fn lock(buffer: &'static Mutex<Vec<u8>>) -> MutexGuard<'static, Vec<u8>> {
    // A guest panic traps (wasm32 is `panic = "abort"`), so no lock is ever left poisoned.
    buffer
        .lock()
        .expect("a guest panic aborts, so no buffer lock is poisoned")
}

/// Guest-memory address of a slice the host only reads, as the ABI's `u32` pointer.
fn addr<T>(slice: &[T]) -> u32 {
    slice.as_ptr() as u32
}

/// Guest-memory address of a buffer the host writes into; derived from a unique borrow so
/// the write is permitted.
fn addr_mut<T>(slice: &mut [T]) -> u32 {
    slice.as_mut_ptr() as u32
}

/// Byte length of a slice as the ABI's `u32` length (guest memory is below 4 GiB).
fn len32<T>(slice: &[T]) -> u32 {
    u32::try_from(mem::size_of_val(slice)).expect("wasm32 slices fit u32")
}
