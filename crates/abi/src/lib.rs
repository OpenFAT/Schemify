//! Plugin ABI v1, shared by the Schemify host (`schemify-plugin`) and every guest.
//!
//! Spec: `docs/spec/plugin-abi.md` (normative). Everything host and guest must agree on is
//! data in this crate: the version and export/import names, error codes ([`Code`]),
//! capability ids ([`Cap`]), contribution kinds ([`Kind`]), snapshot table schemas
//! ([`schema`]), the event vocabulary ([`event`]) and the output vocabulary ([`output`]).
//! Both directions speak line-record text ([`text`]).
//!
//! With feature `guest` on `wasm32`, the `guest` module is the Rust plugin SDK (`export!`,
//! `Host`, the input arena). Its docs build with
//! `cargo doc -p schemify-abi --features guest --target wasm32-unknown-unknown`.
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

pub mod event;
pub mod output;
pub mod schema;
pub mod text;

#[cfg(all(feature = "guest", target_arch = "wasm32"))]
pub mod guest;

pub use event::Event;
pub use output::{Out, Output};
pub use schema::{ColType, ColumnDef, Scalar, Table, col};

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

/// Packs a guest buffer into the `u64` that `schemify_on_event` / `schemify_call` return.
///
/// `ptr == 0` means "no buffer": for `on_event` that is "no outputs" (`len` 0); for `call`
/// it is failure, with the [`Code`] in `len`.
pub const fn pack(ptr: u32, len: u32) -> u64 {
    ((ptr as u64) << 32) | len as u64
}

/// Splits a value produced by [`pack`] into `(ptr, len)`.
pub const fn unpack(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

/// Error codes (§8). Imports return them negated (`-(code as i32)`); outputs are rejected
/// with them; the health report carries them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Code {
    /// Capability not granted, or the call is not allowed in this context (pure call).
    Denied = 1,
    /// Referenced document, object, plugin or handle no longer exists.
    Gone = 2,
    /// Destination buffer too small, or a host queue/table is at capacity.
    Space = 3,
    /// Unknown table id.
    Table = 4,
    /// Unknown column id, or a typed read of a column of another type.
    Column = 5,
    /// Row range, pointer range or numeric value out of bounds.
    Range = 6,
    /// Malformed line record, value or UTF-8.
    Parse = 7,
    /// Fuel budget exhausted; the event's outputs were discarded.
    Fuel = 8,
    /// Guest ABI version differs from [`ABI_VERSION`].
    Version = 9,
    /// Guest trapped (unreachable, out-of-bounds access, stack overflow, failed allocation).
    Trap = 10,
    /// Module failed validation or linking, or lacks a required export.
    Module = 11,
}

impl Code {
    /// Every code, in numeric order (`ALL[i] as u8 == i + 1`).
    pub const ALL: [Code; 11] = [
        Code::Denied,
        Code::Gone,
        Code::Space,
        Code::Table,
        Code::Column,
        Code::Range,
        Code::Parse,
        Code::Fuel,
        Code::Version,
        Code::Trap,
        Code::Module,
    ];
    const NAMES: [&'static str; 11] = [
        "E_DENIED",
        "E_GONE",
        "E_SPACE",
        "E_TABLE",
        "E_COLUMN",
        "E_RANGE",
        "E_PARSE",
        "E_FUEL",
        "E_VERSION",
        "E_TRAP",
        "E_MODULE",
    ];

    /// Returns the spec name (`E_DENIED`, …), which is also the wire form in events.
    pub const fn name(self) -> &'static str {
        Self::NAMES[self as usize - 1]
    }

    /// Parses a spec name; `None` for anything else.
    pub fn parse(name: &str) -> Option<Code> {
        let i = Self::NAMES.iter().position(|&n| n == name)?;
        Some(Self::ALL[i])
    }

    /// Returns the code with numeric value `raw`; `None` outside `1..=11`.
    pub fn from_raw(raw: u32) -> Option<Code> {
        let i = usize::try_from(raw.checked_sub(1)?).ok()?;
        Self::ALL.get(i).copied()
    }

    /// Returns the negated value an import returns for this error.
    pub const fn neg(self) -> i32 {
        -(self as i32)
    }

    /// Decodes an import's `i32` return: non-negative is a value, negative is an error.
    ///
    /// # Errors
    ///
    /// The code for a negative return; [`Code::Range`] for a negative value that is no code
    /// (only a misbehaving host produces one).
    pub fn check(ret: i32) -> Result<u32, Code> {
        match u32::try_from(ret) {
            Ok(v) => Ok(v),
            Err(_) => Err(Code::from_raw(ret.unsigned_abs()).unwrap_or(Code::Range)),
        }
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Declares a closed `#[repr(u8)]` enum whose variants have one wire name each.
macro_rules! named_enum {
    ($(#[$m:meta])* pub enum $T:ident { $($(#[$vm:meta])* $V:ident = $s:literal,)* }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u8)]
        pub enum $T { $($(#[$vm])* $V,)* }

        impl $T {
            /// Every variant in discriminant order (`ALL[i] as usize == i`).
            pub const ALL: &'static [$T] = &[$($T::$V,)*];

            /// Returns the wire name.
            pub const fn name(self) -> &'static str {
                match self { $($T::$V => $s,)* }
            }

            /// Parses a wire name; `None` for anything else.
            pub fn parse(name: &str) -> Option<$T> {
                match name { $($s => Some($T::$V),)* _ => None }
            }
        }
    };
}
pub(crate) use named_enum;

named_enum! {
    /// Capabilities a user grants at install (§5, D7). Without the grant the import returns
    /// `-E_DENIED`.
    pub enum Cap {
        /// `fs_read` of project-relative paths.
        ReadProject = "read_project",
        /// `fs_write` of project-relative paths, and `fs_remove` under `.schemify/`.
        WriteProject = "write_project",
        /// `spawn` of one exact binary name; the grant is written `spawn:<binary>`.
        Spawn = "spawn",
        /// `fetch`.
        Network = "network",
        /// `clipboard_get` / `clipboard_set`.
        Clipboard = "clipboard",
    }
}

impl Cap {
    /// Parses one manifest grant (`read_project`, `spawn:ngspice`, …) into the capability
    /// and its argument (the binary name for `spawn`, empty otherwise).
    ///
    /// Returns `None` for an unknown capability, a `spawn` without a binary, a binary name
    /// containing a path separator (grants name a binary, never a path), or an argument on a
    /// capability that takes none.
    pub fn parse_grant(grant: &str) -> Option<(Cap, &str)> {
        let (name, arg) = grant.split_once(':').unwrap_or((grant, ""));
        let cap = Cap::parse(name)?;
        let ok = match cap {
            Cap::Spawn => !arg.is_empty() && !arg.contains(['/', '\\']) && text::is_bare(arg),
            _ => arg.is_empty() && !grant.contains(':'),
        };
        ok.then_some((cap, arg))
    }
}

named_enum! {
    /// Contribution kinds (§1). Each `contribute <kind>` replaces the plugin's previous set
    /// of that kind (and scope, for [`Kind::Annotations`]).
    pub enum Kind {
        /// UI nodes and styles (`node`, `style`, `remove`, `set` records).
        Ui = "ui",
        /// Command registry rows (`command <name> <label>`).
        Commands = "commands",
        /// Key bindings (`key <chord> <command>`).
        Keymap = "keymap",
        /// Annotation marks per scope (`mark <path-or-id> <severity> <text>`).
        Annotations = "annotations",
        /// Expression functions (`function <name> <fn_id> <arity>`, D20).
        Functions = "functions",
        /// Netlist dialects (`dialect <lang>`).
        Dialects = "dialects",
        /// Code-block runners (`runner <lang>`).
        Runners = "runners",
        /// Check codes (`check <code> <severity>`).
        Checks = "checks",
        /// Constraint types (`constraint <name>`).
        ConstraintTypes = "constraint_types",
    }
}

impl Kind {
    /// Number of kinds.
    pub const COUNT: usize = Kind::ALL.len();

    /// Top-level record words legal inside a contribution of this kind; anything else is
    /// rejected with `E_PARSE`.
    pub const fn words(self) -> &'static [&'static str] {
        match self {
            Kind::Ui => &["node", "style", "remove", "set"],
            Kind::Commands => &["command"],
            Kind::Keymap => &["key"],
            Kind::Annotations => &["mark"],
            Kind::Functions => &["function"],
            Kind::Dialects => &["dialect"],
            Kind::Runners => &["runner"],
            Kind::Checks => &["check"],
            Kind::ConstraintTypes => &["constraint"],
        }
    }

    /// Returns whether the kind carries a scope (`contribute annotations <scope>`).
    pub const fn scoped(self) -> bool {
        matches!(self, Kind::Annotations)
    }
}

named_enum! {
    /// Log levels of the `log` import and output record.
    pub enum Level {
        /// Something failed.
        Error = "error",
        /// Something is suspicious.
        Warn = "warn",
        /// Progress and results.
        Info = "info",
        /// Developer detail.
        Debug = "debug",
    }
}

named_enum! {
    /// Edit transaction modes (edits spec §1).
    pub enum EditMode {
        /// Validate, apply, push to undo, journal.
        Commit = "commit",
        /// Validate and store in `staged_edits` for review in the visual diff.
        Stage = "stage",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_by_value_and_name() {
        for (i, &c) in Code::ALL.iter().enumerate() {
            assert_eq!(c as usize, i + 1);
            assert_eq!(Code::from_raw(c as u32), Some(c));
            assert_eq!(Code::parse(c.name()), Some(c));
            assert_eq!(Code::check(c.neg()), Err(c));
        }
        assert_eq!(Code::from_raw(0), None);
        assert_eq!(Code::from_raw(12), None);
        assert_eq!(Code::check(0), Ok(0));
        assert_eq!(Code::check(i32::MAX), Ok(i32::MAX as u32));
        assert_eq!(Code::check(i32::MIN), Err(Code::Range));
        assert_eq!(Code::check(-99), Err(Code::Range));
    }

    #[test]
    fn grants_parse_exact_forms_only() {
        assert_eq!(
            Cap::parse_grant("read_project"),
            Some((Cap::ReadProject, ""))
        );
        assert_eq!(
            Cap::parse_grant("spawn:ngspice"),
            Some((Cap::Spawn, "ngspice"))
        );
        for bad in [
            "spawn",
            "spawn:",
            "spawn:/bin/sh",
            "spawn:a\\b",
            "network:x",
            "net",
            "",
        ] {
            assert_eq!(Cap::parse_grant(bad), None, "{bad}");
        }
    }

    #[test]
    fn named_enums_are_dense_and_round_trip() {
        for (i, &k) in Kind::ALL.iter().enumerate() {
            assert_eq!(k as usize, i);
            assert_eq!(Kind::parse(k.name()), Some(k));
            assert!(!k.words().is_empty());
        }
        assert_eq!(Kind::COUNT, 9);
        assert_eq!(Kind::parse("nope"), None);
        assert_eq!(pack(7, 9), (7u64 << 32) | 9);
        assert_eq!(unpack(pack(u32::MAX, 3)), (u32::MAX, 3));
    }
}
