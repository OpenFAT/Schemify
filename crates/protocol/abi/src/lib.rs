//! Plugin ABI v1, shared by the Schemify host (`schemify-plugin`) and every guest: the
//! numbers and names both sides agree on, and the line-record codec both directions speak.
//!
//! Cites: `docs/spec/plugin-abi.md` (normative) §2 (exports, limits), §3 (imports), §4
//! (tables), §5 (capabilities), §6 (events), §7 (outputs), §8 (codes); D3a, D7, D20. Every
//! value host and guest must agree on is data here: the version and export/import names,
//! error codes ([`Code`]), capability ids ([`Cap`]), contribution kinds ([`Kind`]), snapshot
//! table schemas ([`schema`]), the event vocabulary ([`event`]) and the output vocabulary
//! ([`output`]), both written in line-record text ([`text`]).
//!
//! With feature `guest` on `wasm32`, the `guest` module is the Rust plugin SDK (`export!`,
//! `Host`, the input arena). Its docs build with
//! `cargo doc -p schemify-abi --features guest --target wasm32-unknown-unknown`.
//!
//! Contents:
//! - `pub mod event`, `pub mod output`, `pub mod schema`, `pub mod text`, `pub mod guest`
//! - re-exports: `Code`, `Cap`, `Kind`, `Level`, `EditMode`, `Event`, `Out`, `Output`,
//!   `ColType`, `ColumnDef`, `Scalar`, `Table`, `col`
//! - `pub const ABI_VERSION: u32`, `pub const FUEL_DEFAULT: u64`,
//!   `pub const MEMORY_BYTES_MAX: usize`, `pub const IMPORT_MODULE: &str`
//! - `pub mod exports`: `ABI`, `ALLOC`, `ON_EVENT`, `CALL`, `MEMORY`
//! - `pub const fn pack(ptr: u32, len: u32) -> u64`
//! - `pub const fn unpack(packed: u64) -> (u32, u32)`
//!
//! # Examples
//!
//! The host encodes an event; the guest SDK parses it back without copying:
//!
//! ```
//! use schemify_abi::Event;
//!
//! let mut bytes = Vec::new();
//! Event::Command { name: "count_nets.run", args: "" }.encode(&mut bytes)?;
//! assert_eq!(Event::parse(&bytes)?, Event::Command { name: "count_nets.run", args: "" });
//! # Ok::<(), schemify_abi::Code>(())
//! ```

// `vocab` first: its `named_enum!` is textually scoped and `schema` uses it.
#[macro_use]
mod vocab;
pub mod event;
pub mod output;
pub mod schema;
pub mod text;

#[cfg(all(feature = "guest", target_arch = "wasm32"))]
pub mod guest;

pub use event::Event;
pub use output::{Out, Output};
pub use schema::{ColType, ColumnDef, Scalar, Table, col};
pub use vocab::{Cap, Code, EditMode, Kind, Level};

/// ABI version a guest returns from `schemify_abi`; any other value refuses the load (§2).
pub const ABI_VERSION: u32 = 1;

/// Fuel granted per `schemify_on_event` / `schemify_call` unless the user config overrides it
/// (§2). One unit is roughly one wasm instruction.
pub const FUEL_DEFAULT: u64 = 100_000_000;

/// Linear-memory cap per plugin in bytes (§2). A `memory.grow` past it returns -1.
pub const MEMORY_BYTES_MAX: usize = 256 << 20;

/// Module name of every host import (§3).
pub const IMPORT_MODULE: &str = "schemify";

/// Export names a guest module provides (§2). `CALL` is optional; the rest are required.
pub mod exports {
    /// `() -> u32`: the guest's ABI version.
    pub const ABI: &str = "schemify_abi";
    /// `(len: u32) -> u32`: guest-side buffer for host-written input; 0 = out of memory.
    pub const ALLOC: &str = "schemify_alloc";
    /// `(ptr: u32, len: u32) -> u64`: event in, packed output buffer out.
    pub const ON_EVENT: &str = "schemify_on_event";
    /// `(fn_id: u32, ptr: u32, len: u32) -> u64`: pure expression function (D20).
    pub const CALL: &str = "schemify_call";
    /// The guest's linear memory.
    pub const MEMORY: &str = "memory";
}

/// Wasm page size; the memory cap must be whole pages for the limiter to hit it exactly.
const WASM_PAGE_BYTES: usize = 64 << 10;
const _: () = assert!(MEMORY_BYTES_MAX.is_multiple_of(WASM_PAGE_BYTES));
const _: () = assert!(unpack(pack(u32::MAX, 0)).0 == u32::MAX);
const _: () = assert!(unpack(pack(0, u32::MAX)).1 == u32::MAX);

/// Packs a guest buffer into the `u64` that `schemify_on_event` / `schemify_call` return.
///
/// `ptr == 0` means "no buffer": for `on_event` that is "no outputs" (`len` 0); for `call`
/// it is failure, with the [`Code`] in `len`.
#[must_use]
pub const fn pack(ptr: u32, len: u32) -> u64 {
    ((ptr as u64) << 32) | len as u64
}

/// Splits a value produced by [`pack`] into `(ptr, len)`; the inverse of [`pack`].
#[must_use]
pub const fn unpack(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_puts_ptr_high_and_len_low() {
        assert_eq!(pack(7, 9), (7u64 << 32) | 9);
        assert_eq!(unpack(pack(u32::MAX, 3)), (u32::MAX, 3));
        assert_eq!(unpack(0), (0, 0));
    }
}
