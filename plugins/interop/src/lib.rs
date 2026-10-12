//! The interop plugin (D24 as amended 2026-10-12): conversions between Schemify and other
//! tools' formats. Today that is the clean-room xschem importer (D1, D14): `.sch`/`.sym`
//! designs and PDK libraries, SPICE model-card bin ranges, tech-file grids and Cadence
//! `cdfDump` output. Virtuoso import/export and xschem export are to come, beside these
//! modules. It is never linked into the core: the editor loads it as a wasm guest (the
//! `plugin` module, `wasm32-unknown-unknown` only), and the native command line
//! `schemify-interop` (the `cli` module, feature `cli`) runs the same code over the file
//! system.
//!
//! Two layers. The **readers** turn text into data-oriented tables and never evaluate,
//! execute or resolve anything. The **translation** ([`translate_library`], spec
//! `docs/spec/import.md`) turns those tables into Schemify files (`.shprim`/`.shcomp`/
//! `.shbench`, a project manifest and an import report); [`check_output`] is its
//! oracle (every file loads in `doc`, round-trips, and every expression compiles).
//! Behaviour comes from `docs/research/` and from golden netlists produced by running
//! xschem, never from xschem source (D1).
//!
//! | Transform | Reads | Writes |
//! |---|---|---|
//! | [`translate_library`] | `(path, bytes)` of `.sym`/`.sch` + [`Options`] | `(path, bytes)` outputs + [`Report`] |
//! | [`tcl::pdk_vars`], [`tcl::drc_procs`] | PDK `xschemrc` text | project `set` entries, `drc` rules |
//! | [`check_output`] | translated outputs | list of problems (empty = valid) |
//! | [`xschem::read`] | `.sch`/`.sym` text | [`xschem::XschemFile`]: record tables, dropped-record issues, D33 roundings |
//! | [`attr::Attrs::parse_into`] | one attribute string | key/value rows (unescaped, source spans) |
//! | [`attr::classify`] | one value or text | [`attr::Idioms`] bitset (corpus audit) |
//! | [`symbol::extract`] | [`xschem::XschemFile`] | [`symbol::SymbolInterface`]: attributes, template, pins in netlist order |
//! | [`spice::scan`] | model cards, through a caller loader | [`spice::ModelScan`]: models, bins, subckts, OSDI evidence |
//! | [`tech::read_lyt`], [`tech::read_magic`] | KLayout / Magic tech files | manufacturing grid in picometres |
//! | [`cdf::read`] | `cdfDump` SKILL text | [`cdf::CdfDump`]: parameter tables and a review list |
//!
//! One lexer per syntax: [`xschem::read`] for records, [`attr`] for attribute strings,
//! `spice` (`logical_lines`, `words`, `lex_number`) for every SPICE text the translator
//! reads (model cards, `.subckt` ports, bench `.param`s, numbers in expressions).
//!
//! Every reader is total over its input: malformed parts land in an issue table with a
//! location instead of a panic, and each reader asserts a count-conservation invariant on
//! exit (records or lines read = kept + reported).

// SIMD triage (simd-loops): inputs are single files of kilobytes (the 3,218-file corpus is
// ~60 MB, read in ~0.1 s), every scanner is a byte state machine (a chain dependency),
// so the scalar loops stay.

#![deny(unsafe_code)] // `forbid` would reject the guest SDK's `export!` (it allows it)
#![warn(missing_docs)]

pub mod attr;
pub mod cdf;
pub mod check;
#[cfg(feature = "cli")]
pub mod cli;
pub mod core;
pub mod decimal;
pub mod design;
pub mod job;
mod limits;
pub mod map;
pub mod nets;
pub mod notes;
pub mod profile;
pub mod remap;
pub mod report;
pub mod retarget;
pub mod sfyx;
pub mod skill;
pub mod spice;
pub mod symbol;
pub mod tcl;
pub mod tech;
pub mod template;
pub mod translate;
pub mod virtuoso;
pub mod xschem;
pub mod xschem_out;

// The guest target only: the WASI build of `schemify-interop` (CI determinism) must not
// export guest entry points that import the `schemify` host module.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod plugin;

pub use crate::core::DEVICE_MAP;
pub use check::check_output;
pub use report::Report;
pub use translate::{Options, translate_library};

/// Byte range into an arena string owned by the table that hands it out.
///
/// A `Span` only means something together with the arena it was cut from; each table
/// names its arena. Offsets fit `u32` because readers cap input at [`MAX_INPUT_BYTES`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    /// Byte offset of the first byte.
    pub start: u32,
    /// Length in bytes.
    pub len: u32,
}

impl Span {
    /// Returns the spanned text.
    ///
    /// # Panics
    ///
    /// Panics if the span is not a char-boundary range of `arena`, i.e. if it was cut
    /// from a different arena.
    pub fn of<'a>(&self, arena: &'a str) -> &'a str {
        let start = self.start as usize;
        &arena[start..start + self.len as usize]
    }

    pub(crate) fn new(start: usize, end: usize) -> Span {
        assert!(start <= end, "span start {start} after end {end}");
        assert!(end <= u32::MAX as usize, "span end {end} beyond u32");
        Span {
            start: start as u32,
            len: (end - start) as u32,
        }
    }
}

/// Largest input [`xschem::read`] accepts, in bytes (2^30, the cap of
/// `docs/spec/format.md` §2). Larger inputs are refused before parsing.
pub const MAX_INPUT_BYTES: usize = 1 << 30;

/// Appends `text` to `arena` and returns its span.
pub(crate) fn push_span(arena: &mut String, text: &str) -> Span {
    let start = arena.len();
    arena.push_str(text);
    Span::new(start, arena.len())
}
