//! Store-side merge of an imported cell (spec `interop.md` §2 store, §7.1, §7.3): the
//! fresh document built from a dump, the existing local file and the export's stash
//! become the file to write.
//!
//! A document is cut into **units**: the header, the rank-1 block (pins, params, impls),
//! the rank-2 block (symbol graphics), and every other record by its identity (kind and
//! id or name). [`stash`] keeps, at export time, every unit the Virtuoso round trip would
//! not reproduce, tagged with a digest of what the import *will* produce for it (`vimg`).
//! [`merge`] then takes, per unit: the local (or stashed) unit when the Virtuoso side left
//! it unchanged (the fresh unit equals the stashed digest, or the local unit when there
//! is no digest); otherwise the fresh unit, enriched with the Schemify-only attributes of
//! the local one. Schemify-only records (docs, waivers, constraints, …) come from the
//! local file, else the stash, and are dropped with a note when their subject is gone.

use std::collections::{BTreeMap, BTreeSet};

use schemify_format::{Document, FileKind, RecordKind, parse, parse_coord, write};

use crate::design::{copy_attr, copy_header, owned_doc};
use crate::notes::Notes;

/// Header attribute of a stash: digest of the header the import produces.
const VIMG_HEADER: &str = "vimg_header";
/// Header attribute of a stash: digest of the rank-1 block the import produces (present
/// only when the stash's rank-1 block differs from it).
const VIMG_RANK1: &str = "vimg_rank1";
/// Header attribute of a stash: digest of the rank-2 block the import produces.
const VIMG_RANK2: &str = "vimg_rank2";
/// Record attribute of a stash: digest of the record the import produces.
const VIMG: &str = "vimg";

/// Pin attributes no Virtuoso terminal holds (§7.1).
pub(crate) const SCHEMIFY_PIN_ATTRS: [&str; 2] = ["domain", "number"];
/// Param attributes no CDF parameter holds (§7.1): `valid` and `vderived` come from the
/// characterization's rules (§8.3).
pub(crate) const SCHEMIFY_PARAM_ATTRS: [&str; 9] = [
    "min", "max", "step", "grid", "show", "computed", "cached", "valid", "vderived",
];
/// Impl attributes no Virtuoso view holds (§7.1).
pub(crate) const SCHEMIFY_IMPL_ATTRS: [&str; 3] = ["model", "file", "ignore"];

/// Whether an impl `format` of `dialect` is one the Virtuoso side produces (from simInfo:
/// `spectre`, and `cdl` from auCdl); any other dialect's format is Schemify's (§8.1).
pub(crate) fn virtuoso_format(dialect: Option<&str>) -> bool {
    matches!(dialect, Some("spectre" | "cdl"))
}

/// Whether attribute `key` (first value `first`) of a rank-1 record of `kind` is Schemify's
/// alone (§7.1): the attribute lists above, and an impl `format` the Virtuoso side does not
/// produce.
///
/// # Panics
///
/// When `kind` is not a rank-1 kind (pin, param, impl).
pub(crate) fn schemify_only_attr(kind: RecordKind, key: &str, first: Option<&str>) -> bool {
    match kind {
        RecordKind::Pin => SCHEMIFY_PIN_ATTRS.contains(&key),
        RecordKind::Param => SCHEMIFY_PARAM_ATTRS.contains(&key),
        RecordKind::Impl => {
            SCHEMIFY_IMPL_ATTRS.contains(&key) || (key == "format" && !virtuoso_format(first))
        }
        k => panic!("{k:?} is not a rank-1 record"),
    }
}

/// Units per document this module handles (format.md's per-file rank-3 bound).
pub const UNITS_MAX: usize = 1 << 22;

/// FNV-1a 64 of `text`, as 16 lowercase hex digits: the stash digests and the cellview
/// fingerprints (§7.5). Stable across releases, since both are stored in files.
pub fn digest(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let out = format!("{h:016x}");
    debug_assert_eq!(out.len(), 16);
    out
}

/// Identity of a record outside ranks 0–2.
type Key = (RecordKind, String, String);

fn key(doc: &Document<'_>, r: u32) -> Key {
    let k = doc.record_kind(r);
    let w = if k == RecordKind::Unknown {
        doc.record_word(r).to_owned()
    } else {
        String::new()
    };
    (k, w, doc.record_key(r).0.to_owned())
}

/// Canonical text of records `rs` of `doc`, attributes `skip` left out.
fn text(doc: &Document<'_>, rs: &[u32], skip: &[&str]) -> String {
    let mut d = Document::new(doc.kind());
    for &r in rs {
        if r == 0 {
            copy_header(&mut d, doc, skip);
            continue;
        }
        copy_skipping(&mut d, doc, r, skip);
    }
    let t = write(&d);
    t.split_once('\n')
        .map_or(String::new(), |(_, rest)| rest.to_owned())
}

/// Appends record `r` of `src` to `out` without the attributes named in `skip`.
fn copy_skipping(out: &mut Document<'static>, src: &Document<'_>, r: u32, skip: &[&str]) {
    if skip.is_empty() || !src.attrs(r).any(|a| skip.contains(&src.attr_key(a))) {
        out.copy_record(src, r);
        return;
    }
    let k = src.record_kind(r);
    if k == RecordKind::Unknown {
        out.begin_unknown_record(src.record_word(r));
    } else {
        out.begin_record(k);
    }
    for c in src.comments(r) {
        out.push_comment(c);
    }
    for f in src.fields(r) {
        out.push_field(f);
    }
    for a in src.attrs(r) {
        if !skip.contains(&src.attr_key(a)) {
            copy_attr(out, src, a);
        }
    }
    for v in src.verbatim(r) {
        out.push_verbatim(v);
    }
}

fn ranked(doc: &Document<'_>, rank: u8) -> Vec<u32> {
    (1..doc.record_count())
        .filter(|&r| doc.record_kind(r).rank() == rank)
        .collect()
}

fn others(doc: &Document<'_>) -> Vec<u32> {
    (1..doc.record_count())
        .filter(|&r| doc.record_kind(r).rank() > 2)
        .collect()
}

/// Wires that meet a collinear wire at an end. Virtuoso merges such a run and splits it
/// again where wires T-join, and the pieces' ids can come back exchanged (live ICADVM20.1
/// run), so the stash keeps their geometry and the import puts the ids back (§7.3).
fn collinear_joints(doc: &Document<'_>) -> BTreeSet<u32> {
    let mut at: BTreeMap<[i32; 2], Vec<(u32, [i64; 2])>> = BTreeMap::new();
    for r in (1..doc.record_count()).filter(|&r| doc.record_kind(r) == RecordKind::Wire) {
        let c = |k| doc.field(r, k).and_then(|f| parse_coord(f).ok());
        let (Some(x0), Some(y0), Some(x1), Some(y1)) = (c(1), c(2), c(3), c(4)) else {
            continue;
        };
        let d = [i64::from(x1) - i64::from(x0), i64::from(y1) - i64::from(y0)];
        at.entry([x0, y0]).or_default().push((r, d));
        at.entry([x1, y1]).or_default().push((r, d));
    }
    let mut out = BTreeSet::new();
    for ends in at.values() {
        for (k, (r, d)) in ends.iter().enumerate() {
            for (q, e) in &ends[k + 1..] {
                if q != r && d[0] * e[1] == d[1] * e[0] {
                    out.extend([*r, *q]);
                }
            }
        }
    }
    debug_assert!(out.iter().all(|&r| doc.record_kind(r) == RecordKind::Wire));
    out
}

fn header_attr<'d>(doc: &'d Document<'_>, k: &str) -> Option<&'d str> {
    doc.find_attr(0, k).and_then(|a| doc.attr_value(a, 0))
}

/// The stash of `orig` (§7.3): canonical text of a document of the same kind holding
/// every unit that `image` (what the import produces from the export) does not reproduce,
/// each tagged with the digest of its image. `None` when the image reproduces everything.
/// The rank-1 block is always present for `.shprim`/`.shcomp` (kind rules); only its
/// `vimg_rank1` tag says it is stashed.
pub fn stash(orig: &Document<'_>, image: Option<&Document<'_>>) -> Option<String> {
    let mut s = Document::new(orig.kind());
    let mut any = false;
    let ht = text(orig, &[0], &[]);
    let hi = image.map(|i| text(i, &[0], &[]));
    let r1 = ranked(orig, 1);
    let r1t = text(orig, &r1, &[]);
    let r1i = image.map(|i| text(i, &ranked(i, 1), &[]));
    let r2 = ranked(orig, 2);
    let r2t = text(orig, &r2, &[]);
    let r2i = image.map(|i| text(i, &ranked(i, 2), &[]));
    if hi.as_ref() != Some(&ht) {
        any = true;
        copy_header(&mut s, orig, &[]);
        if let Some(h) = &hi {
            s.push_attr(VIMG_HEADER);
            s.push_value(&digest(h));
        }
    }
    let r1_stashed = r1i.as_ref() != Some(&r1t);
    if r1_stashed {
        any = true;
        s.push_attr(VIMG_RANK1);
        s.push_value(&r1i.as_deref().map_or("none".into(), digest));
    }
    let r2_stashed = r2i.as_ref() != Some(&r2t) && !r2.is_empty();
    if r2_stashed {
        any = true;
        s.push_attr(VIMG_RANK2);
        s.push_value(&r2i.as_deref().map_or("none".into(), digest));
    }
    if r1_stashed || matches!(orig.kind(), FileKind::Shprim | FileKind::Shcomp) {
        for &r in &r1 {
            s.copy_record(orig, r);
        }
    }
    if r2_stashed {
        for &r in &r2 {
            s.copy_record(orig, r);
        }
    }
    let img_keys: Vec<(Key, u32)> = image.map_or(Vec::new(), |i| {
        others(i).into_iter().map(|r| (key(i, r), r)).collect()
    });
    // Rank-4 order is semantic (format.md §4): when the image orders the rank-4 units it
    // shares with `orig` differently, every rank-4 unit is stashed, so the merge (which
    // keeps the base's order) restores it.
    let rank4 = |d: &Document<'_>| -> Vec<Key> {
        (1..d.record_count())
            .filter(|&r| d.record_kind(r).rank() == 4)
            .map(|r| key(d, r))
            .collect()
    };
    let reorder4 = image.is_some_and(|i| {
        let (o, m) = (rank4(orig), rank4(i));
        let shared = |a: &[Key], b: &[Key]| {
            a.iter()
                .filter(|k| b.contains(k))
                .cloned()
                .collect::<Vec<_>>()
        };
        shared(&o, &m) != shared(&m, &o)
    });
    let joints = collinear_joints(orig);
    for r in others(orig) {
        let k = key(orig, r);
        let mine = text(orig, &[r], &[]);
        let theirs = img_keys
            .iter()
            .find(|(ik, _)| *ik == k)
            .map(|(_, ir)| text(image.expect("keys come from the image"), &[*ir], &[]));
        if theirs.as_ref() == Some(&mine) && !(reorder4 && k.0.rank() == 4) && !joints.contains(&r)
        {
            continue;
        }
        any = true;
        copy_skipping(&mut s, orig, r, &[]);
        if let Some(t) = theirs {
            s.push_attr(VIMG);
            s.push_value(&digest(&t));
        }
    }
    for c in orig.eof_comments() {
        any = true;
        s.push_eof_comment(c);
    }
    any.then(|| write(&s))
}

/// Parses stash text (the concatenated `schemifyRecords<k>` properties).
///
/// # Errors
///
/// The text is not a valid Schemify document of `kind`.
pub fn parse_stash(text: &str, kind: FileKind) -> Result<Document<'static>, String> {
    let d = parse(text.as_bytes()).map_err(|e| {
        e.diagnostics
            .first()
            .map_or("stash does not parse".into(), |d| {
                format!("stash line {}: {} {}", d.line, d.code.as_str(), d.detail)
            })
    })?;
    if d.kind() != kind {
        return Err(format!("stash is a {:?}, the cell a {kind:?}", d.kind()));
    }
    let mut out = owned_doc(&d);
    for c in d.eof_comments() {
        out.push_eof_comment(c);
    }
    Ok(out)
}

/// Whether records of kind `k` are Schemify's (§7.1) in a file of kind `fk`; `maestro`
/// says the dump carried this bench's maestro setup (then bench tables are Virtuoso's).
pub fn schemify_owned(k: RecordKind, fk: FileKind, maestro: bool) -> bool {
    match k {
        RecordKind::Waive
        | RecordKind::Constraint
        | RecordKind::Prose
        | RecordKind::Montecarlo
        | RecordKind::Optimize
        | RecordKind::Config
        | RecordKind::Unknown => true,
        RecordKind::Var => fk != FileKind::Shbench || !maestro,
        RecordKind::Code => fk != FileKind::Shbench || !maestro,
        RecordKind::Corner | RecordKind::Sweep | RecordKind::Output | RecordKind::Spec => !maestro,
        RecordKind::Body => true,
        _ => false,
    }
}

/// Merges `fresh` (built from the dump) with `local` (the existing file) and `stash` (the
/// export's, from the dump) into the file to write. `cell` names rows in `notes`
/// (`dropped:stale-schemify-data`). See the module docs for the per-unit rule.
///
/// # Panics
///
/// When the documents' kinds differ or a document holds more than [`UNITS_MAX`] records.
pub fn merge(
    fresh: &Document<'_>,
    local: Option<&Document<'_>>,
    stash: Option<&Document<'_>>,
    maestro: bool,
    cell: &str,
    notes: &mut Notes,
) -> Document<'static> {
    let fk = fresh.kind();
    for d in [local, stash].into_iter().flatten() {
        assert_eq!(d.kind(), fk, "{cell}: merging documents of different kinds");
    }
    assert!((fresh.record_count() as usize) < UNITS_MAX);
    let base = local.or(stash);
    let mut out = Document::new(fk);
    // Header.
    let fh = text(fresh, &[0], &[]);
    let s_head = stash.and_then(|s| header_attr(s, VIMG_HEADER).map(str::to_owned));
    let head_src: Option<(&Document<'_>, bool)> = match (local, stash) {
        (Some(l), _) => Some((
            l,
            s_head
                .as_ref()
                .map_or(stripped_header(l) == stripped_header(fresh), |d| {
                    *d == digest(&fh)
                }),
        )),
        (None, Some(s)) if s_head.is_some() => Some((s, s_head.as_deref() == Some(&digest(&fh)))),
        _ => None,
    };
    let vimgs = [VIMG_HEADER, VIMG_RANK1, VIMG_RANK2];
    match head_src {
        Some((src, true)) => copy_header(&mut out, src, &vimgs),
        Some((src, false)) => {
            copy_header(&mut out, fresh, &[]);
            for a in src.attrs(0) {
                let k = src.attr_key(a);
                if !vimgs.contains(&k) && fresh.find_attr(0, k).is_none() {
                    copy_attr(&mut out, src, a);
                }
            }
        }
        None => copy_header(&mut out, fresh, &[]),
    }
    // Rank 1 and rank 2 blocks.
    for (rank, tag) in [(1u8, VIMG_RANK1), (2u8, VIMG_RANK2)] {
        let f = ranked(fresh, rank);
        let ft = text(fresh, &f, &[]);
        let s_dig = stash.and_then(|s| header_attr(s, tag).map(str::to_owned));
        let src: Option<(&Document<'_>, bool)> = match (local, stash) {
            (Some(l), _) => {
                let same = if rank == 1 {
                    stripped_rank1(l) == stripped_rank1(fresh)
                } else {
                    text(l, &ranked(l, rank), &[]) == ft
                };
                Some((l, s_dig.as_ref().map_or(same, |d| *d == digest(&ft))))
            }
            (None, Some(s)) if s_dig.is_some() => Some((s, s_dig.as_deref() == Some(&digest(&ft)))),
            _ => None,
        };
        match src {
            Some((b, true)) => {
                for r in ranked(b, rank) {
                    out.copy_record(b, r);
                }
            }
            Some((b, false)) if rank == 1 => enrich_rank1(&mut out, fresh, b, cell, notes),
            _ => {
                for r in f {
                    out.copy_record(fresh, r);
                }
            }
        }
    }
    // Ranks 3–6 by identity.
    let fkeys: Vec<(Key, u32)> = others(fresh)
        .into_iter()
        .map(|r| (key(fresh, r), r))
        .collect();
    let lkeys: Vec<(Key, u32)> = local.map_or(Vec::new(), |l| {
        others(l).into_iter().map(|r| (key(l, r), r)).collect()
    });
    let skeys: Vec<(Key, u32)> = stash.map_or(Vec::new(), |s| {
        others(s).into_iter().map(|r| (key(s, r), r)).collect()
    });
    let find = |ks: &[(Key, u32)], k: &Key| ks.iter().find(|(x, _)| x == k).map(|(_, r)| *r);
    let mut kept: Vec<(Key, &Document<'_>, u32, bool)> = Vec::new();
    for (k, fr) in &fkeys {
        let ftext = text(fresh, &[*fr], &[]);
        let srec = find(&skeys, k);
        let s_dig = srec.and_then(|r| {
            let s = stash.expect("stash keys");
            s.find_attr(r, VIMG)
                .and_then(|a| s.attr_value(a, 0))
                .map(str::to_owned)
        });
        let pick: (&Document<'_>, u32, bool) = match (local, find(&lkeys, k)) {
            (Some(l), Some(lr)) => {
                let same = s_dig
                    .as_ref()
                    .map_or(text(l, &[lr], &[]) == ftext, |d| *d == digest(&ftext));
                if same {
                    (l, lr, false)
                } else {
                    (fresh, *fr, true)
                }
            }
            (Some(_), None) => (fresh, *fr, false),
            (None, _) => match (srec, &s_dig) {
                (Some(sr), Some(d)) if *d == digest(&ftext) => (stash.expect("stash"), sr, false),
                (Some(_), _) => (fresh, *fr, true),
                (None, _) => (fresh, *fr, false),
            },
        };
        kept.push((k.clone(), pick.0, pick.1, pick.2));
    }
    // Schemify-only records of the base absent from the fresh document.
    if let Some(b) = base {
        let bkeys = if local.is_some() { &lkeys } else { &skeys };
        for (k, br) in bkeys {
            if find(&fkeys, k).is_some() {
                continue;
            }
            // A unit stashed without a digest had no Virtuoso form: it stays Schemify's.
            let unexported = |s: &Document<'_>| {
                find(&skeys, k).is_some_and(|sr| s.find_attr(sr, VIMG).is_none())
            };
            let owned = if local.is_some() {
                schemify_owned(k.0, fk, maestro) || stash.is_some_and(unexported)
            } else {
                b.find_attr(*br, VIMG).is_none()
            };
            if owned {
                kept.push((k.clone(), b, *br, false));
            }
        }
    }
    // Ranks 4 and 5 keep file order (format.md §4): units the base has keep the base's
    // order, fresh-only units follow (the writer sorts ranks 3 and 6 anyway).
    if base.is_some() {
        let bkeys = if local.is_some() { &lkeys } else { &skeys };
        kept.sort_by_key(|e| {
            bkeys
                .iter()
                .position(|(x, _)| *x == e.0)
                .unwrap_or(usize::MAX)
        });
    }
    // Subjects still alive?
    let ids: BTreeSet<String> = kept
        .iter()
        .filter(|(k, ..)| {
            k.0.rank() == 3 && k.0 != RecordKind::Waive && k.0 != RecordKind::Constraint
        })
        .map(|(k, ..)| k.2.clone())
        .collect();
    let impls: BTreeSet<String> = (1..out.record_count())
        .filter(|&r| out.record_kind(r) == RecordKind::Impl)
        .filter_map(|r| out.field(r, 0).map(str::to_owned))
        .collect();
    for (k, src, r, enrich) in kept {
        let alive = match k.0 {
            RecordKind::Waive => ids.contains(src.field(r, 1).unwrap_or("")),
            RecordKind::Constraint => src
                .attrs(r)
                .filter(|a| src.attr_key(*a) == "member")
                .all(|a| src.attr_value(a, 0).is_some_and(|m| ids.contains(m))),
            RecordKind::Body => impls.contains(src.field(r, 0).unwrap_or("")),
            _ => true,
        };
        if !alive {
            notes.dropped(
                "stale-schemify-data",
                cell,
                format!("{} {}: its subject is gone", src.record_word(r), k.2),
            );
            continue;
        }
        if enrich && k.0 == RecordKind::Shape {
            // Schemify-only attributes of a shape the Virtuoso side redrew.
            let from = local.or(stash).and_then(|b| {
                let ks: Vec<(Key, u32)> = others(b).into_iter().map(|x| (key(b, x), x)).collect();
                find(&ks, &k).map(|x| (b, x))
            });
            copy_skipping(&mut out, src, r, &[VIMG]);
            if let Some((b, br)) = from {
                for a in b.attrs(br) {
                    let key = b.attr_key(a);
                    if key != VIMG
                        && src.find_attr(r, key).is_none()
                        && ["layer", "fill", "width", "closed"].contains(&key)
                    {
                        copy_attr(&mut out, b, a);
                    }
                }
            }
            continue;
        }
        copy_skipping(&mut out, src, r, &[VIMG]);
    }
    if let Some(b) = base {
        for c in b.eof_comments() {
            out.push_eof_comment(c);
        }
    }
    fix_default(&mut out);
    out
}

/// The header of a local file as the import would produce it (only `provenance` and
/// `default` are Virtuoso's): the "unchanged" test when there is no stash.
fn stripped_header(l: &Document<'_>) -> String {
    let mut d = Document::new(l.kind());
    for a in l.attrs(0) {
        let k = l.attr_key(a);
        if k == "provenance" || k == "default" {
            d.push_attr(k);
            for v in l.attr_values(a) {
                d.push_value(if k == "default" {
                    canonical_impl_name(l, v)
                } else {
                    v
                });
            }
        }
    }
    text(&d, &[0], &[])
}

/// The name the import gives an impl of `name`'s language.
fn canonical_impl_name<'a>(l: &'a Document<'_>, name: &'a str) -> &'a str {
    (1..l.record_count())
        .find(|&r| l.record_kind(r) == RecordKind::Impl && l.field(r, 0) == Some(name))
        .and_then(|r| l.field(r, 1))
        .map_or(name, |lang| match lang {
            "schematic" | "spice" | "veriloga" | "verilog" => lang,
            _ => name,
        })
}

/// The rank-1 block of a local file without the Schemify-only attributes (§7.1), impl
/// names as the import spells them: the "unchanged" test when there is no stash.
fn stripped_rank1(l: &Document<'_>) -> String {
    let mut d = Document::new(l.kind());
    for r in ranked(l, 1) {
        let kind = l.record_kind(r);
        d.begin_record(kind);
        for (i, f) in l.fields(r).enumerate() {
            d.push_field(if kind == RecordKind::Impl && i == 0 {
                canonical_impl_name(l, f)
            } else {
                f
            });
        }
        for a in l.attrs(r) {
            if !schemify_only_attr(kind, l.attr_key(a), l.attr_value(a, 0)) {
                copy_attr(&mut d, l, a);
            }
        }
    }
    text(&d, &ranked(&d, 1), &[])
}

/// A `default` naming an impl that exists whenever there is more than one (format E025).
fn fix_default(out: &mut Document<'static>) {
    let impls: Vec<String> = (1..out.record_count())
        .filter(|&r| out.record_kind(r) == RecordKind::Impl)
        .filter_map(|r| out.field(r, 0).map(str::to_owned))
        .collect();
    let def = header_attr(out, "default").map(str::to_owned);
    if impls.len() <= 1 || def.as_ref().is_some_and(|d| impls.contains(d)) {
        return;
    }
    // Rebuild with a corrected header (documents are append-only).
    let mut d = Document::new(out.kind());
    copy_header(&mut d, out, &["default"]);
    d.push_attr("default");
    d.push_value(&impls[0]);
    for r in 1..out.record_count() {
        d.copy_record(out, r);
    }
    for c in out.eof_comments() {
        d.push_eof_comment(c);
    }
    *out = d;
}

/// The fresh rank-1 block with the base's Schemify-only attributes (§7.1): pin `domain`
/// `number`; param `min max step grid show computed cached`; impl names, `model file
/// ignore` and formats for dialects Virtuoso does not produce.
fn enrich_rank1(
    out: &mut Document<'static>,
    fresh: &Document<'_>,
    base: &Document<'_>,
    cell: &str,
    notes: &mut Notes,
) {
    let find = |kind: RecordKind, name: &str| {
        ranked(base, 1)
            .into_iter()
            .find(|&r| base.record_kind(r) == kind && base.field(r, 0) == Some(name))
    };
    let mut used_impls: Vec<u32> = Vec::new();
    for r in ranked(fresh, 1) {
        let kind = fresh.record_kind(r);
        let name = fresh.field(r, 0).unwrap_or("");
        let b = match kind {
            RecordKind::Impl => ranked(base, 1).into_iter().find(|&x| {
                base.record_kind(x) == RecordKind::Impl
                    && base.field(x, 1) == fresh.field(r, 1)
                    && !used_impls.contains(&x)
            }),
            _ => find(kind, name),
        };
        let Some(b) = b else {
            out.copy_record(fresh, r);
            continue;
        };
        out.begin_record(kind);
        for (i, f) in fresh.fields(r).enumerate() {
            // An impl keeps its Schemify name.
            out.push_field(if kind == RecordKind::Impl && i == 0 {
                base.field(b, 0).unwrap_or(f)
            } else {
                f
            });
        }
        for a in fresh.attrs(r) {
            let k = fresh.attr_key(a);
            if kind == RecordKind::Impl
                && k == "format"
                && !virtuoso_format(fresh.attr_value(a, 0))
                && base.attrs(b).any(|x| {
                    base.attr_key(x) == "format" && base.attr_value(x, 0) == fresh.attr_value(a, 0)
                })
            {
                continue; // the local spice/xyce/verilog format wins (§8.1)
            }
            copy_attr(out, fresh, a);
        }
        for a in base.attrs(b) {
            let k = base.attr_key(a);
            let keep = schemify_only_attr(kind, k, base.attr_value(a, 0));
            if keep && (k == "format" || fresh.find_attr(r, k).is_none()) {
                copy_attr(out, base, a);
            }
        }
        if kind == RecordKind::Impl {
            used_impls.push(b);
        }
    }
    for r in ranked(base, 1) {
        let kind = base.record_kind(r);
        let name = base.field(r, 0).unwrap_or("");
        let gone = match kind {
            RecordKind::Param => !ranked(fresh, 1)
                .into_iter()
                .any(|x| fresh.record_kind(x) == kind && fresh.field(x, 0) == Some(name)),
            RecordKind::Impl => !used_impls.contains(&r),
            _ => false,
        };
        if !gone {
            continue;
        }
        let lang = base.field(r, 1).unwrap_or("");
        if kind == RecordKind::Impl && !matches!(lang, "schematic" | "veriloga" | "verilog") {
            // An impl Virtuoso has no view for stays (plugin languages, spice text).
            out.copy_record(base, r);
            continue;
        }
        if !base.attrs(r).is_empty() || kind == RecordKind::Impl {
            notes.dropped(
                "stale-schemify-data",
                cell,
                format!("{} {name}: gone on the Virtuoso side", base.record_word(r)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(t: &str) -> Document<'static> {
        owned_doc(&parse(t.as_bytes()).unwrap())
    }

    /// The rank-1 attributes of spec §7.3's enrichment list are Schemify's; every other key
    /// of the format's pin, param and impl schemas is the Virtuoso side's (negative space).
    #[test]
    fn schemify_only_attributes_are_the_spec_list() {
        use RecordKind as K;
        let spec: [(K, &[&str]); 3] = [
            (K::Pin, &["domain", "number"]),
            (
                K::Param,
                &[
                    "min", "max", "step", "grid", "show", "computed", "cached", "valid", "vderived",
                ],
            ),
            (K::Impl, &["model", "file", "ignore"]),
        ];
        let others: [(K, &[&str]); 3] = [
            (K::Pin, &["inherit", "alias", "vplabel"]),
            (K::Param, &["kind", "unit", "choices", "doc"]),
            (K::Impl, &["default", "lang"]),
        ];
        for (k, keys) in spec {
            for key in keys {
                assert!(schemify_only_attr(k, key, None), "{k:?} {key}");
            }
        }
        for (k, keys) in others {
            for key in keys {
                assert!(!schemify_only_attr(k, key, None), "{k:?} {key}");
            }
        }
        // Formats: spectre and cdl come from simInfo, every other dialect is Schemify's.
        for (dialect, theirs) in [("spectre", true), ("cdl", true), ("spice", false)] {
            assert_eq!(
                schemify_only_attr(K::Impl, "format", Some(dialect)),
                !theirs,
                "{dialect}"
            );
        }
        assert!(schemify_only_attr(K::Impl, "format", Some("xyce")));
        assert!(schemify_only_attr(K::Impl, "format", None));
        // A pin or param `format` is no impl format.
        assert!(!schemify_only_attr(K::Pin, "format", Some("spice")));
    }

    #[test]
    #[should_panic(expected = "is not a rank-1 record")]
    fn schemify_only_attributes_are_a_rank_1_question() {
        schemify_only_attr(RecordKind::Wire, "bus", None);
    }

    #[test]
    fn stash_then_merge_restores_the_original() {
        let orig = doc(
            "schemify 1 shbench\n  doc \"a bench\"\n\nwire aaaaaa 0 0 10 0\n\nwaive bbbbbb aaaaaa w\n  reason ok\n\nprose cccccc\n  | hello\n",
        );
        let image = doc("schemify 1 shbench\n\nwire aaaaaa 0 0 10 0\n");
        let st = stash(&orig, Some(&image)).unwrap();
        let s = parse_stash(&st, FileKind::Shbench).unwrap();
        let mut notes = Notes::default();
        let m = merge(&image, None, Some(&s), false, "c", &mut notes);
        assert_eq!(write(&m), write(&orig));
        // The waived wire deleted on the Virtuoso side: the waiver goes with a note.
        let gone = doc("schemify 1 shbench\n");
        let m = merge(&gone, None, Some(&s), false, "c", &mut notes);
        assert!(!write(&m).contains("waive"));
        assert_eq!(notes.rows.len(), 1, "{notes:?}");
        assert!(stash(&image, Some(&image)).is_none());
    }

    #[test]
    fn wires_meeting_collinear_are_stashed_even_when_reproduced() {
        // The ota pair: two vertical pieces meeting at (320, 250), a T from the right.
        let orig = doc("schemify 1 shbench

wire w00013 320 230 320 250

wire w00014 320 250 320 270

wire w00015 320 250 400 250
");
        let st = stash(&orig, Some(&orig)).expect("the collinear pair is stashed");
        assert!(st.contains("w00013") && st.contains("w00014"), "{st}");
        assert!(
            !st.contains("w00015"),
            "a T-joining wire is not collinear: {st}"
        );
        let mut notes = Notes::default();
        let s = parse_stash(&st, FileKind::Shbench).unwrap();
        let m = merge(&orig, None, Some(&s), false, "c", &mut notes);
        assert_eq!(write(&m), write(&orig));
    }
}
