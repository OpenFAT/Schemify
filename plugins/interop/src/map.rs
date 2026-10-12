//! `interop.map`: cell, pin and parameter translation between Schemify libraries and
//! Virtuoso libraries, and gm/Id retarget classes (spec `interop.md` §8.2); plus
//! [`resolve`], the export-side lookup that turns a Design and a mapping into a [`Plan`].
//!
//! The file is line records without a header (format.md §2). It is read with the format
//! crate's own reader by prefixing a `journal` header, the one file kind whose records are
//! all kept as written, so the lexical rules (quoting, escapes, attributes) are exactly
//! the format's.

use std::collections::BTreeMap;

use schemify_format::{Document, FileKind, parse};

use crate::design::Design;
use crate::virtuoso::devices;

/// Rows a mapping file may hold (maps plus classes), a bound on the reader's work.
pub const ROWS_MAX: usize = 1 << 16;

/// One `map` row: a Schemify cell and the Virtuoso cell it is exchanged with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapRow {
    /// The Schemify cell, `lib:cell`.
    pub cellref: String,
    /// Virtuoso library.
    pub lib: String,
    /// Virtuoso cell.
    pub cell: String,
    /// Virtuoso view (default `symbol`).
    pub view: String,
    /// `(schemify pin, virtuoso terminal)`.
    pub pins: Vec<(String, String)>,
    /// `(schemify param, virtuoso param, scale)`: Virtuoso value = Schemify value × scale.
    pub params: Vec<(String, String, Option<String>)>,
    /// Schemify params neither sent nor taken.
    pub drops: Vec<String>,
    /// `(virtuoso param, value)` always sent.
    pub sets: Vec<(String, String)>,
    /// The ngspice template of the Virtuoso cell's mirror.
    pub spice: Option<String>,
    /// Identical geometry: no re-mastering, pins and params by name.
    pub same: bool,
    /// 1-based line of the `map` record.
    pub line: u32,
}

/// One `class` row (gm/Id retarget, §10).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassRow {
    /// Class name.
    pub name: String,
    /// `virtuoso_target` hint.
    pub hint: String,
    /// `n` or `p`, when given.
    pub role: Option<char>,
    /// 1-based line.
    pub line: u32,
}

/// A parsed mapping file. Empty = identity everywhere (mirrors, `schemify_core`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mapping {
    /// `map` rows, in file order.
    pub maps: Vec<MapRow>,
    /// `class` rows, in file order.
    pub classes: Vec<ClassRow>,
}

impl Mapping {
    /// Parses `interop.map` text.
    ///
    /// # Errors
    ///
    /// Every malformed row, one per line (`line N: …`): a record other than `map`/`class`,
    /// wrong arity, an attribute the record does not take, a `scale` that is not an
    /// expression, `same` beside other attributes, a duplicate cell, Virtuoso target,
    /// class, pin or param, a param both mapped and dropped, more than [`ROWS_MAX`] rows.
    pub fn parse(text: &str) -> Result<Mapping, Vec<String>> {
        let src = format!("schemify 1 journal\n{text}");
        let doc = parse(src.as_bytes()).map_err(|e| {
            e.diagnostics
                .iter()
                .map(|d| {
                    format!(
                        "line {}: {} {}",
                        d.line.saturating_sub(1),
                        d.code.as_str(),
                        d.detail
                    )
                })
                .collect::<Vec<_>>()
        })?;
        assert_eq!(doc.kind(), FileKind::Journal);
        let mut m = Mapping::default();
        let mut errs = Vec::new();
        if doc.record_count() as usize > ROWS_MAX + 1 {
            return Err(vec![format!("more than {ROWS_MAX} rows")]);
        }
        for r in 1..doc.record_count() {
            let line = doc.record_line(r).saturating_sub(1);
            let fields: Vec<&str> = doc.fields(r).collect();
            if doc.verbatim(r).len() > 0 {
                errs.push(format!("line {line}: verbatim lines are not allowed"));
            }
            match doc.record_word(r) {
                "map" => match map_row(&doc, r, &fields, line) {
                    Ok(row) => m.maps.push(row),
                    Err(e) => errs.extend(e),
                },
                "class" => match class_row(&doc, r, &fields, line) {
                    Ok(row) => m.classes.push(row),
                    Err(e) => errs.extend(e),
                },
                w => errs.push(format!("line {line}: unknown record `{w}` (map or class)")),
            }
        }
        // Uniqueness across rows.
        let mut seen: BTreeMap<&str, u32> = BTreeMap::new();
        let mut targets: BTreeMap<(&str, &str), u32> = BTreeMap::new();
        for row in &m.maps {
            if let Some(l) = seen.insert(&row.cellref, row.line) {
                errs.push(format!(
                    "line {}: {} is already mapped on line {l}",
                    row.line, row.cellref
                ));
            }
            if let Some(l) = targets.insert((&row.lib, &row.cell), row.line) {
                errs.push(format!(
                    "line {}: Virtuoso cell {}/{} is already the target of line {l}",
                    row.line, row.lib, row.cell
                ));
            }
        }
        let mut classes: BTreeMap<&str, u32> = BTreeMap::new();
        for c in &m.classes {
            if let Some(l) = classes.insert(&c.name, c.line) {
                errs.push(format!(
                    "line {}: class {} is already defined on line {l}",
                    c.line, c.name
                ));
            }
        }
        if errs.is_empty() {
            assert!(m.maps.len() + m.classes.len() < doc.record_count() as usize);
            Ok(m)
        } else {
            Err(errs)
        }
    }

    /// The `map` row of Schemify cell `cellref` (`lib:cell`).
    pub fn by_cellref(&self, cellref: &str) -> Option<&MapRow> {
        self.maps.iter().find(|r| r.cellref == cellref)
    }

    /// The `map` row whose Virtuoso target is `lib/cell`.
    pub fn by_virtuoso(&self, lib: &str, cell: &str) -> Option<&MapRow> {
        self.maps.iter().find(|r| r.lib == lib && r.cell == cell)
    }
}

fn attr_values<'a>(doc: &'a Document<'_>, a: u32) -> Vec<&'a str> {
    doc.attr_values(a).collect()
}

fn map_row(doc: &Document<'_>, r: u32, f: &[&str], line: u32) -> Result<MapRow, Vec<String>> {
    let mut errs = Vec::new();
    if !(f.len() == 4 || f.len() == 5) || f[1] != "virtuoso" {
        return Err(vec![format!(
            "line {line}: `map <lib:cell> virtuoso <lib> <cell> [view]`"
        )]);
    }
    if f[0]
        .split_once(':')
        .is_none_or(|(l, c)| l.is_empty() || c.is_empty() || c.contains(':'))
    {
        errs.push(format!(
            "line {line}: `{}` is not a `lib:cell` reference",
            f[0]
        ));
    }
    let mut row = MapRow {
        cellref: f[0].to_owned(),
        lib: f[2].to_owned(),
        cell: f[3].to_owned(),
        view: f.get(4).unwrap_or(&"symbol").to_string(),
        line,
        ..MapRow::default()
    };
    let mut others = 0;
    for a in doc.attrs(r) {
        let at = doc.attr_line(a).saturating_sub(1);
        let v = attr_values(doc, a);
        let key = doc.attr_key(a);
        if key != "same" {
            others += 1;
        }
        let ok = match (key, v.as_slice()) {
            ("pin", [s, t]) => {
                if row.pins.iter().any(|p| p.0 == *s || p.1 == *t) {
                    errs.push(format!("line {at}: pin {s} or {t} mapped twice"));
                }
                row.pins.push(((*s).into(), (*t).into()));
                true
            }
            ("param", [s, t]) => {
                row.params.push(((*s).into(), (*t).into(), None));
                true
            }
            ("param", [s, t, "scale", k]) => {
                if schemify_expr::Program::compile(k, schemify_expr::Mode::Scalar).is_err() {
                    errs.push(format!("line {at}: scale `{k}` is not an expression"));
                }
                row.params
                    .push(((*s).into(), (*t).into(), Some((*k).into())));
                true
            }
            ("drop", [p]) => {
                row.drops.push((*p).into());
                true
            }
            ("set", [p, val]) => {
                row.sets.push(((*p).into(), (*val).into()));
                true
            }
            ("spice", [t]) => {
                if row.spice.replace((*t).into()).is_some() {
                    errs.push(format!("line {at}: second `spice`"));
                }
                true
            }
            ("same", []) => {
                row.same = true;
                true
            }
            _ => false,
        };
        if !ok {
            errs.push(format!(
                "line {at}: `{key}` with {} value(s) is not a map attribute (pin, param, drop, set, spice, same)",
                v.len()
            ));
        }
    }
    if row.same && others > 0 {
        errs.push(format!("line {line}: `same` takes no other attributes"));
    }
    let mut names: Vec<&str> = row.params.iter().map(|p| p.0.as_str()).collect();
    names.sort_unstable();
    if names.windows(2).any(|w| w[0] == w[1]) {
        errs.push(format!("line {line}: a param is mapped twice"));
    }
    if let Some(d) = row.drops.iter().find(|d| names.contains(&d.as_str())) {
        errs.push(format!("line {line}: param {d} is both mapped and dropped"));
    }
    if errs.is_empty() { Ok(row) } else { Err(errs) }
}

fn class_row(doc: &Document<'_>, r: u32, f: &[&str], line: u32) -> Result<ClassRow, Vec<String>> {
    if f.len() != 3 || f[1] != "virtuoso_target" {
        return Err(vec![format!(
            "line {line}: `class <name> virtuoso_target <hint>`"
        )]);
    }
    let mut row = ClassRow {
        name: f[0].into(),
        hint: f[2].into(),
        role: None,
        line,
    };
    let mut errs = Vec::new();
    for a in doc.attrs(r) {
        let at = doc.attr_line(a).saturating_sub(1);
        match (doc.attr_key(a), attr_values(doc, a).as_slice()) {
            ("role", ["n"]) => row.role = Some('n'),
            ("role", ["p"]) => row.role = Some('p'),
            (k, _) => errs.push(format!("line {at}: `{k}`: a class takes only `role n|p`")),
        }
    }
    if errs.is_empty() { Ok(row) } else { Err(errs) }
}

// ---------------------------------------------------------------------------------------
// resolve: Design + Mapping → Plan (§8.2 lookups)
// ---------------------------------------------------------------------------------------

/// How a cell crosses to Virtuoso.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// Not in the export's closure.
    Unused,
    /// A user cell written in full (`"design"` cell).
    Design,
    /// A Virtuoso mirror (`provenance virtuoso <lib>/<cell>`): referenced, never written.
    Mirror,
    /// A bundled core cell written to `schemify_core` (or renamed by `--lib core=…`).
    Core,
    /// Sent to the Virtuoso cell of `map` row `k`.
    Map(u32),
    /// Re-mastered onto analogLib (`--analoglib`), device row `k` of [`devices::DEVICES`];
    /// sources pick the waveform cell per instance.
    AnalogLib(u32),
}

/// Library of the bundled core cells in Virtuoso.
pub const CORE_LIB: &str = "schemify_core";

/// Virtuoso library of the project's root directory (Schemify library `""`) unless
/// `--lib =<name>` renames it.
pub const ROOT_LIB: &str = "schemify";

/// The export plan: per Design cell, where it goes. Columns are parallel to `d.cells`.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// How each cell crosses.
    pub how: Vec<How>,
    /// Virtuoso library.
    pub lib: Vec<String>,
    /// Virtuoso cell.
    pub cell: Vec<String>,
    /// Virtuoso view instances of the cell use.
    pub view: Vec<String>,
}

impl Plan {
    /// True when instances of cell `ci` change master geometry (§6.4).
    pub fn remastered(&self, ci: usize, m: &Mapping) -> bool {
        match self.how[ci] {
            How::AnalogLib(_) => true,
            How::Map(k) => !m.maps[k as usize].same,
            _ => false,
        }
    }
}

/// The Virtuoso library name of Schemify library `lib` (`--lib` renames, `""` → [`ROOT_LIB`],
/// `core` → [`CORE_LIB`]).
pub fn virtuoso_lib(lib: &str, lib_map: &BTreeMap<String, String>) -> String {
    if let Some(v) = lib_map.get(lib) {
        return v.clone();
    }
    match lib {
        "" => ROOT_LIB.into(),
        "core" => CORE_LIB.into(),
        l => l.into(),
    }
}

/// The closure of `tops` (cells instantiated from them, transitively), as a mask over
/// `d.cells`.
pub fn closure(d: &Design, tops: &[usize]) -> Vec<bool> {
    let mut seen = vec![false; d.cells.len()];
    let mut stack: Vec<usize> = tops.to_vec();
    while let Some(ci) = stack.pop() {
        assert!(ci < d.cells.len(), "top {ci} is not a cell");
        if std::mem::replace(&mut seen[ci], true) {
            continue;
        }
        let c = &d.cells[ci];
        for r in c.records(schemify_format::RecordKind::Inst) {
            if let Some(t) = d.resolve(ci, c.field(r, 1)) {
                stack.push(t);
            }
        }
    }
    seen
}

/// The provenance target `lib/cell` of a Virtuoso mirror cell.
pub fn mirror_of(c: &crate::design::Cell) -> Option<(String, String)> {
    let p = c.header("provenance")?;
    let rest = p.strip_prefix("virtuoso ")?;
    let (l, cell) = rest.trim().split_once('/')?;
    (!l.is_empty() && !cell.is_empty()).then(|| (l.to_owned(), cell.to_owned()))
}

/// Resolves every cell of the closure of `tops` (§8.2 lookups, in order: a `map` row; a
/// mirror; `core:*` → `schemify_core` or analogLib with `analoglib`; a user cell).
///
/// # Errors
///
/// One line per problem: a pin of a mapped cell without a `pin` row, a `pin`/`param` row
/// naming something the cell lacks.
pub fn resolve(
    d: &Design,
    tops: &[usize],
    m: &Mapping,
    analoglib: bool,
    lib_map: &BTreeMap<String, String>,
) -> Result<Plan, Vec<String>> {
    let used = closure(d, tops);
    let n = d.cells.len();
    let mut p = Plan {
        how: vec![How::Unused; n],
        lib: vec![String::new(); n],
        cell: vec![String::new(); n],
        view: vec![String::new(); n],
    };
    let mut errs = Vec::new();
    for (ci, &u) in used.iter().enumerate() {
        if !u {
            continue;
        }
        let c = &d.cells[ci];
        let cellref = format!("{}:{}", c.lib, c.name);
        let (how, lib, cell, view) =
            if let Some(k) = m.maps.iter().position(|r| r.cellref == cellref) {
                let row = &m.maps[k];
                let pins = crate::design::sym_pins(c);
                if !row.same {
                    for (k2, pn) in pins.name.iter().enumerate() {
                        if pins.alias[k2].is_none() && !row.pins.iter().any(|x| &x.0 == pn) {
                            errs.push(format!(
                                "{cellref}: pin {pn} has no `pin` row (interop.map line {})",
                                row.line
                            ));
                        }
                    }
                }
                for (s, _) in &row.pins {
                    if !pins.name.contains(s) {
                        errs.push(format!(
                            "{cellref}: interop.map line {}: no pin {s}",
                            row.line
                        ));
                    }
                }
                for (s, _, _) in &row.params {
                    if !c
                        .records(schemify_format::RecordKind::Param)
                        .any(|r| c.field(r, 0) == s)
                    {
                        errs.push(format!(
                            "{cellref}: interop.map line {}: no param {s}",
                            row.line
                        ));
                    }
                }
                (
                    How::Map(k as u32),
                    row.lib.clone(),
                    row.cell.clone(),
                    row.view.clone(),
                )
            } else if let Some((l, cl)) = mirror_of(c) {
                (How::Mirror, l, cl, "symbol".into())
            } else if c.lib == "core" {
                match devices::DEVICES
                    .iter()
                    .position(|dv| dv.core == c.name && !dv.alib.is_empty())
                {
                    Some(k) if analoglib => (
                        How::AnalogLib(k as u32),
                        "analogLib".into(),
                        devices::DEVICES[k].alib[0].into(),
                        "symbol".into(),
                    ),
                    _ => (
                        How::Core,
                        virtuoso_lib("core", lib_map),
                        c.name.clone(),
                        "symbol".into(),
                    ),
                }
            } else {
                (
                    How::Design,
                    virtuoso_lib(&c.lib, lib_map),
                    c.name.clone(),
                    "symbol".into(),
                )
            };
        p.how[ci] = how;
        p.lib[ci] = lib;
        p.cell[ci] = cell;
        p.view[ci] = view;
    }
    assert_eq!(p.how.len(), n);
    if errs.is_empty() { Ok(p) } else { Err(errs) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = "map sky130:nfet_01v8 virtuoso sky130_fd_pr nfet_01v8\n  pin d D\n  pin g G\n  pin s S\n  pin b B\n  param W w scale 1e-6\n  param L l scale 1e-6\n  param nf nf\n  param mult m\n  drop ad\n  spice \"X{name} {D} {G} {S} {B} sky130_fd_pr__nfet_01v8 w={w} l={l} nf={nf} m={m}\"\n\nclass nmos virtuoso_target nch\n  role n\n";

    #[test]
    fn parses_the_spec_example() {
        let m = Mapping::parse(EXAMPLE).unwrap();
        assert_eq!(m.maps.len(), 1);
        let r = &m.maps[0];
        assert_eq!(
            (r.lib.as_str(), r.cell.as_str(), r.view.as_str()),
            ("sky130_fd_pr", "nfet_01v8", "symbol")
        );
        assert_eq!(r.pins.len(), 4);
        assert_eq!(r.params[0], ("W".into(), "w".into(), Some("1e-6".into())));
        assert_eq!(r.drops, ["ad"]);
        assert!(r.spice.as_deref().unwrap().starts_with("X{name}"));
        assert_eq!(m.classes[0].role, Some('n'));
        assert_eq!(r.line, 1);
        assert_eq!(m.classes[0].line, 13);
        assert!(m.by_virtuoso("sky130_fd_pr", "nfet_01v8").is_some());
        assert!(Mapping::parse("").unwrap().maps.is_empty());
    }

    #[test]
    fn every_error_is_listed() {
        for (bad, want) in [
            ("frob x\n", "unknown record"),
            ("map a:b virtuoso L\n", "map <lib:cell>"),
            ("map ab virtuoso L C\n", "not a `lib:cell`"),
            ("map a:b virtuoso L C\n  wat 1\n", "not a map attribute"),
            ("map a:b virtuoso L C\n  same\n  pin x X\n", "`same` takes"),
            (
                "map a:b virtuoso L C\n  param x y scale )\n",
                "not an expression",
            ),
            (
                "map a:b virtuoso L C\nmap a:b virtuoso L D\n",
                "already mapped",
            ),
            (
                "map a:b virtuoso L C\nmap a:c virtuoso L C\n",
                "already the target",
            ),
            (
                "map a:b virtuoso L C\n  pin x X\n  pin x Y\n",
                "mapped twice",
            ),
            (
                "map a:b virtuoso L C\n  param x X\n  drop x\n",
                "both mapped and dropped",
            ),
            ("class c virtuoso_target h\n  role q\n", "only `role"),
            (
                "class c virtuoso_target h\nclass c virtuoso_target h\n",
                "already defined",
            ),
            ("map \"a\n", "E028"),
        ] {
            let e = Mapping::parse(bad).expect_err(bad);
            assert!(e.iter().any(|l| l.contains(want)), "{bad}: {e:?}");
            assert!(e.iter().all(|l| l.starts_with("line ")), "{e:?}");
        }
        let two = Mapping::parse("frob\nmap x virtuoso a b\n").unwrap_err();
        assert_eq!(two.len(), 2, "{two:?}");
    }
}
