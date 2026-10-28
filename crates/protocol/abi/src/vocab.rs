//! The ABI's closed vocabularies: wire name or number → enum, and back.
//!
//! Cites: plugin-abi §8 (error codes), §5 and D7 (capability grants, amended 2026-10-22 for
//! `read:<NAME>`), §1 and §7 (contribution kinds and their record words), §3 (log levels),
//! edits spec §1 (edit modes).
//!
//! Contents:
//! - `pub enum Code`: `ALL`, `name`, `parse`, `from_raw`, `neg`, `check`, `Display`,
//!   `FromStr`
//! - `pub enum Cap`: `parse_grant`, `env_name`
//! - `pub enum Kind`: `COUNT`, `words`, `scoped`
//! - `pub enum Level`, `pub enum EditMode`
//! - every named enum (`Cap`, `Kind`, `Level`, `EditMode`, `schema::ColType`): `ALL`,
//!   `pub const fn name(self) -> &'static str`, `pub fn parse(name: &str) -> Option<Self>`,
//!   `FromStr` (`Err = Code`)

use std::fmt;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use crate::text;

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
    /// Guest ABI version differs from [`crate::ABI_VERSION`].
    Version = 9,
    /// Guest trapped (unreachable, out-of-bounds access, stack overflow, failed allocation).
    Trap = 10,
    /// Module failed validation or linking, or lacks a required export.
    Module = 11,
}

/// Declares a closed `#[repr(u8)]` enum whose variants have one wire name each, numbered
/// densely from 0.
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
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self { $($T::$V => $s,)* }
            }

            /// Parses a wire name; `None` for anything else, including another case.
            #[must_use]
            pub fn parse(name: &str) -> Option<$T> {
                let parsed = match name { $($s => Some($T::$V),)* _ => None };
                if let Some(value) = parsed {
                    debug_assert_eq!(value.name(), name, "name and parse are inverses");
                }
                parsed
            }
        }

        impl std::str::FromStr for $T {
            type Err = $crate::Code;

            /// As the inherent `parse`, failing with [`Code::Parse`](crate::Code::Parse).
            fn from_str(name: &str) -> Result<$T, $crate::Code> {
                $T::parse(name).ok_or($crate::Code::Parse)
            }
        }
    };
}

named_enum! {
    /// Capabilities a user grants at install (§5, D7). Without the grant the import returns
    /// `-E_DENIED`.
    pub enum Cap {
        /// `fs_read` / `fs_list` of project-relative paths.
        ReadProject = "read_project",
        /// `fs_write` of project-relative paths, and `fs_remove` under `.schemify/`.
        WriteProject = "write_project",
        /// `spawn` of one exact binary name; the grant is written `spawn:<binary>`.
        Spawn = "spawn",
        /// `fetch`.
        Network = "network",
        /// `clipboard_get` / `clipboard_set`.
        Clipboard = "clipboard",
        /// `fs_read` / `fs_list` under the directory an environment variable names, as
        /// `${NAME}/…` paths; the grant is written `read:<NAME>` (a PDK, a tool's library).
        ReadEnv = "read",
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

named_enum! {
    /// Log levels of the `log` import and output record; the import takes the discriminant.
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

const CODE_NAMES: [&str; 11] = [
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
/// Longest environment variable name a `read:` grant may carry.
const ENV_NAME_BYTES_MAX: usize = 64;
const _: () = assert!(Code::Module as usize == Code::ALL.len());
// `name` indexes `CODE_NAMES` with `code - 1` and `neg` promises a negative value: both need
// the first code to be 1.
const _: () = assert!(Code::ALL[0] as u8 == 1);
const _: () = assert!(Kind::COUNT == 9);

impl Code {
    /// Every code, in numeric order (`ALL[i] as u8 == i + 1`). An array, not a slice, so
    /// generators can `map` it.
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

    /// Returns the spec name (`E_DENIED`, …), which is also the wire form in events.
    #[must_use]
    pub const fn name(self) -> &'static str {
        CODE_NAMES[self as usize - 1]
    }

    /// Parses a spec name; `None` for anything else.
    #[must_use]
    pub fn parse(name: &str) -> Option<Code> {
        let index = CODE_NAMES.iter().position(|&known| known == name)?;
        let code = Code::ALL[index];
        debug_assert_eq!(code.name(), name, "name and parse are inverses");
        Some(code)
    }

    /// Returns the code with numeric value `raw`; `None` outside `1..=11`.
    #[must_use]
    pub fn from_raw(raw: u32) -> Option<Code> {
        let index = usize::try_from(raw.checked_sub(1)?).ok()?;
        let code = Code::ALL.get(index).copied()?;
        debug_assert_eq!(code as u32, raw);
        Some(code)
    }

    /// Returns the negated value an import returns for this error (always `< 0`).
    #[must_use]
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
            Ok(value) => Ok(value),
            Err(_) => Err(Code::from_raw(ret.unsigned_abs()).unwrap_or(Code::Range)),
        }
    }
}

impl Display for Code {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Code {
    type Err = Code;

    /// As [`Code::parse`], failing with [`Code::Parse`].
    fn from_str(name: &str) -> Result<Code, Code> {
        Code::parse(name).ok_or(Code::Parse)
    }
}

impl Cap {
    /// Parses one manifest grant (`read_project`, `spawn:ngspice`, …) into the capability
    /// and its argument (the binary name for `spawn`, the variable for `read`, empty
    /// otherwise). The argument borrows from `grant`.
    ///
    /// Returns `None` for an unknown capability, a `spawn` without a binary, a binary name
    /// containing a path separator (grants name a binary, never a path), a `read` whose
    /// argument is not an environment variable name ([`Cap::env_name`]), or an argument on a
    /// capability that takes none.
    #[must_use]
    pub fn parse_grant(grant: &str) -> Option<(Cap, &str)> {
        let (name, arg) = grant.split_once(':').unwrap_or((grant, ""));
        let cap = Cap::parse(name)?;
        let well_formed = match cap {
            Cap::Spawn => !arg.is_empty() && !arg.contains(['/', '\\']) && text::is_bare(arg),
            Cap::ReadEnv => Cap::env_name(arg),
            Cap::ReadProject | Cap::WriteProject | Cap::Network | Cap::Clipboard => {
                arg.is_empty() && !grant.contains(':')
            }
        };
        if well_formed && matches!(cap, Cap::Spawn | Cap::ReadEnv) {
            debug_assert!(!arg.is_empty());
        }
        debug_assert!(
            grant.ends_with(arg),
            "the argument borrows the grant's tail"
        );
        well_formed.then_some((cap, arg))
    }

    /// Whether `name` can follow `read:`: an environment variable name, `[A-Z_][A-Z0-9_]*`,
    /// at most 64 bytes.
    #[must_use]
    pub fn env_name(name: &str) -> bool {
        let Some((&first, tail)) = name.as_bytes().split_first() else {
            return false;
        };
        let valid = name.len() <= ENV_NAME_BYTES_MAX
            && (first.is_ascii_uppercase() || first == b'_')
            && tail
                .iter()
                .all(|&byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if valid {
            debug_assert!(text::is_bare(name), "a `read:` argument is written bare");
        }
        valid
    }
}

impl Kind {
    /// Number of kinds.
    pub const COUNT: usize = Kind::ALL.len();

    /// Top-level record words legal inside a contribution of this kind; anything else is
    /// rejected with `E_PARSE`.
    #[must_use]
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
    #[must_use]
    pub const fn scoped(self) -> bool {
        matches!(self, Kind::Annotations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: the §8 table (numbers 1..=11 in this order, `E_` names).
    #[test]
    fn codes_round_trip_by_value_and_name() {
        for (i, &c) in Code::ALL.iter().enumerate() {
            assert_eq!(c as usize, i + 1);
            assert_eq!(Code::from_raw(c as u32), Some(c));
            assert_eq!(Code::parse(c.name()), Some(c));
            assert_eq!(Code::check(c.neg()), Err(c));
            assert!(c.neg() < 0);
            assert_eq!(c.to_string(), c.name());
        }
        assert_eq!(Code::Parse.name(), "E_PARSE");
        assert_eq!(Code::from_raw(0), None);
        assert_eq!(Code::from_raw(12), None);
        assert_eq!(Code::parse("e_parse"), None);
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
            "spawn:a b",
            "network:x",
            "net",
            "",
            "read",
            "read:",
            "read:pdk_root",
            "read:1PDK",
            "read:PDK/ROOT",
            "read:PDK-ROOT",
            "read_project:X",
            "read_project:",
        ] {
            assert_eq!(Cap::parse_grant(bad), None, "{bad}");
        }
        assert_eq!(
            Cap::parse_grant("read:PDK_ROOT"),
            Some((Cap::ReadEnv, "PDK_ROOT"))
        );
        assert_eq!(Cap::parse_grant("read:_X9"), Some((Cap::ReadEnv, "_X9")));
        assert!(Cap::env_name(&"A".repeat(64)));
        assert!(!Cap::env_name(&"A".repeat(65)));
    }

    #[test]
    fn named_enums_are_dense_and_round_trip() {
        for (i, &k) in Kind::ALL.iter().enumerate() {
            assert_eq!(k as usize, i);
            assert_eq!(Kind::parse(k.name()), Some(k));
            assert!(!k.words().is_empty());
            assert_eq!(k.scoped(), k == Kind::Annotations);
        }
        for (i, &l) in Level::ALL.iter().enumerate() {
            assert_eq!(l as usize, i);
            assert_eq!(Level::parse(l.name()), Some(l));
        }
        assert_eq!(Kind::COUNT, 9);
        assert_eq!(Kind::parse("nope"), None);
        assert_eq!(Kind::parse("UI"), None);
        assert_eq!(EditMode::parse("stage"), Some(EditMode::Stage));
    }

    /// `FromStr` agrees with the inherent `parse` on every name and fails with `E_PARSE`.
    #[test]
    fn from_str_matches_parse() {
        for &kind in Kind::ALL {
            let parsed: Result<Kind, Code> = kind.name().parse();
            assert_eq!(parsed, Ok(kind));
        }
        for code in Code::ALL {
            let parsed: Result<Code, Code> = code.name().parse();
            assert_eq!(parsed, Ok(code));
        }
        let mode: Result<EditMode, Code> = "Stage".parse();
        assert_eq!(mode, Err(Code::Parse));
        let code: Result<Code, Code> = "E_NOPE".parse();
        assert_eq!(code, Err(Code::Parse));
    }
}
