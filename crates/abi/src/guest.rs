//! Rust guest SDK (feature `guest`, `wasm32` only): the §2 exports via [`export!`], typed
//! reads over the §3 imports via [`Host`], and the input arena behind `schemify_alloc`.
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
//! This is the crate's only module with unsafe code: the extern import block and the calls
//! into it that hand the host a pointer into our own memory.

#![allow(unsafe_code)]

use std::ops::Range;
use std::sync::{Mutex, PoisonError};

use crate::{Code, ColType, Event, Level, Out, Scalar, Table, pack, text};

pub use crate::export;

mod sys {
    #[link(wasm_import_module = "schemify")]
    unsafe extern "C" {
        pub safe fn table_len(table: u32) -> i32;
        pub safe fn table_epoch(table: u32) -> i32;
        pub fn read_column(t: u32, c: u32, start: u32, count: u32, dst: u32, dst_len: u32) -> i32;
        pub fn read_strings(t: u32, c: u32, start: u32, count: u32, dst: u32, dst_len: u32) -> i32;
        pub fn read_record(doc: u32, id: u32, dst: u32, dst_len: u32) -> i32;
        pub fn eval(doc: u32, ptr: u32, len: u32) -> i32;
        pub fn log(level: u32, ptr: u32, len: u32);
        pub fn fs_read(path: u32, path_len: u32) -> i32;
        pub fn fs_write(path: u32, path_len: u32, data: u32, data_len: u32) -> i32;
        pub fn fs_remove(path: u32, path_len: u32) -> i32;
        pub fn spawn(argv: u32, argv_len: u32, stdin: u32, stdin_len: u32) -> i32;
        pub fn fetch(url: u32, url_len: u32, body: u32, body_len: u32) -> i32;
        pub safe fn clipboard_get() -> i32;
        pub fn clipboard_set(ptr: u32, len: u32) -> i32;
    }
}

/// Guest-memory address of a slice the host only reads, as the ABI's `u32` pointer.
fn addr<T>(s: &[T]) -> u32 {
    s.as_ptr() as u32
}

/// Guest-memory address of a buffer the host writes into; derived from a unique borrow so
/// the write is permitted.
fn addr_mut<T>(s: &mut [T]) -> u32 {
    s.as_mut_ptr() as u32
}

/// Byte length of a slice as the ABI's `u32` length (guest memory is below 4 GiB).
fn len32<T>(s: &[T]) -> u32 {
    u32::try_from(std::mem::size_of_val(s)).expect("wasm32 slices fit u32")
}

/// Reads through the host imports. Exists only inside an event handler: the SDK passes
/// `&Host` to the function given to [`export!`]; pure `schemify_call` functions get none,
/// because every import is denied there (D20).
#[derive(Debug)]
pub struct Host {
    _private: (),
}

const HOST: Host = Host { _private: () };

impl Host {
    /// Returns the row count of `table`.
    ///
    /// # Errors
    ///
    /// `E_DENIED` outside an event handler.
    pub fn len(&self, table: Table) -> Result<u32, Code> {
        Code::check(sys::table_len(table as u32))
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
        let def = table.columns().get(column as usize).ok_or(Code::Column)?;
        if def.ty != T::TY || T::TY == ColType::Str {
            return Err(Code::Column);
        }
        let count = rows.end.checked_sub(rows.start).ok_or(Code::Range)?;
        let mut dst = vec![T::default(); count as usize];
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
        let def = table.columns().get(column as usize).ok_or(Code::Column)?;
        if def.ty != ColType::Str {
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
            let s = tail.get(..len).ok_or(Code::Parse)?;
            out.push(String::from_utf8(s.to_vec()).map_err(|_| Code::Parse)?);
            rest = &tail[len..];
        }
        if out.len() != count as usize || !rest.is_empty() {
            return Err(Code::Parse);
        }
        Ok(out)
    }

    /// Returns document record `id` of `doc` as line-record text (the diagonal route for
    /// "just give me this object").
    ///
    /// # Errors
    ///
    /// `E_GONE` if the record or document does not exist.
    pub fn record(&self, doc: u32, id: u32) -> Result<String, Code> {
        let bytes = fill_retrying(256, |dst| {
            // SAFETY: as in `strings`: an initialized, exclusive buffer of `dst.len()` bytes.
            unsafe {
                let dst_len = len32(dst);
                sys::read_record(doc, id, addr_mut(dst), dst_len)
            }
        })?;
        String::from_utf8(bytes).map_err(|_| Code::Parse)
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

    /// Requests the bytes of a project-relative file (capability `read_project`). Completion
    /// arrives as `Event::IoDone`.
    ///
    /// # Errors
    ///
    /// `E_DENIED` without the grant or for a path that leaves the project; `E_SPACE` when the
    /// host queue is full.
    pub fn fs_read(&self, path: &str) -> Result<u32, Code> {
        // SAFETY: the host only reads the given byte range.
        Code::check(unsafe { sys::fs_read(addr(path.as_bytes()), len32(path.as_bytes())) })
    }

    /// Requests writing `data` to a project-relative file (capability `write_project`).
    ///
    /// # Errors
    ///
    /// As [`Host::fs_read`].
    pub fn fs_write(&self, path: &str, data: &[u8]) -> Result<u32, Code> {
        let p = path.as_bytes();
        // SAFETY: the host only reads the given byte ranges.
        Code::check(unsafe { sys::fs_write(addr(p), len32(p), addr(data), len32(data)) })
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
        let p = path.as_bytes();
        // SAFETY: the host only reads the given byte range.
        Code::check(unsafe { sys::fs_remove(addr(p), len32(p)) })
    }

    /// Requests running `argv` (capability `spawn:<argv[0]>`, native only); stdout arrives
    /// in `Event::IoDone` with the exit status.
    ///
    /// # Errors
    ///
    /// `E_DENIED` unless `argv[0]` is exactly a granted binary; `E_PARSE` for empty `argv`.
    pub fn spawn(&self, argv: &[&str], stdin: &[u8]) -> Result<u32, Code> {
        let mut line = String::new();
        for (i, a) in argv.iter().enumerate() {
            if i > 0 {
                line.push(' ');
            }
            text::push_value(&mut line, a);
        }
        let l = line.as_bytes();
        // SAFETY: the host only reads the given byte ranges.
        Code::check(unsafe { sys::spawn(addr(l), len32(l), addr(stdin), len32(stdin)) })
    }

    /// Requests an HTTP fetch of `url` with `body` (capability `network`).
    ///
    /// # Errors
    ///
    /// `E_DENIED` without the grant.
    pub fn fetch(&self, url: &str, body: &[u8]) -> Result<u32, Code> {
        let u = url.as_bytes();
        // SAFETY: the host only reads the given byte ranges.
        Code::check(unsafe { sys::fetch(addr(u), len32(u), addr(body), len32(body)) })
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
        let t = text.as_bytes();
        // SAFETY: the host only reads the given byte range.
        Code::check(unsafe { sys::clipboard_set(addr(t), len32(t)) })
    }
}

/// Calls a variable-size read; on `-E_SPACE` the host put the needed size in the first
/// four bytes, so grow once and retry.
fn fill_retrying(guess: usize, mut read: impl FnMut(&mut [u8]) -> i32) -> Result<Vec<u8>, Code> {
    let mut buf = vec![0u8; guess.max(4)];
    for _ in 0..2 {
        match Code::check(read(&mut buf)) {
            Ok(n) => {
                buf.truncate(n as usize);
                return Ok(buf);
            }
            Err(Code::Space) => {
                let need = u32::from_le_bytes(buf[..4].try_into().expect("4 bytes")) as usize;
                if need <= buf.len() {
                    return Err(Code::Space);
                }
                buf = vec![0u8; need];
            }
            Err(code) => return Err(code),
        }
    }
    Err(Code::Space)
}

/// Host-written input of the current call. `schemify_alloc` resets it (one input per call),
/// so it behaves as an arena with call lifetime.
// ponytail: a one-slot arena, because the host writes exactly one buffer per call; grow it
// into a chunked bump arena if a future ABI writes several inputs per call.
static INPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// Output of the last call; must outlive the export's return until the host copies it.
static OUTPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

#[doc(hidden)]
pub fn __alloc(len: u32) -> u32 {
    let mut input = INPUT.lock().unwrap_or_else(PoisonError::into_inner);
    input.clear();
    if input.try_reserve_exact(len as usize).is_err() {
        return 0;
    }
    input.resize(len as usize, 0);
    let ptr = input.as_mut_ptr() as u32;
    debug_assert!(ptr != 0 && input.len() == len as usize);
    ptr
}

/// Takes the input buffer if `(ptr, len)` is what `__alloc` handed out; `None` otherwise.
fn take_input(ptr: u32, len: u32) -> Option<Vec<u8>> {
    let mut input = INPUT.lock().unwrap_or_else(PoisonError::into_inner);
    let ok = input.as_ptr() as u32 == ptr && input.len() == len as usize;
    ok.then(|| std::mem::take(&mut *input))
}

/// Parks `bytes` as the call's output and returns the packed pointer; 0 for no output.
fn publish(bytes: Vec<u8>) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
    *output = bytes;
    pack(output.as_ptr() as u32, len32(&output))
}

#[doc(hidden)]
pub fn __on_event(ptr: u32, len: u32, handler: fn(Event<'_>, &Host, &mut Out)) -> u64 {
    let Some(input) = take_input(ptr, len) else {
        return 0;
    };
    let mut out = Out::new();
    match Event::parse(&input) {
        Ok(event) => handler(event, &HOST, &mut out),
        Err(code) => out.log_at(Level::Error, &format!("unparsable event ({code})")),
    }
    publish(out.into_string().into_bytes())
}

#[doc(hidden)]
pub fn __call(
    fn_id: u32,
    ptr: u32,
    len: u32,
    f: fn(u32, &str, &mut String) -> Result<(), Code>,
) -> u64 {
    let Some(input) = take_input(ptr, len) else {
        return pack(0, Code::Range as u32);
    };
    let Ok(args) = std::str::from_utf8(&input) else {
        return pack(0, Code::Parse as u32);
    };
    let mut result = String::new();
    match f(fn_id, args, &mut result) {
        Ok(()) if !result.is_empty() => publish(result.into_bytes()),
        Ok(()) => pack(0, Code::Parse as u32),
        Err(code) => pack(0, code as u32),
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
