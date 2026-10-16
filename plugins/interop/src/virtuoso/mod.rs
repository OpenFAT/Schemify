//! Cadence Virtuoso bridge (spec `interop.md` §4–§9): Design ⇄ `.sfyx` records.
//!
//! Spine ([`export`], [`import`]): see the spec's §1 table. Submodules: `read` (records →
//! Design), `write` (Design → records), `ael` (values, §3.4), `devices` (§6.5–§6.6),
//! `ade` (maestro ⇄ bench, §9).

use schemify_format::{Document, RecordKind};

use crate::design::{Design, Files};
use crate::map::Mapping;
use crate::merge;
use crate::notes::Notes;

pub mod ade;
pub mod ael;
pub mod characterize;
pub mod devices;
pub mod read;
pub mod write;

/// Export settings.
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    /// Schemify library → Virtuoso library renames (`--lib L=V`); unlisted keep their name.
    pub lib_map: std::collections::BTreeMap<String, String>,
    /// Re-master `core:*` instances onto analogLib (§6.4) instead of exporting `schemify_core`.
    pub analoglib: bool,
    /// The producer string of the header.
    pub producer: String,
}

/// An export's result.
#[derive(Clone, Debug, Default)]
pub struct Export {
    /// The `.sfyx` text.
    pub sfyx: String,
    /// Report rows.
    pub notes: Notes,
    /// The OCEAN scripts (§9.6), `(relative path, text)` sorted by path: `<cell>.ocn` per
    /// bench (its maestro test) and its definition files `<cell>/<file>`.
    pub ocean: Vec<(String, String)>,
}

/// Exports cells `tops` of `d` (and every cell they instantiate that is not a Virtuoso
/// mirror) as one `.sfyx` text.
///
/// # Errors
///
/// Problems that make the export meaningless (an unmapped pin, a cell the mapping names but
/// the Design lacks), one per line; nothing is written then.
pub fn export(
    d: &Design,
    tops: &[usize],
    map: &Mapping,
    opts: &ExportOptions,
) -> Result<Export, Vec<String>> {
    write::export(d, tops, map, opts)
}

/// Import settings.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// Project-relative directory for libraries the manifest does not know (default: the
    /// library name).
    pub lib_dir: std::collections::BTreeMap<String, String>,
    /// Virtuoso library → Schemify library (`--into`, the inverse of an export's `--lib`);
    /// unlisted libraries keep their name, `schemify` maps to the project root and
    /// `schemify_core` to the bundled `core`.
    pub lib_map: std::collections::BTreeMap<String, String>,
    /// Re-master analogLib instances (§6.5 rows) onto core cells (§6.4).
    pub analoglib: bool,
}

/// An import's result: files to write (project-relative path, bytes) and the report.
#[derive(Clone, Debug, Default)]
pub struct Import {
    /// Files to write, sorted by path; includes `schemify.project` when libraries were added.
    pub files: Vec<(String, Vec<u8>)>,
    /// Report rows.
    pub notes: Notes,
}

/// The project files an import of `sfyx` reads to merge (§7.3): the manifest and every
/// target cell file's candidate paths. The guest reads these (a failed read means absent)
/// before calling [`import`].
///
/// # Errors
///
/// The text is not a complete `.sfyx` file.
pub fn import_targets(sfyx: &str) -> Result<Vec<String>, Vec<String>> {
    read::targets(sfyx)
}

/// Converts a `.sfyx` dump into Schemify files, merging with `existing` (§7.3).
///
/// # Errors
///
/// The text is not a complete `.sfyx` file, or a record is malformed (with its line).
pub fn import(
    sfyx: &str,
    existing: &Files,
    map: &Mapping,
    opts: &ImportOptions,
) -> Result<Import, Vec<String>> {
    read::import(sfyx, existing, map, opts)
}

/// The part of a cell one Virtuoso cellview carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Rank 1 and 2: pins, params, impls, symbol graphics (the symbol view and its CDF).
    Symbol,
    /// Rank 3 drawn objects (the schematic view).
    Schematic,
    /// Rank 1 only: pins, params, impls, what the export derives a CDF from (`vcdf`).
    Interface,
}

/// A fingerprint of what the records of `doc` that make up `part` give Virtuoso (FNV-1a 64
/// of their Virtuoso-owned fields, 16 hex digits). Import stores it as header attribute `vsym`/`vsch`; export marks a
/// cellview whose fingerprint still matches with `schemifyKeep` so the loader leaves the
/// library's cellview untouched (spec §7.5). Cost: one canonical write of the part.
pub fn fingerprint(doc: &Document<'_>, part: Part) -> String {
    use RecordKind as K;
    let kinds: &[RecordKind] = match part {
        Part::Symbol => &[
            K::Pin,
            K::Param,
            K::Impl,
            K::Line,
            K::Rect,
            K::Arc,
            K::Circle,
            K::Poly,
            K::Stext,
        ],
        Part::Interface => &[K::Pin, K::Param, K::Impl],
        Part::Schematic => &[
            K::Inst,
            K::Wire,
            K::Label,
            K::Port,
            K::Noconn,
            K::Text,
            K::Shape,
        ],
    };
    // The projection: only what Virtuoso holds. Schemify-only attributes (§7.1) are left
    // out, so adding a bound or an ngspice format never counts as a change to the cellview,
    // while any edit to what the cellview carries does.
    let mut lines: Vec<String> = Vec::new();
    for r in 1..doc.record_count() {
        let k = doc.record_kind(r);
        if !kinds.contains(&k) {
            continue;
        }
        let mut text = String::new();
        text.push_str(doc.record_word(r));
        for f in doc.fields(r) {
            text.push(' ');
            text.push_str(f);
        }
        text.push('\n');
        // Attribute lines sorted: the order attributes were pushed in is not meaning, and the
        // writer reorders them (unknown ones last), so a fresh import and the file it writes
        // must fingerprint the same (a mismatch silently loses §7.5 keep).
        let mut attr_lines: Vec<String> = Vec::new();
        for a in doc.attrs(r) {
            let key = doc.attr_key(a);
            let schemify_only = match k {
                K::Pin | K::Param => merge::schemify_only_attr(k, key, doc.attr_value(a, 0)),
                // Only the format: an impl's `model file ignore` stay in the fingerprint, which
                // is stored in files and so never changes meaning.
                K::Impl => key == "format" && !merge::virtuoso_format(doc.attr_value(a, 0)),
                _ => false,
            };
            if schemify_only {
                continue;
            }
            let mut line = format!("  {key}");
            for v in doc.attr_values(a) {
                line.push(' ');
                line.push_str(v);
            }
            line.push('\n');
            attr_lines.push(line);
        }
        attr_lines.sort();
        text.push_str(&attr_lines.concat());
        for v in doc.verbatim(r) {
            text.push_str("  | ");
            text.push_str(v);
            text.push('\n');
        }
        lines.push(text);
    }
    // Rank 3 is a sorted rank (format spec §4): its order is the writer's, not meaning.
    if part == Part::Schematic {
        lines.sort();
    }
    merge::digest(&lines.concat())
}

/// Header attribute holding the fingerprint of `part`.
pub fn fingerprint_attr(part: Part) -> &'static str {
    match part {
        Part::Symbol => "vsym",
        Part::Schematic => "vsch",
        Part::Interface => "vcdf",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use schemify_format::{FileKind, parse, write};

    /// A symbol pin `p` with `attrs` pushed in the given order.
    fn pin_doc(attrs: &[(&str, &str)]) -> Document<'static> {
        let mut d = Document::new(FileKind::Shprim);
        d.begin_record(RecordKind::Pin);
        for f in ["p", "inout", "0", "0"] {
            d.push_field(f);
        }
        for (k, v) in attrs {
            d.push_attr(k);
            d.push_value(v);
        }
        // A .shprim needs an impl (E025).
        d.begin_record(RecordKind::Impl);
        d.push_field("spice");
        d.push_field("spice");
        d.push_attr("format");
        d.push_value("spice");
        d.push_value("X{name} {p} sub");
        d
    }

    const ATTRS: [(&str, &str); 4] = [
        ("vplabel", "none"),
        ("inherit", "vdd"),
        ("vpfig", "8"),
        ("alias", "q"),
    ];

    proptest! {
        /// The fingerprint ignores attribute order and survives the writer (§7.5): a fresh
        /// import and the file written from it agree, whatever order import pushed in.
        #[test]
        fn fingerprints_ignore_attribute_order(perm in Just(ATTRS.to_vec()).prop_shuffle()) {
            let d = pin_doc(&perm);
            let text = write(&d);
            let reread = parse(text.as_bytes()).unwrap();
            let fp = fingerprint(&d, Part::Symbol);
            prop_assert_eq!(&fp, &fingerprint(&reread, Part::Symbol));
            prop_assert_eq!(&fp, &fingerprint(&pin_doc(&ATTRS), Part::Symbol));
        }
    }

    #[test]
    fn any_value_change_changes_the_fingerprint() {
        let base = fingerprint(&pin_doc(&ATTRS), Part::Symbol);
        for k in 0..ATTRS.len() {
            let mut changed = ATTRS.to_vec();
            changed[k].1 = "other";
            assert_ne!(
                base,
                fingerprint(&pin_doc(&changed), Part::Symbol),
                "{}",
                ATTRS[k].0
            );
        }
        // A Schemify-only attribute (§7.1) is not part of what Virtuoso holds.
        let mut extra = ATTRS.to_vec();
        extra.push(("domain", "analog"));
        assert_eq!(base, fingerprint(&pin_doc(&extra), Part::Symbol));
    }
}
