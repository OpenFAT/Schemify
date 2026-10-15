//! `.sfyx` records → typed dump → Schemify files (spec `interop.md` §6, §7.3).
//!
//! Spine of [`import`]: `sfyx::read` → [`dump`] (records into per-kind tables, every
//! record checked, with its line) → interfaces of every dump cell (pins, params, simInfo
//! formats) → one fresh document per target file (`build`) → `ade::import` for benches
//! → `merge::merge` with the existing file or the export's stash → manifest rows → files.
//!
//! The dump model ([`Dump`], [`Cv`] and its rows) is shared with `write`, which builds the
//! same tables from a Design and serializes them.

use std::collections::{BTreeMap, BTreeSet};

use schemify_format::{Document, FileKind, IdRng, Orient, RecordKind, fresh_id, is_id, parse};

use super::ade;
use super::ael::{self, AelName};
use super::devices;
use super::{Import, ImportOptions};
use crate::design::Files;
use crate::map::{CORE_LIB, Mapping, ROOT_LIB};
use crate::merge;
use crate::notes::Notes;
use crate::profile::{self, net_from_virtuoso, point_from_virtuoso};
use crate::sfyx::{self, Error, Record, Value};

// Data design (six questions): in = one `.sfyx` text (≤ 2^30 bytes, ≤ 2^24 records); out
// = one Schemify document per design/ref cell. Rows per kind live in `Vec`s per
// cellview (existence-based: a kind with no records has an empty table); refs are the
// dump's integers, unique per cellview, looked up through a sorted index. Lifetime: one
// import call. Access: each kind is walked once in dump order by the builder.

/// Objects per cellview the reader accepts (format.md's rank-3 bound).
pub const OBJECTS_MAX: usize = 1 << 22;

/// A terminal (`term`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Term {
    /// Terminal name, Virtuoso syntax.
    pub name: String,
    /// `input output inputOutput switch …`.
    pub dir: String,
    /// Inherited-connection expression or `""`.
    pub netexpr: String,
}

/// A symbol pin rectangle (`pinfig`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PinFig {
    /// Ref.
    pub r: u32,
    /// Terminal name.
    pub term: String,
    /// `x1 y1 x2 y2`, 1/upi inch, y up.
    pub b: [i32; 4],
}

/// An instance (`inst`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inst {
    /// Ref.
    pub r: u32,
    /// Instance name, Virtuoso syntax.
    pub name: String,
    /// Master library.
    pub lib: String,
    /// Master cell.
    pub cell: String,
    /// Master view.
    pub view: String,
    /// Origin.
    pub xy: [i32; 2],
    /// Virtuoso orientation.
    pub orient: String,
}

/// A schematic pin (`spin`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spin {
    /// Ref.
    pub r: u32,
    /// Terminal name.
    pub term: String,
    /// Direction.
    pub dir: String,
    /// Pin master library.
    pub lib: String,
    /// Pin master cell.
    pub cell: String,
    /// Pin master view.
    pub view: String,
    /// Origin.
    pub xy: [i32; 2],
    /// Orientation.
    pub orient: String,
}

/// A wire figure (`wire`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wire {
    /// Ref.
    pub r: u32,
    /// Points (≥ 2).
    pub pts: Vec<[i32; 2]>,
    /// Width, 0 = narrow.
    pub width: i32,
}

/// A label (`label`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label {
    /// Ref.
    pub r: u32,
    /// Ref of the glued figure, 0 = none.
    pub parent: u32,
    /// Text.
    pub text: String,
    /// Origin.
    pub xy: [i32; 2],
    /// Orientation.
    pub orient: String,
    /// Justification.
    pub justify: String,
    /// Font.
    pub font: String,
    /// Height, 1/upi inch.
    pub height: i32,
    /// `normal`, `NLP` or `IL`.
    pub kind: String,
    /// Layer.
    pub layer: String,
    /// Purpose.
    pub purpose: String,
}

/// Any other figure (`shape`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    /// Ref.
    pub r: u32,
    /// `line rect polygon ellipse arc path`.
    pub kind: String,
    /// Layer.
    pub layer: String,
    /// Purpose.
    pub purpose: String,
    /// Points, box, or box plus angles (micro-radians).
    pub coords: Vec<i32>,
    /// Width.
    pub width: i32,
}

/// A property (`prop`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prop {
    /// Ref of the owner, 0 = the cellview.
    pub r: u32,
    /// Name.
    pub name: String,
    /// `string int float boolean ILList`.
    pub ty: String,
    /// Value text.
    pub value: String,
}

/// A loader-only connection request (`conn`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conn {
    /// Instance ref.
    pub r: u32,
    /// Terminal.
    pub term: String,
    /// Net, Virtuoso syntax.
    pub net: String,
}

/// One cellview and its objects, one table per record kind.
#[derive(Clone, Debug, Default)]
pub struct Cv {
    /// View name.
    pub view: String,
    /// `schematic` or `schematicSymbol`.
    pub vtype: String,
    /// Units per inch.
    pub upi: i32,
    /// Terminals.
    pub terms: Vec<Term>,
    /// Symbol pin figures.
    pub pinfigs: Vec<PinFig>,
    /// Instances.
    pub insts: Vec<Inst>,
    /// Schematic pins.
    pub spins: Vec<Spin>,
    /// Wires.
    pub wires: Vec<Wire>,
    /// Labels.
    pub labels: Vec<Label>,
    /// Shapes.
    pub shapes: Vec<Shape>,
    /// Properties.
    pub props: Vec<Prop>,
    /// Every ref an object record of this cellview defined (§4.2: unique per cellview).
    pub refs: BTreeSet<u32>,
    /// Connection requests.
    pub conns: Vec<Conn>,
    /// `intent` records (§10), kept whole.
    pub intents: Vec<Record>,
    /// Records with a tag this reader does not know, kept for the report.
    pub unknown: Vec<Record>,
}

impl Cv {
    /// Object records in this cellview (every row of every kind).
    pub fn objects(&self) -> usize {
        self.terms.len()
            + self.pinfigs.len()
            + self.insts.len()
            + self.spins.len()
            + self.wires.len()
            + self.labels.len()
            + self.shapes.len()
            + self.props.len()
            + self.conns.len()
            + self.intents.len()
            + self.unknown.len()
    }

    /// Value of property `name` of object `r`.
    pub fn prop(&self, r: u32, name: &str) -> Option<&str> {
        self.props
            .iter()
            .find(|p| p.r == r && p.name == name)
            .map(|p| p.value.as_str())
    }
}

/// One CDF parameter (`param`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CdfParam {
    /// Name.
    pub name: String,
    /// `string int float boolean radio cyclic button netSet`.
    pub ty: String,
    /// Default value text.
    pub def: String,
    /// Prompt.
    pub prompt: String,
    /// Units name or `""`.
    pub units: String,
    /// Flags (`parseAsNumber parseAsCEL storeDefault dontSave hidden`).
    pub flags: Vec<String>,
    /// Choices.
    pub choices: Vec<String>,
    /// Callback text.
    pub callback: String,
}

/// A simInfo value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimVal {
    /// A string or symbol name.
    Str(String),
    /// A list of names (`termOrder`, `instParameters`, `otherParameters`).
    List(Vec<String>),
    /// Pairs (`propMapping`, `termMapping`).
    Pairs(Vec<(String, String)>),
}

/// One simulator's simInfo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimInfo {
    /// Simulator (`spectre`, `auCdl`, …).
    pub sim: String,
    /// `(key, value)` in record order.
    pub fields: Vec<(String, SimVal)>,
}

impl SimInfo {
    /// Field `key`.
    pub fn get(&self, key: &str) -> Option<&SimVal> {
        self.fields.iter().find(|f| f.0 == key).map(|f| &f.1)
    }

    /// Field `key` as a string.
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            SimVal::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Field `key` as a list (a string field is a one-element list).
    pub fn list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(SimVal::List(v)) => v.clone(),
            Some(SimVal::Str(s)) if !s.is_empty() && s != "nil" => vec![s.clone()],
            _ => Vec::new(),
        }
    }

    /// Field `key` as pairs.
    pub fn pairs(&self, key: &str) -> Vec<(String, String)> {
        match self.get(key) {
            Some(SimVal::Pairs(v)) => v.clone(),
            _ => Vec::new(),
        }
    }
}

/// A cell's base CDF.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cdf {
    /// `paramLabelSet`.
    pub label_set: String,
    /// Parameters, CDF order.
    pub params: Vec<CdfParam>,
    /// simInfo per simulator.
    pub siminfo: Vec<SimInfo>,
}

impl Cdf {
    /// simInfo of `sim`.
    pub fn sim(&self, sim: &str) -> Option<&SimInfo> {
        self.siminfo.iter().find(|s| s.sim == sim)
    }
}

/// A text view (`textview`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextView {
    /// View name.
    pub view: String,
    /// Master file name.
    pub file: String,
    /// Content.
    pub text: String,
}

/// One cell of a dump.
#[derive(Clone, Debug, Default)]
pub struct DCell {
    /// Library.
    pub lib: String,
    /// Cell.
    pub name: String,
    /// Role `"design"` (true) or `"ref"`.
    pub design: bool,
    /// Base CDF.
    pub cdf: Option<Cdf>,
    /// Cellviews in dump order.
    pub cvs: Vec<Cv>,
    /// Text views.
    pub textviews: Vec<TextView>,
    /// Line of the `cell` record.
    pub line: u32,
}

impl DCell {
    /// The cellview named `view`.
    pub fn cv(&self, view: &str) -> Option<&Cv> {
        self.cvs.iter().find(|c| c.view == view)
    }
}

/// A parsed `.sfyx` file.
#[derive(Clone, Debug, Default)]
pub struct Dump {
    /// The producer string of the header.
    pub producer: String,
    /// Cells in dump order.
    pub cells: Vec<DCell>,
    /// Maestro tests: each starts with its `maestro` record.
    pub maestro: Vec<Vec<Record>>,
    /// Records outside any cellview with a tag this reader does not know.
    pub unknown: Vec<Record>,
}

/// The CDF parameter types a `param` record may carry.
pub const CDF_TYPES: [&str; 8] = [
    "string", "int", "float", "boolean", "radio", "cyclic", "button", "netSet",
];

/// The maestro-context record tags (§4.2, owned by `ade`).
const ADE_TAGS: [&str; 8] = [
    "maestro", "var", "analysis", "corner", "model", "output", "spec", "defn",
];

fn err(r: &Record, msg: impl Into<String>) -> Error {
    Error {
        line: r.line,
        msg: format!("{}: {}", r.tag, msg.into()),
    }
}

fn arity(r: &Record, n: usize) -> Result<(), Error> {
    if r.fields.len() == n {
        Ok(())
    } else {
        Err(err(r, format!("{} fields, expected {n}", r.fields.len())))
    }
}

fn ints(r: &Record, v: &[Value], what: &str) -> Result<Vec<i32>, Error> {
    v.iter()
        .map(|x| {
            x.as_int()
                .ok_or_else(|| err(r, format!("{what} holds a non-integer")))
        })
        .collect()
}

fn strs(r: &Record, v: &[Value], what: &str) -> Result<Vec<String>, Error> {
    v.iter()
        .map(|x| {
            x.as_str()
                .map(str::to_owned)
                .ok_or_else(|| err(r, format!("{what} holds a non-string")))
        })
        .collect()
}

fn orient(r: &Record, i: usize) -> Result<String, Error> {
    let o = r.str(i)?;
    profile::orient_from_virtuoso(o)
        .map(|_| o.to_owned())
        .ok_or_else(|| err(r, format!("unknown orientation `{o}`")))
}

/// Parses records into a [`Dump`]: every record's arity and element types, its context
/// (an object needs a cellview, a cellview a cell, a cell a library, `param`/`siminfo` a
/// `cdf`), positive refs unique per cellview, glued parents and property owners that
/// exist. Unknown tags are kept (in their cellview, else file-level) for the report.
///
/// # Errors
///
/// The first malformed record, with its line; a missing header or `("end")`.
pub fn dump(records: &[Record]) -> Result<Dump, Error> {
    sfyx::require_complete(records)?;
    let mut d = Dump {
        producer: records[0].str(1).unwrap_or("").to_owned(),
        ..Dump::default()
    };
    let mut lib: Option<String> = None;
    let mut in_cell = false;
    let mut in_cdf = false;
    let mut in_maestro = false;
    let mut cells_seen: BTreeSet<(String, String)> = BTreeSet::new();
    for r in &records[1..records.len() - 1] {
        let tag = r.tag.as_str();
        if ADE_TAGS.contains(&tag) {
            if tag == "maestro" {
                in_maestro = true;
                d.maestro.push(vec![r.clone()]);
            } else if in_maestro {
                d.maestro.last_mut().expect("in maestro").push(r.clone());
            } else {
                return Err(err(r, "outside a maestro context"));
            }
            continue;
        }
        if in_maestro
            && (is_object(tag)
                || matches!(
                    tag,
                    "lib" | "cell" | "cv" | "cdf" | "param" | "siminfo" | "textview"
                ))
        {
            return Err(err(r, "cell records after the maestro records"));
        }
        match tag {
            "lib" => {
                arity(r, 1)?;
                lib = Some(r.str(0)?.to_owned());
                in_cell = false;
                in_cdf = false;
            }
            "cell" => {
                arity(r, 2)?;
                let l = lib.clone().ok_or_else(|| err(r, "before any `lib`"))?;
                let design = match r.str(1)? {
                    "design" => true,
                    "ref" => false,
                    o => return Err(err(r, format!("role `{o}` (design or ref)"))),
                };
                let name = r.str(0)?.to_owned();
                if !cells_seen.insert((l.clone(), name.clone())) {
                    return Err(err(r, format!("{l}/{name} appears twice")));
                }
                d.cells.push(DCell {
                    lib: l,
                    name,
                    design,
                    line: r.line,
                    ..DCell::default()
                });
                in_cell = true;
                in_cdf = false;
            }
            "cdf" | "param" | "siminfo" | "textview" | "cv" if !in_cell => {
                return Err(err(r, "outside a cell"));
            }
            "cdf" => {
                arity(r, 1)?;
                let c = d.cells.last_mut().expect("in cell");
                if c.cdf.is_some() {
                    return Err(err(r, "second cdf"));
                }
                c.cdf = Some(Cdf {
                    label_set: r.text(0)?,
                    ..Cdf::default()
                });
                in_cdf = true;
            }
            "param" | "siminfo" if !in_cdf => return Err(err(r, "before the cell's `cdf`")),
            "param" => {
                arity(r, 8)?;
                let ty = r.str(1)?;
                if !CDF_TYPES.contains(&ty) {
                    return Err(err(r, format!("type `{ty}`")));
                }
                let p = CdfParam {
                    name: r.str(0)?.to_owned(),
                    ty: ty.to_owned(),
                    def: r.str(2)?.to_owned(),
                    prompt: r.str(3)?.to_owned(),
                    units: r.str(4)?.to_owned(),
                    flags: strs(r, r.list(5)?, "flags")?,
                    choices: strs(r, r.list(6)?, "choices")?,
                    callback: r.text(7)?,
                };
                let cdf = d.cells.last_mut().unwrap().cdf.as_mut().unwrap();
                if cdf.params.iter().any(|x| x.name == p.name) {
                    return Err(err(r, format!("param {} twice", p.name)));
                }
                cdf.params.push(p);
            }
            "siminfo" => {
                arity(r, 2)?;
                let mut s = SimInfo {
                    sim: r.str(0)?.to_owned(),
                    fields: Vec::new(),
                };
                for f in r.list(1)? {
                    let kv = f.items();
                    let (Some(Value::Str(k)), Some(v), 2) = (kv.first(), kv.get(1), kv.len())
                    else {
                        return Err(err(r, "a field is not (key value)"));
                    };
                    let val = match v {
                        Value::Str(s) => SimVal::Str(s.clone()),
                        Value::List(items)
                            if items.iter().all(|x| matches!(x, Value::List(_)))
                                && !items.is_empty() =>
                        {
                            let mut pairs = Vec::new();
                            for p in items {
                                let pv = p.items();
                                match (pv.first(), pv.get(1), pv.len()) {
                                    (Some(Value::Str(a)), Some(Value::Str(b)), 2) => {
                                        pairs.push((a.clone(), b.clone()))
                                    }
                                    _ => {
                                        return Err(err(
                                            r,
                                            format!("{k}: a pair is not (name name)"),
                                        ));
                                    }
                                }
                            }
                            SimVal::Pairs(pairs)
                        }
                        Value::List(items) => SimVal::List(strs(r, items, k)?),
                        Value::Int(_) => return Err(err(r, format!("{k}: an integer value"))),
                    };
                    s.fields.push((k.clone(), val));
                }
                let cdf = d.cells.last_mut().unwrap().cdf.as_mut().unwrap();
                if cdf.sim(&s.sim).is_some() {
                    return Err(err(r, format!("second {} simInfo", s.sim)));
                }
                cdf.siminfo.push(s);
            }
            "textview" => {
                arity(r, 3)?;
                d.cells.last_mut().unwrap().textviews.push(TextView {
                    view: r.str(0)?.to_owned(),
                    file: r.str(1)?.to_owned(),
                    text: r.text(2)?,
                });
            }
            "cv" => {
                arity(r, 3)?;
                let upi = r.int(2)?;
                if upi <= 0 {
                    return Err(err(r, "units per inch must be positive"));
                }
                let c = d.cells.last_mut().unwrap();
                let view = r.str(0)?.to_owned();
                if c.cv(&view).is_some() {
                    return Err(err(r, format!("view {view} twice")));
                }
                let vtype = r.str(1)?;
                if !c.design && vtype == "schematic" {
                    return Err(err(r, "a ref cell carries its symbol and CDF only (§4.2)"));
                }
                if vtype != "schematic" && vtype != "schematicSymbol" {
                    return Err(err(r, format!("view type `{vtype}`")));
                }
                c.cvs.push(Cv {
                    view,
                    vtype: vtype.to_owned(),
                    upi,
                    ..Cv::default()
                });
                in_cdf = false;
            }
            _ => {
                let Some(cv) = (if in_cell {
                    d.cells.last_mut().and_then(|c| c.cvs.last_mut())
                } else {
                    None
                }) else {
                    if is_object(tag) {
                        return Err(err(r, "outside a cellview"));
                    }
                    d.unknown.push(r.clone());
                    continue;
                };
                if cv.objects() >= OBJECTS_MAX {
                    return Err(err(
                        r,
                        format!("more than {OBJECTS_MAX} objects in a cellview"),
                    ));
                }
                object(cv, r)?;
            }
        }
    }
    for c in &d.cells {
        for cv in &c.cvs {
            check_refs(cv).map_err(|m| Error {
                line: c.line,
                msg: format!("{}/{} {}: {m}", c.lib, c.name, cv.view),
            })?;
        }
    }
    for t in &d.maestro {
        ade::check(t)?;
    }
    Ok(d)
}

fn is_object(tag: &str) -> bool {
    matches!(
        tag,
        "term"
            | "pinfig"
            | "inst"
            | "spin"
            | "wire"
            | "label"
            | "shape"
            | "prop"
            | "conn"
            | "intent"
    )
}

fn object(cv: &mut Cv, r: &Record) -> Result<(), Error> {
    let pos = |r: &Record, i: usize| -> Result<u32, Error> {
        let v = r.int(i)?;
        u32::try_from(v)
            .ok()
            .filter(|v| *v > 0)
            .ok_or_else(|| err(r, "refs are positive"))
    };
    // Objects belong to one kind of cellview (§4.2, §6.2): terminals and pin figures to a
    // symbol (a schematic's terminals travel as `spin`s), wires, schematic pins and loader
    // records to a schematic.
    let schematic_only = matches!(r.tag.as_str(), "spin" | "wire" | "conn" | "intent");
    let symbol_only = matches!(r.tag.as_str(), "term" | "pinfig");
    if (schematic_only && cv.vtype != "schematic") || (symbol_only && cv.vtype != "schematicSymbol")
    {
        return Err(err(
            r,
            format!("`{}` does not belong in a {} cellview", r.tag, cv.vtype),
        ));
    }
    // An instance inside a symbol (a pin master placed without flattening) has no Schemify
    // form: it is reported, not refused, since a real symbol may hold one.
    if r.tag == "inst" && cv.vtype == "schematicSymbol" {
        cv.unknown.push(r.clone());
        return Ok(());
    }
    // A record that defines an object must not reuse a ref (a duplicated line would
    // otherwise become two objects of one identity).
    if matches!(
        r.tag.as_str(),
        "pinfig" | "inst" | "spin" | "wire" | "label" | "shape"
    ) {
        let ref_ = pos(r, 0)?;
        if !cv.refs.insert(ref_) {
            return Err(err(r, format!("ref {ref_} defined twice in this cellview")));
        }
    }
    match r.tag.as_str() {
        "term" => {
            arity(r, 3)?;
            let name = r.str(0)?.to_owned();
            if cv.terms.iter().any(|t| t.name == name) {
                return Err(err(r, format!("terminal {name} twice")));
            }
            cv.terms.push(Term {
                name,
                dir: r.str(1)?.to_owned(),
                netexpr: r.str(2)?.to_owned(),
            });
        }
        "pinfig" => {
            arity(r, 6)?;
            cv.pinfigs.push(PinFig {
                r: pos(r, 0)?,
                term: r.str(1)?.to_owned(),
                b: [r.int(2)?, r.int(3)?, r.int(4)?, r.int(5)?],
            });
        }
        "inst" => {
            arity(r, 8)?;
            cv.insts.push(Inst {
                r: pos(r, 0)?,
                name: r.str(1)?.to_owned(),
                lib: r.str(2)?.to_owned(),
                cell: r.str(3)?.to_owned(),
                view: r.str(4)?.to_owned(),
                xy: [r.int(5)?, r.int(6)?],
                orient: orient(r, 7)?,
            });
        }
        "spin" => {
            arity(r, 9)?;
            cv.spins.push(Spin {
                r: pos(r, 0)?,
                term: r.str(1)?.to_owned(),
                dir: r.str(2)?.to_owned(),
                lib: r.str(3)?.to_owned(),
                cell: r.str(4)?.to_owned(),
                view: r.str(5)?.to_owned(),
                xy: [r.int(6)?, r.int(7)?],
                orient: orient(r, 8)?,
            });
        }
        "wire" => {
            arity(r, 3)?;
            let p = ints(r, r.list(1)?, "points")?;
            if p.len() < 4 || p.len() % 2 != 0 {
                return Err(err(r, "points: an even count of at least 4"));
            }
            cv.wires.push(Wire {
                r: pos(r, 0)?,
                pts: p.chunks(2).map(|c| [c[0], c[1]]).collect(),
                width: r.int(2)?,
            });
        }
        "label" => {
            arity(r, 12)?;
            let kind = r.str(9)?;
            if !matches!(kind, "normal" | "NLP" | "IL") {
                return Err(err(r, format!("kind `{kind}`")));
            }
            let parent = r.int(1)?;
            cv.labels.push(Label {
                r: pos(r, 0)?,
                parent: u32::try_from(parent).map_err(|_| err(r, "negative parent"))?,
                text: r.str(2)?.to_owned(),
                xy: [r.int(3)?, r.int(4)?],
                orient: orient(r, 5)?,
                justify: r.str(6)?.to_owned(),
                font: r.str(7)?.to_owned(),
                height: r.int(8)?,
                kind: kind.to_owned(),
                layer: r.str(10)?.to_owned(),
                purpose: r.str(11)?.to_owned(),
            });
        }
        "shape" => {
            arity(r, 6)?;
            let kind = r.str(1)?;
            let coords = ints(r, r.list(4)?, "coords")?;
            let ok = match kind {
                "line" | "path" => coords.len() >= 4 && coords.len() % 2 == 0,
                "polygon" => coords.len() >= 6 && coords.len() % 2 == 0,
                "rect" | "ellipse" => coords.len() == 4,
                "arc" => coords.len() == 6,
                _ => return Err(err(r, format!("kind `{kind}`"))),
            };
            if !ok {
                return Err(err(r, format!("{} coordinates for a {kind}", coords.len())));
            }
            cv.shapes.push(Shape {
                r: pos(r, 0)?,
                kind: kind.to_owned(),
                layer: r.str(2)?.to_owned(),
                purpose: r.str(3)?.to_owned(),
                coords,
                width: r.int(5)?,
            });
        }
        "prop" => {
            arity(r, 4)?;
            let ty = r.str(2)?;
            if !matches!(
                ty,
                "string" | "int" | "float" | "boolean" | "ILList" | "netSet"
            ) {
                return Err(err(r, format!("type `{ty}`")));
            }
            let owner = r.int(0)?;
            cv.props.push(Prop {
                r: u32::try_from(owner).map_err(|_| err(r, "negative owner"))?,
                name: r.str(1)?.to_owned(),
                ty: ty.to_owned(),
                value: r.str(3)?.to_owned(),
            });
        }
        "conn" => {
            arity(r, 3)?;
            cv.conns.push(Conn {
                r: pos(r, 0)?,
                term: r.str(1)?.to_owned(),
                net: r.str(2)?.to_owned(),
            });
        }
        "intent" => {
            arity(r, 3)?;
            pos(r, 0)?;
            cv.intents.push(r.clone());
        }
        _ => cv.unknown.push(r.clone()),
    }
    Ok(())
}

/// Refs unique; parents, owners and connection targets exist.
fn check_refs(cv: &Cv) -> Result<(), String> {
    let mut refs: Vec<u32> = cv
        .pinfigs
        .iter()
        .map(|x| x.r)
        .chain(cv.insts.iter().map(|x| x.r))
        .chain(cv.spins.iter().map(|x| x.r))
        .chain(cv.wires.iter().map(|x| x.r))
        .chain(cv.labels.iter().map(|x| x.r))
        .chain(cv.shapes.iter().map(|x| x.r))
        .collect();
    let n = refs.len();
    refs.sort_unstable();
    refs.dedup();
    if refs.len() != n {
        return Err("a ref is used twice".into());
    }
    let has = |r: u32| refs.binary_search(&r).is_ok();
    for l in &cv.labels {
        if l.parent != 0 && !has(l.parent) {
            return Err(format!("label {} glued to unknown ref {}", l.r, l.parent));
        }
    }
    for p in &cv.props {
        if p.r != 0 && !has(p.r) {
            return Err(format!("property {} of unknown ref {}", p.name, p.r));
        }
    }
    for c in &cv.conns {
        if !cv.insts.iter().any(|i| i.r == c.r) {
            return Err(format!("conn of unknown instance {}", c.r));
        }
    }
    for f in &cv.pinfigs {
        if !cv.terms.iter().any(|t| t.name == f.term) {
            return Err(format!("pinfig {} of unknown terminal {}", f.r, f.term));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Import: dump → Schemify documents
// ---------------------------------------------------------------------------------------

/// Where one dump cell lands.
#[derive(Clone, Debug, Default)]
struct Target {
    /// A file is produced.
    write: bool,
    /// Schemify library (`core` for the bundled cells).
    slib: String,
    /// Project-relative directory.
    dir: String,
    /// File stem.
    stem: String,
    /// File kind.
    kind: Option<FileKind>,
}

impl Target {
    fn path(&self) -> String {
        let ext = self.kind.map_or("shprim", FileKind::word);
        if self.dir.is_empty() {
            format!("{}.{ext}", self.stem)
        } else {
            format!("{}/{}.{ext}", self.dir, self.stem)
        }
    }
}

/// A pin of an interface, Schemify spelling and coordinates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PinRow {
    name: String,
    dir: String,
    at: [i32; 2],
    inherit: Option<String>,
    alias: Option<String>,
}

/// What instances of a master need: pins (port order), params with their Schemify kinds
/// and defaults, and the netSet property name of each inherited pin.
#[derive(Clone, Debug, Default)]
struct Iface {
    pins: Vec<PinRow>,
    /// `(name, kind, default)`.
    params: Vec<(String, String, String)>,
    /// `(virtuoso netSet prop, schemify pin)`.
    inherit_props: Vec<(String, String)>,
}

impl Iface {
    fn kind(&self, p: &str) -> Option<&str> {
        self.params.iter().find(|x| x.0 == p).map(|x| x.1.as_str())
    }
    fn def(&self, p: &str) -> Option<&str> {
        self.params.iter().find(|x| x.0 == p).map(|x| x.2.as_str())
    }
}

/// One import's lookup tables.
struct Ctx<'a> {
    dump: &'a Dump,
    map: &'a Mapping,
    opts: &'a ImportOptions,
    existing: &'a Files,
    /// Schemify library → directory (manifest, then options).
    libs: BTreeMap<String, String>,
    targets: Vec<Target>,
    ifaces: Vec<Option<Iface>>,
    /// (lib, cell) of every cell a maestro test names.
    benches: BTreeSet<(String, String)>,
}

/// Schemify library of Virtuoso library `vlib`.
fn slib_of(vlib: &str, opts: &ImportOptions) -> String {
    if let Some(s) = opts.lib_map.get(vlib) {
        return s.clone();
    }
    match vlib {
        ROOT_LIB => String::new(),
        CORE_LIB => "core".into(),
        l => l.into(),
    }
}

fn consumed(lib: &str, cell: &str) -> bool {
    (lib == "basic" && matches!(cell, "ipin" | "opin" | "iopin" | "noConn"))
        || devices::is_global_symbol(lib, cell)
}

impl<'a> Ctx<'a> {
    fn new(
        dump: &'a Dump,
        existing: &'a Files,
        map: &'a Mapping,
        opts: &'a ImportOptions,
        notes: &mut Notes,
    ) -> Ctx<'a> {
        let mut libs = existing
            .get("schemify.project")
            .and_then(|m| crate::design::manifest_libs(m).ok())
            .unwrap_or_default();
        for (l, d) in &opts.lib_dir {
            libs.entry(l.clone()).or_insert_with(|| d.clone());
        }
        let benches = dump
            .maestro
            .iter()
            .filter_map(|t| {
                let m = t.first()?;
                Some((m.str(1).ok()?.to_owned(), m.str(2).ok()?.to_owned()))
            })
            .collect();
        let mut ctx = Ctx {
            dump,
            map,
            opts,
            existing,
            libs,
            targets: Vec::new(),
            ifaces: Vec::new(),
            benches,
        };
        for k in 0..dump.cells.len() {
            let t = ctx.target(k, notes);
            ctx.targets.push(t);
        }
        for k in 0..dump.cells.len() {
            let i = ctx.iface_of_dump(k);
            ctx.ifaces.push(i);
        }
        ctx
    }

    fn dir_of(&self, slib: &str) -> String {
        if slib.is_empty() {
            return String::new();
        }
        self.libs
            .get(slib)
            .cloned()
            .unwrap_or_else(|| slib.to_owned())
    }

    fn target(&self, k: usize, notes: &mut Notes) -> Target {
        let c = &self.dump.cells[k];
        let slib = slib_of(&c.lib, self.opts);
        let mut t = Target {
            dir: self.dir_of(&slib),
            slib,
            stem: c.name.clone(),
            ..Target::default()
        };
        if t.slib == "core" || consumed(&c.lib, &c.name) {
            return t;
        }
        let sym = c.cv("symbol").is_some();
        let sch = c.cv("schematic");
        t.kind = if !c.design {
            Some(FileKind::Shprim)
        } else if self.benches.contains(&(c.lib.clone(), c.name.clone())) {
            Some(FileKind::Shbench)
        } else if sym && sch.is_some() {
            Some(FileKind::Shcomp)
        } else if sym {
            Some(FileKind::Shprim)
        } else if let Some(s) = sch {
            Some(if s.spins.is_empty() {
                FileKind::Shbench
            } else {
                FileKind::Shcomp
            })
        } else {
            notes.review(
                "format",
                &format!("{}/{}", c.lib, c.name),
                "no symbol or schematic view: not imported",
            );
            None
        };
        t.write = t.kind.is_some();
        t
    }

    /// The dump index of `lib/cell`.
    fn find(&self, lib: &str, cell: &str) -> Option<usize> {
        self.dump
            .cells
            .iter()
            .position(|c| c.lib == lib && c.name == cell)
    }

    fn iface_of_dump(&self, k: usize) -> Option<Iface> {
        let c = &self.dump.cells[k];
        let mut i = Iface::default();
        if let Some(sym) = c.cv("symbol") {
            i.pins = pins_in(c, sym, &mut Notes::default());
            for p in &i.pins {
                if p.inherit.is_some() {
                    i.inherit_props
                        .push((super::write::vname(&p.name), p.name.clone()));
                }
            }
        }
        if let Some(sch) = c.cv("schematic") {
            for l in &sch.labels {
                if let Some((p, d)) = parse_netexpr(&l.text) {
                    let name = net_from_virtuoso(&p).text;
                    if !i.pins.iter().any(|x| x.name == name) {
                        i.inherit_props.push((p.clone(), name.clone()));
                        i.pins.push(PinRow {
                            name,
                            dir: "inout".into(),
                            at: [0, 0],
                            inherit: Some(d),
                            alias: None,
                        });
                    }
                }
            }
        }
        if let Some(cdf) = &c.cdf {
            for p in params_in(c, cdf, &mut Notes::default()) {
                i.params.push((p.name, p.kind, p.default));
            }
            for p in cdf.params.iter().filter(|p| p.ty == "netSet") {
                if let Some(pin) = i
                    .pins
                    .iter()
                    .find(|x| super::write::vname(&x.name) == p.name)
                {
                    let pn = pin.name.clone();
                    if !i.inherit_props.iter().any(|x| x.0 == p.name) {
                        i.inherit_props.push((p.name.clone(), pn));
                    }
                }
            }
        }
        Some(i)
    }

    /// Interface of a Schemify cell file text.
    fn iface_of_text(bytes: &[u8]) -> Option<Iface> {
        let d = parse(bytes).ok()?;
        let mut i = Iface::default();
        for r in 1..d.record_count() {
            match d.record_kind(r) {
                RecordKind::Pin => {
                    let attr = |k: &str| {
                        d.find_attr(r, k)
                            .and_then(|a| d.attr_value(a, 0))
                            .map(str::to_owned)
                    };
                    let row = PinRow {
                        name: d.field(r, 0)?.to_owned(),
                        dir: d.field(r, 1)?.to_owned(),
                        at: [
                            schemify_format::parse_coord(d.field(r, 2)?).ok()?,
                            schemify_format::parse_coord(d.field(r, 3)?).ok()?,
                        ],
                        inherit: attr("inherit"),
                        alias: attr("alias"),
                    };
                    if row.inherit.is_some() {
                        i.inherit_props
                            .push((super::write::vname(&row.name), row.name.clone()));
                    }
                    i.pins.push(row);
                }
                RecordKind::Param => {
                    let kind = d
                        .find_attr(r, "kind")
                        .and_then(|a| d.attr_value(a, 0))
                        .unwrap_or("number");
                    i.params.push((
                        d.field(r, 0)?.to_owned(),
                        kind.to_owned(),
                        d.field(r, 1)?.to_owned(),
                    ));
                }
                _ => {}
            }
        }
        Some(i)
    }

    /// Where an instance of `lib/cell` points and what its master looks like.
    fn master(&self, lib: &str, cell: &str) -> Master {
        if let Some(row) = self.map.by_virtuoso(lib, cell) {
            let (sl, stem) = row.cellref.split_once(':').unwrap_or(("", &row.cellref));
            return Master {
                slib: sl.to_owned(),
                dir: self.dir_of(sl),
                stem: stem.to_owned(),
                iface: self.project_iface(sl, stem),
                row: self
                    .map
                    .maps
                    .iter()
                    .position(|r| r.lib == lib && r.cell == cell),
                ..Master::default()
            };
        }
        if self.opts.analoglib
            && lib == "analogLib"
            && let Some((dv, wave)) = devices::by_alib(cell)
        {
            return Master {
                slib: "core".into(),
                stem: dv.core.into(),
                iface: self.project_iface("core", dv.core),
                alib: Some((
                    devices::DEVICES
                        .iter()
                        .position(|x| x.core == dv.core)
                        .expect("row"),
                    wave,
                )),
                ..Master::default()
            };
        }
        let slib = slib_of(lib, self.opts);
        if slib == "core" {
            return Master {
                stem: cell.into(),
                iface: self.project_iface("core", cell),
                source: devices::by_core(cell).and_then(|d| d.source),
                slib,
                ..Master::default()
            };
        }
        let iface = match self.find(lib, cell) {
            Some(k) => self.ifaces[k].clone(),
            None => self.project_iface(&slib, cell),
        };
        Master {
            dir: self.dir_of(&slib),
            slib,
            stem: cell.into(),
            iface,
            ..Master::default()
        }
    }

    fn project_iface(&self, slib: &str, stem: &str) -> Option<Iface> {
        if slib == "core" {
            let f = format!("{stem}.shprim");
            return schemify_doc::core_library()
                .iter()
                .find(|(n, _)| *n == f)
                .and_then(|(_, b)| Ctx::iface_of_text(b));
        }
        let dir = self.dir_of(slib);
        ["shcomp", "shprim"].iter().find_map(|ext| {
            let p = if dir.is_empty() {
                format!("{stem}.{ext}")
            } else {
                format!("{dir}/{stem}.{ext}")
            };
            self.existing.get(&p).and_then(|b| Ctx::iface_of_text(b))
        })
    }
}

/// A resolved instance master.
#[derive(Clone, Debug, Default)]
struct Master {
    slib: String,
    dir: String,
    stem: String,
    iface: Option<Iface>,
    /// `map` row (reverse lookup).
    row: Option<usize>,
    /// analogLib → core re-mastering: device row, waveform.
    alib: Option<(usize, usize)>,
    /// A core source: current (true) or voltage.
    source: Option<bool>,
}

impl Master {
    fn cellref(&self, from_dir: &str) -> Option<String> {
        if self.slib == "core" {
            return Some(format!("core:{}", self.stem));
        }
        if self.dir == from_dir {
            Some(format!("./{}", self.stem))
        } else if !self.slib.is_empty() {
            Some(format!("{}:{}", self.slib, self.stem))
        } else {
            None
        }
    }
}

/// Converts a dump point, reporting a rounded conversion.
fn pt(xy: [i32; 2], upi: i32, what: &str, cell: &str, notes: &mut Notes) -> [i32; 2] {
    match point_from_virtuoso(xy[0], xy[1], upi) {
        Some(c) => {
            if !c.exact {
                notes.add(
                    "rounding",
                    cell,
                    format!("{what} at ({}, {})", xy[0], xy[1]),
                );
            }
            c.p
        }
        None => {
            notes.review("name", cell, format!("{what}: coordinate out of range"));
            [0, 0]
        }
    }
}

/// The centre of a box (exact through doubling).
fn centre(b: [i32; 4], upi: i32, what: &str, cell: &str, notes: &mut Notes) -> [i32; 2] {
    let x = i64::from(b[0]) + i64::from(b[2]);
    let y = i64::from(b[1]) + i64::from(b[3]);
    match (i32::try_from(x), i32::try_from(y), upi.checked_mul(2)) {
        (Ok(x), Ok(y), Some(u2)) => pt([x, y], u2, what, cell, notes),
        _ => pt([b[0], b[1]], upi, what, cell, notes),
    }
}

/// The symbol pins of a dump cell (§6.2 symbol pin row): one pin per terminal at its first
/// pin rectangle, further rectangles as alias pins, in spectre `termOrder` else sorted.
fn pins_in(c: &DCell, sym: &Cv, notes: &mut Notes) -> Vec<PinRow> {
    let cell = format!("{}/{}", c.lib, c.name);
    let order: Vec<String> = c
        .cdf
        .as_ref()
        .and_then(|d| d.sim("spectre"))
        .map(|s| s.list("termOrder"))
        .unwrap_or_default();
    let mut terms: Vec<&Term> = sym.terms.iter().collect();
    let all_listed = !order.is_empty() && terms.iter().all(|t| order.contains(&t.name));
    if all_listed {
        terms.sort_by_key(|t| order.iter().position(|o| *o == t.name).expect("listed"));
    } else {
        terms.sort_by(|a, b| a.name.cmp(&b.name));
    }
    let mut out = Vec::new();
    for t in terms {
        let net = net_from_virtuoso(&t.name);
        if net.review {
            notes.review(
                "name",
                &cell,
                format!("terminal `{}` kept verbatim", t.name),
            );
        }
        let dir = match t.dir.as_str() {
            "input" => "in",
            "output" => "out",
            _ => "inout",
        };
        let inherit = parse_netexpr(&t.netexpr).map(|(_, d)| d);
        let figs: Vec<&PinFig> = sym.pinfigs.iter().filter(|f| f.term == t.name).collect();
        let main = figs
            .iter()
            .find(|f| sym.prop(f.r, "schemifyPin").is_none())
            .or(figs.first());
        let at = main.map_or([0, 0], |f| {
            centre(f.b, sym.upi, &format!("pin {}", t.name), &cell, notes)
        });
        out.push(PinRow {
            name: net.text.clone(),
            dir: dir.into(),
            at,
            inherit,
            alias: None,
        });
        let mut n = 1;
        for f in &figs {
            if main.is_some_and(|m| m.r == f.r) {
                continue;
            }
            n += 1;
            let name = sym
                .prop(f.r, "schemifyPin")
                .map_or(format!("{}_{n}", net.text), str::to_owned);
            out.push(PinRow {
                name,
                dir: dir.into(),
                at: centre(f.b, sym.upi, &format!("pin {}", t.name), &cell, notes),
                inherit: None,
                alias: Some(net.text.clone()),
            });
        }
    }
    out
}

/// `[@p:%:d!]` → `(p, d)` with the default's `!` dropped (`gnd!` → `GND`, §3.3).
fn parse_netexpr(e: &str) -> Option<(String, String)> {
    let inner = e.trim().strip_prefix("[@")?.strip_suffix(']')?;
    let mut parts = inner.splitn(3, ':');
    let p = parts.next()?;
    let fmt = parts.next()?;
    let d = parts.next()?;
    if p.is_empty() || fmt != "%" || d.is_empty() {
        return None;
    }
    let n = net_from_virtuoso(d);
    let def = if n.global {
        n.text
    } else {
        n.text.trim_end_matches('!').to_owned()
    };
    Some((p.to_owned(), def))
}

/// One imported param.
#[derive(Clone, Debug)]
struct ParamIn {
    name: String,
    kind: String,
    default: String,
    unit: Option<&'static str>,
    doc: Option<String>,
    choices: Vec<String>,
}

/// The §6.3 import table over a cell's CDF.
fn params_in(c: &DCell, cdf: &Cdf, notes: &mut Notes) -> Vec<ParamIn> {
    let cell = format!("{}/{}", c.lib, c.name);
    let names: Vec<&str> = cdf.params.iter().map(|p| p.name.as_str()).collect();
    let mut out = Vec::new();
    for p in &cdf.params {
        if !p.callback.trim().is_empty() {
            notes.review(
                "callback",
                &cell,
                format!("param {}: callback not carried", p.name),
            );
        }
        let (kind, default) = match p.ty.as_str() {
            "button" | "netSet" => continue,
            "int" => (
                "int",
                if p.def.trim().parse::<i64>().is_ok() {
                    p.def.trim().to_owned()
                } else {
                    "0".into()
                },
            ),
            "float" => (
                "number",
                own_default(&p.def, &names).unwrap_or_else(|| "0".into()),
            ),
            "boolean" => (
                "bool",
                ael::bool_from_cdf(&p.def).unwrap_or("false").to_owned(),
            ),
            "radio" | "cyclic" => ("choice", ael::literal_to_template(&p.def)),
            _ => {
                let numeric = p.flags.iter().any(|f| f == "parseAsNumber");
                match own_default(&p.def, &names).filter(|_| numeric) {
                    Some(d) => ("number", d),
                    None => ("string", ael::literal_to_template(&p.def)),
                }
            }
        };
        out.push(ParamIn {
            name: p.name.clone(),
            kind: kind.into(),
            default,
            unit: ael::unit_from_cdf(&p.units),
            doc: (!p.prompt.is_empty() && p.prompt != p.name).then(|| p.prompt.clone()),
            choices: p.choices.clone(),
        });
    }
    out
}

/// A CDF default as a Schemify expression when it is numeric: translates, compiles, and
/// names only params of the same cell (`iPar("x")` → `x`).
fn own_default(def: &str, names: &[&str]) -> Option<String> {
    if def.trim().is_empty() {
        return None;
    }
    let t = ael::from_ael(
        def,
        |n| {
            if names.contains(&n) {
                AelName::Param
            } else {
                AelName::Unknown
            }
        },
        |n| names.contains(&n).then(|| n.to_owned()),
    )?;
    if t.review.is_some() {
        return None;
    }
    let p = schemify_expr::Program::compile(&t.text, schemify_expr::Mode::Scalar).ok()?;
    p.idents().all(|i| names.contains(&i)).then_some(t.text)
}

// ---------------------------------------------------------------------------------------
// Formats from simInfo (§6.3)
// ---------------------------------------------------------------------------------------

/// A pin list for a template: explicit holes when every pin is an identifier, else `{pins}`.
fn pins_template(order: &[String]) -> String {
    let ident = |s: &str| {
        let mut ch = s.chars();
        ch.next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && ch.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if order.iter().all(|p| ident(p)) && !order.is_empty() {
        order
            .iter()
            .map(|p| format!("{{{p}}}"))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        "{pins}".into()
    }
}

/// `(k, c)` per `instParameters` entry that names a CDF param (through `propMapping`).
fn inst_params(si: &SimInfo, params: &[ParamIn]) -> Vec<(String, String, bool)> {
    let pm = si.pairs("propMapping");
    si.list("instParameters")
        .into_iter()
        .filter_map(|k| {
            let c = pm
                .iter()
                .find(|(s, _)| *s == k)
                .map_or(k.clone(), |(_, c)| c.clone());
            let p = params.iter().find(|p| p.name == c)?;
            Some((k, c, p.kind != "string"))
        })
        .collect()
}

/// Terminal order of a simInfo in Schemify pin names (falling back to the pin order).
fn sim_order(si: &SimInfo, pins: &[PinRow]) -> Vec<String> {
    let ordinary: Vec<&PinRow> = pins.iter().filter(|p| p.alias.is_none()).collect();
    let order = si.list("termOrder");
    let named: Vec<String> = order.iter().map(|t| net_from_virtuoso(t).text).collect();
    if !named.is_empty()
        && named.len() == ordinary.len()
        && named.iter().all(|n| ordinary.iter().any(|p| p.name == *n))
    {
        named
    } else {
        ordinary.iter().map(|p| p.name.clone()).collect()
    }
}

fn spectre_format(c: &DCell, si: &SimInfo, params: &[ParamIn], pins: &[PinRow]) -> String {
    let order = sim_order(si, pins);
    let has_model = params
        .iter()
        .any(|p| p.name == "model" && !p.default.is_empty());
    let master = if has_model {
        "{model}".to_owned()
    } else {
        si.str("componentName")
            .filter(|s| !s.is_empty() && *s != "nil")
            .unwrap_or(&c.name)
            .to_owned()
    };
    let mut t = format!("{{name}} ({}) {master}", pins_template(&order));
    for (k, cn, num) in inst_params(si, params) {
        if num {
            t.push_str(&format!(" {k}={{{cn}}}"));
        } else {
            t.push_str(&super::write::optional_param(&k, &cn));
        }
    }
    t
}

fn cdl_format(
    c: &DCell,
    si: &SimInfo,
    params: &[ParamIn],
    pins: &[PinRow],
    notes: &mut Notes,
) -> String {
    let order = sim_order(si, pins);
    let kv: String = inst_params(si, params)
        .iter()
        .map(|(k, cn, _)| format!(" {k}={{{cn}}}"))
        .collect();
    let pins = pins_template(&order);
    if si.str("netlistProcedure") == Some("ansCdlSubcktCall") {
        let comp = si
            .str("componentName")
            .filter(|s| !s.is_empty() && *s != "nil")
            .unwrap_or(&c.name);
        let prefix = si
            .str("namePrefix")
            .filter(|s| !s.is_empty())
            .unwrap_or("X");
        return format!("{prefix}{{name}} {pins} / {comp}{kv}");
    }
    notes.review(
        "cdl-approximate",
        &format!("{}/{}", c.lib, c.name),
        "auCdl format from simInfo (research §3.4)",
    );
    let prefix = si
        .str("namePrefix")
        .filter(|s| !s.is_empty() && *s != "nil")
        .unwrap_or("X");
    let model_name = si.str("modelName").filter(|s| !s.is_empty() && *s != "nil");
    let has_model = params.iter().any(|p| p.name == "model");
    if matches!(prefix, "R" | "C" | "L" | "D")
        && let Some(m) = model_name
    {
        return format!("{prefix}{{name}} {pins}{kv} $[{m}]");
    }
    let model = match model_name {
        Some(m) => format!(" {m}"),
        None if has_model => " {model}".into(),
        None => format!(" {}", c.name),
    };
    format!("{prefix}{{name}} {pins}{model}{kv}")
}

// ---------------------------------------------------------------------------------------
// Ids (§2, §7.2)
// ---------------------------------------------------------------------------------------

/// Id assignment for one file: `schemifyId` when valid and unused, else
/// `IdRng::for_import("<lib>/<cell>", ordinal)`.
struct Ids {
    seed: String,
    taken: BTreeSet<String>,
    ordinal: u32,
}

impl Ids {
    fn new(seed: String) -> Ids {
        Ids {
            seed,
            taken: BTreeSet::new(),
            ordinal: 0,
        }
    }

    /// The id of the next object (in dump order), from `wanted` when possible.
    fn next(&mut self, wanted: Option<&str>, reserved: &BTreeSet<String>) -> String {
        self.ordinal += 1;
        if let Some(w) = wanted
            && is_id(w)
            && self.taken.insert(w.to_owned())
        {
            return w.to_owned();
        }
        let mut rng = IdRng::for_import(&self.seed, self.ordinal);
        let id = fresh_id(&mut rng, |c| self.taken.contains(c) || reserved.contains(c));
        let s = id.as_str().to_owned();
        self.taken.insert(s.clone());
        s
    }
}

// ---------------------------------------------------------------------------------------
// Cell build
// ---------------------------------------------------------------------------------------

/// Per-cellview accounting for count conservation (§12).
#[derive(Default)]
struct Tally {
    out: usize,
    reported: usize,
}

fn begin(doc: &mut Document<'static>, k: RecordKind) {
    doc.begin_record(k);
}

fn kv(doc: &mut Document<'static>, k: &str, v: &str) {
    doc.push_attr(k);
    doc.push_value(v);
}

/// Text height → Schemify `size` milli (`size = height · 16 / upi`, §3.5).
fn size_milli(height: i32, upi: i32) -> i32 {
    let h = i64::from(height) * profile::MILLI_PER_INCH / i64::from(upi);
    i32::try_from((h + 5) / 10).unwrap_or(i32::MAX)
}

fn coord_text(milli: i32) -> String {
    let mut s = String::new();
    schemify_format::format_coord(milli, &mut s);
    s
}

/// A Schemify graphic: form, values (milli-units), arc angles (micro-radians), closed.
type GraphicIn = (String, Vec<i32>, Option<(i32, i32)>, bool);

/// A Virtuoso arc/ellipse/… as Schemify graphic `(form, values)` in milli-units, or
/// `None` when it is not representable. `upi` converts.
fn shape_in(sh: &Shape, upi: i32, cell: &str, notes: &mut Notes) -> Option<GraphicIn> {
    let p = |k: usize, notes: &mut Notes| {
        pt(
            [sh.coords[2 * k], sh.coords[2 * k + 1]],
            upi,
            &sh.kind,
            cell,
            notes,
        )
    };
    match sh.kind.as_str() {
        "line" | "path" => {
            let pts: Vec<i32> = (0..sh.coords.len() / 2).flat_map(|k| p(k, notes)).collect();
            if pts.len() == 4 {
                Some(("line".into(), pts, None, false))
            } else {
                Some(("poly".into(), pts, None, false))
            }
        }
        "polygon" => {
            let pts: Vec<i32> = (0..sh.coords.len() / 2).flat_map(|k| p(k, notes)).collect();
            Some(("poly".into(), pts, None, true))
        }
        "rect" => {
            let (a, b) = (p(0, notes), p(1, notes));
            Some((
                "rect".into(),
                vec![
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[0].max(b[0]),
                    a[1].max(b[1]),
                ],
                None,
                false,
            ))
        }
        "ellipse" | "arc" => {
            let (a, b) = (p(0, notes), p(1, notes));
            let (w, h) = ((b[0] - a[0]).abs(), (b[1] - a[1]).abs());
            let cx = (a[0] + b[0]) / 2;
            let cy = (a[1] + b[1]) / 2;
            if w != h {
                notes.review(
                    "format",
                    cell,
                    format!("{} with an elliptic box drawn as a polygon", sh.kind),
                );
                let n = 32;
                let mut pts = Vec::new();
                for k in 0..n {
                    let t = std::f64::consts::TAU * f64::from(k) / f64::from(n);
                    pts.push(cx + (f64::from(w) / 2.0 * t.cos()).round() as i32);
                    pts.push(cy + (f64::from(h) / 2.0 * t.sin()).round() as i32);
                }
                return Some(("poly".into(), pts, None, true));
            }
            if sh.kind == "ellipse" {
                return Some(("circle".into(), vec![cx, cy, w / 2], None, false));
            }
            Some((
                "arc".into(),
                vec![cx, cy, w / 2],
                Some((sh.coords[4], sh.coords[5])),
                false,
            ))
        }
        _ => None,
    }
}

/// Degrees (milli) of micro-radians.
fn mdeg(urad: i32) -> i32 {
    (f64::from(urad) / 1e6)
        .to_degrees()
        .mul_add(1000.0, 0.0)
        .round() as i32
}

/// Schemify `(start, sweep)` milli-degrees of a Virtuoso arc's counter-clockwise
/// `(start, stop)` micro-radians (y flipped).
fn arc_angles(a0: i32, a1: i32) -> (i32, i32) {
    let d0 = mdeg(a0);
    let d1 = mdeg(a1);
    let sweep = (d1 - d0).rem_euclid(360_000);
    let sweep = if sweep == 0 { 360_000 } else { sweep };
    ((-d1).rem_euclid(360_000), sweep)
}

/// Merges arcs that continue one another on the same box (an export of a sweep > π).
fn merge_arcs(shapes: &[&Shape]) -> Vec<Shape> {
    let mut v: Vec<Shape> = shapes.iter().map(|s| (*s).clone()).collect();
    let mut k = 0;
    while k < v.len() {
        if v[k].kind == "arc"
            && let Some(j) = (0..v.len()).find(|&j| {
                j != k
                    && v[j].kind == "arc"
                    && v[j].coords[..4] == v[k].coords[..4]
                    && v[j].layer == v[k].layer
                    && v[j].purpose == v[k].purpose
                    && v[j].coords[4] == v[k].coords[5]
            })
        {
            let stop = v[j].coords[5];
            v[k].coords[5] = stop;
            v.remove(j);
            if j < k {
                k -= 1;
            }
            continue;
        }
        k += 1;
    }
    v
}

/// Attributes of an imported graphic.
#[derive(Clone, Copy, Default)]
struct Style {
    closed: bool,
    fill: bool,
    dash: bool,
}

/// Writes one Schemify graphic record (rank 2 when `id` is `None`, else a rank-3 shape).
fn push_graphic(
    doc: &mut Document<'static>,
    id: Option<&str>,
    form: &str,
    vals: &[i32],
    angles: Option<(i32, i32)>,
    style: Style,
) {
    let Style { closed, fill, dash } = style;
    match id {
        Some(id) => {
            begin(doc, RecordKind::Shape);
            doc.push_field(id);
            doc.push_field(form);
        }
        None => begin(
            doc,
            match form {
                "line" => RecordKind::Line,
                "rect" => RecordKind::Rect,
                "arc" => RecordKind::Arc,
                "circle" => RecordKind::Circle,
                _ => RecordKind::Poly,
            },
        ),
    }
    for v in vals {
        doc.push_field_coord(*v);
    }
    if let Some((a0, a1)) = angles {
        let (s, w) = arc_angles(a0, a1);
        doc.push_field(&coord_text(s));
        doc.push_field(&coord_text(w));
    }
    if dash {
        kv(doc, "dash", "1");
    }
    if fill && form != "line" {
        kv(doc, "fill", "true");
    }
    if closed {
        kv(doc, "closed", "true");
    }
}

/// The `[@…]` groups of NLP text that is nothing but groups (`\` escapes inside); `None`
/// when text sits outside a group or a group is unclosed.
fn nlp_groups(t: &str) -> Option<Vec<&str>> {
    let mut out = Vec::new();
    let mut rest = t;
    while !rest.is_empty() {
        if !rest.starts_with("[@") {
            return None;
        }
        let mut esc = false;
        let end = rest.char_indices().find_map(|(k, ch)| match ch {
            _ if esc => {
                esc = false;
                None
            }
            '\\' => {
                esc = true;
                None
            }
            ']' => Some(k),
            _ => None,
        })?;
        out.push(&rest[..=end]);
        rest = &rest[end + 1..];
    }
    debug_assert!(out.iter().all(|g| g.starts_with("[@") && g.ends_with(']')));
    Some(out)
}

/// NLP label text → Schemify template (§3.5 import), `None` when dropped.
fn nlp_in(text: &str, label_set: &str) -> Option<String> {
    let t = text.trim();
    if let Some(n) = t
        .strip_prefix("cdsParam(")
        .and_then(|x| x.strip_suffix(')'))
    {
        let k: usize = n.trim().parse().ok()?;
        let p = label_set
            .split_whitespace()
            .filter(|w| !w.starts_with('-'))
            .nth(k.checked_sub(1)?)?;
        return Some(format!("{p}={{{p}}}"));
    }
    if t == "cdsName()" {
        return Some("{name}".into());
    }
    // Several groups (`[@w:%/][@l]`, what export writes for a stext with several holes):
    // each translated, concatenated.
    let groups = nlp_groups(t)?;
    if groups.len() > 1 {
        return groups.iter().map(|g| nlp_in(g, label_set)).collect();
    }
    let inner = t.strip_prefix("[@")?.strip_suffix(']')?;
    // Split on unescaped ':'.
    let mut parts: Vec<String> = vec![String::new()];
    let mut esc = false;
    for ch in inner.chars() {
        if esc {
            parts.last_mut().expect("one").push(ch);
            esc = false;
        } else if ch == '\\' {
            esc = true;
        } else if ch == ':' {
            parts.push(String::new());
        } else {
            parts.last_mut().expect("one").push(ch);
        }
    }
    let p = parts[0].as_str();
    let hole = match p {
        "instanceName" => "name",
        "partName" | "cellName" => "cell",
        "refDes" => return None,
        x if x.starts_with("userPartName") || x.starts_with("phyPartName") => return None,
        x => x,
    };
    match parts.len() {
        1 => Some(format!("{{{hole}}}")),
        2 | 3 => {
            // pre%post; the fallback (third part) is lost.
            let fmt = &parts[1];
            let (pre, post) = fmt.split_once('%').unwrap_or((fmt.as_str(), ""));
            Some(format!(
                "{}{{{hole}}}{}",
                ael::literal_to_template(pre),
                ael::literal_to_template(post)
            ))
        }
        _ => None,
    }
}

/// The fresh document of dump cell `k` (before the merge) and its stash text, if any.
fn build(
    ctx: &Ctx<'_>,
    k: usize,
    pending: &mut Vec<Pending>,
    notes: &mut Notes,
) -> (Document<'static>, Option<String>) {
    let c = &ctx.dump.cells[k];
    let t = &ctx.targets[k];
    let kind = t.kind.expect("only written cells are built");
    let cell = format!("{}/{}", c.lib, c.name);
    let mut doc = Document::new(kind);
    let sym = c.cv("symbol");
    let sch = c.cv("schematic");
    // Header.
    if kind == FileKind::Shprim {
        kv(
            &mut doc,
            "provenance",
            &format!("virtuoso {}/{}", c.lib, c.name),
        );
    }
    // Inherited pins from net-expression labels (§6.2).
    let mut expr_pins: Vec<(String, String)> = Vec::new();
    if let Some(s) = sch {
        for l in &s.labels {
            if let Some((p, d)) = parse_netexpr(&l.text)
                && !expr_pins.iter().any(|x| x.0 == p)
            {
                expr_pins.push((p, d));
            }
        }
    }
    let params = c
        .cdf
        .as_ref()
        .map(|d| params_in(c, d, notes))
        .unwrap_or_default();
    let mut pins: Vec<PinRow> = match (sym, sch) {
        (Some(sv), _) => pins_in(c, sv, notes),
        (None, Some(sv)) if kind == FileKind::Shcomp => derived_pins(sv),
        _ => Vec::new(),
    };
    if kind != FileKind::Shbench {
        for (p, d) in &expr_pins {
            let name = net_from_virtuoso(p).text;
            if !pins.iter().any(|x| x.name == name) {
                pins.push(PinRow {
                    name,
                    dir: "inout".into(),
                    at: [0, 0],
                    inherit: Some(d.clone()),
                    alias: None,
                });
            }
        }
    } else if !expr_pins.is_empty() {
        notes.review(
            "name",
            &cell,
            "net expressions in a bench have no inherited pin to land on",
        );
    }
    // Impls.
    // (name, lang, formats, body)
    type ImplOut = (String, String, Vec<(String, String)>, Option<String>);
    let mut impls: Vec<ImplOut> = Vec::new();
    match kind {
        FileKind::Shcomp => impls.push(("schematic".into(), "schematic".into(), Vec::new(), None)),
        FileKind::Shprim => {
            let mut formats = Vec::new();
            if let Some(cdf) = &c.cdf {
                if let Some(si) = cdf.sim("spectre") {
                    formats.push(("spectre".to_owned(), spectre_format(c, si, &params, &pins)));
                }
                if let Some(si) = cdf.sim("auCdl") {
                    formats.push(("cdl".to_owned(), cdl_format(c, si, &params, &pins, notes)));
                }
            }
            let spice = ctx
                .map
                .by_virtuoso(&c.lib, &c.name)
                .and_then(|r| r.spice.clone())
                .or_else(|| {
                    (c.lib == "analogLib")
                        .then(|| devices::by_alib(&c.name).map(|(d, w)| d.ngspice[w].to_owned()))
                        .flatten()
                });
            if let Some(s) = spice {
                formats.insert(0, ("spice".into(), s));
            }
            impls.push(("spice".into(), "spice".into(), formats, None));
        }
        _ => {}
    }
    for tv in &c.textviews {
        let (name, lang) = match tv.view.as_str() {
            "veriloga" => ("veriloga", "veriloga"),
            "functional" | "verilog" => ("verilog", "verilog"),
            other => {
                notes.review("format", &cell, format!("text view {other} not imported"));
                continue;
            }
        };
        if kind == FileKind::Shbench {
            notes.review("format", &cell, format!("text view {} of a bench", tv.view));
            continue;
        }
        impls.push((name.into(), lang.into(), Vec::new(), Some(tv.text.clone())));
    }
    if impls.len() > 1 {
        kv(&mut doc, "default", &impls[0].0);
    }
    // Rank 1.
    for p in &pins {
        begin(&mut doc, RecordKind::Pin);
        doc.push_field(&p.name);
        doc.push_field(&p.dir);
        doc.push_field_coord(p.at[0]);
        doc.push_field_coord(p.at[1]);
        if let Some(i) = &p.inherit {
            kv(&mut doc, "inherit", i);
        }
        if let Some(a) = &p.alias {
            kv(&mut doc, "alias", a);
        }
    }
    if kind != FileKind::Shbench {
        for p in &params {
            begin(&mut doc, RecordKind::Param);
            doc.push_field(&p.name);
            doc.push_field(&p.default);
            if p.kind != "number" {
                kv(&mut doc, "kind", &p.kind);
            }
            if let Some(u) = p.unit {
                kv(&mut doc, "unit", u);
            }
            if p.kind == "choice" && !p.choices.is_empty() {
                doc.push_attr("choices");
                for ch in &p.choices {
                    doc.push_value(ch);
                }
            }
            if let Some(d) = &p.doc {
                kv(&mut doc, "doc", d);
            }
        }
    }
    for (name, lang, formats, _) in &impls {
        begin(&mut doc, RecordKind::Impl);
        doc.push_field(name);
        doc.push_field(lang);
        for (d, f) in formats {
            doc.push_attr("format");
            doc.push_value(d);
            doc.push_value(f);
        }
    }
    // Rank 2: the symbol.
    let mut stash_text: Option<String> = None;
    if let Some(sv) = sym {
        let mut tally = Tally::default();
        tally.out += sv.terms.len() + sv.pinfigs.len();
        let label_set = c.cdf.as_ref().map_or("", |d| d.label_set.as_str());
        let body: Vec<&Shape> = sv
            .shapes
            .iter()
            .filter(|s| !(s.layer == "instance" && s.purpose == "drawing"))
            .collect();
        let selboxes = sv.shapes.len() - body.len();
        if selboxes > 0 {
            notes.dropped("selection-box", &cell, format!("{selboxes} object(s)"));
            tally.reported += selboxes;
        }
        let merged = merge_arcs(&body);
        tally.out += body.len();
        for sh in &merged {
            match shape_in(sh, sv.upi, &cell, notes) {
                Some((form, vals, ang, closed)) => push_graphic(
                    &mut doc,
                    None,
                    &form,
                    &vals,
                    ang,
                    Style {
                        closed,
                        fill: sh.purpose == "drawing1",
                        dash: false,
                    },
                ),
                None => notes.dropped("display-only", &cell, format!("symbol {}", sh.kind)),
            }
        }
        let mut pin_labels = 0;
        for l in &sv.labels {
            if l.layer == "pin" && (l.purpose == "label" || l.purpose == "annotate") {
                pin_labels += 1;
                continue;
            }
            let tmpl = match l.kind.as_str() {
                "normal" => Some(ael::literal_to_template(&l.text)),
                _ => nlp_in(&l.text, label_set),
            };
            tally.out += 1;
            let Some(tmpl) = tmpl.filter(|t| !t.is_empty()) else {
                notes.review(
                    "template",
                    &cell,
                    format!("symbol label `{}` dropped", l.text),
                );
                continue;
            };
            let at = pt(l.xy, sv.upi, "label", &cell, notes);
            begin(&mut doc, RecordKind::Stext);
            doc.push_field_coord(at[0]);
            doc.push_field_coord(at[1]);
            doc.push_field(
                profile::orient_from_virtuoso(&l.orient)
                    .unwrap_or(Orient::R0)
                    .as_str(),
            );
            doc.push_field(&tmpl);
            kv(&mut doc, "size", &coord_text(size_milli(l.height, sv.upi)));
            if l.justify.ends_with("Center") {
                doc.push_attr("hcenter");
            }
            if l.justify.starts_with("center") {
                doc.push_attr("vcenter");
            }
        }
        if pin_labels > 0 {
            notes.dropped("pin-label", &cell, format!("{pin_labels} object(s)"));
            tally.reported += pin_labels;
        }
        let st = cv_props(sv, &cell, notes, &mut tally);
        if kind == FileKind::Shprim {
            stash_text = st;
        }
        report_rest(sv, &cell, notes, &mut tally);
        assert_eq!(
            tally.out + tally.reported,
            sv.objects(),
            "{cell} symbol: count conservation"
        );
    }
    if kind == FileKind::Shcomp && sym.is_none() {
        derived_symbol(&mut doc, &pins);
    }
    // Rank 3: the schematic.
    if let Some(sv) = sch {
        let st = schematic_in(ctx, k, sv, &mut doc, &pins, pending, notes);
        if kind != FileKind::Shprim {
            stash_text = st;
        }
    }
    // Rank 5: text view bodies.
    for (name, _, _, body) in &impls {
        if let Some(b) = body {
            begin(&mut doc, RecordKind::Body);
            doc.push_field(name);
            for line in b.strip_suffix('\n').unwrap_or(b).split('\n') {
                doc.push_verbatim(line.trim_end_matches('\r'));
            }
        }
    }
    (doc, stash_text)
}

/// Pins of a schematic without a symbol: one per schematic pin, inputs on the left,
/// outputs and inouts on the right, 20 units apart (§6.1).
fn derived_pins(sv: &Cv) -> Vec<PinRow> {
    let mut terms: Vec<(String, String)> = sv
        .spins
        .iter()
        .map(|s| {
            (
                net_from_virtuoso(&s.term).text,
                match s.dir.as_str() {
                    "input" => "in",
                    "output" => "out",
                    _ => "inout",
                }
                .to_owned(),
            )
        })
        .collect();
    terms.sort();
    terms.dedup_by(|a, b| a.0 == b.0);
    let (mut l, mut r) = (0, 0);
    terms
        .into_iter()
        .map(|(name, dir)| {
            let left = dir == "in";
            let n = if left { &mut l } else { &mut r };
            let at = [if left { -40_000 } else { 40_000 }, 20_000 * *n];
            *n += 1;
            PinRow {
                name,
                dir,
                at,
                inherit: None,
                alias: None,
            }
        })
        .collect()
}

fn derived_symbol(doc: &mut Document<'static>, pins: &[PinRow]) {
    let rows = pins.iter().map(|p| p.at[1]).max().unwrap_or(0);
    begin(doc, RecordKind::Rect);
    for v in [-20_000, -10_000, 20_000, rows + 10_000] {
        doc.push_field_coord(v);
    }
    for p in pins {
        begin(doc, RecordKind::Line);
        let edge = if p.at[0] < 0 { -20_000 } else { 20_000 };
        for v in [p.at[0].min(edge), p.at[1], p.at[0].max(edge), p.at[1]] {
            doc.push_field_coord(v);
        }
    }
    begin(doc, RecordKind::Stext);
    doc.push_field_coord(-20_000);
    doc.push_field_coord(-20_000);
    doc.push_field("R0");
    doc.push_field("{name}");
}

/// Cellview properties: the stash (`schemifyRecords<k>`, joined in numeric order) and
/// everything else reported.
fn cv_props(sv: &Cv, cell: &str, notes: &mut Notes, tally: &mut Tally) -> Option<String> {
    let mut chunks: Vec<(u32, &str)> = Vec::new();
    for p in sv.props.iter().filter(|p| p.r == 0) {
        match p
            .name
            .strip_prefix("schemifyRecords")
            .and_then(|n| n.parse::<u32>().ok())
        {
            Some(n) => chunks.push((n, &p.value)),
            None => {
                notes.dropped(
                    "display-only",
                    cell,
                    format!("cellview property {}", p.name),
                );
                tally.reported += 1;
            }
        }
    }
    tally.out += chunks.len();
    chunks.sort();
    (!chunks.is_empty()).then(|| chunks.iter().map(|c| c.1).collect())
}

/// Reports conns, intents and unknown records; counts properties of objects (consumed by
/// their owners' builders) as out.
fn report_rest(sv: &Cv, cell: &str, notes: &mut Notes, tally: &mut Tally) {
    for c in &sv.conns {
        notes.dropped(
            "display-only",
            cell,
            format!("conn {} {} (loader-only)", c.term, c.net),
        );
    }
    for r in &sv.intents {
        notes.dropped(
            "unknown-record",
            cell,
            format!("intent on line {} (§10, not imported)", r.line),
        );
    }
    for r in &sv.unknown {
        notes.dropped(
            "unknown-record",
            cell,
            format!("`{}` on line {}", r.tag, r.line),
        );
    }
    tally.reported += sv.conns.len() + sv.intents.len() + sv.unknown.len();
    tally.out += sv.props.iter().filter(|p| p.r != 0).count();
}

/// The schematic objects (§6.2) of dump cell `k` into `doc`; returns the stash text.
/// An instance waiting for the Schemify-side re-mastering pass (§6.4).
struct Pending {
    /// Instance id.
    id: String,
    /// The new master.
    cellref: String,
    /// Its overrides, already in the new master's names.
    attrs: Vec<(String, String)>,
    /// Device row of [`devices::DEVICES`].
    device: usize,
}

fn schematic_in(
    ctx: &Ctx<'_>,
    k: usize,
    sv: &Cv,
    doc: &mut Document<'static>,
    pins: &[PinRow],
    pending: &mut Vec<Pending>,
    notes: &mut Notes,
) -> Option<String> {
    let c = &ctx.dump.cells[k];
    let t = &ctx.targets[k];
    let kind = t.kind.expect("written");
    let cell = format!("{}/{}", c.lib, c.name);
    let upi = sv.upi;
    let mut tally = Tally::default();
    // Every schemifyId in dump order claims first; duplicates get fresh ids (§7.2).
    let reserved: BTreeSet<String> = BTreeSet::new();
    let mut ids = Ids::new(format!("{}/{}", c.lib, c.name));
    // Properties by owner: one pass, so per-object lookups stay logarithmic.
    let mut by_owner: BTreeMap<u32, Vec<&Prop>> = BTreeMap::new();
    for p in &sv.props {
        by_owner.entry(p.r).or_default().push(p);
    }
    let prop = |r: u32, name: &str| {
        by_owner
            .get(&r)
            .and_then(|v| v.iter().find(|p| p.name == name))
            .map(|p| p.value.as_str())
    };
    let id_of = |r: u32| prop(r, "schemifyId");
    // Pre-claim: ids wanted by objects, in dump order, so a fresh id never steals one.
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for p in sv
        .props
        .iter()
        .filter(|p| p.name == "schemifyId" && is_id(&p.value))
    {
        claimed.insert(p.value.clone());
    }
    let reserved = if claimed.is_empty() {
        reserved
    } else {
        claimed
    };
    let own_params: Vec<String> = c
        .cdf
        .as_ref()
        .map(|d| d.params.iter().map(|p| p.name.clone()).collect())
        .unwrap_or_default();
    let bench = kind == FileKind::Shbench;
    // Instances.
    for inst in &sv.insts {
        tally.out += 1;
        let at = pt(
            inst.xy,
            upi,
            &format!("instance {}", inst.name),
            &cell,
            notes,
        );
        let o = profile::orient_from_virtuoso(&inst.orient).unwrap_or(Orient::R0);
        if inst.lib == "basic" && inst.cell == "noConn" {
            let id = ids.next(id_of(inst.r), &reserved);
            begin(doc, RecordKind::Noconn);
            doc.push_field(&id);
            doc.push_field_coord(at[0]);
            doc.push_field_coord(at[1]);
            continue;
        }
        if devices::is_global_symbol(&inst.lib, &inst.cell) {
            // A global label at the symbol's pin point.
            let gi = ctx
                .find(&inst.lib, &inst.cell)
                .and_then(|g| ctx.ifaces[g].clone());
            let (pin_at, net) = match gi.as_ref().and_then(|i| i.pins.first()) {
                Some(p) => (
                    crate::design::place(p.at, o, at).unwrap_or(at),
                    ctx.find(&inst.lib, &inst.cell)
                        .and_then(|g| ctx.dump.cells[g].cv("symbol"))
                        .and_then(|s| s.terms.first())
                        .map_or_else(|| format!("{}!", inst.cell), |t| t.name.clone()),
                ),
                None => {
                    notes.review(
                        "name",
                        &cell,
                        format!(
                            "{}/{} symbol not in the dump: label at its origin",
                            inst.lib, inst.cell
                        ),
                    );
                    (at, format!("{}!", inst.cell))
                }
            };
            let n = match prop(inst.r, "schemifyNet") {
                Some(sn) => match sn.strip_suffix(" global") {
                    Some(t) => profile::Net {
                        text: t.to_owned(),
                        global: true,
                        review: false,
                    },
                    None => profile::Net {
                        text: sn.to_owned(),
                        global: false,
                        review: false,
                    },
                },
                None => net_from_virtuoso(&net),
            };
            let id = ids.next(id_of(inst.r), &reserved);
            begin(doc, RecordKind::Label);
            doc.push_field(&id);
            doc.push_field_coord(pin_at[0]);
            doc.push_field_coord(pin_at[1]);
            doc.push_field(o.as_str());
            doc.push_field(&n.text);
            if n.global {
                kv(doc, "global", "true");
            }
            // The symbol instance's name, so export draws the same instance (§6.1); a name
            // the exporter generated (`sfyG<n>`) is not worth keeping.
            if !inst.name.starts_with("sfyG") {
                kv(doc, "vinst", &inst.name);
            }
            continue;
        }
        let m = ctx.master(&inst.lib, &inst.cell);
        let Some(cref) = m.cellref(&t.dir) else {
            notes.review(
                "name",
                &cell,
                format!(
                    "{}: master {}/{} is a root cell seen from another directory",
                    inst.name, inst.lib, inst.cell
                ),
            );
            continue;
        };
        if m.iface.is_none() {
            notes.review(
                "name",
                &cell,
                format!(
                    "{}: master {}/{} is not in the dump or the project",
                    inst.name, inst.lib, inst.cell
                ),
            );
        }
        // §6.4 on the Schemify side: the instance keeps its analogLib mirror (whose pins
        // are known) until the re-mastering pass moves it onto the core cell.
        let remap_old = m
            .alib
            .and_then(|_| ctx.find(&inst.lib, &inst.cell))
            .filter(|&j| ctx.targets[j].write);
        let new_cref = cref.clone();
        let cref = match remap_old {
            Some(j) => {
                let tj = &ctx.targets[j];
                Master {
                    slib: tj.slib.clone(),
                    dir: tj.dir.clone(),
                    stem: tj.stem.clone(),
                    ..Master::default()
                }
                .cellref(&t.dir)
                .unwrap_or(cref)
            }
            None => {
                if m.alib.is_some() {
                    notes.add(
                        "remapped",
                        &cell,
                        format!(
                            "{}: {}/{} → {new_cref} in place (analogLib geometry unknown)",
                            inst.name, inst.lib, inst.cell
                        ),
                    );
                }
                cref
            }
        };
        let id = ids.next(id_of(inst.r), &reserved);
        begin(doc, RecordKind::Inst);
        doc.push_field(&id);
        doc.push_field(&cref);
        doc.push_attr("at");
        doc.push_value_coord(at[0]);
        doc.push_value_coord(at[1]);
        doc.push_value(o.as_str());
        kv(doc, "name", &profile::inst_from_virtuoso(&inst.name));
        let props: Vec<&Prop> = by_owner.get(&inst.r).cloned().unwrap_or_default();
        if props
            .iter()
            .any(|p| p.name == "nlAction" && p.value == "ignore")
        {
            kv(doc, "ignore", "true");
        }
        let iface = m.iface.clone().unwrap_or_default();
        let row = m.row.map(|r| &ctx.map.maps[r]);
        let dev = m.alib.map(|(d, _)| &devices::DEVICES[d]);
        // Sources: the waveform params become the `value` text (§6.6).
        let src = m.source.or_else(|| dev.and_then(|d| d.source));
        let mut overrides: Vec<(String, String)> = Vec::new();
        let mut inherits: Vec<(String, String)> = Vec::new();
        let raw = |n: &str| props.iter().find(|p| p.name == n).map(|p| p.value.clone());
        let mut wave_params: BTreeMap<String, String> = BTreeMap::new();
        for p in &props {
            match p.name.as_str() {
                "schemifyId" | "nlAction" => continue,
                _ => {}
            }
            if let Some((_, pin)) = iface.inherit_props.iter().find(|x| x.0 == p.name) {
                inherits.push((pin.clone(), net_from_virtuoso(&p.value).text));
                continue;
            }
            if p.ty == "netSet" {
                // An override of a net expression deeper than the master's own inherited
                // pins: Schemify's `inherit` names a pin of the master, so it has no place.
                notes.review(
                    "inherit",
                    &cell,
                    format!(
                        "{}: netSet {} = {} overrides a net expression below {}/{}",
                        inst.name, p.name, p.value, inst.lib, inst.cell
                    ),
                );
                continue;
            }
            if let Some(current) = src
                && devices::source_cdf_params(current).contains(&p.name.as_str())
            {
                wave_params.insert(p.name.clone(), p.value.clone());
                continue;
            }
            // Renames back.
            let (sname, scale) = if let Some(row) = row {
                if row.sets.iter().any(|s| s.0 == p.name) {
                    continue;
                }
                match row.params.iter().find(|x| x.1 == p.name) {
                    Some(x) => (x.0.clone(), x.2.clone()),
                    None => (p.name.clone(), None),
                }
            } else if let Some(dv) = dev {
                match dv.params.iter().find(|x| x.1 == p.name) {
                    Some(x) => (x.0.to_owned(), None),
                    None => continue,
                }
            } else {
                (p.name.clone(), None)
            };
            let pkind = iface.kind(&sname).map(str::to_owned);
            let value = match (pkind.as_deref(), p.ty.as_str()) {
                (_, "boolean") => ael::bool_from_cdf(&p.value).map(str::to_owned),
                (Some("int"), _) | (_, "int") => Some(p.value.trim().to_owned()),
                (Some("string" | "choice"), _) => Some(ael::literal_to_template(&p.value)),
                (Some("bool"), _) => ael::bool_from_cdf(&p.value).map(str::to_owned),
                (kind, _) => {
                    let tr = ael::from_ael(
                        &p.value,
                        |n| {
                            if own_params.iter().any(|x| x == n) {
                                AelName::Param
                            } else if bench {
                                AelName::BenchVar
                            } else {
                                AelName::Unknown
                            }
                        },
                        |n| {
                            raw(n)
                                .and_then(|v| {
                                    ael::from_ael(&v, |_| AelName::Param, |_| None).map(|t| t.text)
                                })
                                .or_else(|| iface.def(n).map(str::to_owned))
                                .map(|v| format!("({v})"))
                        },
                    );
                    match tr {
                        Some(tr)
                            if schemify_expr::Program::compile(
                                &tr.text,
                                schemify_expr::Mode::Scalar,
                            )
                            .is_ok() =>
                        {
                            if let Some(why) = tr.review {
                                notes.review(
                                    why,
                                    &cell,
                                    format!("{}: {} = {}", inst.name, p.name, p.value),
                                );
                            }
                            Some(tr.text)
                        }
                        _ if kind.is_none() => Some(ael::literal_to_template(&p.value)),
                        _ => {
                            notes.review(
                                "expression",
                                &cell,
                                format!("{}: {} = `{}` dropped", inst.name, p.name, p.value),
                            );
                            None
                        }
                    }
                }
            };
            let Some(mut value) = value else { continue };
            if let Some(f) = scale {
                value = format!("({value})/({f})");
                if let Some(v) = ael::fold_numeric(&value) {
                    value = v;
                }
            }
            overrides.push((sname, value));
        }
        if let Some(current) = src
            && (!wave_params.is_empty() || dev.is_some())
        {
            let wave = match (dev, m.alib) {
                (Some(_), Some((_, w))) => w,
                _ => wave_params
                    .get("srcType")
                    .and_then(|s| devices::WAVEFORMS.iter().position(|w| w.1 == s))
                    .unwrap_or(0),
            };
            let text = devices::format_source(wave, current, |n| {
                wave_params.get(n).filter(|v| !v.is_empty()).map(|v| {
                    ael::from_ael(v, |_| AelName::Param, |_| None).map_or(v.clone(), |t| t.text)
                })
            });
            overrides.push(("value".into(), text));
        }
        // Instance overrides in the master's param order (then the unknown ones).
        overrides.sort_by_key(|(n, _)| {
            iface
                .params
                .iter()
                .position(|p| p.0 == *n)
                .unwrap_or(usize::MAX)
        });
        if remap_old.is_some() {
            notes.add(
                "remapped",
                &cell,
                format!("{}: {}/{} → {new_cref}", inst.name, inst.lib, inst.cell),
            );
            pending.push(Pending {
                id: id.clone(),
                cellref: new_cref,
                attrs: overrides,
                device: m.alib.map(|a| a.0).expect("remap_old needs a device row"),
            });
            continue;
        }
        for (n, v) in overrides {
            kv(doc, &n, &v);
        }
        for (p, n) in inherits {
            doc.push_attr("inherit");
            doc.push_value(&p);
            doc.push_value(&n);
        }
    }
    // Schematic pins.
    for s in &sv.spins {
        tally.out += 1;
        let at = pt(s.xy, upi, &format!("pin {}", s.term), &cell, notes);
        let o = profile::orient_from_virtuoso(&s.orient).unwrap_or(Orient::R0);
        let name = sv
            .prop(s.r, "schemifyNet")
            .map_or_else(|| net_from_virtuoso(&s.term).text, str::to_owned);
        let id = ids.next(id_of(s.r), &reserved);
        if bench {
            begin(doc, RecordKind::Label);
        } else {
            if !pins.iter().any(|p| p.name == name) {
                notes.review(
                    "name",
                    &cell,
                    format!("schematic pin {name} has no symbol terminal"),
                );
            }
            begin(doc, RecordKind::Port);
        }
        doc.push_field(&id);
        doc.push_field_coord(at[0]);
        doc.push_field_coord(at[1]);
        doc.push_field(o.as_str());
        doc.push_field(&name);
    }
    // Wires: segments, stubs removed.
    let mut wire_pts: BTreeMap<u32, Vec<[i32; 2]>> = BTreeMap::new();
    let all_pts: Vec<DumpWire<'_>> = sv
        .wires
        .iter()
        .filter(|w| prop(w.r, "schemifyStub").is_none())
        .map(|w| {
            let pts = w
                .pts
                .iter()
                .map(|p| pt(*p, upi, "wire", &cell, notes))
                .collect();
            (w.r, pts, id_of(w.r).filter(|i| is_id(i)))
        })
        .collect();
    let (rejoined, absorbed) = rejoin_split_wires(&all_pts);
    for w in &sv.wires {
        tally.out += 1;
        let pts: Vec<[i32; 2]> = w
            .pts
            .iter()
            .map(|p| pt(*p, upi, "wire", &cell, notes))
            .collect();
        wire_pts.insert(w.r, pts.clone());
        if prop(w.r, "schemifyStub").is_some() || absorbed.contains(&w.r) {
            continue;
        }
        let pts = rejoined.get(&w.r).cloned().unwrap_or(pts);
        let mut first = true;
        for seg in pts.windows(2) {
            if seg[0] == seg[1] {
                continue;
            }
            let id = ids.next(if first { id_of(w.r) } else { None }, &reserved);
            first = false;
            let (a, b) = if (seg[1][0], seg[1][1]) < (seg[0][0], seg[0][1]) {
                (seg[1], seg[0])
            } else {
                (seg[0], seg[1])
            };
            begin(doc, RecordKind::Wire);
            doc.push_field(&id);
            for v in [a[0], a[1], b[0], b[1]] {
                doc.push_field_coord(v);
            }
            if w.width > 0 {
                kv(doc, "bus", "true");
            }
        }
    }
    // Labels.
    let mut dropped_alias = 0;
    let mut dropped_display = 0;
    for l in &sv.labels {
        tally.out += 1;
        let o = profile::orient_from_virtuoso(&l.orient).unwrap_or(Orient::R0);
        let origin = pt(l.xy, upi, "label", &cell, notes);
        match (l.layer.as_str(), l.purpose.as_str()) {
            ("annotate", "drawing3") => {
                dropped_alias += 1;
                tally.out -= 1;
                tally.reported += 1;
            }
            ("text", "drawing") => {
                let id = ids.next(id_of(l.r), &reserved);
                begin(doc, RecordKind::Text);
                doc.push_field(&id);
                doc.push_field_coord(origin[0]);
                doc.push_field_coord(origin[1]);
                doc.push_field(o.as_str());
                doc.push_field(&l.text);
                kv(doc, "size", &coord_text(size_milli(l.height, upi)));
            }
            ("wire", "label") | ("pin", "annotate") => {
                let at = match wire_pts.get(&l.parent) {
                    Some(p) => project(origin, p),
                    None => {
                        if l.parent == 0 {
                            notes.review(
                                "name",
                                &cell,
                                format!("label `{}` is glued to nothing", l.text),
                            );
                        }
                        origin
                    }
                };
                let (text, global) = if let Some((p, _)) = parse_netexpr(&l.text) {
                    (net_from_virtuoso(&p).text, false)
                } else if let Some(sn) = prop(l.r, "schemifyNet") {
                    match sn.strip_suffix(" global") {
                        Some(n) => (n.to_owned(), true),
                        None => (sn.to_owned(), false),
                    }
                } else {
                    let n = net_from_virtuoso(&l.text);
                    if n.review {
                        notes.review("name", &cell, format!("net `{}` kept verbatim", l.text));
                    }
                    (n.text, n.global)
                };
                let id = ids.next(id_of(l.r), &reserved);
                begin(doc, RecordKind::Label);
                doc.push_field(&id);
                doc.push_field_coord(at[0]);
                doc.push_field_coord(at[1]);
                doc.push_field(o.as_str());
                doc.push_field(&text);
                if global {
                    kv(doc, "global", "true");
                }
            }
            _ => {
                dropped_display += 1;
                tally.out -= 1;
                tally.reported += 1;
            }
        }
    }
    if dropped_alias > 0 {
        notes.dropped(
            "display-only",
            &cell,
            format!("{dropped_alias} wire alias label(s)"),
        );
    }
    if dropped_display > 0 {
        notes.dropped(
            "display-only",
            &cell,
            format!("{dropped_display} display label(s)"),
        );
    }
    // Shapes.
    let shapes: Vec<&Shape> = sv.shapes.iter().collect();
    let merged = merge_arcs(&shapes);
    tally.out += shapes.len();
    let mut dots = 0;
    for sh in &merged {
        if sh.layer == "wire" {
            dots += 1;
            continue;
        }
        if sh.layer != "text" {
            notes.dropped(
                "display-only",
                &cell,
                format!("{} on {}/{}", sh.kind, sh.layer, sh.purpose),
            );
            continue;
        }
        match shape_in(sh, upi, &cell, notes) {
            Some((form, vals, ang, closed)) => {
                let id = ids.next(id_of(sh.r), &reserved);
                push_graphic(
                    doc,
                    Some(&id),
                    &form,
                    &vals,
                    ang,
                    Style {
                        closed,
                        fill: false,
                        dash: sh.purpose == "drawing1",
                    },
                );
            }
            None => notes.dropped("display-only", &cell, format!("note {}", sh.kind)),
        }
    }
    if dots > 0 {
        notes.dropped("solder-dot", &cell, format!("{dots} object(s)"));
    }
    let st = cv_props(sv, &cell, notes, &mut tally);
    report_rest(sv, &cell, notes, &mut tally);
    assert_eq!(
        tally.out + tally.reported,
        sv.objects(),
        "{cell} schematic: count conservation"
    );
    st
}

/// The point of polyline `pts` nearest `p`: the projection on a horizontal or vertical
/// segment, else the nearest end (§6.2).
fn project(p: [i32; 2], pts: &[[i32; 2]]) -> [i32; 2] {
    let mut best = pts[0];
    let mut bd = i64::MAX;
    let d2 = |a: [i32; 2], b: [i32; 2]| {
        let (dx, dy) = (
            i64::from(a[0]) - i64::from(b[0]),
            i64::from(a[1]) - i64::from(b[1]),
        );
        dx * dx + dy * dy
    };
    for s in pts.windows(2) {
        let (a, b) = (s[0], s[1]);
        let q = if a[1] == b[1] {
            [p[0].clamp(a[0].min(b[0]), a[0].max(b[0])), a[1]]
        } else if a[0] == b[0] {
            [a[0], p[1].clamp(a[1].min(b[1]), a[1].max(b[1]))]
        } else if d2(p, a) <= d2(p, b) {
            a
        } else {
            b
        };
        if d2(p, q) < bd {
            bd = d2(p, q);
            best = q;
        }
    }
    best
}

// ---------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------

/// The fresh documents of every written dump cell (after `ade::import`), their stash
/// texts and whether a maestro setup came with them.
struct Fresh {
    docs: Vec<Option<(Document<'static>, Option<String>, bool)>>,
}

fn fresh(ctx: &Ctx<'_>, notes: &mut Notes) -> Fresh {
    let mut docs = Vec::new();
    let mut pending: Vec<Vec<Pending>> = Vec::new();
    for k in 0..ctx.dump.cells.len() {
        let mut p = Vec::new();
        if !ctx.targets[k].write {
            docs.push(None);
            pending.push(p);
            continue;
        }
        let (mut doc, st) = build(ctx, k, &mut p, notes);
        pending.push(p);
        let c = &ctx.dump.cells[k];
        let mut maestro = false;
        for test in &ctx.dump.maestro {
            let m = &test[0];
            if m.str(1).ok() == Some(c.lib.as_str()) && m.str(2).ok() == Some(c.name.as_str()) {
                maestro = true;
                ade::import(test, &mut doc, &c.name, notes);
            }
        }
        docs.push(Some((doc, st, maestro)));
    }
    if pending.iter().any(|p| !p.is_empty()) {
        remaster(ctx, &mut docs, &pending, notes);
    }
    Fresh { docs }
}

/// The Schemify-side re-mastering pass (§6.4): loads the fresh documents (with the
/// project's files and the core library), pulls wires back from the analogLib mirror
/// pins, labels them, and moves each pending instance onto its core cell with stubs and
/// labels at the core pins.
fn remaster(
    ctx: &Ctx<'_>,
    docs: &mut [Option<(Document<'static>, Option<String>, bool)>],
    pending: &[Vec<Pending>],
    notes: &mut Notes,
) {
    use crate::design::{geom, load, sym_pins};
    let mut files: Files = ctx.existing.clone();
    for (k, d) in docs.iter().enumerate() {
        if let Some((doc, _, _)) = d {
            files.insert(
                ctx.targets[k].path(),
                schemify_format::write(doc).into_bytes(),
            );
        }
    }
    let mut manifest = Document::new(FileKind::Project);
    for (l, d) in &ctx.libs {
        begin(&mut manifest, RecordKind::Lib);
        manifest.push_field(l);
        manifest.push_field("path");
        manifest.push_field(d);
    }
    for t in ctx
        .targets
        .iter()
        .filter(|t| t.write && !t.slib.is_empty() && !ctx.libs.contains_key(&t.slib))
    {
        if (1..manifest.record_count()).all(|r| manifest.field(r, 0) != Some(t.slib.as_str())) {
            begin(&mut manifest, RecordKind::Lib);
            manifest.push_field(&t.slib);
            manifest.push_field("path");
            manifest.push_field(&t.dir);
        }
    }
    files.insert(
        "schemify.project".into(),
        schemify_format::write(&manifest).into_bytes(),
    );
    for (k, todo) in pending.iter().enumerate() {
        if todo.is_empty() {
            continue;
        }
        let path = ctx.targets[k].path();
        let mut absent = BTreeSet::new();
        let d = loop {
            match load(&files, &absent, &[path.as_str()]) {
                Ok(d) => break Some(d),
                Err(crate::design::LoadError::Missing(m)) => absent.extend(m),
                Err(crate::design::LoadError::Invalid(p)) => {
                    notes.review(
                        "name",
                        &path,
                        format!("re-mastering skipped: {}", p.join("; ")),
                    );
                    break None;
                }
            }
        };
        let Some(d) = d else { continue };
        let ci = d.find(&path).expect("loaded");
        let g = geom(&d, ci);
        let n = crate::nets::extract(&d, ci, &g);
        let mut rem = Vec::new();
        let mut masters = Vec::new();
        for p in todo {
            let Some(inst) = g.inst_id.iter().position(|x| *x == p.id) else {
                continue;
            };
            let core = format!("{}.shprim", devices::DEVICES[p.device].core);
            let new_pins = schemify_doc::core_library()
                .iter()
                .find(|(nm, _)| *nm == core)
                .and_then(|(_, b)| Ctx::iface_of_text(b))
                .map(|i| {
                    i.pins
                        .into_iter()
                        .map(|q| (q.name, q.at))
                        .collect::<Vec<_>>()
                });
            let old_pins = sym_pins(&d.cells[g.inst_master[inst] as usize]);
            let new_names: Vec<String> = new_pins
                .as_ref()
                .map_or(Vec::new(), |v| v.iter().map(|x| x.0.clone()).collect());
            rem.push(crate::remap::Remaster {
                inst,
                pins: (0..old_pins.name.len())
                    .filter_map(|q| new_names.get(q).map(|nm| (q, nm.clone())))
                    .collect(),
                new_pins,
            });
            masters.push((
                inst,
                p.cellref.clone(),
                Some(
                    p.attrs
                        .iter()
                        .map(|(a, v)| (a.clone(), vec![v.clone()]))
                        .collect(),
                ),
            ));
        }
        let e = crate::remap::apply(&g, &n, &rem);
        let (doc, st, m) = docs[k].take().expect("pending cells are built");
        let mut taken: BTreeSet<String> = (1..doc.record_count())
            .filter(|&r| doc.record_kind(r).key() == schemify_format::KeyRule::Id)
            .filter_map(|r| doc.field(r, 0).map(str::to_owned))
            .collect();
        let seed = format!("{}/{}/remap", ctx.dump.cells[k].lib, ctx.dump.cells[k].name);
        let mut ord = 0u32;
        let new = crate::remap::apply_to_doc(&d.cells[ci].doc, &g, &e, &masters, || {
            ord += 1;
            let mut rng = IdRng::for_import(&seed, ord);
            let id = fresh_id(&mut rng, |c| taken.contains(c))
                .as_str()
                .to_owned();
            taken.insert(id.clone());
            id
        });
        docs[k] = Some((new, st, m));
    }
}

/// See [`super::import_targets`].
pub fn targets(sfyx: &str) -> Result<Vec<String>, Vec<String>> {
    let recs = sfyx::read(sfyx).map_err(|e| vec![e.to_string()])?;
    let d = dump(&recs).map_err(|e| vec![e.to_string()])?;
    let opts = ImportOptions::default();
    let mut out: BTreeSet<String> =
        ["schemify.project".to_owned(), "interop.map".to_owned()].into();
    for c in &d.cells {
        let slib = slib_of(&c.lib, &opts);
        if slib == "core" || consumed(&c.lib, &c.name) {
            continue;
        }
        for ext in ["shcomp", "shprim", "shbench"] {
            out.insert(if slib.is_empty() {
                format!("{}.{ext}", c.name)
            } else {
                format!("{slib}/{}.{ext}", c.name)
            });
        }
    }
    Ok(out.into_iter().collect())
}

/// See [`super::import`].
pub fn import(
    sfyx: &str,
    existing: &Files,
    map: &Mapping,
    opts: &ImportOptions,
) -> Result<Import, Vec<String>> {
    let recs = sfyx::read(sfyx).map_err(|e| vec![e.to_string()])?;
    let d = dump(&recs).map_err(|e| vec![e.to_string()])?;
    let mut notes = Notes::default();
    for r in &d.unknown {
        notes.dropped(
            "unknown-record",
            "-",
            format!("`{}` on line {}", r.tag, r.line),
        );
    }
    let ctx = Ctx::new(&d, existing, map, opts, &mut notes);
    let fr = fresh(&ctx, &mut notes);
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut new_libs: BTreeMap<String, String> = BTreeMap::new();
    let manifest = existing
        .get("schemify.project")
        .and_then(|m| crate::design::manifest_libs(m).ok())
        .unwrap_or_default();
    for (k, f) in fr.docs.into_iter().enumerate() {
        let Some((doc, st, maestro)) = f else {
            continue;
        };
        let t = &ctx.targets[k];
        let path = t.path();
        let cell = path.clone();
        let local = existing
            .get(&path)
            .and_then(|b| merge::parse_stash(std::str::from_utf8(b).ok()?, doc.kind()).ok());
        let stash = st.and_then(|s| match merge::parse_stash(&s, doc.kind()) {
            Ok(d) => Some(d),
            Err(e) => {
                notes.review("format", &cell, format!("stash ignored: {e}"));
                None
            }
        });
        let out = merge::merge(
            &doc,
            local.as_ref(),
            stash.as_ref(),
            maestro,
            &cell,
            &mut notes,
        );
        let out = with_fingerprints(&out, &doc);
        let text = schemify_format::write(&out);
        if let Some(old) = existing.get(&path)
            && old.as_slice() != text.as_bytes()
        {
            notes.add("updated", &cell, "the import changed this file");
        }
        if !t.slib.is_empty() && !manifest.contains_key(&t.slib) {
            new_libs.insert(t.slib.clone(), t.dir.clone());
        }
        files.push((path, text.into_bytes()));
    }
    if !new_libs.is_empty() {
        files.push((
            "schemify.project".into(),
            manifest_with(existing.get("schemify.project"), &new_libs).into_bytes(),
        ));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        files.windows(2).all(|w| w[0].0 < w[1].0),
        "one file per path"
    );
    Ok(Import { files, notes })
}

/// `doc` (the merged file) with header attributes `vsym`/`vsch` set to the fingerprints of
/// the parts its kind has, taken from `fresh` (what the dump holds, before local data was
/// merged, §7.5): an export can then tell a cellview nobody changed on the Schemify side.
fn with_fingerprints(doc: &Document<'static>, fresh: &Document<'static>) -> Document<'static> {
    use super::{Part, fingerprint, fingerprint_attr};
    let parts: &[Part] = match doc.kind() {
        FileKind::Shprim => &[Part::Symbol],
        FileKind::Shcomp => &[Part::Symbol, Part::Schematic],
        _ => &[Part::Schematic],
    };
    let mut out = Document::new(doc.kind());
    for a in doc.attrs(0) {
        let k = doc.attr_key(a);
        if k == "vsym" || k == "vsch" {
            continue;
        }
        out.push_attr(k);
        for v in doc.attr_values(a) {
            out.push_value(v);
        }
    }
    for p in parts {
        out.push_attr(fingerprint_attr(*p));
        out.push_value(&fingerprint(fresh, *p));
    }
    for r in 1..doc.record_count() {
        out.copy_record(doc, r);
    }
    assert_eq!(out.record_count(), doc.record_count());
    out
}

/// The manifest with `lib <name> path <dir>` rows added for `new`.
fn manifest_with(old: Option<&Vec<u8>>, new: &BTreeMap<String, String>) -> String {
    let mut doc = Document::new(FileKind::Project);
    if let Some(src) = old.and_then(|b| parse(b).ok()) {
        for a in src.attrs(0) {
            doc.push_attr(src.attr_key(a));
            for v in src.attr_values(a) {
                doc.push_value(v);
            }
        }
        for r in 1..src.record_count() {
            doc.copy_record(&src, r);
        }
    }
    for (l, d) in new {
        begin(&mut doc, RecordKind::Lib);
        doc.push_field(l);
        doc.push_field("path");
        doc.push_field(d);
    }
    schemify_format::write(&doc)
}

/// The fresh documents an import of `records` produces (no merge), keyed by (Virtuoso
/// library, cell): the export's stash oracle. Libraries map back through the inverse of
/// the export's renames; masters outside the dump resolve through `d`'s files.
pub(crate) fn image(
    records: &[Record],
    plan: &crate::map::Plan,
    d: &crate::design::Design,
    map: &Mapping,
) -> BTreeMap<(String, String), Document<'static>> {
    let dump = dump(records).expect("the exporter writes valid records");
    let mut iopts = ImportOptions::default();
    for (ci, c) in d.cells.iter().enumerate() {
        if plan.how[ci] == crate::map::How::Design {
            iopts.lib_map.insert(plan.lib[ci].clone(), c.lib.clone());
        }
    }
    iopts.lib_dir = d.libs.clone();
    let mut files: Files = Files::new();
    let mut manifest = Document::new(FileKind::Project);
    for (l, dir) in &d.libs {
        begin(&mut manifest, RecordKind::Lib);
        manifest.push_field(l);
        manifest.push_field("path");
        manifest.push_field(dir);
    }
    files.insert(
        "schemify.project".into(),
        schemify_format::write(&manifest).into_bytes(),
    );
    for c in &d.cells {
        if c.lib != "core" {
            files.insert(c.path.clone(), c.text().into_bytes());
        }
    }
    let mut notes = Notes::default();
    let ctx = Ctx::new(&dump, &files, map, &iopts, &mut notes);
    let fr = fresh(&ctx, &mut notes);
    let mut out = BTreeMap::new();
    for (k, f) in fr.docs.into_iter().enumerate() {
        if let Some((doc, _, _)) = f {
            out.insert((dump.cells[k].lib.clone(), dump.cells[k].name.clone()), doc);
        }
    }
    out
}

/// A dumped wire for [`rejoin_split_wires`]: (ref, points, its schemifyId when valid).
type DumpWire<'a> = (u32, Vec<[i32; 2]>, Option<&'a str>);

/// Virtuoso splits a wire where another wire T-joins it (`schCreateWire "draw"`, live
/// ICADVM20.1 run 2026-10-15; GUESS G5 said it would not), and the extra piece comes back
/// without the id or with a copy of it. Each such piece of a two-point wire is folded into the
/// collinear two-point wire it touches that owns the id, so the wire reads back as drawn.
/// Returns (owner ref → its rejoined two points, refs folded away). `wires`: (ref, points,
/// schemifyId) in dump order, stubs excluded.
fn rejoin_split_wires(wires: &[DumpWire<'_>]) -> (BTreeMap<u32, Vec<[i32; 2]>>, BTreeSet<u32>) {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut owners: Vec<(u32, [[i32; 2]; 2])> = Vec::new();
    let mut pieces: Vec<(u32, [[i32; 2]; 2])> = Vec::new();
    for (r, pts, id) in wires {
        let [a, b] = pts[..] else { continue };
        if a == b {
            continue;
        }
        match id {
            Some(i) if seen.insert(i) => owners.push((*r, [a, b])),
            _ => pieces.push((*r, [a, b])),
        }
    }
    let collinear = |s: [[i32; 2]; 2], p: [i32; 2]| {
        let d = [i64::from(s[1][0] - s[0][0]), i64::from(s[1][1] - s[0][1])];
        let v = [i64::from(p[0] - s[0][0]), i64::from(p[1] - s[0][1])];
        d[0] * v[1] - d[1] * v[0] == 0
    };
    let mut absorbed = BTreeSet::new();
    let mut changed = true;
    while changed {
        changed = false;
        for (pr, ps) in &pieces {
            if absorbed.contains(pr) {
                continue;
            }
            let hit = owners.iter_mut().find(|(_, os)| {
                collinear(*os, ps[0])
                    && collinear(*os, ps[1])
                    && (os.contains(&ps[0]) || os.contains(&ps[1]))
            });
            if let Some((_, os)) = hit {
                let mut all = [os[0], os[1], ps[0], ps[1]];
                all.sort_unstable();
                *os = [all[0], all[3]];
                absorbed.insert(*pr);
                changed = true;
            }
        }
    }
    let rejoined = owners
        .into_iter()
        .filter(|(r, s)| wires.iter().any(|(wr, pts, _)| wr == r && pts[..] != s[..]))
        .map(|(r, s)| (r, s.to_vec()))
        .collect();
    (rejoined, absorbed)
}
