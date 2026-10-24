//! Line-record codec for every Schemify text file (D9): documents, project manifest,
//! lockfile, user config and the edit journal.
//!
//! Owns no tables. Parses bytes into a flat record model ([`Document`]), writes the
//! canonical form ([`write()`]), and merges/diffs at the record level ([`merge3`],
//! [`diff`]) keyed by kind + id or kind + name. [`parse_prefix`] reads only a symbol
//! interface without touching the schematic. Normative spec: `docs/spec/format.md`.
//!
//! The schema is data: [`FileKind::records`] lists the allowed [`RecordKind`]s and each
//! kind's [`RecordSchema`] gives its rank, identity rule, positional shape and known
//! attributes. Semantics beyond that (expressions, templates, cell references) belong
//! to later layers.
//!
//! # Examples
//!
//! Parse, read, edit through the builder, and write canonically:
//!
//! ```
//! use schemify_format::{parse, parse_coord, write, Document, Orient, RecordKind};
//!
//! let text = "schemify 1 shbench\n  doc \"OTA bench\"\n\ninst 2kq9vd ./ota5t\n  at 0 -12.5 R0\n  name X1\n";
//! let doc = parse(text.as_bytes())?;
//! assert_eq!(write(&doc), text); // canonical input is a fixed point
//!
//! let inst = 1;
//! assert_eq!(doc.record_kind(inst), RecordKind::Inst);
//! let at = doc.find_attr(inst, "at").unwrap();
//! assert_eq!(doc.attr_value(at, 1).map(parse_coord), Some(Ok(-12_500)));
//!
//! // Build a document from scratch, e.g. from the `doc` crate's tables.
//! let mut out = Document::new(doc.kind());
//! out.begin_record(RecordKind::Wire);
//! out.push_field("0d2k1m");
//! for milli in [10_000, 0, 0, 0] {
//!     out.push_field_coord(milli);
//! }
//! out.begin_record(RecordKind::Label);
//! out.push_field("2ma7xe");
//! out.push_field_coord(0);
//! out.push_field_coord(0);
//! out.push_field(Orient::R0.as_str());
//! out.push_field("tail");
//! // Endpoints are normalized; kinds are separated by a blank line.
//! assert_eq!(write(&out), "schemify 1 shbench\n\nwire 0d2k1m 0 0 10 0\n\nlabel 2ma7xe 0 0 R0 tail\n");
//! # Ok::<(), schemify_format::ParseError>(())
//! ```

mod diag;
mod document;
mod id;
mod merge;
mod reader;
mod scalar;
mod schema;
mod writer;

pub use diag::{Code, Diagnostic, NO_RECORD, ParseError};
pub use document::{Document, NONE, Texts, VERSION};
pub use id::{ID_ALPHABET, ID_LEN_MAX, ID_LEN_NEW, IdRng, NewId, fresh_id, is_id};
pub use merge::{Change, ChangeKind, Diff, Merged, Parts, diff, merge3};
pub use reader::{MAX_FILE_BYTES, parse, parse_prefix};
pub use scalar::{CoordError, MILLI_PER_UNIT, Orient, format_coord, is_number, parse_coord};
pub use schema::{
    AttrSchema, FileKind, KeyRule, RANK_INHERIT, RANK_MAX, RANK_POSITIONAL, RecordKind,
    RecordSchema, Rest, SHAPE_FORMS, Shape, Ty, shape_arity,
};
pub use writer::{write, write_into};
