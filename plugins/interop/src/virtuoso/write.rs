//! Design → `.sfyx` records (spec `interop.md` §6, §7.3).
//!
//! Spine of [`export`]: `map::resolve` → per exported cell, `cell_out` builds the dump
//! tables ([`DCell`]: CDF, symbol and schematic cellviews, text views; `remap::apply` edits
//! for re-mastered instances) → `ade::export` per bench → the image import of those
//! records (`read::image`) → `merge::stash` per cell (Schemify-only data and every record
//! the image does not reproduce) → [`records`] (sorted, refs assigned) → `sfyx::write`.

use std::collections::BTreeMap;

use schemify_format::{FileKind, Orient, RecordKind, format_coord, parse_coord};

use super::ael::{self, Name};
use super::devices;
use super::read::{
    self, Cdf, CdfParam, Conn, Cv, DCell, Dump, Inst, Label, PinFig, Prop, Shape, SimInfo, SimVal,
    Spin, Term, TextView, Wire,
};
use super::{Export, ExportOptions, ade};
use crate::design::{Cell, Design, geom, sym_pins};
use crate::map::{self, How, Mapping, Plan};
use crate::merge;
use crate::nets;
use crate::notes::Notes;
use crate::profile::{self, MILLI_PER_INCH, net_from_virtuoso, net_to_virtuoso};
use crate::remap::{self, Remaster, STEP};
use crate::sfyx::{self, Record, Value, i, pairs, s};

/// Units per inch the exporter writes (§3.1): one unit is one Schemify milli-unit.
pub const UPI: i32 = MILLI_PER_INCH as i32;

/// Half the side of an exported symbol pin rectangle (1/80 inch).
pub const PIN_HALF: i32 = 2_000;

/// Width of an exported bus wire (1/32 inch); 0 is a narrow wire.
pub const BUS_WIDTH: i32 = 5_000;

/// Height of exported wire and pin-name labels (1/16 inch).
pub const LABEL_HEIGHT: i32 = 10_000;

/// Text size whose height is one milli-unit × 10: `height = size × 10_000` (§3.5).
pub const HEIGHT_PER_SIZE_MILLI: i32 = 10;

/// Default `size` of a `stext` and of a `text` when the record has none (the editor's).
pub const STEXT_SIZE_MILLI: i32 = 300;
/// See [`STEXT_SIZE_MILLI`].
pub const TEXT_SIZE_MILLI: i32 = 400;

/// Bytes per `schemifyRecords<k>` property (§7.3).
pub const STASH_CHUNK: usize = 4000;

/// Virtuoso DBU per user unit the off-grid check assumes (§3.1).
const DBU: i32 = 160;

/// See [`super::export`].
pub fn export(
    d: &Design,
    tops: &[usize],
    m: &Mapping,
    opts: &ExportOptions,
) -> Result<Export, Vec<String>> {
    for &t in tops {
        if t >= d.cells.len() {
            return Err(vec![format!("top {t} is not a cell of the design")]);
        }
    }
    let plan = map::resolve(d, tops, m, opts.analoglib, &opts.lib_map)?;
    let mut notes = Notes::default();
    let mut dump = Dump {
        producer: opts.producer.clone(),
        ..Dump::default()
    };
    // (dump cell index, design cell index) of every written cell.
    let mut written = Vec::new();
    for ci in 0..d.cells.len() {
        if !matches!(plan.how[ci], How::Design | How::Core) {
            continue;
        }
        let c = cell_out(d, ci, &plan, m, opts.retarget, &mut notes);
        if d.cells[ci].kind() == FileKind::Shbench {
            let recs = ade::export(d, ci, &plan.lib[ci], &mut notes);
            if !recs.is_empty() {
                dump.maestro.push(recs);
            }
        }
        written.push((dump.cells.len(), ci));
        dump.cells.push(c);
    }
    // §7.3: what the Virtuoso side cannot carry rides along as stash properties.
    let first = records(&dump);
    let image = read::image(&first, &plan, d, m);
    for (k, ci) in written {
        let c = &d.cells[ci];
        let img = image.get(&(dump.cells[k].lib.clone(), dump.cells[k].name.clone()));
        if img.is_none() && plan.how[ci] == How::Core {
            continue; // the bundled core: never imported, so nothing to restore
        }
        let Some(text) = merge::stash(&c.doc, img) else {
            continue;
        };
        let view = if c.kind() == FileKind::Shprim {
            "symbol"
        } else {
            "schematic"
        };
        let cv = dump.cells[k]
            .cvs
            .iter_mut()
            .find(|v| v.view == view)
            .expect("every cell kind has its stash view");
        for (n, chunk) in chunk_text(&text, STASH_CHUNK).into_iter().enumerate() {
            cv.props.push(Prop {
                r: 0,
                name: format!("schemifyRecords{}", n + 1),
                ty: "string".into(),
                value: chunk,
            });
        }
    }
    let recs = records(&dump);
    assert!(
        recs.first().is_some_and(|r| r.tag == "sfyx")
            && recs.last().is_some_and(|r| r.tag == "end")
    );
    Ok(Export {
        sfyx: sfyx::write(&recs),
        notes,
    })
}

/// Splits `text` into chunks of at most `max` bytes on char boundaries.
pub fn chunk_text(text: &str, max: usize) -> Vec<String> {
    assert!(max >= 4, "chunks must hold any char");
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut cut = rest.len().min(max);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(rest[..cut].to_owned());
        rest = &rest[cut..];
    }
    out
}

// ---------------------------------------------------------------------------------------
// Serialization: Dump → records (§4.2 order, refs assigned after sorting)
// ---------------------------------------------------------------------------------------

/// The records of `d` in §4.2 order: header; per library (sorted), its `ref` cells then
/// its `design` cells (each sorted by name) with CDF, cellviews (sorted by view, objects
/// in kind order, each kind sorted by name, else coordinates, else text) and text views;
/// maestro records; `end`. Refs are renumbered 1… in emission order, so the same tables
/// always give the same records.
pub fn records(d: &Dump) -> Vec<Record> {
    let mut out = vec![Record::new("sfyx", vec![i(1), s(&d.producer)])];
    let mut order: Vec<&DCell> = d.cells.iter().collect();
    order.sort_by(|a, b| (&a.lib, a.design, &a.name).cmp(&(&b.lib, b.design, &b.name)));
    let mut lib: Option<&str> = None;
    for c in order {
        if lib != Some(c.lib.as_str()) {
            out.push(Record::new("lib", vec![s(&c.lib)]));
            lib = Some(&c.lib);
        }
        out.push(Record::new(
            "cell",
            vec![s(&c.name), s(if c.design { "design" } else { "ref" })],
        ));
        if let Some(cdf) = &c.cdf {
            out.push(Record::new("cdf", vec![sfyx::text(&cdf.label_set)]));
            for p in &cdf.params {
                out.push(Record::new(
                    "param",
                    vec![
                        s(&p.name),
                        s(&p.ty),
                        s(&p.def),
                        s(&p.prompt),
                        s(&p.units),
                        Value::List(p.flags.iter().map(|f| s(f)).collect()),
                        Value::List(p.choices.iter().map(|f| s(f)).collect()),
                        sfyx::text(&p.callback),
                    ],
                ));
            }
            for si in &cdf.siminfo {
                let fields = si
                    .fields
                    .iter()
                    .map(|(k, v)| {
                        let v = match v {
                            SimVal::Str(x) => s(x),
                            SimVal::List(l) => Value::List(l.iter().map(|x| s(x)).collect()),
                            SimVal::Pairs(p) => {
                                pairs(p.iter().map(|(a, b)| (a.as_str(), b.as_str())))
                            }
                        };
                        Value::List(vec![s(k), v])
                    })
                    .collect();
                out.push(Record::new(
                    "siminfo",
                    vec![s(&si.sim), Value::List(fields)],
                ));
            }
        }
        let mut cvs: Vec<&Cv> = c.cvs.iter().collect();
        cvs.sort_by(|a, b| a.view.cmp(&b.view));
        for cv in cvs {
            cv_records(cv, &mut out);
        }
        let mut tvs: Vec<&TextView> = c.textviews.iter().collect();
        tvs.sort_by(|a, b| a.view.cmp(&b.view));
        for t in tvs {
            out.push(Record::new(
                "textview",
                vec![s(&t.view), s(&t.file), sfyx::text(&t.text)],
            ));
        }
    }
    // Maestro tests by (cell, test name), as the SKILL dumper writes them (§4.2).
    let mut tests: Vec<&Vec<Record>> = d.maestro.iter().collect();
    let key = |t: &Vec<Record>| -> (String, String) {
        let r = &t[0];
        (
            r.str(2).unwrap_or("").to_owned(),
            r.str(0).unwrap_or("").to_owned(),
        )
    };
    tests.sort_by_key(|t| key(t));
    for test in tests {
        out.extend(test.iter().cloned());
    }
    out.push(Record::new("end", vec![]));
    out
}

/// Marks `cv` with `schemifyKeep` when `part` of `c` is unchanged since it was imported
/// (its fingerprint equals the header attribute import wrote, §7.5).
fn keep(c: &Cell, part: super::Part, mut cv: Cv) -> Cv {
    if c.header(super::fingerprint_attr(part)) == Some(super::fingerprint(&c.doc, part).as_str()) {
        cv.props.push(Prop {
            r: 0,
            name: "schemifyKeep".into(),
            ty: "string".into(),
            value: "1".into(),
        });
    }
    cv
}

/// A wire's points as written: a two-point wire from its smaller end (x, then y), as the
/// SKILL dumper writes it, so drawing direction never shows as a difference (§4.2).
fn canon(p: &[[i32; 2]]) -> Vec<[i32; 2]> {
    let mut v = p.to_vec();
    if v.len() == 2 && v[1] < v[0] {
        v.swap(0, 1);
    }
    v
}

fn pts(v: &[[i32; 2]]) -> Value {
    Value::List(v.iter().flat_map(|p| [i(p[0]), i(p[1])]).collect())
}

fn cv_records(cv: &Cv, out: &mut Vec<Record>) {
    out.push(Record::new(
        "cv",
        vec![s(&cv.view), s(&cv.vtype), i(cv.upi)],
    ));
    let mut terms: Vec<&Term> = cv.terms.iter().collect();
    terms.sort_by(|a, b| a.name.cmp(&b.name));
    for t in terms {
        out.push(Record::new(
            "term",
            vec![s(&t.name), s(&t.dir), s(&t.netexpr)],
        ));
    }
    // Old ref → new ref.
    let mut map: BTreeMap<u32, u32> = BTreeMap::new();
    let mut next = 1u32;
    let mut renum = |old: u32, map: &mut BTreeMap<u32, u32>| -> u32 {
        let n = next;
        next += 1;
        let prev = map.insert(old, n);
        assert!(prev.is_none(), "provisional ref {old} used twice");
        n
    };
    let mut pf: Vec<&PinFig> = cv.pinfigs.iter().collect();
    pf.sort_by(|a, b| (&a.term, a.b).cmp(&(&b.term, b.b)));
    let pf: Vec<(u32, &PinFig)> = pf.into_iter().map(|p| (renum(p.r, &mut map), p)).collect();
    let mut ins: Vec<&Inst> = cv.insts.iter().collect();
    ins.sort_by(|a, b| (&a.name, a.xy, &a.lib, &a.cell).cmp(&(&b.name, b.xy, &b.lib, &b.cell)));
    let ins: Vec<(u32, &Inst)> = ins.into_iter().map(|p| (renum(p.r, &mut map), p)).collect();
    let mut sp: Vec<&Spin> = cv.spins.iter().collect();
    sp.sort_by(|a, b| (&a.term, a.xy).cmp(&(&b.term, b.xy)));
    let sp: Vec<(u32, &Spin)> = sp.into_iter().map(|p| (renum(p.r, &mut map), p)).collect();
    let mut wi: Vec<&Wire> = cv.wires.iter().collect();
    wi.sort_by_key(|w| (canon(&w.pts), w.width));
    let wi: Vec<(u32, &Wire)> = wi.into_iter().map(|p| (renum(p.r, &mut map), p)).collect();
    let mut la: Vec<&Label> = cv.labels.iter().collect();
    la.sort_by(|a, b| {
        (a.xy, &a.text, &a.layer, &a.purpose, &a.kind, &a.orient)
            .cmp(&(b.xy, &b.text, &b.layer, &b.purpose, &b.kind, &b.orient))
    });
    let la: Vec<(u32, &Label)> = la.into_iter().map(|p| (renum(p.r, &mut map), p)).collect();
    let mut sh: Vec<&Shape> = cv.shapes.iter().collect();
    sh.sort_by(|a, b| {
        (&a.coords, &a.kind, &a.layer, &a.purpose).cmp(&(&b.coords, &b.kind, &b.layer, &b.purpose))
    });
    let sh: Vec<(u32, &Shape)> = sh.into_iter().map(|p| (renum(p.r, &mut map), p)).collect();
    let rr = |old: u32| -> u32 {
        if old == 0 {
            0
        } else {
            *map.get(&old)
                .unwrap_or_else(|| panic!("unknown provisional ref {old}"))
        }
    };
    for (r, p) in pf {
        out.push(Record::new(
            "pinfig",
            vec![
                i(r as i32),
                s(&p.term),
                i(p.b[0]),
                i(p.b[1]),
                i(p.b[2]),
                i(p.b[3]),
            ],
        ));
    }
    for (r, x) in ins {
        out.push(Record::new(
            "inst",
            vec![
                i(r as i32),
                s(&x.name),
                s(&x.lib),
                s(&x.cell),
                s(&x.view),
                i(x.xy[0]),
                i(x.xy[1]),
                s(&x.orient),
            ],
        ));
    }
    for (r, x) in sp {
        out.push(Record::new(
            "spin",
            vec![
                i(r as i32),
                s(&x.term),
                s(&x.dir),
                s(&x.lib),
                s(&x.cell),
                s(&x.view),
                i(x.xy[0]),
                i(x.xy[1]),
                s(&x.orient),
            ],
        ));
    }
    for (r, w) in wi {
        out.push(Record::new(
            "wire",
            vec![i(r as i32), pts(&canon(&w.pts)), i(w.width)],
        ));
    }
    for (r, l) in la {
        out.push(Record::new(
            "label",
            vec![
                i(r as i32),
                i(rr(l.parent) as i32),
                s(&l.text),
                i(l.xy[0]),
                i(l.xy[1]),
                s(&l.orient),
                s(&l.justify),
                s(&l.font),
                i(l.height),
                s(&l.kind),
                s(&l.layer),
                s(&l.purpose),
            ],
        ));
    }
    for (r, x) in sh {
        out.push(Record::new(
            "shape",
            vec![
                i(r as i32),
                s(&x.kind),
                s(&x.layer),
                s(&x.purpose),
                Value::List(x.coords.iter().map(|v| i(*v)).collect()),
                i(x.width),
            ],
        ));
    }
    let mut props: Vec<(u32, &Prop)> = cv.props.iter().map(|p| (rr(p.r), p)).collect();
    props.sort_by(|a, b| (a.0, &a.1.name).cmp(&(b.0, &b.1.name)));
    for (r, p) in props {
        out.push(Record::new(
            "prop",
            vec![i(r as i32), s(&p.name), s(&p.ty), s(&p.value)],
        ));
    }
    let mut conns: Vec<(u32, &Conn)> = cv.conns.iter().map(|c| (rr(c.r), c)).collect();
    conns.sort_by(|a, b| (a.0, &a.1.term).cmp(&(b.0, &b.1.term)));
    for (r, c) in conns {
        out.push(Record::new(
            "conn",
            vec![i(r as i32), s(&c.term), s(&c.net)],
        ));
    }
    for rec in &cv.intents {
        let mut rec = rec.clone();
        if let Some(Value::Int(old)) = rec.fields.first().cloned() {
            rec.fields[0] = i(rr(old as u32) as i32);
        }
        out.push(rec);
    }
}

// ---------------------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------------------

/// Builds the dump tables of Design cell `ci`.
fn cell_out(
    d: &Design,
    ci: usize,
    plan: &Plan,
    m: &Mapping,
    retarget: bool,
    notes: &mut Notes,
) -> DCell {
    let c = &d.cells[ci];
    let mut out = DCell {
        lib: plan.lib[ci].clone(),
        name: plan.cell[ci].clone(),
        design: true,
        ..DCell::default()
    };
    let core = plan.how[ci] == How::Core;
    let mut ids = Ids::default();
    match c.kind() {
        FileKind::Shprim | FileKind::Shcomp => {
            let cdf = cdf_out(c, core, notes);
            // A component without parameters whose pins are in Virtuoso's default order
            // (sorted names, §6.3) needs no CDF: none is written, so a cell that had none
            // in Virtuoso gets none back.
            let names: Vec<String> = {
                let p = sym_pins(c);
                p.name
                    .into_iter()
                    .zip(p.alias)
                    .filter(|(_, a)| a.is_none())
                    .map(|(n, _)| vname(&n))
                    .collect()
            };
            let sorted = names.windows(2).all(|w| w[0].as_bytes() < w[1].as_bytes());
            let bare = c.kind() == FileKind::Shcomp && cdf.params.is_empty() && sorted;
            out.cdf = (!bare).then_some(cdf);
            out.cvs
                .push(keep(c, super::Part::Symbol, symbol_out(c, &mut ids, notes)));
            if c.kind() == FileKind::Shcomp {
                let sch = schematic_out(d, ci, plan, m, retarget, &mut ids, notes);
                out.cvs.push(keep(c, super::Part::Schematic, sch));
            }
            out.textviews = textviews_out(c, notes);
        }
        FileKind::Shbench => {
            let sch = schematic_out(d, ci, plan, m, retarget, &mut ids, notes);
            out.cvs.push(keep(c, super::Part::Schematic, sch));
        }
        k => unreachable!("design cells are cell files, got {k:?}"),
    }
    assert!(!out.cvs.is_empty());
    out
}

/// Provisional refs of one cell's cellviews.
#[derive(Default)]
struct Ids {
    next: u32,
}

impl Ids {
    fn take(&mut self) -> u32 {
        self.next += 1;
        self.next
    }
}

/// Converts a Schemify point to export units, reporting it when off Virtuoso's grid.
fn conv(p: [i32; 2], what: &str, cell: &Cell, notes: &mut Notes) -> [i32; 2] {
    if !profile::on_dbu_grid(p[0], DBU) || !profile::on_dbu_grid(p[1], DBU) {
        notes.add(
            "off-grid",
            &cell.path,
            format!("{what} at ({}, {})", milli(p[0]), milli(p[1])),
        );
    }
    profile::point_to_virtuoso(p).unwrap_or([p[0], i32::MAX])
}

fn milli(v: i32) -> String {
    let mut s = String::new();
    format_coord(v, &mut s);
    s
}

/// Coordinates of a Schemify record's fields `from..` as milli-units.
fn coords(c: &Cell, r: u32, from: usize) -> Vec<i32> {
    c.doc
        .fields(r)
        .skip(from)
        .map(|t| parse_coord(t).unwrap_or(0))
        .collect()
}

fn vdir(dir: &str) -> &'static str {
    match dir {
        "in" => "input",
        "out" => "output",
        _ => "inputOutput",
    }
}

/// The Virtuoso spelling of a pin or terminal name.
pub fn vname(pin: &str) -> String {
    net_to_virtuoso(pin, false).0
}

// ---------------------------------------------------------------------------------------
// CDF and simInfo (§6.3)
// ---------------------------------------------------------------------------------------

/// Classifies identifiers of cell `c`'s own defaults: its params (`iPar`), its vars
/// (inlined).
fn own_scope(c: &Cell) -> impl FnMut(&str) -> Name + '_ {
    move |n: &str| {
        if c.records(RecordKind::Param).any(|r| c.field(r, 0) == n) {
            Name::OwnParam
        } else if let Some(r) = c.records(RecordKind::Var).find(|r| c.field(*r, 0) == n) {
            Name::CellVar(c.field(r, 1).to_owned())
        } else {
            Name::Unknown
        }
    }
}

fn cdf_out(c: &Cell, core: bool, notes: &mut Notes) -> Cdf {
    let mut cdf = Cdf::default();
    let src = if core {
        devices::by_core(&c.name).and_then(|dv| dv.source)
    } else {
        None
    };
    for r in c.records(RecordKind::Param) {
        let name = c.field(r, 0);
        if let (Some(current), "value") = (src, name) {
            cdf.params.extend(source_params(c.field(r, 1), current));
            continue;
        }
        cdf.params.push(param_out(c, r, notes));
    }
    // Inherited pins: netSet params carry the instance overrides (§6.2).
    let pins = sym_pins(c);
    for k in 0..pins.name.len() {
        if let (Some(net), None) = (&pins.inherit[k], &pins.alias[k]) {
            cdf.params.push(CdfParam {
                name: vname(&pins.name[k]),
                ty: "netSet".into(),
                def: net_to_virtuoso(net, true).0,
                prompt: pins.name[k].clone(),
                flags: vec!["storeDefault".into()],
                ..CdfParam::default()
            });
        }
    }
    let order: Vec<String> = pins
        .name
        .iter()
        .zip(&pins.alias)
        .filter(|(_, a)| a.is_none())
        .map(|(n, _)| vname(n))
        .collect();
    let inst_params: Vec<String> = c
        .records(RecordKind::Param)
        .filter(|r| c.attr(*r, "computed").is_none())
        .map(|r| c.field(r, 0).to_owned())
        .collect();
    match c.kind() {
        FileKind::Shcomp => {
            cdf.siminfo.push(SimInfo {
                sim: "spectre".into(),
                fields: vec![
                    ("termOrder".into(), SimVal::List(order.clone())),
                    ("instParameters".into(), SimVal::List(inst_params.clone())),
                ],
            });
            cdf.siminfo.push(SimInfo {
                sim: "auCdl".into(),
                fields: vec![
                    (
                        "netlistProcedure".into(),
                        SimVal::Str("ansCdlSubcktCall".into()),
                    ),
                    ("termOrder".into(), SimVal::List(order)),
                    ("instParameters".into(), SimVal::List(inst_params)),
                    ("namePrefix".into(), SimVal::Str("X".into())),
                ],
            });
        }
        FileKind::Shprim if core => {
            if let Some(dv) = devices::by_core(&c.name).filter(|dv| !dv.spectre.is_empty()) {
                let mut f = Vec::new();
                if dv.spectre == "{model}" {
                    f.push(("otherParameters".into(), SimVal::List(vec!["model".into()])));
                } else {
                    f.push(("componentName".into(), SimVal::Str(dv.spectre.into())));
                }
                f.push(("termOrder".into(), SimVal::List(order)));
                let have: Vec<&str> = cdf.params.iter().map(|p| p.name.as_str()).collect();
                let used: Vec<&(&str, &str)> = dv
                    .spectre_params
                    .iter()
                    .filter(|(_, p)| have.contains(p))
                    .collect();
                f.push((
                    "instParameters".into(),
                    SimVal::List(used.iter().map(|(s, _)| (*s).into()).collect()),
                ));
                let pm: Vec<(String, String)> = used
                    .iter()
                    .filter(|(s, p)| s != p)
                    .map(|(s, p)| ((*s).into(), (*p).into()))
                    .collect();
                if !pm.is_empty() {
                    f.push(("propMapping".into(), SimVal::Pairs(pm)));
                }
                cdf.siminfo.push(SimInfo {
                    sim: "spectre".into(),
                    fields: f,
                });
            } else {
                notes.review(
                    "format",
                    &c.path,
                    "no spectre simInfo for this core cell (§6.5)",
                );
            }
        }
        FileKind::Shprim => {
            let impl_r = c.records(RecordKind::Impl).next();
            let fmt = |dialect: &str| {
                impl_r.and_then(|r| {
                    c.doc
                        .attrs(r)
                        .filter(|a| {
                            c.doc.attr_key(*a) == "format"
                                && c.doc.attr_value(*a, 0) == Some(dialect)
                        })
                        .find_map(|a| c.doc.attr_value(a, 1))
                })
            };
            let params: Vec<(String, String)> = c
                .records(RecordKind::Param)
                .map(|r| {
                    (
                        c.field(r, 0).to_owned(),
                        c.attr(r, "kind").unwrap_or("number").to_owned(),
                    )
                })
                .collect();
            let pin_names: Vec<String> = pins
                .name
                .iter()
                .zip(&pins.alias)
                .filter(|(_, a)| a.is_none())
                .map(|(n, _)| n.clone())
                .collect();
            match fmt("spectre").map(|t| spectre_siminfo(t, &pin_names, &params)) {
                Some(Some(si)) => cdf.siminfo.push(si),
                other => {
                    if other.is_some() {
                        notes.review(
                            "format",
                            &c.path,
                            "format spectre is not `{name} ({pins}) master k={p}…`",
                        );
                    }
                    cdf.siminfo.push(SimInfo {
                        sim: "spectre".into(),
                        fields: vec![("termOrder".into(), SimVal::List(order.clone()))],
                    });
                }
            }
            if let Some(t) = fmt("cdl") {
                match cdl_siminfo(t, &pin_names, &params) {
                    Some(si) => cdf.siminfo.push(si),
                    None => notes.review(
                        "format",
                        &c.path,
                        "format cdl does not match a CDL netlist procedure",
                    ),
                }
            }
        }
        _ => {}
    }
    assert!(
        cdf.params
            .iter()
            .all(|p| read::CDF_TYPES.contains(&p.ty.as_str()))
    );
    cdf
}

/// The CDF params of a core source's `value` (§6.6): `srcType` and every waveform param,
/// defaults from parsing the core default.
fn source_params(default: &str, current: bool) -> Vec<CdfParam> {
    let parsed = devices::parse_source(default, current);
    let mut out = Vec::new();
    for n in devices::source_cdf_params(current) {
        if n == "srcType" {
            out.push(CdfParam {
                name: n.into(),
                ty: "cyclic".into(),
                def: devices::WAVEFORMS[parsed.as_ref().map_or(0, |p| p.wave)]
                    .1
                    .into(),
                prompt: n.into(),
                choices: devices::WAVEFORMS.iter().map(|w| w.1.to_owned()).collect(),
                ..CdfParam::default()
            });
            continue;
        }
        let def = parsed
            .as_ref()
            .and_then(|p| p.params.iter().find(|x| x.0 == n))
            .map_or(String::new(), |x| x.1.clone());
        out.push(CdfParam {
            name: n.into(),
            ty: if n == "tvpairs" {
                "int".into()
            } else {
                "string".into()
            },
            def: if n == "tvpairs" && def.is_empty() {
                "0".into()
            } else {
                def
            },
            prompt: n.into(),
            flags: if n == "tvpairs" {
                vec![]
            } else {
                vec!["parseAsNumber".into(), "parseAsCEL".into()]
            },
            ..CdfParam::default()
        });
    }
    out
}

/// One Schemify param as a CDF parameter (§6.3 table).
fn param_out(c: &Cell, r: u32, notes: &mut Notes) -> CdfParam {
    let name = c.field(r, 0);
    let default = c.field(r, 1);
    let kind = c.attr(r, "kind").unwrap_or("number");
    let mut p = CdfParam {
        name: name.to_owned(),
        prompt: c.attr(r, "doc").unwrap_or(name).to_owned(),
        units: c
            .attr(r, "unit")
            .and_then(ael::unit_to_cdf)
            .unwrap_or("")
            .to_owned(),
        ..CdfParam::default()
    };
    let num = |e: &str, notes: &mut Notes| match ael::to_ael(e, own_scope(c)) {
        Ok(t) => {
            if let Some(why) = t.review {
                notes.review(why, &c.path, format!("param {name}: {e}"));
            }
            t.text
        }
        Err(why) => {
            notes.review(
                why,
                &c.path,
                format!("param {name}: `{e}` has no AEL form; CDF default left empty"),
            );
            String::new()
        }
    };
    if let Some(comp) = c.attr(r, "computed") {
        notes.review(
            "computed",
            &c.path,
            format!("param {name} is computed: exported as a CEL default"),
        );
        p.ty = "string".into();
        p.flags = vec!["parseAsNumber".into(), "parseAsCEL".into()];
        p.def = num(comp, notes);
        return p;
    }
    match kind {
        "int" => {
            p.ty = "int".into();
            p.def = if default.parse::<i64>().is_ok() {
                default.to_owned()
            } else {
                notes.review(
                    "expression",
                    &c.path,
                    format!("param {name}: int default `{default}` is not a literal"),
                );
                "0".into()
            };
        }
        "bool" => {
            p.ty = "boolean".into();
            p.def = ael::bool_to_cdf(default)
                .unwrap_or_else(|| {
                    notes.review(
                        "expression",
                        &c.path,
                        format!("param {name}: bool default `{default}` is not a literal"),
                    );
                    "nil"
                })
                .into();
        }
        "choice" => {
            p.ty = "cyclic".into();
            p.choices = c
                .doc
                .find_attr(r, "choices")
                .map(|a| c.doc.attr_values(a).map(str::to_owned).collect())
                .unwrap_or_default();
            p.def = literal(default, name, c, notes);
        }
        "string" => {
            p.ty = "string".into();
            p.def = literal(default, name, c, notes);
        }
        _ => {
            p.ty = "string".into();
            p.flags = vec!["parseAsNumber".into(), "parseAsCEL".into()];
            p.def = num(default, notes);
        }
    }
    p
}

fn literal(t: &str, what: &str, c: &Cell, notes: &mut Notes) -> String {
    ael::template_to_literal(t).unwrap_or_else(|| {
        notes.review(
            "template",
            &c.path,
            format!("{what}: template `{t}` sent as text"),
        );
        t.to_owned()
    })
}

/// The pattern a string param takes in an imported spectre format: ` k=<c>` only when set.
pub fn optional_param(k: &str, c: &str) -> String {
    format!("{{{c} != \"\" ? \" {k}=\" + {c} : \"\"}}")
}

/// Splits a template into literal runs and `{hole}`s (`{{`/`}}` kept as text). `None` on
/// unbalanced braces.
fn template_items(t: &str) -> Option<Vec<(bool, String)>> {
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut lit = String::new();
    let b: Vec<char> = t.chars().collect();
    let mut k = 0;
    while k < b.len() {
        match b[k] {
            '{' if b.get(k + 1) == Some(&'{') => {
                lit.push('{');
                k += 2;
            }
            '}' if b.get(k + 1) == Some(&'}') => {
                lit.push('}');
                k += 2;
            }
            '{' => {
                if !lit.is_empty() {
                    out.push((false, std::mem::take(&mut lit)));
                }
                let mut depth = 0;
                let mut q = false;
                let start = k + 1;
                let mut end = None;
                for (j, &ch) in b.iter().enumerate().skip(start) {
                    match ch {
                        '"' => q = !q,
                        '{' if !q => depth += 1,
                        '}' if !q && depth == 0 => {
                            end = Some(j);
                            break;
                        }
                        '}' if !q => depth -= 1,
                        _ => {}
                    }
                }
                let end = end?;
                out.push((true, b[start..end].iter().collect()));
                k = end + 1;
            }
            '}' => return None,
            c => {
                lit.push(c);
                k += 1;
            }
        }
    }
    if !lit.is_empty() {
        out.push((false, lit));
    }
    Some(out)
}

/// The `k={c}` tail items of a format: `( k, c )` per item, or `None` when the tail has
/// another shape. `sep` is the separator a plain item starts with.
fn param_tail(
    items: &[(bool, String)],
    params: &[(String, String)],
) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut k = 0;
    let is_param = |c: &str| params.iter().any(|p| p.0 == c);
    while k < items.len() {
        match &items[k] {
            (false, lit)
                if lit.starts_with(' ')
                    && lit.ends_with('=')
                    && lit.len() > 2
                    && !lit[1..lit.len() - 1].contains(char::is_whitespace) =>
            {
                let key = lit[1..lit.len() - 1].to_owned();
                let (true, c) = items.get(k + 1)? else {
                    return None;
                };
                if !is_param(c) {
                    return None;
                }
                out.push((key, c.clone()));
                k += 2;
            }
            (true, hole) => {
                // `{c != "" ? " k=" + c : ""}`
                let c = hole.split_once(" != \"\" ? \" ")?.0.to_owned();
                let key = hole
                    .split_once(" != \"\" ? \" ")?
                    .1
                    .split_once("=\" + ")?
                    .0
                    .to_owned();
                if !is_param(&c) || optional_param(&key, &c) != format!("{{{hole}}}") {
                    return None;
                }
                out.push((key, c));
                k += 1;
            }
            _ => return None,
        }
    }
    Some(out)
}

/// The pins of a template's pin list: `{pins}` → all pins, else each `{pin}` in order
/// (they must be a permutation of `pins`).
fn pin_list(items: &[(bool, String)], pins: &[String]) -> Option<Vec<String>> {
    if let [(true, h)] = items
        && h == "pins"
    {
        return Some(pins.to_vec());
    }
    let mut out = Vec::new();
    for (k, it) in items.iter().enumerate() {
        match it {
            (true, h) if pins.contains(h) => out.push(h.clone()),
            (false, l) if l == " " && k % 2 == 1 => {}
            _ => return None,
        }
    }
    let mut a = out.clone();
    a.sort();
    let mut b = pins.to_vec();
    b.sort();
    (a == b).then_some(out)
}

/// Reverses the §6.3 spectre rule: `{name} ({pins…}) master k={c}…` → spectre simInfo.
pub fn spectre_siminfo(t: &str, pins: &[String], params: &[(String, String)]) -> Option<SimInfo> {
    let items = template_items(t)?;
    let [(true, n), (false, open), rest @ ..] = items.as_slice() else {
        return None;
    };
    if n != "name" || open != " (" {
        return None;
    }
    let close = rest.iter().position(|it| !it.0 && it.1.starts_with(") "))?;
    let order = pin_list(&rest[..close], pins)?;
    let after = &rest[close].1[2..];
    let mut tail: Vec<(bool, String)> = rest[close + 1..].to_vec();
    let mut f = Vec::new();
    if after.is_empty() {
        // master is a hole: only `{model}`
        let (true, h) = tail.first()? else {
            return None;
        };
        if h != "model" {
            return None;
        }
        tail.remove(0);
        f.push(("otherParameters".into(), SimVal::List(vec!["model".into()])));
    } else {
        // `master k=` … : the literal holds the master and maybe the first ` k=`.
        let (master, first) = match after.split_once(' ') {
            Some((m, k)) => (m, Some(format!(" {k}"))),
            None => (after, None),
        };
        if master.is_empty() || master.contains(['{', '}']) {
            return None;
        }
        if let Some(k) = first {
            tail.insert(0, (false, k));
        }
        f.push(("componentName".into(), SimVal::Str(master.into())));
    }
    let kv = param_tail(&tail, params)?;
    f.push((
        "termOrder".into(),
        SimVal::List(order.iter().map(|p| vname(p)).collect()),
    ));
    f.push((
        "instParameters".into(),
        SimVal::List(kv.iter().map(|x| x.0.clone()).collect()),
    ));
    let pm: Vec<(String, String)> = kv.iter().filter(|x| x.0 != x.1).cloned().collect();
    if !pm.is_empty() {
        f.push(("propMapping".into(), SimVal::Pairs(pm)));
    }
    Some(SimInfo {
        sim: "spectre".into(),
        fields: f,
    })
}

/// Reverses the §6.3 auCdl rules (`X{name} {pins} / comp k={c}…`, or `P{name} {pins}
/// model K={c}… [$[M]]`).
pub fn cdl_siminfo(t: &str, pins: &[String], params: &[(String, String)]) -> Option<SimInfo> {
    let items = template_items(t)?;
    let [(false, prefix), (true, n), (false, sp), rest @ ..] = items.as_slice() else {
        return None;
    };
    if n != "name" || sp != " " || prefix.is_empty() || prefix.contains(' ') {
        return None;
    }
    // Pins run until the first literal that is not a single space.
    let end = rest
        .iter()
        .position(|it| !it.0 && it.1 != " ")
        .unwrap_or(rest.len());
    let order = pin_list(&rest[..end], pins)?;
    let lit = rest.get(end).map_or("", |x| x.1.as_str());
    let mut tail: Vec<(bool, String)> = rest.get(end + 1..).unwrap_or(&[]).to_vec();
    let mut f = Vec::new();
    let split_first = |s: &str, tail: &mut Vec<(bool, String)>| -> Option<String> {
        let s = s.strip_prefix(' ')?;
        match s.split_once(' ') {
            Some((m, k)) => {
                tail.insert(0, (false, format!(" {k}")));
                Some(m.to_owned())
            }
            None => Some(s.to_owned()),
        }
    };
    if let Some(after) = lit.strip_prefix(" / ") {
        let comp = split_first(&format!(" {after}"), &mut tail)?;
        f.push((
            "netlistProcedure".into(),
            SimVal::Str("ansCdlSubcktCall".into()),
        ));
        f.push(("componentName".into(), SimVal::Str(comp)));
        f.push(("namePrefix".into(), SimVal::Str(prefix.clone())));
    } else {
        f.push((
            "netlistProcedure".into(),
            SimVal::Str("ansCdlCompPrim".into()),
        ));
        f.push(("namePrefix".into(), SimVal::Str(prefix.clone())));
        // `$[M]` at the end (R C L D).
        let mut model_tag = None;
        if let Some((false, last)) = tail.last().cloned()
            && let Some(m) = last.strip_prefix(" $[").and_then(|x| x.strip_suffix(']'))
        {
            model_tag = Some(m.to_owned());
            tail.pop();
        }
        if lit.is_empty() {
            let (true, h) = tail.first()? else {
                return None;
            };
            if h != "model" {
                return None;
            }
            tail.remove(0);
            f.push(("otherParameters".into(), SimVal::List(vec!["model".into()])));
        } else if lit.starts_with(' ') && !lit.contains('=') && model_tag.is_none() {
            let m = split_first(lit, &mut tail)?;
            f.push(("modelName".into(), SimVal::Str(m)));
        } else {
            tail.insert(0, (false, lit.to_owned()));
        }
        if let Some(m) = model_tag {
            f.push(("modelName".into(), SimVal::Str(m)));
        }
    }
    let kv = param_tail(&tail, params)?;
    f.push((
        "termOrder".into(),
        SimVal::List(order.iter().map(|p| vname(p)).collect()),
    ));
    f.push((
        "instParameters".into(),
        SimVal::List(kv.iter().map(|x| x.0.clone()).collect()),
    ));
    let pm: Vec<(String, String)> = kv.iter().filter(|x| x.0 != x.1).cloned().collect();
    if !pm.is_empty() {
        f.push(("propMapping".into(), SimVal::Pairs(pm)));
    }
    Some(SimInfo {
        sim: "auCdl".into(),
        fields: f,
    })
}

fn textviews_out(c: &Cell, notes: &mut Notes) -> Vec<TextView> {
    let mut out = Vec::new();
    for r in c.records(RecordKind::Impl) {
        let name = c.field(r, 0);
        let (view, file) = match c.field(r, 1) {
            "veriloga" => ("veriloga", "veriloga.va"),
            "verilog" => ("functional", "verilog.v"),
            _ => continue,
        };
        let body: Option<Vec<&str>> = c
            .records(RecordKind::Body)
            .find(|b| c.field(*b, 0) == name)
            .map(|b| c.doc.verbatim(b).collect());
        match body {
            Some(lines) => out.push(TextView {
                view: view.into(),
                file: file.into(),
                text: lines.join("\n") + "\n",
            }),
            None => notes.review(
                "format",
                &c.path,
                format!("impl {name}: an external `file` is not sent as a text view"),
            ),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Symbol (§6.2 symbol rows, §3.5 labels)
// ---------------------------------------------------------------------------------------

fn symbol_out(c: &Cell, ids: &mut Ids, notes: &mut Notes) -> Cv {
    let mut cv = Cv {
        view: "symbol".into(),
        vtype: "schematicSymbol".into(),
        upi: UPI,
        ..Cv::default()
    };
    let mut bbox: Option<[i32; 4]> = None;
    let grow = |p: [i32; 2], bbox: &mut Option<[i32; 4]>| {
        let b = bbox.get_or_insert([p[0], p[1], p[0], p[1]]);
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    };
    let pins = sym_pins(c);
    for k in 0..pins.name.len() {
        let at = conv(pins.at[k], &format!("pin {}", pins.name[k]), c, notes);
        let term = vname(pins.alias[k].as_deref().unwrap_or(&pins.name[k]));
        if pins.alias[k].is_none() {
            let netexpr = pins.inherit[k].as_ref().map_or(String::new(), |net| {
                format!("[@{}:%:{}]", term, net_to_virtuoso(net, true).0)
            });
            cv.terms.push(Term {
                name: term.clone(),
                dir: vdir(&pins.dir[k]).into(),
                netexpr,
            });
        }
        let r = ids.take();
        cv.pinfigs.push(PinFig {
            r,
            term: term.clone(),
            b: [
                at[0] - PIN_HALF,
                at[1] - PIN_HALF,
                at[0] + PIN_HALF,
                at[1] + PIN_HALF,
            ],
        });
        grow([at[0] - PIN_HALF, at[1] - PIN_HALF], &mut bbox);
        grow([at[0] + PIN_HALF, at[1] + PIN_HALF], &mut bbox);
        if pins.alias[k].is_some() {
            cv.props.push(Prop {
                r,
                name: "schemifyPin".into(),
                ty: "string".into(),
                value: pins.name[k].clone(),
            });
        }
        cv.labels.push(Label {
            r: ids.take(),
            parent: r,
            text: term,
            xy: at,
            orient: "R0".into(),
            justify: "centerLeft".into(),
            font: "stick".into(),
            height: LABEL_HEIGHT / 2,
            kind: "normal".into(),
            layer: "pin".into(),
            purpose: "label".into(),
        });
    }
    for r in 1..c.doc.record_count() {
        let kind = c.doc.record_kind(r);
        if kind.rank() != 2 {
            continue;
        }
        if kind == RecordKind::Stext {
            if let Some(l) = stext_out(c, r, ids, notes) {
                grow(l.xy, &mut bbox);
                cv.labels.push(l);
            }
            continue;
        }
        let fill = c.attr(r, "fill") == Some("true");
        for sh in graphic_out(
            c,
            r,
            kind,
            1,
            "device",
            if fill { "drawing1" } else { "drawing" },
            ids,
            notes,
        ) {
            for p in sh
                .coords
                .chunks(2)
                .take(if sh.kind == "arc" { 2 } else { usize::MAX })
            {
                if p.len() == 2 {
                    grow([p[0], p[1]], &mut bbox);
                }
            }
            cv.shapes.push(sh);
        }
    }
    if let Some(b) = bbox {
        cv.shapes.push(Shape {
            r: ids.take(),
            kind: "rect".into(),
            layer: "instance".into(),
            purpose: "drawing".into(),
            coords: b.to_vec(),
            width: 0,
        });
    }
    cv
}

/// Virtuoso justification of a Schemify text anchor (`hcenter`/`vcenter` flags).
pub fn justify(hcenter: bool, vcenter: bool) -> &'static str {
    match (hcenter, vcenter) {
        (false, false) => "upperLeft",
        (true, false) => "upperCenter",
        (false, true) => "centerLeft",
        (true, true) => "centerCenter",
    }
}

/// `height = size × 10_000` milli (§3.5); `size` absent → `default_milli`.
fn height(c: &Cell, r: u32, default_milli: i32) -> i32 {
    let s = c
        .attr(r, "size")
        .and_then(|t| parse_coord(t).ok())
        .unwrap_or(default_milli);
    s.saturating_mul(HEIGHT_PER_SIZE_MILLI)
}

/// Escapes NLP-reserved characters of literal text.
pub fn nlp_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\\' | '[' | ']' | ':' | '%') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn stext_out(c: &Cell, r: u32, ids: &mut Ids, notes: &mut Notes) -> Option<Label> {
    let at = conv(
        [c.coord(r, 0).unwrap_or(0), c.coord(r, 1).unwrap_or(0)],
        "stext",
        c,
        notes,
    );
    let o = Orient::parse(c.field(r, 2)).unwrap_or(Orient::R0);
    let t = c.field(r, 3);
    let items = template_items(t)?;
    let params: Vec<&str> = c
        .records(RecordKind::Param)
        .map(|p| c.field(p, 0))
        .collect();
    let (text, kind, layer, purpose) = match items.as_slice() {
        [(true, h)] if h == "name" => ("[@instanceName]".to_owned(), "NLP", "instance", "label"),
        [(true, h)] if h == "cell" => ("[@partName]".to_owned(), "NLP", "device", "label"),
        [] => return None,
        _ if items.iter().all(|x| !x.0) => (
            items.iter().map(|x| x.1.as_str()).collect(),
            "normal",
            "annotate",
            "drawing",
        ),
        _ => {
            let holes: Vec<&(bool, String)> = items.iter().filter(|x| x.0).collect();
            let p = holes[0].1.as_str();
            if holes.len() != 1 || !params.contains(&p) {
                notes.review("template", &c.path, format!("stext `{t}` dropped"));
                return None;
            }
            let pos = items.iter().position(|x| x.0).expect("one hole");
            let pre: String = items[..pos].iter().map(|x| x.1.as_str()).collect();
            let post: String = items[pos + 1..].iter().map(|x| x.1.as_str()).collect();
            let text = if pre.is_empty() && post.is_empty() {
                format!("[@{p}]")
            } else {
                format!("[@{p}:{}%{}]", nlp_escape(&pre), nlp_escape(&post))
            };
            (text, "NLP", "annotate", "drawing")
        }
    };
    Some(Label {
        r: ids.take(),
        parent: 0,
        text,
        xy: at,
        orient: profile::orient_to_virtuoso(o).into(),
        justify: justify(
            c.doc.find_attr(r, "hcenter").is_some(),
            c.doc.find_attr(r, "vcenter").is_some(),
        )
        .into(),
        font: "stick".into(),
        height: height(c, r, STEXT_SIZE_MILLI),
        kind: kind.into(),
        layer: layer.into(),
        purpose: purpose.into(),
    })
}

/// Micro-radians of `deg` degrees, normalized into `[0, 2π)`.
pub fn urad(deg: f64) -> i32 {
    let d = deg.rem_euclid(360.0);
    (d.to_radians() * 1e6).round() as i32
}

/// A rank-2 graphic or a rank-3 shape (fields from `from`) as Virtuoso shapes.
#[allow(clippy::too_many_arguments)]
fn graphic_out(
    c: &Cell,
    r: u32,
    kind: RecordKind,
    from: usize,
    layer: &str,
    purpose: &str,
    ids: &mut Ids,
    notes: &mut Notes,
) -> Vec<Shape> {
    let form = match kind {
        RecordKind::Line => "line",
        RecordKind::Rect => "rect",
        RecordKind::Arc => "arc",
        RecordKind::Circle => "circle",
        RecordKind::Poly => "poly",
        _ => c.field(r, 1),
    };
    let first = if kind == RecordKind::Shape {
        from + 1
    } else {
        0
    };
    let raw: Vec<&str> = c.doc.fields(r).skip(first).collect();
    let v = coords(c, r, first);
    let shape = |kind: &str, coords: Vec<i32>, ids: &mut Ids| Shape {
        r: ids.take(),
        kind: kind.into(),
        layer: layer.into(),
        purpose: purpose.into(),
        coords,
        width: 0,
    };
    let p = |k: usize, notes: &mut Notes| conv([v[2 * k], v[2 * k + 1]], form, c, notes);
    match form {
        "line" if v.len() == 4 => {
            let (a, b) = (p(0, notes), p(1, notes));
            vec![shape("line", vec![a[0], a[1], b[0], b[1]], ids)]
        }
        "rect" if v.len() == 4 => {
            let (a, b) = (p(0, notes), p(1, notes));
            vec![shape(
                "rect",
                vec![
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[0].max(b[0]),
                    a[1].max(b[1]),
                ],
                ids,
            )]
        }
        "circle" if v.len() == 3 => {
            let o = conv([v[0], v[1]], form, c, notes);
            vec![shape(
                "ellipse",
                vec![o[0] - v[2], o[1] - v[2], o[0] + v[2], o[1] + v[2]],
                ids,
            )]
        }
        "arc" if v.len() == 5 => {
            let o = conv([v[0], v[1]], form, c, notes);
            let rad = v[2];
            let start: f64 = raw[3].parse().unwrap_or(0.0);
            let mut sweep: f64 = raw[4].parse().unwrap_or(0.0);
            let mut start = start;
            if sweep < 0.0 {
                start += sweep;
                sweep = -sweep;
            }
            let bx = vec![o[0] - rad, o[1] - rad, o[0] + rad, o[1] + rad];
            if sweep >= 360.0 {
                return vec![shape("ellipse", bx, ids)];
            }
            // Screen-clockwise in y-down is counter-clockwise in y-up with negated angles.
            let a0 = -(start + sweep);
            let a1 = -start;
            if sweep > 180.0 {
                let mid = a0 + sweep / 2.0;
                let mut s1 = bx.clone();
                s1.extend([urad(a0), urad(mid)]);
                let mut s2 = bx;
                s2.extend([urad(mid), urad(a1)]);
                vec![shape("arc", s1, ids), shape("arc", s2, ids)]
            } else {
                let mut s1 = bx;
                s1.extend([urad(a0), urad(a1)]);
                vec![shape("arc", s1, ids)]
            }
        }
        "poly" if v.len() >= 4 && v.len().is_multiple_of(2) => {
            let closed = c.attr(r, "closed") == Some("true");
            let mut pts: Vec<[i32; 2]> = (0..v.len() / 2).map(|k| p(k, notes)).collect();
            if closed && pts.len() > 1 && pts.first() == pts.last() {
                pts.pop();
            }
            let flat: Vec<i32> = pts.iter().flat_map(|q| [q[0], q[1]]).collect();
            if closed && pts.len() >= 3 {
                vec![shape("polygon", flat, ids)]
            } else {
                vec![shape("line", flat, ids)]
            }
        }
        _ => {
            notes.dropped(
                "display-only",
                &c.path,
                format!("{form} with {} values", v.len()),
            );
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------------------
// Schematic (§6.2 schematic rows, §6.4 edits)
// ---------------------------------------------------------------------------------------

/// Identifier classes of an override written in cell `c` (the instance's parent).
fn parent_scope(c: &Cell) -> impl FnMut(&str) -> Name + '_ {
    let bench = c.kind() == FileKind::Shbench;
    move |n: &str| {
        if c.records(RecordKind::Param).any(|r| c.field(r, 0) == n) {
            Name::ParentParam
        } else if let Some(r) = c.records(RecordKind::Var).find(|r| c.field(*r, 0) == n) {
            if bench {
                Name::BenchVar
            } else {
                Name::CellVar(c.field(r, 1).to_owned())
            }
        } else {
            Name::Unknown
        }
    }
}

/// The source waveform cell (index into the device row's `alib`) and params of a core
/// source instance's effective `value`.
fn source_props(
    value: &str,
    current: bool,
    c: &Cell,
    inst: &str,
    notes: &mut Notes,
) -> devices::Source {
    devices::parse_source(value, current).unwrap_or_else(|| {
        notes.review(
            "source",
            &c.path,
            format!("{inst}: `{value}` is not SPICE source text; sent as the DC value"),
        );
        devices::Source {
            wave: 0,
            params: vec![(
                (if current { "idc" } else { "vdc" }).into(),
                value.to_owned(),
            )],
        }
    })
}

fn schematic_out(
    d: &Design,
    ci: usize,
    plan: &Plan,
    m: &Mapping,
    retarget: bool,
    ids: &mut Ids,
    notes: &mut Notes,
) -> Cv {
    let c = &d.cells[ci];
    let g = geom(d, ci);
    let n = nets::extract(d, ci, &g);
    // §10.1: instances sized by gm/Id intent become class placeholders.
    let intents: Vec<crate::retarget::Intent> = if retarget {
        let (v, rn) = crate::retarget::intents(d, ci, |e| crate::retarget::cell_value(d, ci, e));
        notes.rows.extend(rn.rows);
        v
    } else {
        Vec::new()
    };
    let intent_of = |k: usize| intents.iter().find(|t| t.inst_id == g.inst_id[k]);
    let mut cv = Cv {
        view: "schematic".into(),
        vtype: "schematic".into(),
        upi: UPI,
        ..Cv::default()
    };
    // §6.4: re-mastered instances.
    let mut todo = Vec::new();
    for k in 0..g.inst_id.len() {
        let mi = g.inst_master[k] as usize;
        if intent_of(k).is_some() {
            // The target master is chosen on the target machine: every drawn pin goes
            // through a `conn` record under its own name (the class pin, §10.2 `pin`).
            let pins = sym_pins(&d.cells[mi]);
            todo.push(Remaster {
                inst: k,
                pins: (0..pins.name.len())
                    .filter(|&p| pins.inherit[p].is_none() && pins.alias[p].is_none())
                    .map(|p| (p, pins.name[p].clone()))
                    .collect(),
                new_pins: None,
            });
            continue;
        }
        if !plan.remastered(mi, m) {
            continue;
        }
        let pins = sym_pins(&d.cells[mi]);
        let names: Vec<String> = match plan.how[mi] {
            How::Map(row) => pins
                .name
                .iter()
                .map(|p| {
                    m.maps[row as usize]
                        .pins
                        .iter()
                        .find(|x| &x.0 == p)
                        .map_or(String::new(), |x| x.1.clone())
                })
                .collect(),
            How::AnalogLib(row) => {
                let dv = &devices::DEVICES[row as usize];
                (0..pins.name.len())
                    .map(|k| dv.alib_pins.get(k).map_or(String::new(), |s| (*s).into()))
                    .collect()
            }
            _ => unreachable!("only map rows and analogLib re-master"),
        };
        // Known geometry: a mirror of the target cell in the Design.
        let target = (plan.lib[mi].as_str(), plan.cell[mi].as_str());
        let new_pins = d
            .cells
            .iter()
            .find(|x| map::mirror_of(x).is_some_and(|(l, cl)| (l.as_str(), cl.as_str()) == target))
            .map(|x| {
                let sp = sym_pins(x);
                sp.name
                    .iter()
                    .zip(&sp.at)
                    .map(|(nm, at)| (vname(nm), *at))
                    .collect()
            });
        todo.push(Remaster {
            inst: k,
            pins: names
                .into_iter()
                .enumerate()
                .filter(|(_, n)| !n.is_empty())
                .collect(),
            new_pins,
        });
        notes.add(
            "remapped",
            &c.path,
            format!(
                "{}: {} → {}/{}",
                g.inst_name[k], d.cells[mi].name, target.0, target.1
            ),
        );
    }
    let edits = remap::apply(&g, &n, &todo);
    // Wires (after the edits) in Schemify coordinates, for gluing labels.
    // (wire ref, segment) of every wire label can glue to.
    let mut segs: Vec<(u32, [i32; 4])> = Vec::new();
    for w in 0..g.wire.len() {
        if edits.wire_dead.binary_search(&w).is_ok() {
            continue;
        }
        let mut seg = g.wire[w];
        for (_, end, p) in edits.wire_end.iter().filter(|x| x.0 == w) {
            seg[end * 2] = p[0];
            seg[end * 2 + 1] = p[1];
        }
        let r = ids.take();
        segs.push((r, seg));
        let a = conv(
            [seg[0], seg[1]],
            &format!("wire {}", g.wire_id[w]),
            c,
            notes,
        );
        let b = conv(
            [seg[2], seg[3]],
            &format!("wire {}", g.wire_id[w]),
            c,
            notes,
        );
        cv.wires.push(Wire {
            r,
            pts: vec![a, b],
            width: if g.wire_bus[w] { BUS_WIDTH } else { 0 },
        });
        id_prop(&mut cv, r, &g.wire_id[w]);
    }
    // Instances.
    let mut names: Vec<String> = Vec::new();
    let mut inst_ref = Vec::new();
    for k in 0..g.inst_id.len() {
        let mi = g.inst_master[k] as usize;
        let r = ids.take();
        inst_ref.push(r);
        let rec = g.inst_rec[k];
        let name = profile::inst_to_virtuoso(&g.inst_name[k]).unwrap_or_else(|| {
            notes.review(
                "name",
                &c.path,
                format!("instance name `{}` kept verbatim", g.inst_name[k]),
            );
            g.inst_name[k].clone()
        });
        names.push(name.clone());
        if let Some(t) = intent_of(k) {
            // A class placeholder: library "", the class as the cell, no parameters (§10.1).
            id_prop(&mut cv, r, &g.inst_id[k]);
            let class = m
                .classes
                .iter()
                .find(|cl| cl.name == t.class)
                .map_or(t.class.clone(), |cl| cl.name.clone());
            cv.intents.push(t.to_record(r as i32));
            cv.insts.push(Inst {
                r,
                name,
                lib: String::new(),
                cell: class,
                view: String::new(),
                xy: conv(
                    g.inst_at[k],
                    &format!("instance {}", g.inst_name[k]),
                    c,
                    notes,
                ),
                orient: profile::orient_to_virtuoso(g.inst_orient[k]).into(),
            });
            continue;
        }
        let mut cell = plan.cell[mi].clone();
        let master = &d.cells[mi];
        let src = match plan.how[mi] {
            How::Core | How::AnalogLib(_) => {
                devices::by_core(&master.name).and_then(|dv| dv.source)
            }
            _ => None,
        };
        let at = conv(
            g.inst_at[k],
            &format!("instance {}", g.inst_name[k]),
            c,
            notes,
        );
        id_prop(&mut cv, r, &g.inst_id[k]);
        match c.attr(rec, "ignore") {
            Some("true") => cv.props.push(Prop {
                r,
                name: "nlAction".into(),
                ty: "string".into(),
                value: "ignore".into(),
            }),
            Some(o) => notes.review(
                "expression",
                &c.path,
                format!("{}: `ignore {o}` has no Virtuoso form", g.inst_name[k]),
            ),
            None => {}
        }
        let row = match plan.how[mi] {
            How::Map(rw) if !m.maps[rw as usize].same => Some(&m.maps[rw as usize]),
            _ => None,
        };
        let dev = match plan.how[mi] {
            How::AnalogLib(rw) => Some(&devices::DEVICES[rw as usize]),
            _ => None,
        };
        let mut value_override: Option<String> = None;
        for a in c.doc.attrs(rec) {
            let key = c.doc.attr_key(a);
            let val = c.doc.attr_value(a, 0).unwrap_or("");
            match key {
                "at" | "name" | "ignore" => continue,
                "inherit" => {
                    let pin = val;
                    let net = c.doc.attr_value(a, 1).unwrap_or("");
                    cv.props.push(Prop {
                        r,
                        name: vname(pin),
                        ty: "string".into(),
                        value: net_to_virtuoso(net, false).0,
                    });
                    continue;
                }
                _ => {}
            }
            if src.is_some() && key == "value" {
                value_override = Some(val.to_owned());
                continue;
            }
            // Renames (map rows, analogLib rows).
            let (pname, scale) = if let Some(row) = row {
                if row.drops.iter().any(|x| x == key) {
                    continue;
                }
                match row.params.iter().find(|p| p.0 == key) {
                    Some(p) => (p.1.clone(), p.2.clone()),
                    None => (key.to_owned(), None),
                }
            } else if let Some(dv) = dev {
                match dv.params.iter().find(|p| p.0 == key) {
                    Some(p) => (p.1.to_owned(), None),
                    None => {
                        notes.add(
                            "remapped",
                            &c.path,
                            format!(
                                "{}: param {key} has no analogLib counterpart; not sent",
                                g.inst_name[k]
                            ),
                        );
                        continue;
                    }
                }
            } else {
                (key.to_owned(), None)
            };
            let kind = master
                .records(RecordKind::Param)
                .find(|p| master.field(*p, 0) == key)
                .map(|p| master.attr(p, "kind").unwrap_or("number"));
            let val_scaled;
            let val = match &scale {
                Some(f) => {
                    let e = format!("({val})*({f})");
                    val_scaled = ael::fold_numeric(&e).unwrap_or(e);
                    val_scaled.as_str()
                }
                None => val,
            };
            if let Some(p) = override_prop(c, r, &pname, kind, val, &g.inst_name[k], notes) {
                // Virtuoso stores no CDF value equal to its default (storeDefault "no"), so an
                // override equal to the master's default is not sent: the stash (§7.3) keeps
                // it as Schemify-only data and a round trip restores it.
                let default = master
                    .records(RecordKind::Param)
                    .find(|q| master.field(*q, 0) == key)
                    .map(|q| master.field(q, 1).to_owned());
                let same = default.is_some_and(|dv| {
                    let dv = match &scale {
                        Some(f) => {
                            let e = format!("({dv})*({f})");
                            ael::fold_numeric(&e).unwrap_or(e)
                        }
                        None => dv,
                    };
                    override_prop(c, r, &pname, kind, &dv, "", &mut Notes::default())
                        .is_some_and(|d| d.ty == p.ty && d.value == p.value)
                });
                if !same {
                    cv.props.push(p);
                }
            }
        }
        if let Some(current) = src {
            let effective = value_override.clone().unwrap_or_else(|| {
                master
                    .records(RecordKind::Param)
                    .find(|p| master.field(*p, 0) == "value")
                    .map_or("0".into(), |p| master.field(p, 1).to_owned())
            });
            // Only an override is sent to schemify_core (D40); analogLib gets the value.
            if value_override.is_some() || dev.is_some() {
                let sp = source_props(&effective, current, c, &g.inst_name[k], notes);
                if let Some(dv) = dev {
                    cell = dv.alib[sp.wave].into();
                } else {
                    cv.props.push(Prop {
                        r,
                        name: "srcType".into(),
                        ty: "string".into(),
                        value: devices::WAVEFORMS[sp.wave].1.into(),
                    });
                }
                for (pn, pv) in sp.params {
                    let ty = if pn == "tvpairs" { "int" } else { "string" };
                    cv.props.push(Prop {
                        r,
                        name: pn,
                        ty: ty.into(),
                        value: pv,
                    });
                }
            }
        }
        if let Some(row) = row {
            for (p, v) in &row.sets {
                cv.props.push(Prop {
                    r,
                    name: p.clone(),
                    ty: "string".into(),
                    value: v.clone(),
                });
            }
        }
        cv.insts.push(Inst {
            r,
            name,
            lib: plan.lib[mi].clone(),
            cell,
            view: plan.view[mi].clone(),
            xy: at,
            orient: profile::orient_to_virtuoso(g.inst_orient[k]).into(),
        });
    }
    // Labels: the cell's, then the edits'.
    let pin_points: Vec<([i32; 2], [i32; 2])> = (0..g.inst_id.len())
        .flat_map(|k| {
            let ps = sym_pins(&d.cells[g.inst_master[k] as usize]);
            let (o, at) = (g.inst_orient[k], g.inst_at[k]);
            ps.at
                .iter()
                .zip(&ps.inherit)
                .filter(|(_, inh)| inh.is_none())
                .filter_map(move |(p, _)| crate::design::place(*p, o, at).map(|q| (q, at)))
                .collect::<Vec<_>>()
        })
        .collect();
    // (id, anchor, net, global, orientation, Virtuoso instance name of a global symbol)
    type LabelOut<'a> = (
        Option<&'a str>,
        [i32; 2],
        String,
        bool,
        Orient,
        Option<&'a str>,
    );
    let mut all_labels: Vec<LabelOut> = (0..g.label_at.len())
        .map(|k| {
            let o = Orient::parse(c.field(g.label_rec[k], 3)).unwrap_or(Orient::R0);
            (
                Some(g.label_id[k].as_str()),
                g.label_at[k],
                g.label_net[k].clone(),
                g.label_global[k],
                o,
                c.attr(g.label_rec[k], "vinst"),
            )
        })
        .collect();
    for l in &edits.labels {
        all_labels.push((None, l.at, l.net.clone(), l.global, Orient::R0, None));
    }
    for (from, l) in &edits.stubs {
        let r = ids.take();
        let (a, b) = (conv(*from, "stub", c, notes), conv(l.at, "stub", c, notes));
        cv.wires.push(Wire {
            r,
            pts: vec![a, b],
            width: 0,
        });
        segs.push((r, [from[0], from[1], l.at[0], l.at[1]]));
        all_labels.push((None, l.at, l.net.clone(), l.global, Orient::R0, None));
    }
    for (id, at, net, global, o, vinst) in all_labels {
        let r = ids.take();
        let (text, review) = net_to_virtuoso(&net, global);
        if review {
            notes.review("name", &c.path, format!("net `{net}` kept verbatim"));
        }
        let back = net_from_virtuoso(&text);
        if (back.text.as_str(), back.global) != (net.as_str(), global) {
            cv.props.push(Prop {
                r,
                name: "schemifyNet".into(),
                ty: "string".into(),
                value: if global {
                    format!("{net} global")
                } else {
                    net.clone()
                },
            });
        }
        // A global net with an analogLib global symbol (`gnd!`, `vdd!`, …) is drawn as that
        // symbol, Virtuoso's idiom; import turns the symbol back into this label (§6.1).
        // GUESS(G3): the symbol's pin sits at its origin.
        // Only a label that came from such a symbol, or one on no wire (which would
        // otherwise need a stub), becomes the symbol; a wire label stays a wire label.
        let on_wire = segs.iter().any(|(_, s)| nets::on_segment(at, *s));
        if let Some(sym) = text
            .strip_suffix('!')
            .filter(|b| devices::GLOBAL_SYMBOLS.contains(b))
            .filter(|_| vinst.is_some() || !on_wire)
        {
            if let Some(id) = id {
                id_prop(&mut cv, r, id);
            }
            cv.insts.push(Inst {
                r,
                name: vinst.map_or_else(|| format!("sfyG{r}"), str::to_owned),
                lib: "analogLib".into(),
                cell: sym.to_owned(),
                view: "symbol".into(),
                xy: conv(at, "label", c, notes),
                orient: profile::orient_to_virtuoso(o).into(),
            });
            continue;
        }
        // ponytail: O(labels × wires) gluing; a sorted segment index for 10^5-wire cells.
        let parent = match segs.iter().find(|(_, s)| nets::on_segment(at, *s)) {
            Some((wr, _)) => *wr,
            None => {
                // §6.2: a stub away from the instance whose pin this is.
                let origin = pin_points
                    .iter()
                    .find(|(p, _)| *p == at)
                    .map(|(_, o)| *o)
                    .unwrap_or([at[0] - 1, at[1]]);
                let dir = remap::away(origin, at);
                let to = [at[0] + dir[0] * STEP, at[1] + dir[1] * STEP];
                let sr = ids.take();
                let (a, b) = (conv(at, "stub", c, notes), conv(to, "stub", c, notes));
                cv.wires.push(Wire {
                    r: sr,
                    pts: vec![a, b],
                    width: 0,
                });
                cv.props.push(Prop {
                    r: sr,
                    name: "schemifyStub".into(),
                    ty: "string".into(),
                    value: "1".into(),
                });
                sr
            }
        };
        if let Some(id) = id {
            id_prop(&mut cv, r, id);
        }
        cv.labels.push(Label {
            r,
            parent,
            text,
            xy: conv(at, "label", c, notes),
            orient: profile::orient_to_virtuoso(o).into(),
            justify: "lowerLeft".into(),
            font: "stick".into(),
            height: LABEL_HEIGHT,
            kind: "normal".into(),
            layer: "wire".into(),
            purpose: "label".into(),
        });
    }
    // Ports → schematic pins.
    let own = sym_pins(c);
    for k in 0..g.port_at.len() {
        let r = ids.take();
        let pin = &g.port_pin[k];
        let dir = own
            .name
            .iter()
            .position(|p| p == pin)
            .map_or("inputOutput", |q| vdir(&own.dir[q]));
        let term = vname(pin);
        if net_from_virtuoso(&term).text != *pin {
            cv.props.push(Prop {
                r,
                name: "schemifyNet".into(),
                ty: "string".into(),
                value: pin.clone(),
            });
        }
        let o = Orient::parse(c.field(g.port_rec[k], 3)).unwrap_or(Orient::R0);
        id_prop(&mut cv, r, &g.port_id[k]);
        cv.spins.push(Spin {
            r,
            term,
            dir: dir.into(),
            lib: "basic".into(),
            cell: match dir {
                "input" => "ipin",
                "output" => "opin",
                _ => "iopin",
            }
            .into(),
            view: "symbol".into(),
            xy: conv(g.port_at[k], "port", c, notes),
            orient: profile::orient_to_virtuoso(o).into(),
        });
    }
    // Noconns → basic/noConn instances.
    let mut nc = 0;
    for k in 0..g.noconn_at.len() {
        let r = ids.take();
        let name = loop {
            let nm = format!("NC{nc}");
            nc += 1;
            if !names.contains(&nm) {
                break nm;
            }
        };
        id_prop(&mut cv, r, &g.noconn_id[k]);
        cv.insts.push(Inst {
            r,
            name,
            lib: "basic".into(),
            cell: "noConn".into(),
            view: "symbol".into(),
            xy: conv(g.noconn_at[k], "noconn", c, notes),
            orient: "R0".into(),
        });
    }
    // Texts → note labels; shapes → note shapes.
    for r in c.records(RecordKind::Text) {
        let lr = ids.take();
        let o = Orient::parse(c.field(r, 3)).unwrap_or(Orient::R0);
        id_prop(&mut cv, lr, c.field(r, 0));
        cv.labels.push(Label {
            r: lr,
            parent: 0,
            text: c.field(r, 4).to_owned(),
            xy: conv(
                [c.coord(r, 1).unwrap_or(0), c.coord(r, 2).unwrap_or(0)],
                "text",
                c,
                notes,
            ),
            orient: profile::orient_to_virtuoso(o).into(),
            justify: "upperLeft".into(),
            font: "stick".into(),
            height: height(c, r, TEXT_SIZE_MILLI),
            kind: "normal".into(),
            layer: "text".into(),
            purpose: "drawing".into(),
        });
    }
    for r in c.records(RecordKind::Shape) {
        let dashed = c.attr(r, "dash").is_some_and(|x| x != "0");
        for sh in graphic_out(
            c,
            r,
            RecordKind::Shape,
            1,
            "text",
            if dashed { "drawing1" } else { "drawing" },
            ids,
            notes,
        ) {
            id_prop(&mut cv, sh.r, c.field(r, 0));
            cv.shapes.push(sh);
        }
    }
    // §6.4 step 2: loader connections for masters of unknown geometry.
    for (k, term, l) in &edits.conns {
        cv.conns.push(Conn {
            r: inst_ref[*k],
            term: term.clone(),
            net: net_to_virtuoso(&l.net, l.global).0,
        });
    }
    // A retarget placeholder's inherited pins (a bulk on `VSS`) have no drawn node: their
    // net is the instance's `inherit` override, else the pin's default global (netlist §5).
    for (k, &ir) in inst_ref.iter().enumerate() {
        if intent_of(k).is_none() {
            continue;
        }
        let pins = sym_pins(&d.cells[g.inst_master[k] as usize]);
        for (p, default) in pins.inherit.iter().enumerate() {
            let Some(default) = default else { continue };
            let over = c.doc.attrs(g.inst_rec[k]).find(|a| {
                c.doc.attr_key(*a) == "inherit"
                    && c.doc.attr_value(*a, 0) == Some(pins.name[p].as_str())
            });
            let (net, global) = match over.and_then(|a| c.doc.attr_value(a, 1)) {
                Some(n) => (n.to_owned(), false),
                None => (default.clone(), true),
            };
            cv.conns.push(Conn {
                r: ir,
                term: pins.name[p].clone(),
                net: net_to_virtuoso(&net, global).0,
            });
        }
    }
    assert_eq!(inst_ref.len(), g.inst_id.len());
    cv
}

fn id_prop(cv: &mut Cv, r: u32, id: &str) {
    cv.props.push(Prop {
        r,
        name: "schemifyId".into(),
        ty: "string".into(),
        value: id.to_owned(),
    });
}

/// One instance override as a CDF property (§6.2, §3.4); `None` (reviewed) when it has
/// no Virtuoso form, so the instance keeps the CDF default.
fn override_prop(
    c: &Cell,
    r: u32,
    pname: &str,
    kind: Option<&str>,
    val: &str,
    inst: &str,
    notes: &mut Notes,
) -> Option<Prop> {
    let prop = |ty: &str, value: String| Prop {
        r,
        name: pname.to_owned(),
        ty: ty.into(),
        value,
    };
    match kind {
        Some("int") => match val.trim().parse::<i64>() {
            Ok(_) => Some(prop("int", val.trim().to_owned())),
            Err(_) => {
                notes.review(
                    "expression",
                    &c.path,
                    format!("{inst}: {pname} = `{val}` is not a literal int"),
                );
                None
            }
        },
        Some("bool") => match val.trim() {
            "true" => Some(prop("boolean", "TRUE".into())),
            "false" => Some(prop("boolean", "FALSE".into())),
            _ => {
                notes.review(
                    "expression",
                    &c.path,
                    format!("{inst}: {pname} = `{val}` is not a literal bool"),
                );
                None
            }
        },
        Some("string" | "choice") | None => match ael::template_to_literal(val) {
            Some(t) => Some(prop("string", t)),
            None => {
                notes.review(
                    "template",
                    &c.path,
                    format!("{inst}: {pname} = `{val}` has template holes"),
                );
                None
            }
        },
        _ => match ael::to_ael(val, parent_scope(c)) {
            Ok(t) => {
                if let Some(why) = t.review {
                    notes.review(why, &c.path, format!("{inst}: {pname} = `{val}`"));
                }
                Some(prop("string", t.text))
            }
            Err(why) => {
                notes.review(
                    why,
                    &c.path,
                    format!("{inst}: {pname} = `{val}` keeps the CDF default"),
                );
                None
            }
        },
    }
}
