//! Design → `.sfyx` records (spec `interop.md` §6, §7.3).
//!
//! Spine of [`export`]: `map::resolve` → per exported cell, `cell_out` builds the dump
//! tables ([`DCell`]: CDF, symbol and schematic cellviews, text views; `remap::apply` edits
//! for re-mastered instances) → `ade::export` per bench → the image import of those
//! records (`read::image`) → `merge::stash` per cell (Schemify-only data and every record
//! the image does not reproduce) → [`records`] (sorted, refs assigned) → `sfyx::write`.

use std::collections::BTreeMap;

use schemify_format::{FileKind, Orient, RecordKind, parse_coord};

use super::ael::{self, Name};
use super::devices;
use super::read::{
    self, Cdf, CdfParam, Conn, Cv, DCell, Dump, Inst, Label, PinFig, Prop, Shape, SimInfo, SimVal,
    Spin, Term, TextView, Wire,
};
use super::{Export, ExportOptions, ade};
use crate::align;
use crate::design::{Cell, Design, geom, sym_pins};
use crate::map::{self, How, Mapping, Plan};
use crate::merge;
use crate::nets;
use crate::notes::Notes;
use crate::profile::{
    self, MILLI_PER_INCH, coord_text, dir_to_virtuoso, net_from_virtuoso, net_to_virtuoso, norm_box,
};
use crate::remap::{self, Remaster, STEP};
use crate::sfyx::{self, Record, Value, i, pairs, s};

/// Units per inch the exporter writes (§3.1): one unit is one Schemify milli-unit.
pub const UPI: i32 = MILLI_PER_INCH as i32;

/// Half the side of an exported symbol pin rectangle (1/80 inch).
pub const PIN_HALF: i32 = 2_000;

/// Width of an exported bus wire (3/80 inch, on Virtuoso's 2-DBU width grid: 1/32 inch came
/// back rounded up to this on a live run); 0 is a narrow wire.
pub const BUS_WIDTH: i32 = 6_000;

/// Height of exported wire and pin-name labels (1/16 inch).
pub const LABEL_HEIGHT: i32 = 10_000;

/// Default `size` of a `stext` and of a `text` when the record has none (the editor's).
pub const STEXT_SIZE_MILLI: i32 = 300;
/// See [`STEXT_SIZE_MILLI`].
pub const TEXT_SIZE_MILLI: i32 = 400;

/// Bytes per `schemifyRecords<k>` property (§7.3).
pub const STASH_CHUNK: usize = 4000;

/// Core params that only shape the ngspice netlist: never sent to an analogLib master and
/// never reported (§6.5).
const NGSPICE_ONLY: [&str; 1] = ["savecurrent"];

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
    let mut ocean: Vec<ade::BenchExport> = Vec::new();
    // (dump cell index, design cell index) of every written cell.
    let mut written = Vec::new();
    for ci in 0..d.cells.len() {
        if !matches!(plan.how[ci], How::Design | How::Core) {
            continue;
        }
        let c = cell_out(d, ci, &plan, m, &mut notes);
        if d.cells[ci].kind() == FileKind::Shbench {
            let mut b = ade::export(
                d,
                ci,
                &plan.lib[ci],
                |p| inst_info(d, &plan, m, ci, p),
                &mut notes,
            );
            if !b.records.is_empty() {
                dump.maestro.push(std::mem::take(&mut b.records));
                ocean.push(b);
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
        for (n, chunk) in sfyx::chunks(&text, STASH_CHUNK).into_iter().enumerate() {
            cv.props
                .push(Prop::string(0, &format!("schemifyRecords{}", n + 1), chunk));
        }
    }
    let recs = records(&dump);
    assert!(
        recs.first().is_some_and(|r| r.tag == "sfyx")
            && recs.last().is_some_and(|r| r.tag == "end")
    );
    // One script per bench cell; a second cell of the same name (another library) gets `_2`, ….
    let mut scripts: Vec<(String, String)> = Vec::new();
    for ade::BenchExport {
        cell: test,
        ocean: text,
        defns,
        ..
    } in ocean
    {
        let mut name = test.clone();
        let mut k = 1;
        while scripts.iter().any(|s| s.0 == format!("{name}.ocn")) {
            k += 1;
            name = format!("{test}_{k}");
        }
        for (f, t) in defns {
            scripts.push((format!("{name}/{f}"), t));
        }
        let text = if name == test {
            text
        } else {
            text.replace(&format!("\"{test}/"), &format!("\"{name}/"))
        };
        scripts.push((format!("{name}.ocn"), text));
    }
    scripts.sort();
    // §12: one `off-grid` row per cell (the count and the first point), not one per point:
    // a symbol drawn on a half grid has dozens and they all mean the same thing.
    let mut grid: Vec<(String, usize, String)> = Vec::new();
    notes.rows.retain(|r| {
        if r.class != "off-grid" {
            return true;
        }
        match grid.iter_mut().find(|g| g.0 == r.cell) {
            Some(g) => g.1 += 1,
            None => grid.push((r.cell.clone(), 1, r.detail.clone())),
        }
        false
    });
    for (cell, n, first) in grid {
        notes.add(
            "off-grid",
            &cell,
            format!("{n} point(s) off the 1/160\" grid (Virtuoso snaps them), first: {first}"),
        );
    }
    Ok(Export {
        sfyx: sfyx::write(&recs),
        notes,
        ocean: scripts,
    })
}

/// What the output translator needs about instance path `path` (Schemify instance names
/// joined by `/`, a subcircuit's SPICE name `X<name>` accepted for `<name>`) of bench `ci`
/// (§9.1): its Virtuoso path and the terminal `i()` reads (the exported master's first pin:
/// analogLib's, a map row's, else the pin's own name). `None` when a segment names no
/// instance.
fn inst_info(d: &Design, plan: &Plan, m: &Mapping, ci: usize, path: &str) -> Option<ade::InstInfo> {
    let mut cur = ci;
    let mut vpath: Vec<String> = Vec::new();
    let mut last: Option<usize> = None;
    for seg in path.split('/') {
        let c = &d.cells[cur];
        let by_name = |n: &str| {
            c.records(RecordKind::Inst)
                .find(|&r| c.attr(r, "name") == Some(n))
        };
        let r = by_name(seg).or_else(|| {
            seg.strip_prefix(['X', 'x']).and_then(by_name).filter(|&r| {
                d.resolve(cur, c.field(r, 1))
                    .is_some_and(|mi| d.cells[mi].kind() == FileKind::Shcomp)
            })
        })?;
        let name = c.attr(r, "name").expect("found by name");
        vpath.push(profile::inst_to_virtuoso(name).unwrap_or_else(|| name.to_owned()));
        let mi = d.resolve(cur, c.field(r, 1))?;
        last = Some(mi);
        cur = mi;
    }
    let mi = last?;
    let first_pin = sym_pins(&d.cells[mi])
        .name
        .first()
        .cloned()
        .unwrap_or_default();
    let terminal = match plan.how[mi] {
        How::AnalogLib(row) => devices::DEVICES[row as usize]
            .alib_pins
            .first()
            .map_or(first_pin.clone(), |p| (*p).to_owned()),
        How::Map(row) => m.maps[row as usize]
            .pins
            .iter()
            .find(|p| p.0 == first_pin)
            .map_or_else(|| vname(&first_pin), |p| p.1.clone()),
        _ => vname(&first_pin),
    };
    Some(ade::InstInfo {
        path: vpath.join("/"),
        terminal,
    })
}

// ---------------------------------------------------------------------------------------
// Serialization: Dump → records (§4.2 order, refs assigned after sorting)
// ---------------------------------------------------------------------------------------

/// The `cdf`, `param` and `siminfo` records of `cdf` (§4.2): params in CDF order, siminfo
/// as stored (the dump writes it sorted by simulator).
pub fn cdf_records(cdf: &Cdf) -> Vec<Record> {
    let mut out = vec![Record::new("cdf", vec![sfyx::text(&cdf.label_set)])];
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
                    SimVal::Pairs(p) => pairs(p.iter().map(|(a, b)| (a.as_str(), b.as_str()))),
                };
                Value::List(vec![s(k), v])
            })
            .collect();
        out.push(Record::new(
            "siminfo",
            vec![s(&si.sim), Value::List(fields)],
        ));
    }
    debug_assert_eq!(out.len(), 1 + cdf.params.len() + cdf.siminfo.len());
    out
}

/// The CDF a cell from Virtuoso had, when its interface (pins, params, impls) is unchanged
/// since the import (`vcdf` fingerprint, §7.5): `Some(None)` for a cell that had no CDF,
/// `Some(Some(cdf))` for the stored records (`vcdfrec`), `None` when the cell is not from
/// Virtuoso or was edited (export then derives the CDF, §6.3).
fn cdf_as_imported(c: &Cell) -> Option<Option<Cdf>> {
    let fp = c.header(super::fingerprint_attr(super::Part::Interface))?;
    if fp != super::fingerprint(&c.doc, super::Part::Interface) {
        return None;
    }
    let recs: Vec<&str> = (0..c.doc.attrs(0).len() as u32)
        .map(|k| c.doc.attrs(0).start + k)
        .filter(|&a| c.doc.attr_key(a) == "vcdfrec")
        .filter_map(|a| c.doc.attr_value(a, 0))
        .collect();
    if recs.is_empty() {
        return Some(None);
    }
    let text = format!(
        "(\"sfyx\" 1 \"vcdf\")\n(\"lib\" \"l\")\n(\"cell\" \"c\" \"design\")\n{}\n(\"end\")\n",
        recs.join("\n")
    );
    let cdf = sfyx::read(&text)
        .ok()
        .and_then(|r| read::dump(&r).ok())
        .and_then(|d| d.cells.into_iter().next())
        .and_then(|c| c.cdf)?;
    Some(Some(cdf))
}

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
            out.extend(cdf_records(cdf));
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
        cv.props.push(Prop::string(0, "schemifyKeep", "1"));
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
        let mut f = vec![
            i(r as i32),
            s(&p.term),
            i(p.b[0]),
            i(p.b[1]),
            i(p.b[2]),
            i(p.b[3]),
        ];
        if !p.dir.is_empty() {
            f.push(s(&p.dir));
        }
        out.push(Record::new("pinfig", f));
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
}

// ---------------------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------------------

/// Builds the dump tables of Design cell `ci`.
fn cell_out(d: &Design, ci: usize, plan: &Plan, m: &Mapping, notes: &mut Notes) -> DCell {
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
            // A cell from Virtuoso whose interface nobody changed gets its CDF back as it
            // was (the reverse stash, §7.5); otherwise the CDF is derived from the cell.
            let as_imported = if core { None } else { cdf_as_imported(c) };
            let cdf = as_imported
                .clone()
                .flatten()
                .unwrap_or_else(|| cdf_out(c, core, notes));
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
            out.cdf = match as_imported {
                Some(stored) => stored,
                None => (!bare).then_some(cdf),
            };
            out.cvs
                .push(keep(c, super::Part::Symbol, symbol_out(c, &mut ids, notes)));
            if c.kind() == FileKind::Shcomp {
                let sch = schematic_out(d, ci, plan, m, &mut ids, notes);
                out.cvs.push(keep(c, super::Part::Schematic, sch));
            }
            out.textviews = textviews_out(c, notes);
        }
        FileKind::Shbench => {
            let sch = schematic_out(d, ci, plan, m, &mut ids, notes);
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
            format!("{what} at ({}, {})", coord_text(p[0]), coord_text(p[1])),
        );
    }
    profile::point_to_virtuoso(p).unwrap_or([p[0], i32::MAX])
}

/// Coordinates of a Schemify record's fields `from..` as milli-units.
fn coords(c: &Cell, r: u32, from: usize) -> Vec<i32> {
    c.doc
        .fields(r)
        .skip(from)
        .map(|t| parse_coord(t).unwrap_or(0))
        .collect()
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
                .attr_values(r, "choices")
                .into_iter()
                .map(str::to_owned)
                .collect();
            p.def = literal(default, name, c, notes);
        }
        "string" => {
            p.ty = "string".into();
            p.def = literal(default, name, c, notes);
            // Virtuoso refuses units on a string parameter not parsed as a number (live
            // ICADVM20.1: "'parseAsNumber' field must be set if 'units' field is set").
            p.units.clear();
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
pub(crate) fn template_items(t: &str) -> Option<Vec<(bool, String)>> {
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

/// The `k={c}` tail items of a format: `(k, c)` per item, each ` k={c}` or the optional
/// form [`optional_param`] writes, every `c` one of `params`; `None` when the tail has any
/// other shape.
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
    let pin_recs: Vec<u32> = c.records(RecordKind::Pin).collect();
    for (k, &pin_rec) in pin_recs.iter().enumerate() {
        let at = conv(pins.at[k], &format!("pin {}", pins.name[k]), c, notes);
        let term = vname(pins.alias[k].as_deref().unwrap_or(&pins.name[k]));
        if pins.alias[k].is_none() {
            let netexpr = pins.inherit[k].as_ref().map_or(String::new(), |net| {
                format!("[@{}:%:{}]", term, net_to_virtuoso(net, true).0)
            });
            cv.terms.push(Term {
                name: term.clone(),
                dir: dir_to_virtuoso(&pins.dir[k]).into(),
                netexpr,
            });
        }
        let r = ids.take();
        // The rectangle as Virtuoso drew it (`vpfig <w> <h>`), else ±PIN_HALF.
        let [hw, hh] = match c.attr_values(pin_rec, "vpfig")[..] {
            [w, h] => [w, h].map(|x| parse_coord(x).map_or(PIN_HALF, |v| v / 2)),
            _ => [PIN_HALF, PIN_HALF],
        };
        cv.pinfigs.push(PinFig {
            r,
            term: term.clone(),
            b: [at[0] - hw, at[1] - hh, at[0] + hw, at[1] + hh],
            dir: String::new(),
        });
        grow([at[0] - hw, at[1] - hh], &mut bbox);
        grow([at[0] + hw, at[1] + hh], &mut bbox);
        if pins.alias[k].is_some() {
            cv.props
                .push(Prop::string(r, "schemifyPin", pins.name[k].clone()));
        }
        // The name label as Virtuoso drew it (`vplabel <dx> <dy> <orient> <justify> <font>
        // <height>` from the pin, or `vplabel none`), else export's.
        let v: Vec<&str> = c.attr_values(pin_rec, "vplabel");
        let (xy, orient, justify, font, height) = match v[..] {
            ["none"] => continue,
            [dx, dy, o, j, f, h] => {
                let d = [parse_coord(dx).unwrap_or(0), parse_coord(dy).unwrap_or(0)];
                let p = [pins.at[k][0] + d[0], pins.at[k][1] + d[1]];
                (
                    conv(p, "pin label", c, notes),
                    o,
                    j,
                    f,
                    h.parse().unwrap_or(LABEL_HEIGHT / 2),
                )
            }
            _ => (at, "R0", "centerLeft", "stick", LABEL_HEIGHT / 2),
        };
        let o = Orient::parse(orient).unwrap_or(Orient::R0);
        let style = ["pin", "label", justify, font, "normal"];
        cv.labels
            .push(label(ids.take(), r, term, xy, o, height, style));
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
    // The selection box as Virtuoso drew it (`vibox`), else the box around the contents.
    // ponytail: a kept box goes stale when the symbol grows in Schemify; union it with the
    // contents' box if that bites.
    let vibox: Vec<&str> = c.attr_values(0, "vibox");
    let bbox = match vibox[..] {
        ["none"] => None,
        [x0, y0, x1, y1] => {
            let v: Vec<i32> = [x0, y0, x1, y1]
                .iter()
                .map(|x| parse_coord(x).unwrap_or(0))
                .collect();
            let (p, q) = (
                conv([v[0], v[1]], "selection box", c, notes),
                conv([v[2], v[3]], "selection box", c, notes),
            );
            Some(norm_box(p, q))
        }
        _ => bbox,
    };
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

/// A label row of object `r` glued to `parent` (0: none). `style` is `[layer, purpose,
/// justify, font, kind]`, the order the `vlabel` attribute keeps them in, so a label read
/// back from `vlabel` and one export draws go through the same door.
fn label(
    r: u32,
    parent: u32,
    text: String,
    xy: [i32; 2],
    o: Orient,
    height: i32,
    style: [&str; 5],
) -> Label {
    debug_assert!(r > 0 && r != parent, "a label is its own object");
    let [layer, purpose, justify, font, kind] = style;
    Label {
        r,
        parent,
        text,
        xy,
        orient: profile::orient_to_virtuoso(o).into(),
        justify: justify.into(),
        font: font.into(),
        height,
        kind: kind.into(),
        layer: layer.into(),
        purpose: purpose.into(),
    }
}

/// `height = size × 10_000` milli (§3.5); `size` absent → `default_milli`.
fn height(c: &Cell, r: u32, default_milli: i32) -> i32 {
    let s = c
        .attr(r, "size")
        .and_then(|t| parse_coord(t).ok())
        .unwrap_or(default_milli);
    // Rounded as Virtuoso stores it, so the image predicts the read-back and an untouched
    // symbol keeps its stashed sizes (§7.3).
    profile::height_of_size(s)
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
    let at = conv(c.point(r, 0), "stext", c, notes);
    let o = c.orient(r, 2);
    let t = c.field(r, 3);
    // A label from Virtuoso whose template is still the one it imported as goes back as
    // drawn: `vlabel <layer> <purpose> <justify> <font> <kind> <text> <template>`.
    let v: Vec<&str> = c.attr_values(r, "vlabel");
    if let [layer, purpose, justify, font, kind, text, tmpl] = v[..]
        && tmpl == t
    {
        let style = [layer, purpose, justify, font, kind];
        let h = height(c, r, STEXT_SIZE_MILLI);
        return Some(label(ids.take(), 0, text.into(), at, o, h, style));
    }
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
            let Some(text) = template_to_nlp(&items, &params) else {
                notes.review("template", &c.path, format!("stext `{t}` dropped"));
                return None;
            };
            (text, "NLP", "annotate", "drawing")
        }
    };
    let j = justify(
        c.doc.find_attr(r, "hcenter").is_some(),
        c.doc.find_attr(r, "vcenter").is_some(),
    );
    let h = height(c, r, STEXT_SIZE_MILLI);
    Some(label(
        ids.take(),
        0,
        text,
        at,
        o,
        h,
        [layer, purpose, j, "stick", kind],
    ))
}

/// The NLP text of a symbol label template with holes (§3.5): every hole a parameter of
/// `params`, bare or through the display formatter `eng(p)` (NLP shows the value as
/// entered), one `[@p:pre%post]` group per hole (`[@p]` when both are empty), the literal
/// before the first hole its `pre`, each literal after a hole its `post`, NLP-escaped.
/// `None` when a hole is anything else. Import's `read::nlp_in` reads it back.
pub(crate) fn template_to_nlp(items: &[(bool, String)], params: &[&str]) -> Option<String> {
    fn param<'a>(h: &'a str, params: &[&str]) -> Option<&'a str> {
        let p = h
            .strip_prefix("eng(")
            .and_then(|x| x.strip_suffix(')'))
            .unwrap_or(h)
            .trim();
        params.contains(&p).then_some(p)
    }
    let names = items
        .iter()
        .filter(|x| x.0)
        .map(|x| param(&x.1, params))
        .collect::<Option<Vec<&str>>>()?;
    let first = items.iter().position(|x| x.0)?;
    let mut pre: String = items[..first].iter().map(|x| x.1.as_str()).collect();
    let mut text = String::new();
    let mut k = first;
    for p in &names {
        let next = items[k + 1..]
            .iter()
            .position(|x| x.0)
            .map_or(items.len(), |n| k + 1 + n);
        let post: String = items[k + 1..next].iter().map(|x| x.1.as_str()).collect();
        if pre.is_empty() && post.is_empty() {
            text.push_str(&format!("[@{p}]"));
        } else {
            text.push_str(&format!(
                "[@{p}:{}%{}]",
                nlp_escape(&pre),
                nlp_escape(&post)
            ));
        }
        pre.clear();
        k = next;
    }
    debug_assert!(text.starts_with("[@"));
    Some(text)
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
            vec![shape("rect", norm_box(a, b).to_vec(), ids)]
        }
        "circle" if v.len() == 3 => {
            let o = conv([v[0], v[1]], form, c, notes);
            // Virtuoso stores the box in whole DBUs, rounded outward (live ICADVM20.1: the
            // radius-2.5 bubble of a pmos at y 0 came back with box y -3..3), so the export
            // writes that box and the image predicts the read-back; the stash keeps the
            // circle (§7.3).
            let g = (MILLI_PER_INCH / i64::from(DBU)) as i32;
            let (lo, hi) = (
                |x: i32| x.div_euclid(g) * g,
                |x: i32| -(-x).div_euclid(g) * g,
            );
            vec![shape(
                "ellipse",
                vec![
                    lo(o[0] - v[2]),
                    lo(o[1] - v[2]),
                    hi(o[0] + v[2]),
                    hi(o[1] + v[2]),
                ],
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
    // A `{expr}` hole becomes its §3.4 value (bench vars bare: ADE design variables); a hole
    // without an AEL form makes the whole text unparsable, never a half-translated CDF.
    let mut reviews: Vec<(&'static str, String)> = Vec::new();
    let parsed = devices::parse_source_with(value, current, |e| {
        let t = ael::to_ael(e, parent_scope(c)).ok()?;
        if let Some(why) = t.review {
            reviews.push((why, format!("{inst}: `{e}` → `{}`", t.text)));
        }
        Some(t.text)
    });
    if parsed.is_some() {
        for (why, detail) in reviews {
            notes.review(why, &c.path, detail);
        }
    }
    parsed.unwrap_or_else(|| {
        notes.review(
            "source",
            &c.path,
            format!(
                "{inst}: `{value}` is not SPICE source text with number or {{expr}} values; \
                 sent as the DC value"
            ),
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
    ids: &mut Ids,
    notes: &mut Notes,
) -> Cv {
    let c = &d.cells[ci];
    let g = geom(d, ci);
    let n = nets::extract(d, ci, &g);
    let mut cv = Cv {
        view: "schematic".into(),
        vtype: "schematic".into(),
        upi: UPI,
        ..Cv::default()
    };
    // §6.4: re-mastered instances.
    let mut todo = Vec::new();
    // Instances placed by pin-pattern alignment (§6.4): index → the new master's origin.
    let mut aligned: BTreeMap<usize, [i32; 2]> = BTreeMap::new();
    for k in 0..g.inst_id.len() {
        let mi = g.inst_master[k] as usize;
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
                    .collect::<Vec<_>>()
            })
            // analogLib's pins, as measured (devices::alib_pins).
            .or_else(|| match plan.how[mi] {
                How::AnalogLib(_) => devices::alib_pins(target.1)
                    .map(|v| v.iter().map(|(t, p)| ((*t).to_owned(), *p)).collect()),
                _ => None,
            });
        let pairs: Vec<(usize, String)> = names
            .into_iter()
            .enumerate()
            .filter(|(_, n)| !n.is_empty())
            .collect();
        // Pin-pattern alignment (§6.4): the new master goes where its pins land on the
        // drawn ones, in the instance's orientation; the pins that still miss get stubs.
        let at = new_pins.as_ref().and_then(|np| {
            align::fit_instance(&pins.at, &pairs, np, g.inst_orient[k], g.inst_at[k])
        });
        if let Some(a) = at {
            aligned.insert(k, a);
        }
        todo.push(Remaster {
            inst: k,
            pins: pairs,
            new_pins,
            at,
        });
        // A core cell on its §6.5 analogLib master is the export's default, not news; a
        // mapping-file row is reported.
        if matches!(plan.how[mi], How::Map(_)) {
            notes.add(
                "remapped",
                &c.path,
                format!(
                    "{}: {} → {}/{}",
                    g.inst_name[k], d.cells[mi].name, target.0, target.1
                ),
            );
        }
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
        let mut cell = plan.cell[mi].clone();
        let master = &d.cells[mi];
        let src = match plan.how[mi] {
            How::Core | How::AnalogLib(_) => {
                devices::by_core(&master.name).and_then(|dv| dv.source)
            }
            _ => None,
        };
        let at = conv(
            aligned.get(&k).copied().unwrap_or(g.inst_at[k]),
            &format!("instance {}", g.inst_name[k]),
            c,
            notes,
        );
        id_prop(&mut cv, r, &g.inst_id[k]);
        match c.attr(rec, "ignore") {
            Some("true") => cv.props.push(Prop::string(r, "nlAction", "ignore")),
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
        let fresh = derived_digest(c, rec, master);
        for a in c.doc.attrs(rec) {
            let key = c.doc.attr_key(a);
            let val = c.doc.attr_value(a, 0).unwrap_or("");
            // A value Virtuoso derived goes back only while its inputs are the ones it came
            // from (D20 as amended 2026-10-15); a stale one is left to the callbacks.
            let (key, val) = match key {
                "cached" => match (c.doc.attr_value(a, 1), c.doc.attr_value(a, 2)) {
                    (Some(v), Some(d)) if fresh.as_deref() == Some(d) => (val, v),
                    _ => continue,
                },
                _ => (key, val),
            };
            match key {
                "at" | "name" | "ignore" => continue,
                "inherit" => {
                    let pin = val;
                    let net = c.doc.attr_value(a, 1).unwrap_or("");
                    // A netSet-typed property: the type the netlister reads as an
                    // inherited-connection override (G13).
                    cv.props.push(Prop::new(
                        r,
                        &vname(pin),
                        "netSet",
                        net_to_virtuoso(net, false).0,
                    ));
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
                    // An override equal to the core default, or of a param that only steers
                    // the ngspice netlist (`savecurrent`: Spectre saves through the test),
                    // loses nothing by staying behind.
                    None if NGSPICE_ONLY.contains(&key)
                        || master
                            .records(RecordKind::Param)
                            .any(|q| master.field(q, 0) == key && master.field(q, 1) == val) =>
                    {
                        continue;
                    }
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
                    cv.props
                        .push(Prop::string(r, "srcType", devices::WAVEFORMS[sp.wave].1));
                }
                for (pn, pv) in sp.params {
                    let ty = if pn == "tvpairs" { "int" } else { "string" };
                    cv.props.push(Prop::new(r, &pn, ty, pv));
                }
            }
        }
        if let Some(row) = row {
            for (p, v) in &row.sets {
                cv.props.push(Prop::string(r, p, v.clone()));
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
    // (id, anchor, net, global, orientation, the label record)
    type LabelOut<'a> = (Option<&'a str>, [i32; 2], String, bool, Orient, Option<u32>);
    let mut all_labels: Vec<LabelOut> = (0..g.label_at.len())
        .map(|k| {
            let o = c.orient(g.label_rec[k], 3);
            (
                Some(g.label_id[k].as_str()),
                g.label_at[k],
                g.label_net[k].clone(),
                g.label_global[k],
                o,
                Some(g.label_rec[k]),
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
    for (id, at, net, global, o, rec) in all_labels {
        // The Virtuoso instance name and library of the global symbol it came from.
        let vinst = rec.and_then(|r| c.attr(r, "vinst"));
        let vlib = rec.and_then(|r| c.attr(r, "vlib")).unwrap_or("analogLib");
        let r = ids.take();
        let (text, review) = net_to_virtuoso(&net, global);
        if review {
            notes.review("name", &c.path, format!("net `{net}` kept verbatim"));
        }
        let back = net_from_virtuoso(&text);
        if (back.text.as_str(), back.global) != (net.as_str(), global) {
            cv.props
                .push(Prop::string(r, "schemifyNet", read::net_prop(&net, global)));
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
                lib: vlib.into(),
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
                cv.props.push(Prop::string(sr, "schemifyStub", "1"));
                // A second label on this point (a label kept on a re-mastered pin) glues
                // here too instead of drawing an identical stub, which Virtuoso refuses.
                segs.push((sr, [at[0], at[1], to[0], to[1]]));
                sr
            }
        };
        if let Some(id) = id {
            id_prop(&mut cv, r, id);
        }
        // What a label from Virtuoso was there: a net expression (`vexpr <text>`) and a style
        // (`vstyle <justify> <font> <height>`), drawn so again.
        let vexpr = rec.and_then(|r| c.attr(r, "vexpr"));
        let vstyle: Vec<&str> = rec.map_or_else(Vec::new, |r| c.attr_values(r, "vstyle"));
        let (justify, font, height) = match vstyle[..] {
            [j, f, h] => (j, f, h.parse().unwrap_or(LABEL_HEIGHT)),
            _ => ("lowerLeft", "stick", LABEL_HEIGHT),
        };
        let kind = if vexpr.is_some() { "NLP" } else { "normal" };
        let text = vexpr.map_or(text, str::to_owned);
        let xy = conv(at, "label", c, notes);
        let style = ["wire", "label", justify, font, kind];
        cv.labels.push(label(r, parent, text, xy, o, height, style));
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
            .map_or("inputOutput", |q| dir_to_virtuoso(&own.dir[q]));
        let term = vname(pin);
        if net_from_virtuoso(&term).text != *pin {
            cv.props
                .push(Prop::string(r, "schemifyNet", read::net_prop(pin, false)));
        }
        let o = c.orient(g.port_rec[k], 3);
        id_prop(&mut cv, r, &g.port_id[k]);
        // A pin that came in as a bare pin shape (`vfig <w> <h>`) goes back as one.
        let fig = match c.attr_values(g.port_rec[k], "vfig")[..] {
            [w, h, ..] => parse_coord(w).ok().zip(parse_coord(h).ok()),
            _ => None,
        };
        if let Some((w, h)) = fig {
            let at = conv(g.port_at[k], "port", c, notes);
            cv.pinfigs.push(PinFig {
                r,
                term,
                b: [at[0] - w / 2, at[1] - h / 2, at[0] + w / 2, at[1] + h / 2],
                dir: dir.into(),
            });
            continue;
        }
        // The pin master it came in on (`vpin <lib> <cell> <view>`), else basic's for the
        // direction.
        let vpin: Vec<&str> = c.attr_values(g.port_rec[k], "vpin");
        let [lib, cell, view] = match vpin[..] {
            [l, c, v] => [l, c, v],
            _ => ["basic", profile::pin_master(dir), "symbol"],
        };
        cv.spins.push(Spin {
            r,
            term,
            dir: dir.into(),
            lib: lib.into(),
            cell: cell.into(),
            view: view.into(),
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
        let o = c.orient(r, 3);
        id_prop(&mut cv, lr, c.field(r, 0));
        // A label that came from Virtuoso on another layer or with another style (`vlabel
        // <layer> <purpose> <justify> <font> <kind>`) goes back as drawn.
        let v: Vec<&str> = c.attr_values(r, "vlabel");
        let [layer, purpose, justify, font, kind] = match v[..] {
            [a, b, j, f, k] => [a, b, j, f, k],
            _ => ["text", "drawing", "upperLeft", "stick", "normal"],
        };
        let xy = conv(c.point(r, 1), "text", c, notes);
        let (text, h) = (c.field(r, 4).to_owned(), height(c, r, TEXT_SIZE_MILLI));
        let style = [layer, purpose, justify, font, kind];
        cv.labels.push(label(lr, 0, text, xy, o, h, style));
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
    assert_eq!(inst_ref.len(), g.inst_id.len());
    cv
}

fn id_prop(cv: &mut Cv, r: u32, id: &str) {
    cv.props.push(Prop::string(r, "schemifyId", id));
}

/// The digest of instance record `rec`'s input values (its overrides, else `master`'s
/// defaults) over `master`'s header `vinputs`, as [`super::characterize::inputs_digest`]
/// computes it; `None` without `vinputs` or for an input that is not a constant.
fn derived_digest(c: &Cell, rec: u32, master: &Cell) -> Option<String> {
    let vinputs: Vec<String> = master
        .attr_values(0, "vinputs")
        .into_iter()
        .map(str::to_owned)
        .collect();
    if vinputs.is_empty() {
        return None;
    }
    super::characterize::inputs_digest(&vinputs, |n| {
        let p = master
            .records(RecordKind::Param)
            .find(|q| master.field(*q, 0) == n)?;
        let template = matches!(master.attr(p, "kind"), Some("string" | "choice"));
        Some((c.attr(rec, n).unwrap_or(master.field(p, 1)), template))
    })
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
    let prop = |ty: &str, value: String| Prop::new(r, pname, ty, value);
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
