//! The Design: the Schemify-shaped tables every converter reads and writes (spec
//! `interop.md` §1–§2). One [`Cell`] per Schemify file, holding the file's record table
//! (`schemify_format::Document`, owned), plus typed views ([`Geom`], [`SymPins`]) built on
//! demand for the transforms that compute with geometry.
//!
//! Loading is pure over a map of project files: the guest, which can read files but not list
//! directories, calls [`load`] repeatedly, reading whatever [`LoadError::Missing`] names
//! until the closure is complete.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use schemify_format::{Document, FileKind, Orient, RecordKind, parse, parse_coord, write};

/// Project files: project-relative path (`/`-separated) → bytes.
pub type Files = BTreeMap<String, Vec<u8>>;

/// Cells per Design (doc's per-project cell capacity).
pub const CELLS_MAX: usize = 1 << 16;

/// Pseudo-directory of the bundled `core` library in [`Cell::path`].
pub const CORE_DIR: &str = "core";

/// One Schemify file.
pub struct Cell {
    /// Project-relative path (`dev/ota.shcomp`; `core/res.shprim` for bundled cells).
    pub path: String,
    /// Library name (`""` for a cell in the project root, `"core"` for bundled cells).
    pub lib: String,
    /// File stem.
    pub name: String,
    /// The file's records.
    pub doc: Document<'static>,
}

impl Cell {
    /// The file kind.
    pub fn kind(&self) -> FileKind {
        self.doc.kind()
    }

    /// The canonical file text.
    pub fn text(&self) -> String {
        write(&self.doc)
    }

    /// Directory part of [`Cell::path`] (`""` at the project root).
    pub fn dir(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(d, _)| d)
    }

    /// Header attribute `key`, if present.
    pub fn header(&self, key: &str) -> Option<&str> {
        let a = self.doc.find_attr(0, key)?;
        self.doc.attr_value(a, 0)
    }

    /// Record indices of kind `k`, in file order.
    pub fn records(&self, k: RecordKind) -> impl Iterator<Item = u32> + '_ {
        (0..self.doc.record_count()).filter(move |&r| self.doc.record_kind(r) == k)
    }

    /// First value of attribute `key` of record `r`.
    pub fn attr(&self, r: u32, key: &str) -> Option<&str> {
        let a = self.doc.find_attr(r, key)?;
        self.doc.attr_value(a, 0)
    }

    /// Field `i` of record `r` (`""` when absent).
    pub fn field(&self, r: u32, i: usize) -> &str {
        self.doc.field(r, i).unwrap_or("")
    }

    /// Field `i` of record `r` as a coordinate.
    pub fn coord(&self, r: u32, i: usize) -> Option<i32> {
        parse_coord(self.doc.field(r, i)?).ok()
    }
}

/// Every cell of one conversion, plus the cellref resolution table.
#[derive(Default)]
pub struct Design {
    /// Cells, in load order (the named tops first, then breadth-first by reference).
    pub cells: Vec<Cell>,
    /// Library name → project-relative directory, from the manifest.
    pub libs: BTreeMap<String, String>,
    /// (cell index, cellref) → cell index, for every reference of every loaded cell.
    resolved: HashMap<(u32, String), u32>,
}

/// Why [`load`] stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// These paths are needed and are neither in the file map nor known absent: read them
    /// (a failed read goes into `absent`) and call [`load`] again.
    Missing(Vec<String>),
    /// Something is wrong with the input; every problem found, one per line.
    Invalid(Vec<String>),
}

/// Reads `schemify.project`'s `lib <name> path <dir>` rows. `git`/`url` libraries are
/// returned as problems: the interop plugin reads only libraries present on disk.
pub fn manifest_libs(text: &[u8]) -> Result<BTreeMap<String, String>, Vec<String>> {
    let doc = parse(text).map_err(|e| vec![format!("schemify.project: {e}")])?;
    if doc.kind() != FileKind::Project {
        return Err(vec!["schemify.project: not a project manifest".into()]);
    }
    let mut libs = BTreeMap::new();
    let mut problems = Vec::new();
    for r in 0..doc.record_count() {
        if doc.record_kind(r) != RecordKind::Lib {
            continue;
        }
        let name = doc.field(r, 0).unwrap_or("");
        match (doc.field(r, 1), doc.field(r, 2)) {
            (Some("path"), Some(dir)) => {
                let dir = dir.trim_end_matches('/');
                let dir = match doc
                    .find_attr(r, "subdir")
                    .and_then(|a| doc.attr_value(a, 0))
                {
                    Some(sub) => format!("{dir}/{sub}"),
                    None => dir.to_owned(),
                };
                libs.insert(name.to_owned(), dir);
            }
            // The technology map (pdk.md §5) is not a cell library: `tech:` cells resolve to
            // their class drawing (see `candidates`).
            (Some("map"), Some(_)) if name == "tech" => {}
            _ => problems.push(format!(
                "lib {name}: only `path` libraries can be converted"
            )),
        }
    }
    if problems.is_empty() {
        Ok(libs)
    } else {
        Err(problems)
    }
}

/// Loads the cells at `tops` (project-relative paths) and everything they instantiate.
///
/// `files` must hold `schemify.project` (when any top uses `lib:cell` references) and every
/// cell file of the closure; `absent` lists paths known not to exist (so the `.shcomp` /
/// `.shprim` candidates of a reference can be tried in order). `core:*` cells come from
/// `schemify_doc::core_library()`.
///
/// # Errors
///
/// [`LoadError::Missing`] with the candidate paths still to read; [`LoadError::Invalid`] for
/// a file that does not parse, a reference no candidate satisfies, more than [`CELLS_MAX`]
/// cells, or a top that is not a cell file.
pub fn load(files: &Files, absent: &BTreeSet<String>, tops: &[&str]) -> Result<Design, LoadError> {
    let mut d = Design::default();
    let mut problems = Vec::new();
    let mut missing = BTreeSet::new();
    if let Some(m) = files.get("schemify.project") {
        match manifest_libs(m) {
            Ok(l) => d.libs = l,
            Err(p) => return Err(LoadError::Invalid(p)),
        }
    } else if !absent.contains("schemify.project") {
        missing.insert("schemify.project".to_owned());
    }
    let mut index: HashMap<String, u32> = HashMap::new();
    let mut queue: Vec<String> = tops.iter().map(|t| (*t).to_owned()).collect();
    let mut k = 0;
    while k < queue.len() {
        let path = queue[k].clone();
        k += 1;
        if index.contains_key(&path) {
            continue;
        }
        if d.cells.len() >= CELLS_MAX {
            return Err(LoadError::Invalid(vec![format!(
                "more than {CELLS_MAX} cells"
            )]));
        }
        let bytes: Vec<u8> = match path.strip_prefix("core/") {
            Some(f) => match schemify_doc::core_library().iter().find(|(n, _)| *n == f) {
                Some((_, b)) => b.to_vec(),
                None => {
                    problems.push(format!("{path}: not a bundled core cell"));
                    continue;
                }
            },
            None => match files.get(&path) {
                Some(b) => b.clone(),
                None => {
                    if !absent.contains(&path) {
                        missing.insert(path.clone());
                    } else {
                        problems.push(format!("{path}: no such file"));
                    }
                    continue;
                }
            },
        };
        let cell = match owned(&path, &bytes, &d.libs) {
            Ok(c) => c,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        let me = d.cells.len() as u32;
        index.insert(path.clone(), me);
        // Queue references.
        for r in cell.records(RecordKind::Inst) {
            let cref = cell.field(r, 1).to_owned();
            match candidates(&cell, &cref, &d.libs) {
                Ok(cands) => {
                    let pick = cands
                        .iter()
                        .find(|c| c.starts_with("core/") || files.contains_key(*c));
                    match pick {
                        Some(p) => queue.push(p.clone()),
                        None => {
                            let open: Vec<&String> =
                                cands.iter().filter(|c| !absent.contains(*c)).collect();
                            if open.is_empty() {
                                problems.push(format!(
                                    "{}: {cref}: no cell file ({})",
                                    cell.path,
                                    cands.join(" or ")
                                ));
                            } else {
                                missing.extend(open.into_iter().cloned());
                            }
                        }
                    }
                }
                Err(e) => problems.push(format!("{}: {e}", cell.path)),
            }
        }
        d.cells.push(cell);
    }
    if !problems.is_empty() {
        return Err(LoadError::Invalid(problems));
    }
    if !missing.is_empty() {
        return Err(LoadError::Missing(missing.into_iter().collect()));
    }
    // Resolution table.
    for (ci, cell) in d.cells.iter().enumerate() {
        for r in cell.records(RecordKind::Inst) {
            let cref = cell.field(r, 1);
            let cands = candidates(cell, cref, &d.libs).expect("checked while queueing");
            let t = cands
                .iter()
                .find_map(|c| index.get(c))
                .expect("closure complete");
            d.resolved.insert((ci as u32, cref.to_owned()), *t);
        }
    }
    assert!(d.cells.len() <= CELLS_MAX);
    Ok(d)
}

/// The candidate paths of `cref` referenced from `cell`, in netlist.md §1 order.
fn candidates(
    cell: &Cell,
    cref: &str,
    libs: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let (dir, stem) = if let Some(stem) = cref.strip_prefix("./") {
        (cell.dir().to_owned(), stem)
    } else if let Some((lib, stem)) = cref.split_once(':') {
        if lib == "core" {
            return Ok(vec![format!("core/{stem}.shprim")]);
        }
        // A technology class (pdk.md §5) is exported as the core cell it is drawn from: the
        // same pins, technology-neutral. With `--retarget` its gm/Id-sized instances become
        // class placeholders the target PDK resolves (§10). ponytail: the active tech.map's
        // PDK cell and `param` rewrites are not applied, so a non-retarget export carries
        // generic nmos4/pmos4 devices; read the map here when someone exports a fixed size.
        if lib == "tech" {
            return match stem {
                "nmos" | "pmos" => Ok(vec![format!("core/{stem}4.shprim")]),
                _ => Err(format!("{cref}: no technology class {stem}")),
            };
        }
        match libs.get(lib) {
            Some(d) => (d.clone(), stem),
            None => return Err(format!("{cref}: library {lib} is not in the manifest")),
        }
    } else {
        return Err(format!("{cref}: not a `lib:cell` or `./cell` reference"));
    };
    if stem.is_empty() || stem.contains(['/', '\\']) {
        return Err(format!("{cref}: bad cell name"));
    }
    let base = if dir.is_empty() {
        stem.to_owned()
    } else {
        format!("{dir}/{stem}")
    };
    Ok(vec![format!("{base}.shcomp"), format!("{base}.shprim")])
}

/// Parses `bytes` into an owned [`Cell`].
fn owned(path: &str, bytes: &[u8], libs: &BTreeMap<String, String>) -> Result<Cell, String> {
    let src = parse(bytes).map_err(|e| format!("{path}: {e}"))?;
    let kind = src.kind();
    if !matches!(
        kind,
        FileKind::Shprim | FileKind::Shcomp | FileKind::Shbench
    ) {
        return Err(format!("{path}: not a cell file"));
    }
    let mut doc = Document::new(kind);
    for a in src.attrs(0) {
        doc.push_attr(src.attr_key(a));
        for v in src.attr_values(a) {
            doc.push_value(v);
        }
    }
    for r in 1..src.record_count() {
        doc.copy_record(&src, r);
    }
    let file = path.rsplit_once('/').map_or(path, |(_, f)| f);
    let name = file.rsplit_once('.').map_or(file, |(s, _)| s).to_owned();
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    let lib = if dir == CORE_DIR && path.starts_with("core/") {
        "core".to_owned()
    } else {
        libs.iter()
            .find(|(_, d)| d.as_str() == dir)
            .map(|(n, _)| n.clone())
            .unwrap_or_default()
    };
    assert_eq!(doc.record_count(), src.record_count());
    Ok(Cell {
        path: path.to_owned(),
        lib,
        name,
        doc,
    })
}

impl Design {
    /// The cell `cref` of cell `from` resolves to (every reference of a loaded Design
    /// resolves; `None` only for a cellref no instance of `from` uses).
    pub fn resolve(&self, from: usize, cref: &str) -> Option<usize> {
        self.resolved
            .get(&(from as u32, cref.to_owned()))
            .map(|&i| i as usize)
    }

    /// The cell at project-relative `path`.
    pub fn find(&self, path: &str) -> Option<usize> {
        self.cells.iter().position(|c| c.path == path)
    }

    /// Adds a cell built by an importer (no references resolved for it).
    ///
    /// # Panics
    ///
    /// When the Design is full or the path is already present.
    pub fn push(&mut self, cell: Cell) -> usize {
        assert!(self.cells.len() < CELLS_MAX, "Design full");
        assert!(
            self.find(&cell.path).is_none(),
            "duplicate cell {}",
            cell.path
        );
        self.cells.push(cell);
        self.cells.len() - 1
    }
}

// ---------------------------------------------------------------------------------------
// Typed views
// ---------------------------------------------------------------------------------------

/// A cell's symbol pins (rank 1), in port order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SymPins {
    /// Pin names.
    pub name: Vec<String>,
    /// `in`, `out` or `inout`.
    pub dir: Vec<String>,
    /// Position, milli-units, symbol-local.
    pub at: Vec<[i32; 2]>,
    /// Inherited-connection default net (`inherit` attribute); `None` for drawn pins.
    pub inherit: Vec<Option<String>>,
    /// The ordinary pin this one is a second position of (`alias`).
    pub alias: Vec<Option<String>>,
}

/// Reads the symbol pins of `cell`.
pub fn sym_pins(cell: &Cell) -> SymPins {
    let mut p = SymPins::default();
    for r in cell.records(RecordKind::Pin) {
        p.name.push(cell.field(r, 0).to_owned());
        p.dir.push(cell.field(r, 1).to_owned());
        p.at.push([cell.coord(r, 2).unwrap_or(0), cell.coord(r, 3).unwrap_or(0)]);
        p.inherit.push(cell.attr(r, "inherit").map(str::to_owned));
        p.alias.push(cell.attr(r, "alias").map(str::to_owned));
    }
    assert_eq!(p.name.len(), p.at.len());
    p
}

/// A cell's schematic objects (rank 3) with typed geometry, SoA per kind. `rec` columns
/// index back into the cell's `doc`.
#[derive(Clone, Debug, Default)]
pub struct Geom {
    /// Instance ids.
    pub inst_id: Vec<String>,
    /// Instance names.
    pub inst_name: Vec<String>,
    /// Cell index of each instance's master.
    pub inst_master: Vec<u32>,
    /// Placement.
    pub inst_at: Vec<[i32; 2]>,
    /// Orientation.
    pub inst_orient: Vec<Orient>,
    /// Record index.
    pub inst_rec: Vec<u32>,
    /// Wire ids.
    pub wire_id: Vec<String>,
    /// `[x1, y1, x2, y2]`.
    pub wire: Vec<[i32; 4]>,
    /// `bus true`.
    pub wire_bus: Vec<bool>,
    /// Record index.
    pub wire_rec: Vec<u32>,
    /// Label ids.
    pub label_id: Vec<String>,
    /// Anchor.
    pub label_at: Vec<[i32; 2]>,
    /// Net expression.
    pub label_net: Vec<String>,
    /// `global true`.
    pub label_global: Vec<bool>,
    /// Record index.
    pub label_rec: Vec<u32>,
    /// Port ids.
    pub port_id: Vec<String>,
    /// Anchor.
    pub port_at: Vec<[i32; 2]>,
    /// Interface pin name.
    pub port_pin: Vec<String>,
    /// Record index.
    pub port_rec: Vec<u32>,
    /// Noconn ids.
    pub noconn_id: Vec<String>,
    /// Anchor.
    pub noconn_at: Vec<[i32; 2]>,
    /// Record index.
    pub noconn_rec: Vec<u32>,
}

/// Builds the schematic view of cell `ci`.
///
/// # Panics
///
/// `ci` must be a loaded cell of `d` (every instance resolves).
pub fn geom(d: &Design, ci: usize) -> Geom {
    let c = &d.cells[ci];
    let mut g = Geom::default();
    let xy = |r: u32, i: usize| [c.coord(r, i).unwrap_or(0), c.coord(r, i + 1).unwrap_or(0)];
    for r in c.records(RecordKind::Inst) {
        g.inst_id.push(c.field(r, 0).to_owned());
        g.inst_name.push(c.attr(r, "name").unwrap_or("").to_owned());
        let m = d
            .resolve(ci, c.field(r, 1))
            .expect("loaded Designs resolve every reference");
        g.inst_master.push(m as u32);
        let a = c.doc.find_attr(r, "at").expect("inst has at (format E029)");
        let v = |i| {
            c.doc
                .attr_value(a, i)
                .and_then(|t| parse_coord(t).ok())
                .unwrap_or(0)
        };
        g.inst_at.push([v(0), v(1)]);
        g.inst_orient.push(
            c.doc
                .attr_value(a, 2)
                .and_then(Orient::parse)
                .unwrap_or(Orient::R0),
        );
        g.inst_rec.push(r);
    }
    for r in c.records(RecordKind::Wire) {
        g.wire_id.push(c.field(r, 0).to_owned());
        let a = xy(r, 1);
        let b = xy(r, 3);
        g.wire.push([a[0], a[1], b[0], b[1]]);
        g.wire_bus.push(c.attr(r, "bus") == Some("true"));
        g.wire_rec.push(r);
    }
    for r in c.records(RecordKind::Label) {
        g.label_id.push(c.field(r, 0).to_owned());
        g.label_at.push(xy(r, 1));
        g.label_net.push(c.field(r, 4).to_owned());
        g.label_global.push(c.attr(r, "global") == Some("true"));
        g.label_rec.push(r);
    }
    for r in c.records(RecordKind::Port) {
        g.port_id.push(c.field(r, 0).to_owned());
        g.port_at.push(xy(r, 1));
        g.port_pin.push(c.field(r, 4).to_owned());
        g.port_rec.push(r);
    }
    for r in c.records(RecordKind::Noconn) {
        g.noconn_id.push(c.field(r, 0).to_owned());
        g.noconn_at.push(xy(r, 1));
        g.noconn_rec.push(r);
    }
    assert_eq!(g.inst_id.len(), g.inst_rec.len());
    assert_eq!(g.wire.len(), g.wire_rec.len());
    g
}

/// Transforms symbol-local `p` by `o` and translates it to `at` (format spec §3); `None` when
/// the result leaves `i32`.
pub fn place(p: [i32; 2], o: Orient, at: [i32; 2]) -> Option<[i32; 2]> {
    crate::translate::place(p, o, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Files {
        list.iter()
            .map(|(p, t)| ((*p).to_owned(), t.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn loads_the_closure_through_missing_rounds() {
        let all = files(&[
            (
                "schemify.project",
                "schemify 1 project\n\nlib dev path dev\n",
            ),
            (
                "tb.shbench",
                "schemify 1 shbench\n\ninst aaaaaa dev:amp\n  at 0 0 R0\n  name X1\n",
            ),
            (
                "dev/amp.shcomp",
                "schemify 1 shcomp\n\npin a in 0 0\n\nimpl schematic schematic\n\ninst bbbbbb core:res\n  at 0 0 R0\n  name R1\n\nport cccccc 0 -30 R0 a\n",
            ),
        ]);
        let mut have = Files::new();
        let mut absent = BTreeSet::new();
        let d = loop {
            match load(&have, &absent, &["tb.shbench"]) {
                Ok(d) => break d,
                Err(LoadError::Missing(paths)) => {
                    for p in paths {
                        match all.get(&p) {
                            Some(b) => {
                                have.insert(p, b.clone());
                            }
                            None => {
                                absent.insert(p);
                            }
                        }
                    }
                }
                Err(LoadError::Invalid(p)) => panic!("{p:?}"),
            }
        };
        let names: Vec<&str> = d.cells.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(names, ["tb.shbench", "dev/amp.shcomp", "core/res.shprim"]);
        assert_eq!(d.cells[1].lib, "dev");
        assert_eq!(d.resolve(0, "dev:amp"), Some(1));
        let g = geom(&d, 1);
        assert_eq!(g.inst_master, [2]);
        assert_eq!(sym_pins(&d.cells[2]).name, ["p", "n"]);
        assert_eq!(
            d.cells[1].text(),
            std::str::from_utf8(&all["dev/amp.shcomp"]).unwrap()
        );
    }

    #[test]
    fn technology_classes_load_as_their_core_drawing() {
        let f = files(&[
            (
                "schemify.project",
                "schemify 1 project\n\nlib tech map pdk/tech.map\n",
            ),
            (
                "a.shcomp",
                "schemify 1 shcomp\n\nimpl schematic schematic\n\ninst aaaaaa tech:nmos\n  at 0 0 R0\n  name M1\n  w 1u\ninst bbbbbb tech:pmos\n  at 0 100 R0\n  name M2\n",
            ),
        ]);
        let d = load(&f, &BTreeSet::new(), &["a.shcomp"]).unwrap();
        assert!(d.libs.is_empty(), "the map is not a library");
        let names: Vec<&str> = d.cells.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            names,
            ["a.shcomp", "core/nmos4.shprim", "core/pmos4.shprim"]
        );
        assert_eq!(d.resolve(0, "tech:nmos"), Some(1));
        let bad = files(&[(
            "b.shcomp",
            "schemify 1 shcomp\n\nimpl schematic schematic\n\ninst aaaaaa tech:res\n  at 0 0 R0\n  name R1\n",
        )]);
        let absent: BTreeSet<String> = ["schemify.project".to_owned()].into();
        match load(&bad, &absent, &["b.shcomp"]) {
            Err(LoadError::Invalid(p)) => {
                assert!(p[0].contains("no technology class res"), "{p:?}")
            }
            other => panic!("{:?}", other.err()),
        }
    }

    #[test]
    fn unresolvable_references_are_reported() {
        let f = files(&[(
            "tb.shbench",
            "schemify 1 shbench\n\ninst aaaaaa nolib:x\n  at 0 0 R0\n  name X1\n",
        )]);
        let absent: BTreeSet<String> = ["schemify.project".to_owned()].into();
        match load(&f, &absent, &["tb.shbench"]) {
            Err(LoadError::Invalid(p)) => assert!(p[0].contains("nolib"), "{p:?}"),
            other => panic!("{:?}", other.err()),
        }
    }
}
