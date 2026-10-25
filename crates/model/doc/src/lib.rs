//! Design data (D9–D11, D19, D28, D30, D35, D40): the only crate that writes cells,
//! symbols, impls, instances, wires, labels, ports, docs blocks, bench tables,
//! constraints, waivers, staged edits and the edit journal. Everything else changes
//! design data by sending edit requests to [`apply_edits`].
//!
//! Pure: bytes in, bytes and requests out. No file system, clock or threads; the app
//! spine does I/O (native fs or OPFS) and hands this crate file contents, directory
//! listings, timestamps and seeds. Builds for `wasm32-unknown-unknown`.
//!
//! Normative specs: `docs/spec/format.md` (records), `docs/spec/edits.md` (transactions,
//! undo, journal, external changes, saves), `docs/spec/netlist.md` §1 (cell resolution).
//!
//! # Tables
//!
//! A [`Document`] is one file as typed column tables (see the `tables` module docs for
//! the data-oriented-design answers): [`Pins`], [`Graphics`], [`Instances`],
//! [`Wires`], [`Placed`] (labels, ports, texts), [`Noconns`] and [`Records`] (every
//! text-only kind), plus one string arena ([`Strings`]) and one [`Pool`] of per-row
//! attribute lines, verbatim lines, comments and coordinates. A [`Project`] owns the
//! manifest, lockfile, library index, cell interfaces and open documents.
//!
//! # Spines
//!
//! - load: `Project::new(manifest, lock)` → `index_library` per library (caller
//!   supplies `(relpath, bytes)` listings) → `open` per opened cell.
//! - frame: [`apply_edits`]`(project, edit_requests)` (then [`Project::applied`] per
//!   outcome for the ops it applied) → `take_journal` per document.
//! - save: `save` → app writes atomically → `saved`.
//! - external change: `external_change` → (conflict → `resolve_conflicts`).
//!
//! # Example
//!
//! ```
//! use schemify_doc::{apply_edits, Edit, EditRequest, Mode, Op, Outcome, Project, RecordBuf, Transaction, NEW_ID};
//! use schemify_format::RecordKind;
//!
//! let mut project = Project::new(None, None, 42).unwrap();
//! let bench = b"schemify 1 shbench\n\nwire 0d2k1m 0 0 10 0\n";
//! let doc = project.open("", "ota_ac.shbench", bench, None).unwrap().handle;
//! let wire = RecordBuf::new(RecordKind::Wire).field(NEW_ID).field("10").field("0").field("10").field("20");
//! let tx = Transaction { label: "draw wire".into(), mode: Mode::Commit, ops: vec![Op::Insert { rec: wire, at: None }] };
//! let out = apply_edits(&mut project, &[EditRequest { doc, edit: Edit::Tx(tx), unix_ms: 0 }]);
//! assert!(matches!(out[0], Outcome::Committed { .. }));
//! assert_eq!(project.doc(doc).unwrap().wires().x1.len(), 2);
//! ```

#![forbid(unsafe_code)]

mod changes;
mod core_lib;
mod document;
mod edit;
mod journal;
mod library;
mod project;
mod record;
mod sha256;
mod strings;
mod tables;
mod techmap;

pub use changes::{Change, Cursor, LOG_MAX, Write};
pub use core_lib::{core_file, core_library};
pub use document::{Document, Loc, TableId};
pub use edit::{Mode, NEW_ID, OPS_MAX, Op, OpError, Target, Transaction};
pub use journal::{ops_from_text, ops_to_text};
pub use library::{
    CELLS_MAX, CellRef, Cells, LIBS_MAX, LibSource, Libs, LockStatus, Locked, TECH_LIB,
};
pub use project::{
    Choice, ConflictBlock, DOCS_MAX, DocHandle, Edit, EditRequest, External, JournalWrite,
    OpenError, Opened, Outcome, Project, Recovery, ReloadError, STAGES_MAX, SaveRequest, Stage,
    UNDO_MAX, apply_edits,
};
pub use record::{Attr, RecordBuf};
/// Format types that appear in this crate's columns and edit targets, re-exported so
/// consumers need no direct `schemify-format` dependency just to name them.
pub use schemify_format::{FileKind, Orient, RecordKind};
pub use sha256::{Digest, Sha256, hex, sha256};
pub use strings::{ObjId, SYMS_MAX, Strings, Sym};
pub use tables::{
    Graphics, Instances, Noconns, POOL_MAX, PinDir, Pins, Placed, Pool, Records, Shapes, Span,
    Wires,
};
pub use techmap::{CLASS_PARAMS, CLASSES, CLASSES_MAX, FLAVORS, TechMap, core_symbol};

use std::fmt;

/// Error and warning codes this crate reports. Shared codes keep their meaning from
/// `docs/spec/format.md` §7 and `docs/spec/netlist.md` §8; `E040`–`E044` and `W002`
/// are doc-level (edits spec §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(missing_docs)] // meanings are in `Code::meaning` and the specs
pub enum Code {
    E001,
    E020,
    E021,
    E022,
    E023,
    E024,
    E025,
    E026,
    E027,
    E028,
    E029,
    E030,
    E040,
    E041,
    E042,
    E043,
    E044,
    W001,
    W002,
}

impl Code {
    /// Returns the spec meaning.
    pub fn meaning(self) -> &'static str {
        match self {
            Code::E001 => "missing lib or cell, or a bench instantiated",
            Code::E020 => "same cell stem with two kinds in one library",
            Code::E021 => "file too large",
            Code::E022 => "coordinate not representable",
            Code::E023 => "record out of rank order",
            Code::E024 => "duplicate name within a kind",
            Code::E025 => "kind rule violated",
            Code::E026 => "zero-length wire",
            Code::E027 => "unknown record",
            Code::E028 => "malformed line",
            Code::E029 => "wrong arity or value form",
            Code::E030 => "override of a computed param",
            Code::E040 => "edit target not found",
            Code::E041 => "id already in use",
            Code::E042 => "reference to a record that does not exist",
            Code::E043 => "operation not allowed on this record",
            Code::E044 => "document is read-only or in conflict",
            Code::W001 => "duplicate id re-assigned",
            Code::W002 => "override equals the default",
        }
    }

    /// Returns whether the code is a warning.
    pub fn is_warning(self) -> bool {
        matches!(self, Code::W001 | Code::W002)
    }

    pub(crate) fn from_format(code: schemify_format::Code) -> Code {
        use schemify_format::Code as F;
        match code {
            F::E021 => Code::E021,
            F::E022 => Code::E022,
            F::E023 => Code::E023,
            F::E024 => Code::E024,
            F::E025 => Code::E025,
            F::E026 => Code::E026,
            F::E027 => Code::E027,
            F::E028 => Code::E028,
            F::E029 => Code::E029,
            F::W001 => Code::W001,
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// One finding about a loaded file or library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Diag {
    /// What was found.
    pub code: Code,
    /// 1-based source line, or 0 when not tied to one line.
    pub line: u32,
    /// Which rule, beyond the code's meaning.
    pub detail: &'static str,
}
