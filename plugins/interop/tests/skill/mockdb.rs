//! A mock Virtuoso database for the SKILL test interpreter (spec `interop.md` §5.4).
//!
//! Each Cadence function checks its arity and argument types against [`SIGNATURES`] (taken
//! from the IC20.1 reference manuals, research `virtuoso-skill-db.md` and
//! `virtuoso-cdf-pdk.md`) and fails as documented: a bad argument signals (`Err`), a refused
//! operation returns `nil`. Behaviour the manuals leave open is marked `GUESS(Gn)`, matching
//! the spec's §13 table; the probe (`skill/sfy_probe.il`) settles each one on a real
//! Virtuoso. The maestro and OCEAN parts live in `mock_mae.rs` and `mock_ocean.rs`.
//!
//! Model: one arena of objects ([`Obj`], an attribute bag with `objType`), addressed by
//! `V::Obj(id)`. Libraries are the cds.lib list ([`MockDb::add_lib`]); cells, views and files
//! are DD objects; a cellview is open-able once created and keeps a backup of its content
//! while opened `"w"`, so a purge or a last close without `dbSave` restores what was saved
//! (or removes a never-saved view). Database order is unspecified in Virtuoso, so list
//! attributes (`shapes`, `instances`, `terminals`, `nets`, `cells`, `views`) come back in
//! *reverse* creation order: a dumper that forgets to sort fails the goldens.
//!
//! ponytail: one CDF level (the base cell CDF is the effective CDF; no library or user
//! levels); objects of a purged cellview stay in the arena (Virtuoso invalidates them).

use super::{Host, V};
use std::collections::BTreeMap;

#[path = "mock_mae.rs"]
pub mod mae;
#[path = "mock_ocean.rs"]
pub mod ocean;

/// Signatures of every Cadence function the mock implements: `(name, args)`. `args` is a
/// space-separated list of argument kinds, `[` starting the optional ones, `?key` naming an
/// accepted keyword: `d` db/DD/CDF object, `D` object or nil, `t` string, `T` string or nil,
/// `S` string or symbol, `x` integer, `n` number, `l` list, `p` point, `b` box, `L` layer
/// (`"name"` or `("layer" "purpose")`), `g` anything. `tests/skill_lint.rs` checks every call
/// in the shipped scripts against this table (maestro/OCEAN functions add their rows here).
pub const SIGNATURES: &[(&str, &str)] = &[
    // DD (skdfref "Design Management Functions")
    ("ddGetObj", "T [T T T D t]"),
    ("ddGetObjWritePath", "d"),
    // Database (skdfref "Database Access Functions")
    ("dbOpenCellViewByType", "g t g [T t D]"),
    ("dbSave", "d [g t t]"),
    ("dbClose", "d"),
    ("dbPurge", "d"),
    ("dbGetOpenCellViews", ""),
    ("dbDeleteObject", "d"),
    ("dbFindTermByName", "d t"),
    ("dbTransformPoint", "p l"),
    ("dbTransformBBox", "b l"),
    ("dbGetTermNetExpr", "d"),
    ("dbMakeNet", "d t [D]"),
    ("dbCreateTerm", "d t t [x g]"),
    ("dbCreatePin", "d D [T D]"),
    ("dbCreateTermNetExpr", "d t"),
    ("dbCreateRect", "d L b"),
    ("dbCreateLine", "d L l"),
    ("dbCreatePath", "d L l n [t]"),
    ("dbCreatePolygon", "d L l"),
    ("dbCreateEllipse", "d L b"),
    ("dbCreateArc", "d L b b"),
    ("dbCreateLabel", "d L p t t t t n"),
    ("dbReplaceProp", "d t t g"),
    ("dbCreateProp", "d t t g"),
    ("dbProduceMemName", "t"),
    // Schematic editor (skcompref "PI Functions")
    ("schCreateInst", "d d T p t [n]"),
    ("schCreateWire", "d t t l n n n [t t]"),
    ("schCreateWireLabel", "d d p t t t t n g"),
    ("schCreateNetExpression", "d t d p t t t n"),
    ("schCreatePin", "d d t t g p t [g g g]"),
    ("schCreateNoteLabel", "d p t t t t n t"),
    ("schGlueLabel", "d d"),
    ("schCheck", "d"),
    ("schSolder", "d p"),
    // CDF (cdfuser ch. 8 "CDF SKILL Summary")
    ("cdfGetBaseCellCDF", "d"),
    (
        "cdfCreateBaseCellCDF",
        "d ?doneProc ?formInitProc ?fieldWidth ?fieldHeight ?buttonFieldWidth ?promptWidth",
    ),
    ("cdfDeleteCDF", "d"),
    (
        "cdfCreateParam",
        "d ?name ?type ?defValue ?units ?parseAsNumber ?choices ?prompt ?use ?display ?editable ?dontSave ?callback ?storeDefault ?parseAsCEL ?description",
    ),
    ("cdfSaveCDF", "d"),
    ("cdfGetInstCDF", "d"),
    ("cdfFindParamByName", "d t"),
    ("cdfGetCellCDF", "d"),
    ("cdfUpdateInstParam", "d"),
    // OCEAN (OCEAN Reference 5.1.41, "Simulation Commands", "Data Access Commands");
    // `...` repeats the last argument kind.
    ("simulator", "S"),
    ("design", "[t t t] ?lib ?cell ?view"),
    ("resultsDir", "t"),
    ("save", "S [t] ... ?categ"),
    ("analysis", "S ?param ?start ?stop ?step ?saveOppoint ?dev"),
    ("desVar", "t g [t g] ..."),
    ("run", "[g] ?jobName ?host"),
    ("getData", "t ?result ?resultsDir"),
    ("value", "g g [g]"),
    // Maestro (IC23.1 ADE SKILL Reference syntax lines, via virtuoso-bridge-lite's function
    // database, MIT; research virtuoso-netlist-ade.md §5.3). Bodies in mock_mae.rs.
    ("maeOpenSetup", "t t t ?application ?histName ?mode"),
    ("maeCloseSession", "?session ?forceClose"),
    ("maeSaveSetup", "?lib ?cell ?view ?session"),
    ("maeGetSetup", "?typeName ?enabled ?run ?session"),
    (
        "maeCreateTest",
        "t ?sourceTest ?lib ?cell ?view ?simulator ?session",
    ),
    ("maeDeleteTest", "t ?run ?session"),
    ("maeSetAnalysis", "t t ?enable ?options ?session"),
    ("maeGetEnabledAnalysis", "t ?session"),
    ("maeGetAnalysis", "t t ?includeEmpty ?option ?session"),
    ("maeSetVar", "t g ?typeName ?typeValue ?session"),
    ("maeGetVar", "t ?typeName ?typeValue ?session"),
    (
        "maeSetCorner",
        "t ?enabled ?enableTests ?disableTests ?session",
    ),
    (
        "maeAddOutput",
        "t t ?outputType ?signalName ?expr ?plot ?save ?session",
    ),
    ("maeGetTestOutputs", "t ?session"),
    (
        "maeSetSpec",
        "t t ?minimum ?maximum ?gt ?lt ?range ?tolerance ?info ?weight ?corner ?session",
    ),
    ("maeSetEnvOption", "t ?options ?session"),
    ("maeGetEnvOption", "t ?includeEmpty ?option ?session"),
    ("maeSetSimOption", "t ?options ?session"),
    ("maeGetSimOption", "t ?includeEmpty ?option ?session"),
    ("axlGetTest", "x t"),
    ("axlGetMainSetupDB", "t"),
    ("axlGetCorner", "x t"),
    ("axlGetEnabled", "x"),
    ("axlGetModels", "x"),
    ("axlGetModel", "x t"),
    ("axlPutModel", "x t ?testName ?blockName"),
    ("axlGetModelFile", "x"),
    ("axlGetModelSection", "x"),
    ("axlSetModelFile", "x t"),
    ("axlSetModelSection", "x t"),
    ("axlGetVars", "x"),
    ("axlGetVar", "x t"),
    ("axlGetVarValue", "x"),
    ("axlGetSpecData", "x t t [t]"),
];

/// Functions with a [`SIGNATURES`] row whose body is in `mock_ocean.rs` or `mock_mae.rs`.
pub const DELEGATED: &[&str] = &[
    "simulator",
    "design",
    "resultsDir",
    "save",
    "analysis",
    "desVar",
    "run",
    "getData",
    "value",
    "maeOpenSetup",
    "maeCloseSession",
    "maeSaveSetup",
    "maeGetSetup",
    "maeCreateTest",
    "maeDeleteTest",
    "maeSetAnalysis",
    "maeGetEnabledAnalysis",
    "maeGetAnalysis",
    "maeSetVar",
    "maeGetVar",
    "maeSetCorner",
    "maeAddOutput",
    "maeGetTestOutputs",
    "maeSetSpec",
    "maeSetEnvOption",
    "maeGetEnvOption",
    "maeSetSimOption",
    "maeGetSimOption",
    "axlGetTest",
    "axlGetMainSetupDB",
    "axlGetCorner",
    "axlGetEnabled",
    "axlGetModels",
    "axlGetModel",
    "axlPutModel",
    "axlGetModelFile",
    "axlGetModelSection",
    "axlSetModelFile",
    "axlSetModelSection",
    "axlGetVars",
    "axlGetVar",
    "axlGetVarValue",
    "axlGetSpecData",
];

/// The arity a signature allows: (required positional, max positional, keywords).
pub fn arity(sig: &str) -> (usize, usize, Vec<&str>) {
    let mut req = 0;
    let mut max = 0;
    let mut opt = false;
    let mut keys = Vec::new();
    for tok in sig.split_whitespace() {
        if tok == "..." {
            max = usize::MAX;
            continue;
        }
        if let Some(k) = tok.strip_prefix('?') {
            keys.push(k);
            continue;
        }
        let t = tok.trim_matches(|c| c == '[' || c == ']');
        if tok.starts_with('[') {
            opt = true;
        }
        if t.is_empty() {
            continue;
        }
        max = max.saturating_add(1);
        if !opt {
            req += 1;
        }
    }
    (req, max, keys)
}

/// One object: an attribute bag. `objType` names its kind.
#[derive(Clone, Debug, Default)]
pub struct Obj {
    /// Attributes by name (relations hold `V::Obj` or lists of them).
    pub attrs: BTreeMap<String, V>,
}

/// The mock database: libraries, cells, cellviews and their objects, CDF, maestro setups.
pub struct MockDb {
    /// Maestro state.
    pub mae: mae::Mae,
    /// OCEAN / simulation state.
    pub ocean: ocean::Ocean,
    objs: Vec<Obj>,
    libs: Vec<u32>,
    cvs: BTreeMap<(String, String, String), u32>,
    backups: BTreeMap<u32, BTreeMap<String, V>>,
    pins_made: u32,
}

impl Default for MockDb {
    fn default() -> Self {
        MockDb {
            mae: mae::Mae::default(),
            ocean: ocean::Ocean::default(),
            objs: vec![Obj::default()],
            libs: Vec::new(),
            cvs: BTreeMap::new(),
            backups: BTreeMap::new(),
            pins_made: 0,
        }
    }
}

const ORIENTS: &[&str] = &["R0", "R90", "R180", "R270", "MY", "MYR90", "MX", "MXR90"];
const JUSTIFY: &[&str] = &[
    "lowerLeft",
    "lowerCenter",
    "lowerRight",
    "centerLeft",
    "centerCenter",
    "centerRight",
    "upperLeft",
    "upperCenter",
    "upperRight",
];
const FONTS: &[&str] = &[
    "euroStyle",
    "gothic",
    "math",
    "roman",
    "script",
    "stick",
    "swedish",
    "fixed",
    "milSpec",
];
const LABEL_TYPES: &[&str] = &["normalLabel", "NLPLabel", "ILLabel"];
const PROP_TYPES: &[&str] = &[
    "boolean", "fileName", "float", "hierProp", "ILExpr", "ILList", "int", "NLPExpr", "netSet",
    "string", "time",
];
const CDF_TYPES: &[&str] = &[
    "string", "int", "float", "radio", "cyclic", "boolean", "button", "netSet",
];
/// Attributes returned in reverse creation order ("database order").
const DB_ORDER: &[&str] = &["shapes", "instances", "terminals", "nets", "cells", "views"];

fn num(v: &V) -> Option<f64> {
    match v {
        V::Int(i) => Some(*i as f64),
        V::Float(f) => Some(*f),
        _ => None,
    }
}

fn pt(v: &V) -> Option<(f64, f64)> {
    match v.items() {
        [x, y] => Some((num(x)?, num(y)?)),
        _ => None,
    }
}

fn bb(v: &V) -> Option<((f64, f64), (f64, f64))> {
    match v.items() {
        [a, b] => Some((pt(a)?, pt(b)?)),
        _ => None,
    }
}

fn vpt(p: (f64, f64)) -> V {
    V::list(vec![V::Float(p.0), V::Float(p.1)])
}

fn vbox(a: (f64, f64), b: (f64, f64)) -> V {
    let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
    let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
    V::list(vec![vpt((x0, y0)), vpt((x1, y1))])
}

fn s(v: &V) -> Option<&str> {
    match v {
        V::Str(x) => Some(x),
        _ => None,
    }
}

/// Orientation point maps in Virtuoso's y-up frame (research §7): mirror first, then turn
/// counter-clockwise.
// GUESS(G1): MYR90 = (-y, -x) and MXR90 = (y, x) (derived; the other six are definitions).
fn orient_map(o: &str, (x, y): (f64, f64)) -> Option<(f64, f64)> {
    Some(match o {
        "R0" => (x, y),
        "R90" => (-y, x),
        "R180" => (-x, -y),
        "R270" => (y, -x),
        "MY" => (-x, y),
        "MYR90" => (-y, -x),
        "MX" => (x, -y),
        "MXR90" => (y, x),
        _ => return None,
    })
}

/// Applies a transform list `(offset orient [mag])`: orient, then magnify, then translate.
fn transform(p: (f64, f64), t: &V) -> Result<(f64, f64), String> {
    let items = t.items();
    let off = items
        .first()
        .and_then(pt)
        .ok_or("transform: needs (offset orient [mag])")?;
    let o = items.get(1).and_then(s).ok_or("transform: orient")?;
    let mag = items.get(2).and_then(num).unwrap_or(1.0);
    let (x, y) = orient_map(o, p).ok_or_else(|| format!("transform: orient {o}"))?;
    Ok((x * mag + off.0, y * mag + off.1))
}

fn bad(name: &str, k: usize, what: &str) -> String {
    format!("*Error* {name}: argument #{k} should be {what}")
}

/// Checks `args` against `sig`; returns positional arguments and keyword arguments.
/// Positional arguments and `(keyword, value)` pairs of a checked call.
type Args = (Vec<V>, Vec<(String, V)>);

fn check(name: &str, sig: &str, args: &[V]) -> Result<Args, String> {
    let (req, max, keys) = arity(sig);
    let kinds: Vec<&str> = sig
        .split_whitespace()
        .filter(|t| !t.starts_with('?'))
        .map(|t| t.trim_matches(|c| c == '[' || c == ']'))
        .filter(|t| !t.is_empty())
        .collect();
    let mut pos = Vec::new();
    let mut kw = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a {
            V::Sym(k) if k.starts_with('?') => {
                let k = &k[1..];
                if !keys.contains(&k) {
                    return Err(format!("*Error* {name}: unknown keyword ?{k}"));
                }
                let v = it
                    .next()
                    .ok_or_else(|| format!("*Error* {name}: ?{k} without a value"))?;
                kw.push((k.to_owned(), v.clone()));
            }
            _ => pos.push(a.clone()),
        }
    }
    if pos.len() < req || pos.len() > max {
        return Err(format!(
            "*Error* {name}: called with {} arguments, takes {req}..{max}",
            pos.len()
        ));
    }
    for (k, (v, kind)) in pos.iter().zip(&kinds).enumerate() {
        let ok = match *kind {
            "d" => matches!(v, V::Obj(_)),
            "D" => matches!(v, V::Obj(_) | V::Nil),
            "t" => matches!(v, V::Str(_)),
            "T" => matches!(v, V::Str(_) | V::Nil),
            "S" => matches!(v, V::Str(_) | V::Sym(_)),
            "x" => matches!(v, V::Int(_)),
            "n" => num(v).is_some(),
            "l" => matches!(v, V::List(_) | V::Nil),
            "p" => pt(v).is_some(),
            "b" => bb(v).is_some(),
            "L" => matches!(v, V::Str(_)) || matches!(v.items(), [V::Str(_), V::Str(_)]),
            _ => true,
        };
        if !ok {
            let what = match *kind {
                "d" => "a database object",
                "D" => "a database object or nil",
                "t" => "a string",
                "T" => "a string or nil",
                "S" => "a string or symbol",
                "x" => "an integer",
                "n" => "a number",
                "l" => "a list",
                "p" => "a point (x y)",
                "b" => "a box ((x0 y0) (x1 y1))",
                _ => "a layer name or (layer purpose)",
            };
            return Err(bad(name, k + 1, what));
        }
    }
    Ok((pos, kw))
}

impl MockDb {
    // ------------------------------------------------------------------ Rust API

    /// Adds library `name` to the cds.lib list; returns its DD object (the existing one when
    /// already defined).
    pub fn add_lib(&mut self, name: &str) -> u32 {
        if let Some(l) = self.lib(name) {
            return l;
        }
        let l = self.new_obj("ddLib", vec![("name", V::str(name))]);
        self.libs.push(l);
        l
    }

    /// The DD object of library `name`, if defined.
    pub fn lib(&self, name: &str) -> Option<u32> {
        self.libs
            .iter()
            .copied()
            .find(|&l| s(&self.a(l, "name")) == Some(name))
    }

    /// Removes cell `lib/cell` from the library (as when its library leaves cds.lib, or the
    /// cell is deleted on disk): `ddGetObj` no longer finds it, its cellviews are gone, and
    /// instances drawn of it stay behind unbound.
    pub fn forget_cell(&mut self, lib: &str, cell: &str) {
        let l = self.lib(lib).expect("library defined");
        let c = self
            .dd_find(lib, Some(cell), None, None)
            .expect("cell exists");
        let kept: Vec<V> = self
            .a(l, "cells")
            .items()
            .iter()
            .filter(|x| Self::obj(x) != Some(c))
            .cloned()
            .collect();
        self.put(l, "cells", V::list(kept));
        self.cvs.retain(|k, _| !(k.0 == lib && k.1 == cell));
    }

    /// The cellview `lib/cell/view` if it exists (saved, or open and not yet saved).
    pub fn cellview(&self, lib: &str, cell: &str, view: &str) -> Option<u32> {
        self.cvs
            .get(&(lib.into(), cell.into(), view.into()))
            .copied()
    }

    /// True when cellview `cv` has been saved at least once and has no unsaved edits.
    pub fn is_saved(&self, cv: u32) -> bool {
        self.a(cv, "sfySaved").truthy() && !self.a(cv, "modifiedButNotSaved").truthy()
    }

    /// `obj~>attr`, exactly as SKILL sees it (attributes, then properties; nil otherwise).
    pub fn attr(&self, obj: u32, attr: &str) -> V {
        self.get_attr(obj, attr)
    }

    /// Calls Cadence function `name` as SKILL would (checked against [`SIGNATURES`]).
    ///
    /// # Errors
    ///
    /// What the function signals: a bad argument, or an unknown function.
    pub fn call_fn(&mut self, name: &str, args: &[V]) -> Result<V, String> {
        self.call_db(name, args)
            .unwrap_or_else(|| Err(format!("undefined function {name}")))
    }

    /// Creates the DD file `lib/cell/view/file` (a text view's master) and returns the path
    /// its contents live at in the interpreter's files.
    pub fn add_text_file(&mut self, lib: &str, cell: &str, view: &str, file: &str) -> String {
        let f = self
            .dd_get(lib, Some(cell), Some(view), Some(file), "a")
            .expect("library defined");
        s(&self.a(f, "readPath")).unwrap_or_default().to_owned()
    }

    /// The base cell CDF of `lib/cell`, if any.
    pub fn base_cdf(&self, lib: &str, cell: &str) -> Option<u32> {
        let c = self.dd_find(lib, Some(cell), None, None)?;
        match self.a(c, "sfyBaseCdf") {
            V::Obj(o) => Some(o),
            _ => None,
        }
    }

    // ------------------------------------------------------------------ arena

    fn new_obj(&mut self, ty: &str, attrs: Vec<(&str, V)>) -> u32 {
        let mut o = Obj::default();
        o.attrs.insert("objType".into(), V::str(ty));
        for (k, v) in attrs {
            o.attrs.insert(k.into(), v);
        }
        self.objs.push(o);
        (self.objs.len() - 1) as u32
    }

    fn a(&self, id: u32, k: &str) -> V {
        self.objs
            .get(id as usize)
            .and_then(|o| o.attrs.get(k))
            .cloned()
            .unwrap_or(V::Nil)
    }

    fn put(&mut self, id: u32, k: &str, v: V) {
        self.objs[id as usize].attrs.insert(k.into(), v);
    }

    fn push(&mut self, id: u32, k: &str, v: V) {
        let mut l = self.a(id, k).items().to_vec();
        l.push(v);
        self.put(id, k, V::list(l));
    }

    fn unlink(&mut self, id: u32, k: &str, x: u32) {
        let l: Vec<V> = self
            .a(id, k)
            .items()
            .iter()
            .filter(|v| !matches!(v, V::Obj(o) if *o == x))
            .cloned()
            .collect();
        self.put(id, k, V::list(l));
    }

    fn ty(&self, id: u32) -> String {
        s(&self.a(id, "objType")).unwrap_or("").to_owned()
    }

    fn obj(v: &V) -> Option<u32> {
        match v {
            V::Obj(o) => Some(*o),
            _ => None,
        }
    }

    // ------------------------------------------------------------------ DD

    fn dd_child(&self, parent: u32, rel: &str, name: &str) -> Option<u32> {
        self.a(parent, rel)
            .items()
            .iter()
            .filter_map(Self::obj)
            .find(|&c| s(&self.a(c, "name")) == Some(name))
    }

    fn dd_find(
        &self,
        lib: &str,
        cell: Option<&str>,
        view: Option<&str>,
        file: Option<&str>,
    ) -> Option<u32> {
        let mut o = self.lib(lib)?;
        for (rel, n) in [("cells", cell), ("views", view), ("files", file)] {
            match n {
                Some(n) => o = self.dd_child(o, rel, n)?,
                None => return Some(o),
            }
        }
        Some(o)
    }

    /// `ddGetObj` with mode `r`, `a` or `w` (creating cells, views and files on `a`/`w`).
    fn dd_get(
        &mut self,
        lib: &str,
        cell: Option<&str>,
        view: Option<&str>,
        file: Option<&str>,
        mode: &str,
    ) -> Option<u32> {
        let l = self.lib(lib)?;
        let create = mode == "a" || mode == "w";
        let mut o = l;
        let mut path = format!("/mock/{lib}");
        for (rel, ty, n) in [
            ("cells", "ddCell", cell),
            ("views", "ddView", view),
            ("files", "ddFile", file),
        ] {
            let Some(n) = n else { return Some(o) };
            path = format!("{path}/{n}");
            match self.dd_child(o, rel, n) {
                Some(c) => {
                    if ty == "ddFile" && mode == "w" {
                        return None; // "fails if a master object already exists"
                    }
                    o = c;
                }
                None if create => {
                    let up = match ty {
                        "ddCell" => "lib",
                        "ddView" => "cell",
                        _ => "view",
                    };
                    let c = self.new_obj(ty, vec![("name", V::str(n)), (up, V::Obj(o))]);
                    if ty == "ddFile" {
                        self.put(c, "readPath", V::str(&path));
                        self.put(c, "writePath", V::str(&path));
                    }
                    self.push(o, rel, V::Obj(c));
                    o = c;
                }
                None => return None,
            }
        }
        Some(o)
    }

    // ------------------------------------------------------------------ cellviews

    fn editable(&self, cv: u32) -> bool {
        matches!(s(&self.a(cv, "mode")), Some("w" | "a"))
    }

    fn touch(&mut self, cv: u32) {
        self.put(cv, "modifiedButNotSaved", V::T);
    }

    fn cv_of(&self, o: u32) -> Option<u32> {
        if self.ty(o) == "cellView" {
            return Some(o);
        }
        Self::obj(&self.a(o, "cellView"))
    }

    fn open_cv(
        &mut self,
        lib: &str,
        cell: &str,
        views: &[String],
        vtype: Option<&str>,
        mode: &str,
    ) -> Result<V, String> {
        if !matches!(mode, "r" | "a" | "w" | "s") {
            return Err(format!(
                "*Error* dbOpenCellViewByType: mode {mode} is not r, a, w or s"
            ));
        }
        if self.lib(lib).is_none() {
            return Ok(V::Nil);
        }
        for view in views {
            let key = (lib.to_owned(), cell.to_owned(), view.clone());
            if let Some(&cv) = self.cvs.get(&key) {
                let n = num(&self.a(cv, "sfyOpen")).unwrap_or(0.0) as i64;
                self.put(cv, "sfyOpen", V::Int(n + 1));
                match mode {
                    "w" => {
                        let keep = self.objs[cv as usize].attrs.clone();
                        self.backups.entry(cv).or_insert(keep);
                        for k in ["instances", "shapes", "terminals", "nets", "prop"] {
                            self.put(cv, k, V::Nil);
                        }
                        self.put(cv, "mode", V::str("w"));
                        self.touch(cv);
                    }
                    "a" | "s" if !self.editable(cv) => {
                        let keep = self.objs[cv as usize].attrs.clone();
                        self.backups.entry(cv).or_insert(keep);
                        self.put(cv, "mode", V::str(mode));
                    }
                    _ => {}
                }
                return Ok(V::Obj(cv));
            }
        }
        // Not found: only a/w/s with a view type create, in the first view named.
        let (Some(vt), Some(view)) = (vtype.filter(|v| !v.is_empty()), views.first()) else {
            return Ok(V::Nil);
        };
        if mode == "r" {
            return Ok(V::Nil);
        }
        let ddc = self
            .dd_get(lib, Some(cell), None, None, "a")
            .expect("lib exists");
        let ddv = self
            .dd_get(lib, Some(cell), Some(view), None, "a")
            .expect("lib exists");
        let ddl = self.lib(lib).expect("lib exists");
        let cv = self.new_obj(
            "cellView",
            vec![
                ("libName", V::str(lib)),
                ("cellName", V::str(cell)),
                ("viewName", V::str(view)),
                ("cellViewType", V::str(vt)),
                // GUESS(G2): schematic and schematicSymbol cellviews: 160 DBU per inch.
                ("DBUPerUU", V::Float(160.0)),
                ("userUnits", V::str("inch")),
                ("mode", V::str(mode)),
                ("lib", V::Obj(ddl)),
                ("cell", V::Obj(ddc)),
                ("sfyView", V::Obj(ddv)),
                ("sfyOpen", V::Int(1)),
                ("modifiedButNotSaved", V::T),
            ],
        );
        self.cvs.insert((lib.into(), cell.into(), view.clone()), cv);
        Ok(V::Obj(cv))
    }

    /// Drops unsaved edits: restores the content saved before the cellview was opened for
    /// edit, or removes a never-saved cellview (and its DD view when it holds no files).
    fn revert(&mut self, cv: u32) {
        if !self.a(cv, "modifiedButNotSaved").truthy() {
            self.backups.remove(&cv);
            return;
        }
        if let Some(b) = self.backups.remove(&cv) {
            self.objs[cv as usize].attrs = b;
            self.put(cv, "mode", V::str("r"));
            return;
        }
        if !self.a(cv, "sfySaved").truthy() {
            let key = (
                s(&self.a(cv, "libName")).unwrap_or("").to_owned(),
                s(&self.a(cv, "cellName")).unwrap_or("").to_owned(),
                s(&self.a(cv, "viewName")).unwrap_or("").to_owned(),
            );
            self.cvs.remove(&key);
            if let (Some(ddc), Some(ddv)) = (
                Self::obj(&self.a(cv, "cell")),
                Self::obj(&self.a(cv, "sfyView")),
            ) && !self.a(ddv, "files").truthy()
            {
                self.unlink(ddc, "views", ddv);
            }
        }
    }

    // ------------------------------------------------------------------ figures

    fn lpp(v: &V) -> (String, String) {
        match v {
            V::Str(l) => (l.to_string(), "drawing".into()),
            _ => {
                let i = v.items();
                (
                    s(&i[0]).unwrap_or("").to_owned(),
                    s(&i[1]).unwrap_or("").to_owned(),
                )
            }
        }
    }

    fn new_shape(
        &mut self,
        cv: u32,
        ty: &str,
        lpp: &(String, String),
        extra: Vec<(&str, V)>,
    ) -> u32 {
        let mut attrs = vec![
            ("cellView", V::Obj(cv)),
            ("layerName", V::str(&lpp.0)),
            ("purpose", V::str(&lpp.1)),
            ("lpp", V::list(vec![V::str(&lpp.0), V::str(&lpp.1)])),
        ];
        attrs.extend(extra);
        let id = self.new_obj(ty, attrs);
        self.push(cv, "shapes", V::Obj(id));
        self.touch(cv);
        id
    }

    fn points(v: &V, min: usize) -> Option<V> {
        let pts: Option<Vec<(f64, f64)>> = v.items().iter().map(pt).collect();
        let pts = pts?;
        if pts.len() < min {
            return None;
        }
        Some(V::list(pts.into_iter().map(vpt).collect()))
    }

    fn new_label(
        &mut self,
        cv: u32,
        lpp: &(String, String),
        a: &[V],
        ltype: &str,
    ) -> Result<u32, String> {
        // a = (point text justify orient font height)
        let just = s(&a[2]).unwrap_or("");
        let orient = s(&a[3]).unwrap_or("");
        let font = s(&a[4]).unwrap_or("");
        if !JUSTIFY.contains(&just) {
            return Err(format!("*Error* label: justification {just}"));
        }
        if !ORIENTS.contains(&orient) {
            return Err(format!("*Error* label: orientation {orient}"));
        }
        if !FONTS.contains(&font) {
            return Err(format!("*Error* label: font {font}"));
        }
        let h = num(&a[5]).unwrap_or(0.0);
        if h <= 0.0 {
            return Err("*Error* label: the height must be a positive number".into());
        }
        Ok(self.new_shape(
            cv,
            "label",
            lpp,
            vec![
                ("xy", vpt(pt(&a[0]).expect("checked"))),
                ("theLabel", a[1].clone()),
                ("justify", V::str(just)),
                ("orient", V::str(orient)),
                ("font", V::str(font)),
                ("height", V::Float(h)),
                ("labelType", V::str(ltype)),
                ("isVisible", V::T),
            ],
        ))
    }

    fn glue(&mut self, label: u32, fig: Option<u32>) {
        if let Some(old) = Self::obj(&self.a(label, "parent")) {
            self.unlink(old, "children", label);
        }
        match fig {
            Some(f) => {
                self.put(label, "parent", V::Obj(f));
                self.push(f, "children", V::Obj(label));
            }
            None => self.put(label, "parent", V::Nil),
        }
    }

    fn is_wire(&self, o: u32) -> bool {
        matches!(self.ty(o).as_str(), "line" | "path") && s(&self.a(o, "layerName")) == Some("wire")
    }

    fn is_pin_fig(&self, o: u32) -> bool {
        (self.ty(o) == "inst" && s(&self.a(o, "purpose")) == Some("pin")) || self.ty(o) == "pin"
    }

    fn new_inst(
        &mut self,
        cv: u32,
        master: u32,
        name: &str,
        xy: (f64, f64),
        orient: &str,
        purpose: &str,
    ) -> u32 {
        let (base, n) = match name.split_once('<') {
            Some((b, r)) => {
                let r = r.trim_end_matches('>');
                let n = match r.split_once(':') {
                    Some((x, y)) => {
                        x.parse::<i64>()
                            .unwrap_or(0)
                            .abs_diff(y.parse::<i64>().unwrap_or(0))
                            + 1
                    }
                    None => 1,
                };
                (b.to_owned(), n as i64)
            }
            None => (name.to_owned(), 1),
        };
        let inst = self.new_obj(
            "inst",
            vec![
                ("cellView", V::Obj(cv)),
                ("name", V::str(name)),
                ("baseName", V::str(&base)),
                ("numInst", V::Int(n)),
                ("libName", self.a(master, "libName")),
                ("cellName", self.a(master, "cellName")),
                ("viewName", self.a(master, "viewName")),
                ("master", V::Obj(master)),
                ("xy", vpt(xy)),
                ("orient", V::str(orient)),
                ("mag", V::Float(1.0)),
                ("purpose", V::str(purpose)),
            ],
        );
        for t in self.a(master, "terminals").items().to_vec() {
            let Some(t) = Self::obj(&t) else { continue };
            let it = self.new_obj(
                "instTerm",
                vec![
                    ("name", self.a(t, "name")),
                    ("term", V::Obj(t)),
                    ("inst", V::Obj(inst)),
                    ("direction", self.a(t, "direction")),
                    ("cellView", V::Obj(cv)),
                ],
            );
            self.push(inst, "instTerms", V::Obj(it));
        }
        self.push(cv, "instances", V::Obj(inst));
        self.touch(cv);
        inst
    }

    fn inst_name_ok(name: &str) -> bool {
        // base[<N[:M]>] only (skdfref "Instance Name Syntax").
        let (base, rest) = match name.split_once('<') {
            Some((b, r)) => (b, Some(r)),
            None => (name, None),
        };
        let base_ok = !base.is_empty()
            && base
                .chars()
                .all(|c| c.is_ascii_graphic() && !"<>(),/".contains(c));
        let rest_ok = match rest {
            None => true,
            Some(r) => r.strip_suffix('>').is_some_and(|r| {
                let parts: Vec<&str> = r.split(':').collect();
                parts.len() <= 2
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            }),
        };
        base_ok && rest_ok
    }

    fn bbox(&self, o: u32) -> Option<((f64, f64), (f64, f64))> {
        let grow = |acc: Option<((f64, f64), (f64, f64))>, p: (f64, f64)| {
            Some(match acc {
                None => (p, p),
                Some((a, b)) => ((a.0.min(p.0), a.1.min(p.1)), (b.0.max(p.0), b.1.max(p.1))),
            })
        };
        match self.ty(o).as_str() {
            "line" | "path" | "polygon" => self
                .a(o, "points")
                .items()
                .iter()
                .filter_map(pt)
                .fold(None, grow),
            "rect" | "ellipse" => bb(&self.a(o, "bBox")),
            "arc" => bb(&self.a(o, "ellipseBBox")),
            "label" | "dot" => pt(&self.a(o, "xy")).map(|p| (p, p)),
            "cellView" => {
                let mut acc = None;
                for f in self.a(o, "shapes").items().iter().filter_map(Self::obj) {
                    if let Some((a, b)) = self.bbox(f) {
                        acc = grow(grow(acc, a), b);
                    }
                }
                acc
            }
            "inst" => {
                let m = Self::obj(&self.a(o, "master"))?;
                let (a, b) = self.bbox(m)?;
                let t = self.inst_transform(o);
                let pa = transform(a, &t).ok()?;
                let pb = transform(b, &t).ok()?;
                let r = vbox(pa, pb);
                bb(&r)
            }
            _ => None,
        }
    }

    fn inst_transform(&self, o: u32) -> V {
        V::list(vec![self.a(o, "xy"), self.a(o, "orient"), self.a(o, "mag")])
    }

    // ------------------------------------------------------------------ props

    fn prop_value_ok(ty: &str, v: &V) -> bool {
        match ty {
            "string" | "time" | "fileName" | "ILExpr" | "NLPExpr" | "netSet" => {
                matches!(v, V::Str(_))
            }
            "int" => matches!(v, V::Int(_)),
            "float" => matches!(v, V::Float(_)),
            "boolean" => matches!(v, V::T | V::Nil),
            "ILList" => matches!(v, V::List(_) | V::Nil),
            _ => true,
        }
    }

    fn find_prop(&self, o: u32, name: &str) -> Option<u32> {
        self.a(o, "prop")
            .items()
            .iter()
            .filter_map(Self::obj)
            .find(|&p| s(&self.a(p, "name")) == Some(name))
    }

    fn set_prop(&mut self, o: u32, name: &str, ty: &str, v: V) -> u32 {
        let p = match self.find_prop(o, name) {
            Some(p) => p,
            None => {
                let p = self.new_obj("prop", vec![("name", V::str(name)), ("object", V::Obj(o))]);
                self.push(o, "prop", V::Obj(p));
                p
            }
        };
        self.put(p, "valueType", V::str(ty));
        self.put(p, "value", v);
        if let Some(cv) = self.cv_of(o) {
            self.touch(cv);
        }
        p
    }

    fn remove_prop(&mut self, o: u32, name: &str) {
        if let Some(p) = self.find_prop(o, name) {
            self.unlink(o, "prop", p);
        }
    }

    /// A property's value as `~>` reads it: booleans as "TRUE"/"FALSE" (skdfref dbGet).
    fn prop_read(&self, p: u32) -> V {
        let v = self.a(p, "value");
        if s(&self.a(p, "valueType")) == Some("boolean") {
            return V::str(if v.truthy() { "TRUE" } else { "FALSE" });
        }
        v
    }

    // ------------------------------------------------------------------ CDF

    fn cdf_param_type_ok(ty: &str, v: &V) -> bool {
        match ty {
            "int" => matches!(v, V::Int(_)),
            "float" => matches!(v, V::Float(_) | V::Int(_)),
            "boolean" => matches!(v, V::T | V::Nil | V::Str(_)),
            "button" => true,
            _ => matches!(v, V::Str(_)),
        }
    }

    fn inst_cdf(&mut self, inst: u32) -> V {
        let (lib, cell) = (self.a(inst, "libName"), self.a(inst, "cellName"));
        let (Some(lib), Some(cell)) = (s(&lib), s(&cell)) else {
            return V::Nil;
        };
        let Some(base) = self.base_cdf(lib, cell) else {
            return V::Nil;
        };
        let cdf = self.new_obj(
            "cdf",
            vec![
                ("type", V::str("instData")),
                ("id", V::Obj(inst)),
                ("simInfo", self.a(base, "simInfo")),
                ("paramLabelSet", self.a(base, "paramLabelSet")),
            ],
        );
        for p in self.a(base, "parameters").items().to_vec() {
            let Some(p) = Self::obj(&p) else { continue };
            let mut o = self.objs[p as usize].clone();
            o.attrs.insert("sfyInst".into(), V::Obj(inst));
            o.attrs.insert("sfyCdf".into(), V::Obj(cdf));
            o.attrs.remove("value");
            self.objs.push(o);
            let id = (self.objs.len() - 1) as u32;
            self.push(cdf, "parameters", V::Obj(id));
        }
        V::Obj(cdf)
    }

    /// `cdfGetCellCDF(ddCell)`: the cell's effective CDF, here a copy of the base CDF held
    /// by the cell as the one in memory (`cdfUpdateInstParam` reads it), made again when the
    /// base CDF changed shape. Its params have no instance: setting `value` sets the
    /// in-memory default, as on a real cell CDF, and it stays set for every later caller,
    /// which is why callers restore it.
    fn cell_cdf(&mut self, cell: u32) -> V {
        if self.ty(cell) != "ddCell" {
            return V::Nil;
        }
        let Some(base) = Self::obj(&self.a(cell, "sfyBaseCdf")) else {
            return V::Nil;
        };
        if let Some(c) = Self::obj(&self.a(cell, "sfyCellCdf"))
            && self.a(c, "parameters").items().len() == self.a(base, "parameters").items().len()
        {
            return V::Obj(c);
        }
        let cdf = self.new_obj(
            "cdf",
            vec![
                ("type", V::str("cellData")),
                ("id", V::Obj(cell)),
                ("simInfo", self.a(base, "simInfo")),
                ("paramLabelSet", self.a(base, "paramLabelSet")),
            ],
        );
        for p in self.a(base, "parameters").items().to_vec() {
            let Some(p) = Self::obj(&p) else { continue };
            let o = self.objs[p as usize].clone();
            self.objs.push(o);
            let id = (self.objs.len() - 1) as u32;
            self.push(cdf, "parameters", V::Obj(id));
        }
        self.put(cell, "sfyCellCdf", V::Obj(cdf));
        V::Obj(cdf)
    }

    /// `cdfUpdateInstParam(inst)`: copies every value of the master cell's in-memory cell
    /// CDF (the latest `cdfGetCellCDF`) onto the instance, through the base parameter so a
    /// value equal to the default leaves no property. Without a cell CDF in memory it
    /// changes nothing.
    fn update_inst_params(&mut self, inst: u32) -> Result<V, String> {
        let (lib, cell) = (self.a(inst, "libName"), self.a(inst, "cellName"));
        let (Some(lib), Some(cell)) = (s(&lib).map(str::to_owned), s(&cell).map(str::to_owned))
        else {
            return Ok(V::Nil);
        };
        let Some(c) = self.dd_find(&lib, Some(&cell), None, None) else {
            return Ok(V::Nil);
        };
        let (Some(cdf), Some(base)) = (
            Self::obj(&self.a(c, "sfyCellCdf")),
            self.base_cdf(&lib, &cell),
        ) else {
            return Ok(V::T);
        };
        for p in self.a(cdf, "parameters").items().to_vec() {
            let Some(p) = Self::obj(&p) else { continue };
            let n = s(&self.a(p, "name")).unwrap_or("").to_owned();
            let Some(bp) = self.cdf_param(base, &n) else {
                continue;
            };
            if s(&self.a(bp, "paramType")) == Some("button") {
                continue;
            }
            let v = self.a(p, "defValue");
            self.set_inst_param(bp, inst, v)?;
        }
        Ok(V::T)
    }

    /// `param~>value = v` on an instance CDF: stores the instance property with the CDF's
    /// type.
    fn set_inst_param(&mut self, p: u32, inst: u32, v: V) -> Result<V, String> {
        let ty = s(&self.a(p, "paramType")).unwrap_or("string").to_owned();
        let name = s(&self.a(p, "name")).unwrap_or("").to_owned();
        if !Self::cdf_param_type_ok(&ty, &v) {
            return Err(format!(
                "*Error* CDF parameter {name} ({ty}): value of the wrong type"
            ));
        }
        let store_default = s(&self.a(p, "storeDefault")) == Some("yes");
        if super::equal(&v, &self.a(p, "defValue")) && !store_default {
            // GUESS(G4): a value equal to the default with storeDefault "no" leaves no
            // property (as the form does); the loader never sends one (spec §5.3).
            self.remove_prop(inst, &name);
            return Ok(v);
        }
        let (pty, pv) = match ty.as_str() {
            "int" => ("int", v.clone()),
            "float" => ("float", V::Float(num(&v).unwrap_or(0.0))),
            "boolean" => (
                "boolean",
                if matches!(&v, V::Str(x) if &**x == "nil" || &**x == "FALSE") {
                    V::Nil
                } else if v.truthy() {
                    V::T
                } else {
                    V::Nil
                },
            ),
            // G13: an inherited-connection override is a netSet-typed property (as
            // virtuoso-bridge-lite writes it with dbReplaceProp ... "netSet").
            "netSet" => ("netSet", v.clone()),
            _ => ("string", v.clone()),
        };
        self.set_prop(inst, &name, pty, pv);
        Ok(v)
    }

    // ------------------------------------------------------------------ dispatch

    fn call_db(&mut self, name: &str, args: &[V]) -> Option<Result<V, String>> {
        let sig = SIGNATURES.iter().find(|(n, _)| *n == name)?.1;
        if DELEGATED.contains(&name) {
            // Signature checked here; the body lives in mock_ocean.rs / mock_mae.rs.
            return check(name, sig, args).err().map(Err);
        }
        Some(check(name, sig, args).and_then(|(a, kw)| self.dispatch(name, &a, &kw)))
    }

    fn dispatch(&mut self, name: &str, a: &[V], kw: &[(String, V)]) -> Result<V, String> {
        let o = |i: usize| -> u32 { Self::obj(&a[i]).unwrap_or(0) };
        let st = |i: usize| -> String { a.get(i).and_then(s).unwrap_or("").to_owned() };
        let key =
            |k: &str| -> Option<V> { kw.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()) };
        Ok(match name {
            "ddGetObj" => {
                let lib = st(0);
                let opt = |i: usize| {
                    a.get(i)
                        .and_then(s)
                        .filter(|x| !x.is_empty())
                        .map(str::to_owned)
                };
                let mode = a.get(5).and_then(s).unwrap_or("r").to_owned();
                if a.get(4).is_some_and(V::truthy) {
                    return Err("*Error* ddGetObj: context objects are not modelled".into());
                }
                let (c, v, f) = (opt(1), opt(2), opt(3));
                match self.dd_get(&lib, c.as_deref(), v.as_deref(), f.as_deref(), &mode) {
                    Some(x) => V::Obj(x),
                    None => V::Nil,
                }
            }
            "ddGetObjWritePath" => self.a(o(0), "writePath"),
            "dbOpenCellViewByType" => {
                let lib = match &a[0] {
                    V::Str(l) => l.to_string(),
                    V::Obj(l) => s(&self.a(*l, "name")).unwrap_or("").to_owned(),
                    _ => return Err(bad(name, 1, "a library name or ddId")),
                };
                let views: Vec<String> = match &a[2] {
                    V::Str(v) => v.split_whitespace().map(str::to_owned).collect(),
                    V::List(l) => l.iter().filter_map(s).map(str::to_owned).collect(),
                    _ => return Err(bad(name, 3, "a view name or list")),
                };
                let vt = a.get(3).and_then(s).map(str::to_owned);
                let mode = a.get(4).and_then(s).unwrap_or("r").to_owned();
                self.open_cv(&lib, &st(1), &views, vt.as_deref(), &mode)?
            }
            "dbSave" => {
                if a.len() > 1 {
                    return Err("*Error* dbSave: saving to another cellview is not modelled".into());
                }
                let cv = o(0);
                if self.ty(cv) != "cellView" || !self.editable(cv) {
                    return Ok(V::Nil);
                }
                self.backups.remove(&cv);
                self.put(cv, "sfySaved", V::T);
                self.put(cv, "modifiedButNotSaved", V::Nil);
                V::T
            }
            "dbClose" | "dbPurge" => {
                let cv = o(0);
                if self.ty(cv) != "cellView" {
                    return Ok(V::Nil);
                }
                let n = num(&self.a(cv, "sfyOpen")).unwrap_or(0.0) as i64 - 1;
                let n = if name == "dbPurge" { 0 } else { n.max(0) };
                self.put(cv, "sfyOpen", V::Int(n));
                if n == 0 {
                    self.revert(cv);
                    self.put(cv, "mode", V::str("r"));
                }
                V::T
            }
            "dbFindTermByName" => self
                .a(o(0), "terminals")
                .items()
                .iter()
                .find(|t| Self::obj(t).is_some_and(|t| s(&self.a(t, "name")) == Some(&st(1))))
                .cloned()
                .unwrap_or(V::Nil),
            "dbTransformPoint" => vpt(transform(pt(&a[0]).expect("checked"), &a[1])?),
            "dbTransformBBox" => {
                let (p, q) = bb(&a[0]).expect("checked");
                vbox(transform(p, &a[1])?, transform(q, &a[1])?)
            }
            "dbGetTermNetExpr" => self.a(o(0), "sfyNetExpr"),
            "dbMakeNet" => {
                let cv = o(0);
                if !self.editable(cv) {
                    return Ok(V::Nil);
                }
                let existing = self
                    .a(cv, "nets")
                    .items()
                    .iter()
                    .find(|n| Self::obj(n).is_some_and(|n| s(&self.a(n, "name")) == Some(&st(1))))
                    .cloned();
                match existing {
                    Some(n) => n,
                    None => {
                        let n = self.new_obj(
                            "net",
                            vec![("name", V::str(&st(1))), ("cellView", V::Obj(cv))],
                        );
                        self.push(cv, "nets", V::Obj(n));
                        self.touch(cv);
                        V::Obj(n)
                    }
                }
            }
            "dbCreateTerm" => {
                let net = o(0);
                let dir = st(2);
                let dirs = [
                    "input",
                    "output",
                    "inputOutput",
                    "jumper",
                    "switch",
                    "unused",
                    "powerInput",
                    "powerOutput",
                    "bidir",
                    "tristate",
                ];
                if !dirs.contains(&dir.as_str()) {
                    return Err(bad(name, 3, "a terminal direction"));
                }
                let Some(cv) = self.cv_of(net) else {
                    return Ok(V::Nil);
                };
                if self.a(net, "term").truthy()
                    || Self::obj(&self.call_fn("dbFindTermByName", &[V::Obj(cv), a[1].clone()])?)
                        .is_some()
                {
                    return Ok(V::Nil);
                }
                let t = self.new_obj(
                    "term",
                    vec![
                        ("name", a[1].clone()),
                        ("direction", V::str(&dir)),
                        ("net", V::Obj(net)),
                        ("cellView", V::Obj(cv)),
                    ],
                );
                self.put(net, "term", V::Obj(t));
                self.push(cv, "terminals", V::Obj(t));
                self.touch(cv);
                V::Obj(t)
            }
            "dbCreatePin" => {
                let net = o(0);
                let Some(cv) = self.cv_of(net) else {
                    return Ok(V::Nil);
                };
                let term = match a.get(3).and_then(Self::obj) {
                    Some(t) => t,
                    None => match Self::obj(&self.a(net, "term")) {
                        Some(t) => t,
                        None => {
                            let n = self.a(net, "name");
                            Self::obj(&self.call_fn(
                                "dbCreateTerm",
                                &[V::Obj(net), n, V::str("inputOutput")],
                            )?)
                            .unwrap_or(0)
                        }
                    },
                };
                let k = self.a(term, "pins").items().len();
                let pname = match a.get(2).and_then(s).filter(|x| !x.is_empty()) {
                    Some(n) => n.to_owned(),
                    None => format!("{}__{k}", s(&self.a(term, "name")).unwrap_or("P")),
                };
                let pin = self.new_obj(
                    "pin",
                    vec![
                        ("name", V::str(&pname)),
                        ("term", V::Obj(term)),
                        ("net", V::Obj(net)),
                        ("cellView", V::Obj(cv)),
                    ],
                );
                if let Some(f) = a.get(1).and_then(Self::obj) {
                    self.put(pin, "fig", V::Obj(f));
                    self.put(pin, "figs", V::list(vec![V::Obj(f)]));
                    self.put(f, "pin", V::Obj(pin));
                    self.put(f, "net", V::Obj(net));
                }
                self.push(term, "pins", V::Obj(pin));
                self.touch(cv);
                V::Obj(pin)
            }
            "dbCreateTermNetExpr" => {
                let t = o(0);
                if self.ty(t) != "term" || self.a(t, "sfyNetExpr").truthy() {
                    return Ok(V::Nil);
                }
                self.put(t, "sfyNetExpr", a[1].clone());
                V::T
            }
            "dbCreateRect" | "dbCreateEllipse" => {
                let cv = o(0);
                if !self.editable(cv) {
                    return Ok(V::Nil);
                }
                let (p, q) = bb(&a[2]).expect("checked");
                let ty = if name == "dbCreateRect" {
                    "rect"
                } else {
                    "ellipse"
                };
                V::Obj(self.new_shape(cv, ty, &Self::lpp(&a[1]), vec![("bBox", vbox(p, q))]))
            }
            "dbCreateLine" | "dbCreatePolygon" | "dbCreatePath" => {
                let cv = o(0);
                if !self.editable(cv) {
                    return Ok(V::Nil);
                }
                let min = if name == "dbCreatePolygon" { 3 } else { 2 };
                let Some(pts) = Self::points(&a[2], min) else {
                    return Err(bad(name, 3, &format!("a list of at least {min} points")));
                };
                let n = pts.items().len() as i64;
                let mut extra = vec![("points", pts), ("nPoints", V::Int(n))];
                let ty = match name {
                    "dbCreateLine" => "line",
                    "dbCreatePolygon" => "polygon",
                    _ => {
                        extra.push(("width", V::Float(num(&a[3]).expect("checked"))));
                        extra.push((
                            "pathStyle",
                            a.get(4).cloned().unwrap_or(V::str("truncateExtend")),
                        ));
                        "path"
                    }
                };
                V::Obj(self.new_shape(cv, ty, &Self::lpp(&a[1]), extra))
            }
            "dbCreateArc" => {
                let cv = o(0);
                if !self.editable(cv) {
                    return Ok(V::Nil);
                }
                let (p, q) = bb(&a[2]).expect("checked");
                // GUESS(G10): the angles start as the arc box implies (modelled as 0..pi) and
                // are then set through startAngle/stopAngle.
                V::Obj(self.new_shape(
                    cv,
                    "arc",
                    &Self::lpp(&a[1]),
                    vec![
                        ("ellipseBBox", vbox(p, q)),
                        ("startAngle", V::Float(0.0)),
                        ("stopAngle", V::Float(std::f64::consts::PI)),
                    ],
                ))
            }
            "dbCreateLabel" => {
                let cv = o(0);
                if !self.editable(cv) {
                    return Ok(V::Nil);
                }
                V::Obj(self.new_label(cv, &Self::lpp(&a[1]), &a[2..8], "normalLabel")?)
            }
            "dbReplaceProp" | "dbCreateProp" => {
                let obj = o(0);
                let ty = st(2);
                if !PROP_TYPES.contains(&ty.as_str()) {
                    return Err(bad(name, 3, "a property type"));
                }
                if !Self::prop_value_ok(&ty, &a[3]) {
                    return Err(bad(name, 4, &format!("a value of type {ty}")));
                }
                if self.cv_of(obj).is_some_and(|cv| !self.editable(cv)) {
                    return Ok(V::Nil);
                }
                if name == "dbCreateProp" && self.find_prop(obj, &st(1)).is_some() {
                    return Ok(V::Nil);
                }
                V::Obj(self.set_prop(obj, &st(1), &ty, a[3].clone()))
            }
            "dbProduceMemName" => {
                let mut out = Vec::new();
                for part in st(0).split(',') {
                    match part.split_once('<') {
                        Some((b, r)) => {
                            let r = r.trim_end_matches('>');
                            let (x, y) = r.split_once(':').unwrap_or((r, r));
                            let (x, y): (i64, i64) =
                                (x.parse().unwrap_or(0), y.parse().unwrap_or(0));
                            let step = if y >= x { 1 } else { -1 };
                            let mut i = x;
                            loop {
                                out.push(V::str(&format!("{b}<{i}>")));
                                if i == y {
                                    break;
                                }
                                i += step;
                            }
                        }
                        None => out.push(V::str(part)),
                    }
                }
                V::list(out)
            }
            "schCreateInst" => {
                let (cv, master) = (o(0), o(1));
                if !self.editable(cv)
                    || s(&self.a(cv, "cellViewType")) != Some("schematic")
                    || self.ty(master) != "cellView"
                    || cv == master
                {
                    return Ok(V::Nil);
                }
                let orient = st(4);
                if !ORIENTS.contains(&orient.as_str()) {
                    return Err(bad(name, 5, "an orientation"));
                }
                let iname = match &a[2] {
                    V::Str(n) => n.to_string(),
                    _ => self.auto_name(cv, "I"),
                };
                if !Self::inst_name_ok(&iname) || self.inst_named(cv, &iname) {
                    return Ok(V::Nil);
                }
                V::Obj(self.new_inst(
                    cv,
                    master,
                    &iname,
                    pt(&a[3]).expect("checked"),
                    &orient,
                    "cell",
                ))
            }
            "schCreatePin" => {
                let (cv, master) = (o(0), o(1));
                let dir = st(3);
                if !["input", "output", "inputOutput", "switch"].contains(&dir.as_str()) {
                    return Err(bad(name, 4, "input, output, inputOutput or switch"));
                }
                let orient = st(6);
                if !ORIENTS.contains(&orient.as_str()) {
                    return Err(bad(name, 7, "an orientation"));
                }
                if !self.editable(cv)
                    || s(&self.a(cv, "cellViewType")) != Some("schematic")
                    || self.ty(master) != "cellView"
                {
                    return Ok(V::Nil);
                }
                let tname = st(2);
                if Self::obj(&self.call_fn("dbFindTermByName", &[V::Obj(cv), V::str(&tname)])?)
                    .is_some()
                {
                    return Ok(V::Nil);
                }
                // GUESS(G11): the pin instance is named PIN<n> and is what schCreatePin returns.
                self.pins_made += 1;
                let iname = format!("PIN{}", self.pins_made);
                let inst = self.new_inst(
                    cv,
                    master,
                    &iname,
                    pt(&a[5]).expect("checked"),
                    &orient,
                    "pin",
                );
                let net = Self::obj(&self.call_fn("dbMakeNet", &[V::Obj(cv), V::str(&tname)])?)
                    .unwrap_or(0);
                let term = Self::obj(
                    &self.call_fn("dbCreateTerm", &[V::Obj(net), V::str(&tname), V::str(&dir)])?,
                )
                .unwrap_or(0);
                let pin = self.new_obj(
                    "pin",
                    vec![
                        ("name", V::str(&tname)),
                        ("term", V::Obj(term)),
                        ("net", V::Obj(net)),
                        ("fig", V::Obj(inst)),
                        ("figs", V::list(vec![V::Obj(inst)])),
                        ("cellView", V::Obj(cv)),
                    ],
                );
                self.push(term, "pins", V::Obj(pin));
                self.put(inst, "pin", V::Obj(pin));
                self.put(inst, "net", V::Obj(net));
                V::Obj(inst)
            }
            "schCreateWire" => {
                let cv = o(0);
                let entry = st(1);
                if entry != "draw" && entry != "route" {
                    return Err(bad(name, 2, "\"draw\" or \"route\""));
                }
                if !["full", "direct", "flight"].contains(&st(2).as_str()) {
                    return Err(bad(name, 3, "\"full\", \"direct\" or \"flight\""));
                }
                if !self.editable(cv) || s(&self.a(cv, "cellViewType")) != Some("schematic") {
                    return Ok(V::Nil);
                }
                let Some(pts) = Self::points(&a[3], 2) else {
                    return Err(bad(name, 4, "a list of at least 2 points"));
                };
                let (xs, ys) = (num(&a[4]).unwrap_or(0.0), num(&a[5]).unwrap_or(0.0));
                let snap = |v: f64, g: f64| if g > 0.0 { (v / g).round() * g } else { v };
                // The database stores every point on the cellview's DBU grid (research §1.4),
                // so a segment shorter than half a DBU vanishes.
                let dbu = num(&self.a(cv, "DBUPerUU")).unwrap_or(160.0);
                let pts: Vec<(f64, f64)> = pts
                    .items()
                    .iter()
                    .filter_map(pt)
                    .map(|(x, y)| (snap(snap(x, xs), 1.0 / dbu), snap(snap(y, ys), 1.0 / dbu)))
                    .collect();
                let width = num(&a[6]).unwrap_or(0.0);
                // A segment that already exists makes the call return nil (research §4.2, VCLI).
                for w in pts.windows(2) {
                    if self.segment_exists(cv, w[0], w[1]) {
                        return Ok(V::Nil);
                    }
                }
                // GUESS(G5): "draw" adds exactly one figure per segment, no solder dots.
                let mut out = Vec::new();
                for w in pts.windows(2) {
                    if w[0] == w[1] {
                        continue;
                    }
                    let lpp = ("wire".to_owned(), "drawing".to_owned());
                    let mut extra = vec![
                        ("points", V::list(vec![vpt(w[0]), vpt(w[1])])),
                        ("nPoints", V::Int(2)),
                    ];
                    let ty = if width > 0.0 {
                        extra.push(("width", V::Float(width)));
                        "path"
                    } else {
                        "line"
                    };
                    out.push(V::Obj(self.new_shape(cv, ty, &lpp, extra)));
                }
                V::list(out)
            }
            "schCreateWireLabel" => {
                let (cv, glue) = (o(0), o(1));
                if !self.editable(cv)
                    || !(self.is_wire(glue) || self.is_pin_fig(glue))
                    || st(3).is_empty()
                {
                    return Ok(V::Nil);
                }
                let lpp = if a[8].truthy() {
                    ("annotate", "drawing3")
                } else {
                    ("wire", "label")
                };
                let l = self.new_label(
                    cv,
                    &(lpp.0.into(), lpp.1.into()),
                    &[
                        a[2].clone(),
                        a[3].clone(),
                        a[4].clone(),
                        a[5].clone(),
                        a[6].clone(),
                        a[7].clone(),
                    ],
                    "normalLabel",
                )?;
                self.glue(l, Some(glue));
                V::Obj(l)
            }
            "schCreateNetExpression" => {
                let (cv, glue) = (o(0), o(2));
                let e = st(1);
                let expr_ok = e.starts_with("[@") && e.contains(":%:") && e.ends_with(']');
                if !self.editable(cv) || !expr_ok {
                    return Ok(V::Nil);
                }
                let lpp = if self.is_wire(glue) {
                    ("wire", "label")
                } else if self.is_pin_fig(glue) {
                    ("pin", "annotate")
                } else {
                    return Ok(V::Nil);
                };
                let l = self.new_label(
                    cv,
                    &(lpp.0.into(), lpp.1.into()),
                    &[
                        a[3].clone(),
                        a[1].clone(),
                        a[4].clone(),
                        a[5].clone(),
                        a[6].clone(),
                        a[7].clone(),
                    ],
                    "NLPLabel",
                )?;
                self.glue(l, Some(glue));
                V::Obj(l)
            }
            "schCreateNoteLabel" => {
                let cv = o(0);
                let ty = st(7);
                if !LABEL_TYPES.contains(&ty.as_str()) {
                    return Err(bad(name, 8, "normalLabel, NLPLabel or ILLabel"));
                }
                if !self.editable(cv) {
                    return Ok(V::Nil);
                }
                V::Obj(self.new_label(cv, &("text".into(), "drawing".into()), &a[1..7], &ty)?)
            }
            "schGlueLabel" => {
                let (l, f) = (o(0), o(1));
                if self.ty(l) != "label" || l == f {
                    return Ok(V::Nil);
                }
                self.glue(l, Some(f));
                V::T
            }
            "schCheck" => {
                let cv = o(0);
                if s(&self.a(cv, "cellViewType")) != Some("schematic") {
                    return Ok(V::Nil);
                }
                // The mock's checker: a wire label glued to nothing is an error.
                let errors = self
                    .a(cv, "shapes")
                    .items()
                    .iter()
                    .filter_map(Self::obj)
                    .filter(|&f| {
                        self.ty(f) == "label"
                            && s(&self.a(f, "layerName")) == Some("wire")
                            && !self.a(f, "parent").truthy()
                    })
                    .count() as i64;
                if self.editable(cv) {
                    // GUESS(G14): extraction stamps bookkeeping properties on the cellview.
                    let n = self.a(cv, "instances").items().len() as i64;
                    self.set_prop(
                        cv,
                        "lastSchematicExtraction",
                        "time",
                        V::str("Oct 12 12:00:00 2026"),
                    );
                    self.set_prop(cv, "instance#", "int", V::Int(n));
                }
                V::list(vec![V::Int(errors), V::Int(0)])
            }
            "schSolder" => {
                let cv = o(0);
                if !self.editable(cv) {
                    return Ok(V::Nil);
                }
                // GUESS(G15): a solder dot is a "dot" on wire drawing.
                let p = pt(&a[1]).expect("checked");
                V::Obj(self.new_shape(
                    cv,
                    "dot",
                    &("wire".into(), "drawing".into()),
                    vec![
                        ("xy", vpt(p)),
                        ("width", V::Float(0.025)),
                        ("height", V::Float(0.025)),
                    ],
                ))
            }
            "cdfGetBaseCellCDF" => self.a(o(0), "sfyBaseCdf"),
            "cdfCreateBaseCellCDF" => {
                let c = o(0);
                if self.ty(c) != "ddCell" || self.a(c, "sfyBaseCdf").truthy() {
                    return Ok(V::Nil);
                }
                let cdf = self.new_obj(
                    "cdf",
                    vec![
                        ("type", V::str("baseCellData")),
                        ("id", V::Obj(c)),
                        ("doneProc", key("doneProc").unwrap_or(V::str(""))),
                        ("formInitProc", key("formInitProc").unwrap_or(V::str(""))),
                    ],
                );
                self.put(c, "sfyBaseCdf", V::Obj(cdf));
                V::Obj(cdf)
            }
            "cdfDeleteCDF" => {
                let cdf = o(0);
                match Self::obj(&self.a(cdf, "id")) {
                    Some(c) if self.ty(c) == "ddCell" => {
                        self.put(c, "sfyBaseCdf", V::Nil);
                        V::T
                    }
                    _ => V::Nil,
                }
            }
            "cdfCreateParam" => {
                let cdf = o(0);
                let (Some(V::Str(pname)), Some(V::Str(ty))) = (key("name"), key("type")) else {
                    return Err(
                        "*Error* cdfCreateParam: ?name and ?type are required strings".into(),
                    );
                };
                if !CDF_TYPES.contains(&&*ty) {
                    return Err(format!("*Error* cdfCreateParam: type {ty}"));
                }
                let def = key("defValue");
                if def.is_none() && !matches!(&*ty, "boolean" | "button") {
                    return Err(format!("*Error* cdfCreateParam: {pname} needs ?defValue"));
                }
                let def = def.unwrap_or(V::Nil);
                if !matches!(&*ty, "button") && def.truthy() && !Self::cdf_param_type_ok(&ty, &def)
                {
                    return Err(format!(
                        "*Error* cdfCreateParam: {pname}: ?defValue does not match type {ty}"
                    ));
                }
                if matches!(&*ty, "radio" | "cyclic") && !matches!(key("choices"), Some(V::List(_)))
                {
                    return Err(format!(
                        "*Error* cdfCreateParam: {pname}: {ty} needs ?choices"
                    ));
                }
                if self.ty(cdf) != "cdf" || self.cdf_param(cdf, &pname).is_some() {
                    return Ok(V::Nil); // an existing name is skipped silently
                }
                // GUESS(G16): every attribute reads back as it was passed ("yes", "t", "nil");
                // an unset one reads nil.
                let mut attrs = vec![
                    ("name", V::Str(pname.clone())),
                    ("paramType", V::Str(ty.clone())),
                    ("defValue", def.clone()),
                    ("sfyCdf", V::Obj(cdf)),
                ];
                for (k, v) in kw {
                    if !matches!(k.as_str(), "name" | "type" | "defValue") {
                        attrs.push((k.as_str(), v.clone()));
                    }
                }
                let p = self.new_obj("cdfParam", attrs);
                self.push(cdf, "parameters", V::Obj(p));
                V::Obj(cdf)
            }
            "cdfSaveCDF" => {
                let cdf = o(0);
                if s(&self.a(cdf, "type")) == Some("baseCellData") {
                    V::T
                } else {
                    V::Nil
                }
            }
            "cdfGetInstCDF" => {
                let i = o(0);
                if self.ty(i) != "inst" {
                    return Ok(V::Nil);
                }
                self.inst_cdf(i)
            }
            "cdfFindParamByName" => self.cdf_param(o(0), &st(1)).map_or(V::Nil, V::Obj),
            "cdfGetCellCDF" => self.cell_cdf(o(0)),
            "cdfUpdateInstParam" => {
                let i = o(0);
                if self.ty(i) != "inst" {
                    return Ok(V::Nil);
                }
                self.update_inst_params(i)?
            }
            // Instances only (what SfyRtPlace deletes): unlinked from their cellview.
            "dbDeleteObject" => {
                let x = o(0);
                if self.ty(x) != "inst" {
                    return Err("*Error* dbDeleteObject: the mock deletes instances only".into());
                }
                let Some(cv) = Self::obj(&self.a(x, "cellView")) else {
                    return Ok(V::Nil);
                };
                if !self.editable(cv) {
                    return Ok(V::Nil);
                }
                let kept: Vec<V> = self
                    .a(cv, "instances")
                    .items()
                    .iter()
                    .filter(|i| Self::obj(i) != Some(x))
                    .cloned()
                    .collect();
                self.put(cv, "instances", V::list(kept));
                self.touch(cv);
                V::T
            }
            "dbGetOpenCellViews" => V::list(
                self.cvs
                    .values()
                    .filter(|&&cv| num(&self.a(cv, "sfyOpen")).unwrap_or(0.0) > 0.0)
                    .map(|&cv| V::Obj(cv))
                    .collect(),
            ),
            _ => unreachable!("{name} has a signature but no body"),
        })
    }

    fn cdf_param(&self, cdf: u32, name: &str) -> Option<u32> {
        self.a(cdf, "parameters")
            .items()
            .iter()
            .filter_map(Self::obj)
            .find(|&p| s(&self.a(p, "name")) == Some(name))
    }

    fn inst_named(&self, cv: u32, name: &str) -> bool {
        self.a(cv, "instances")
            .items()
            .iter()
            .filter_map(Self::obj)
            .any(|i| s(&self.a(i, "name")) == Some(name))
    }

    fn auto_name(&self, cv: u32, prefix: &str) -> String {
        (0..)
            .map(|k| format!("{prefix}{k}"))
            .find(|n| !self.inst_named(cv, n))
            .expect("a free name")
    }

    fn segment_exists(&self, cv: u32, p: (f64, f64), q: (f64, f64)) -> bool {
        self.a(cv, "shapes")
            .items()
            .iter()
            .filter_map(Self::obj)
            .any(|f| {
                if !self.is_wire(f) {
                    return false;
                }
                let pts: Vec<(f64, f64)> =
                    self.a(f, "points").items().iter().filter_map(pt).collect();
                pts.windows(2)
                    .any(|w| (w[0] == p && w[1] == q) || (w[0] == q && w[1] == p))
            })
    }

    fn get_attr(&self, id: u32, attr: &str) -> V {
        let ty = self.ty(id);
        match (ty.as_str(), attr) {
            (_, "bBox") if ty != "rect" && ty != "ellipse" => {
                return self.bbox(id).map_or(V::Nil, |(p, q)| vbox(p, q));
            }
            ("inst", "transform") => return self.inst_transform(id),
            ("prop", "value") => return self.prop_read(id),
            ("cdfParam", "value") => {
                if let Some(inst) = Self::obj(&self.a(id, "sfyInst")) {
                    let n = s(&self.a(id, "name")).unwrap_or("").to_owned();
                    return match self.find_prop(inst, &n) {
                        Some(p) => self.a(p, "value"),
                        None => self.a(id, "defValue"),
                    };
                }
                return self.a(id, "defValue");
            }
            _ => {}
        }
        if let Some(v) = self.objs.get(id as usize).and_then(|o| o.attrs.get(attr)) {
            if DB_ORDER.contains(&attr) {
                let mut l = v.items().to_vec();
                l.reverse();
                return V::list(l);
            }
            return v.clone();
        }
        if ty == "cdf"
            && let Some(p) = self.cdf_param(id, attr)
        {
            return V::Obj(p);
        }
        match self.find_prop(id, attr) {
            Some(p) => self.prop_read(p),
            None => V::Nil,
        }
    }

    fn set_attr(&mut self, id: u32, attr: &str, v: V) -> Result<V, String> {
        let ty = self.ty(id);
        if id as usize >= self.objs.len() || id == 0 {
            return Err(format!("db:{id}~>{attr}: no such object"));
        }
        match (ty.as_str(), attr) {
            ("label", "parent") => {
                let f = Self::obj(&v);
                self.glue(id, f);
            }
            ("label", "labelType") => {
                if !s(&v).is_some_and(|t| LABEL_TYPES.contains(&t)) {
                    return Err(format!("*Error* labelType: {v:?}"));
                }
                self.put(id, attr, v.clone());
            }
            (
                "label",
                "theLabel" | "justify" | "orient" | "font" | "height" | "xy" | "isVisible"
                | "isDrafted" | "isOverbar",
            )
            | ("arc", "startAngle" | "stopAngle" | "ellipseBBox")
            | ("inst", "orient" | "xy" | "name")
            | ("cellView", "DBUPerUU" | "userUnits")
            | ("cdf", _) => {
                if matches!(attr, "startAngle" | "stopAngle") && num(&v).is_none() {
                    return Err(format!("*Error* {attr}: expected a number"));
                }
                if ty == "cdf" && self.cdf_param(id, attr).is_some() {
                    return Err(format!(
                        "*Error* cdf~>{attr}: a parameter cannot be replaced"
                    ));
                }
                self.put(id, attr, v.clone());
            }
            ("cdfParam", "value") => {
                if let Some(inst) = Self::obj(&self.a(id, "sfyInst")) {
                    return self.set_inst_param(id, inst, v);
                }
                self.put(id, "defValue", v.clone());
            }
            ("cdfParam", _) => {
                self.put(id, attr, v.clone());
            }
            (_, _) if self.objs[id as usize].attrs.contains_key(attr) => {
                return Err(format!("*Error* {ty}~>{attr} is not settable"));
            }
            _ => {
                // An unknown name on a database object sets a property of that name.
                let pty = match &v {
                    V::Str(_) => "string",
                    V::Int(_) => "int",
                    V::Float(_) => "float",
                    V::T | V::Nil => "boolean",
                    V::List(_) => "ILList",
                    _ => return Err(format!("*Error* ~>{attr}: unsupported value")),
                };
                self.set_prop(id, attr, pty, v.clone());
            }
        }
        if let Some(cv) = self.cv_of(id) {
            self.touch(cv);
        }
        Ok(v)
    }
}

impl Host for MockDb {
    fn call(&mut self, name: &str, args: &[V]) -> Option<Result<V, String>> {
        if let Some(r) = self.call_db(name, args) {
            return Some(r);
        }
        if let Some(r) = mae::call(self, name, args) {
            return Some(r);
        }
        ocean::call(self, name, args)
    }

    fn get(&mut self, obj: u32, attr: &str) -> Result<V, String> {
        if obj == 0 || obj as usize >= self.objs.len() {
            return Err(format!("db:{obj}~>{attr}: no such object"));
        }
        Ok(self.get_attr(obj, attr))
    }

    fn set(&mut self, obj: u32, attr: &str, value: V) -> Result<V, String> {
        self.set_attr(obj, attr, value)
    }

    fn bound(&self, name: &str) -> bool {
        SIGNATURES.iter().any(|(n, _)| *n == name)
    }
}
