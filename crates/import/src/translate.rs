//! xschem libraries → Schemify libraries (spec `import.md` §1–§7). Pure: bytes in,
//! bytes out, no file system.
//!
//! Spine ([`translate_library`]), one line per transform:
//! 1. `read_sources`: every `.sym`/`.sch` → [`XschemFile`] tables, sorted by path.
//! 2. `classify_symbols`: symbol attributes → class (cell, label, port, code, …).
//! 3. `plan_symbols`: cell symbols → interface plans (pins, params, impls, limits);
//!    `devices/` symbols listed in [`core::DEVICE_MAP`] → `core` targets instead.
//! 4. `plan_cells`: (symbol, schematic) pairs → `.shprim`/`.shcomp`/`.shbench` cells,
//!    plus alternate-schematic cells requested by instances.
//! 5. `name_libs`: cell directories → library names.
//! 6. `emit_cell` per cell → canonical bytes.
//! 7. manifest + report, with count conservation asserted per input file.
//!
//! Data (six questions): in = N files of KB each (corpus: 3,342 files, 60 MB); out =
//! one cell file per (symbol, schematic) pair. Tables are per-kind `Vec`s indexed by
//! `u32` rows (sources, symbols, cells < 2^16 in any real library). Everything lives
//! for one call. The transform is embarrassingly parallel per cell, but at ~0.3 s for
//! the whole corpus it stays sequential.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use schemify_format::{
    Document, FileKind, IdRng, Orient, RecordKind, fresh_id, parse_coord, write,
};

use crate::attr::{Attrs, at_tokens};
use crate::cdf::{CdfDump, CdfType};
use crate::core::{self, DeviceMap};
use crate::limits;
use crate::report::{Class, Drop, Item, Report};
use crate::spice::{self, ModelScan};
use crate::symbol::{self, PinDir, SymbolInterface};
use crate::tcl::{self, DrcProc};
use crate::template::{
    self, ExprMode, FormatCtx, Reason, Review, is_ident, literal, review, spice_number,
    string_value,
};
use crate::xschem::{self, GlobalTag, NumField, XschemFile};

/// Instance attributes that are netlist text of their own: an instance that sets one to
/// other text than its symbol gets a variant cell (spec §3).
const INSTANCE_TEXT: [&str; 3] = ["format", "lvs_format", "spice_sym_def"];

/// Most input files one call accepts (rows are `u32`, report items index files).
pub const MAX_FILES: usize = 1 << 20;

/// Translation knobs. `Default` maps `devices/` to `core` and has no PDK data.
#[derive(Clone, Copy, Debug)]
pub struct Options<'a> {
    /// The xschem library search path (`XSCHEM_LIBRARY_PATH`), as input-path prefixes
    /// in priority order. A symbol reference `r` resolves to the first `<root>/r` among
    /// the inputs, then to `r` next to the referencing schematic.
    pub roots: &'a [String],
    /// PDK path variables from the PDK `xschemrc` ([`tcl::pdk_vars`]): `$VAR` in
    /// netlist text becomes `${VAR}`, and each becomes a project `set` entry.
    pub pdk_vars: &'a [(String, String)],
    /// Map xschem `devices/*.sym` to the bundled `core` library (spec §6).
    pub map_core: bool,
    /// Model-card scans for L/W bin limits.
    pub models: &'a [ModelScan],
    /// Recognised `drc=` procs ([`tcl::drc_procs`]).
    pub drc: &'a [DrcProc],
    /// Manufacturing grid in picometres (tech file).
    pub grid_pm: Option<u32>,
    /// A Cadence `cdfDump`: radio/cyclic choices for same-named cells and params.
    pub cdf: Option<&'a CdfDump>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            roots: &[],
            pdk_vars: &[],
            map_core: true,
            models: &[],
            drc: &[],
            grid_pm: None,
            cdf: None,
        }
    }
}

/// xschem `(rot, flip)` → Schemify orientation, indexed `[rot][flip]`. xschem flips x
/// about the origin, then rotates clockwise on screen (y down) in quarter turns —
/// exactly the `M` then `R` order of format spec §3. Fixed by golden evidence: the
/// corpus test `orientation_table_reproduces_golden_connectivity` derives every row
/// from xschem's netlists independently and asserts this table.
pub const ORIENT_TABLE: [[Orient; 2]; 4] = [
    [Orient::R0, Orient::MR0],
    [Orient::R90, Orient::MR90],
    [Orient::R180, Orient::MR180],
    [Orient::R270, Orient::MR270],
];

/// Maps an xschem rotation/flip pair (any integers; xschem uses `rot` mod 4 and
/// `flip != 0`) to an orientation.
pub fn orient(rot: i16, flip: i16) -> Orient {
    ORIENT_TABLE[rot.rem_euclid(4) as usize][usize::from(flip != 0)]
}

/// Transforms a symbol-local point by `o` and translates it to `at` (format spec §3).
/// `None` when the result leaves `i32` milli-units.
pub fn place(p: [i32; 2], o: Orient, at: [i32; 2]) -> Option<[i32; 2]> {
    let (mut x, mut y) = (i64::from(p[0]), i64::from(p[1]));
    if o.mirrored() {
        x = -x;
    }
    (x, y) = match o.quarter_turns() {
        0 => (x, y),
        1 => (-y, x),
        2 => (-x, -y),
        _ => (y, -x),
    };
    let r = [x + i64::from(at[0]), y + i64::from(at[1])];
    Some([i32::try_from(r[0]).ok()?, i32::try_from(r[1]).ok()?])
}

/// Translates xschem libraries into Schemify files.
///
/// `inputs` are `(path, bytes)` with `/`-separated paths; `.sym` and `.sch` are
/// translated, in path order whatever the input order; any other input is a side file
/// read only where netlist text names it (a `spice_sym_def=".include f.cir"` gives the
/// `.subckt` port order, spec §2). Returns the output files as
/// `(path, bytes)` sorted by path — `<lib>/<cell>.<ext>`, `schemify.project`,
/// `import-report.md`, `import-report.txt` — and the [`Report`].
///
/// Deterministic: the same inputs and options give byte-identical outputs (ids come
/// from [`IdRng::for_import`] seeded with `<lib>/<file>` and the record's line).
///
/// # Panics
///
/// Panics if count conservation fails for any file (an importer bug) or if there are
/// more than [`MAX_FILES`] inputs.
pub fn translate_library(
    inputs: &[(&str, &[u8])],
    opts: &Options,
) -> (Vec<(String, Vec<u8>)>, Report) {
    assert!(inputs.len() <= MAX_FILES, "too many input files");
    let (mut t, mut acc) = Tables::read_sources(inputs, opts);
    t.classify_symbols();
    t.scan_overrides();
    t.plan_symbols(&mut acc);
    t.plan_cells(&mut acc);
    t.plan_variants(&mut acc);
    t.name_libs();
    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(t.cells.len() + 3);
    for c in 0..t.cells.len() {
        files.push(t.emit_cell(c, &mut acc));
    }
    files.push(("schemify.project".to_owned(), t.manifest().into_bytes()));
    let report = t.finish(&files, acc);
    report.assert_conservation();
    files.push((
        "import-report.md".to_owned(),
        report.markdown().into_bytes(),
    ));
    files.push(("import-report.txt".to_owned(), report.text().into_bytes()));
    files.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        files.windows(2).all(|w| w[0].0 != w[1].0),
        "output paths are unique"
    );
    (files, report)
}

// ------------------------------------------------------------------------- tables

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ext {
    Sym,
    Sch,
}

/// One input file.
struct Src {
    path: String,
    ext: Ext,
    file: XschemFile,
    /// `file_version` 1.0 (or absent): symbol references carry no extension.
    v10: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SymClass {
    Cell,
    Label,
    Port,
    Noconn,
    /// Netlist text: `netlist_commands`, `spice_parameters`, pinless symbols with a format.
    Code,
    Drop(Drop),
    Review(Reason),
}

struct Sym {
    src: u32,
    iface: SymbolInterface,
    ty: String,
    class: SymClass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Number,
    Bool,
    Str,
    Choice,
}

impl Kind {
    fn word(self) -> Option<&'static str> {
        match self {
            Kind::Number => None,
            Kind::Bool => Some("bool"),
            Kind::Str => Some("string"),
            Kind::Choice => Some("choice"),
        }
    }
}

struct PinOut {
    /// Schemify name (`xs` plus `_pin` when a parameter has the same name).
    name: String,
    /// xschem name.
    xs: String,
    dir: &'static str,
    xy: [i32; 2],
    inherit: Option<String>,
    /// A second pin of an earlier pin's xschem name: written `alias <that pin>` (xschem
    /// shorts same-named pins; netlist spec §3).
    alias: bool,
}

struct ParamOut {
    name: String,
    default: String,
    kind: Kind,
    choices: Vec<String>,
    attrs: Vec<(&'static str, String)>,
}

struct ImplOut {
    name: &'static str,
    lang: &'static str,
    formats: Vec<(&'static str, String)>,
    ignore: Option<String>,
    body: Vec<String>,
}

/// A cell symbol's translated interface.
struct Plan {
    pins: Vec<PinOut>,
    params: Vec<ParamOut>,
    /// Leaf impl (always present; ignored when the cell is a pure subcircuit).
    leaf: ImplOut,
    /// Formats for a schematic impl (subcircuit-style conversion).
    schematic_formats: Vec<(&'static str, String)>,
    subcircuit: bool,
    /// The symbol has a `spice_sym_def`: xschem netlists it by that text, so the leaf impl
    /// is the default even with a schematic.
    sym_def: bool,
    /// `spice_ignore` of the symbol: applies to every impl.
    ignore: Option<String>,
    prefix: Option<String>,
    spiceprefix: Option<String>,
}

/// What instances need to know about a target cell.
#[derive(Clone, Debug, Default)]
struct Iface {
    pins: Vec<(String, [i32; 2])>,
    implicit: Vec<(String, String)>,
    params: Vec<(String, String, Kind)>,
}

#[derive(Clone, Copy)]
enum Target {
    /// A converted cell (row in `cells`).
    Cell(u32),
    /// A core cell.
    Core(&'static DeviceMap),
}

struct Cell {
    dir: String,
    name: String,
    kind: FileKind,
    sym: Option<u32>,
    sch: Option<u32>,
    /// The symbol's records are counted by this cell (not by an alternate cell).
    tally_sym: bool,
    /// The schematic's records are counted by this cell (not shared or alternate).
    tally_sch: bool,
    /// A bench copy of component `copy_of`'s schematic (its netlist text needs a bench).
    copy_of: Option<u32>,
    lib: u16,
}

/// Records each input record's fate (spec §7 count conservation).
struct Acc {
    out: Vec<u32>,
    items: Vec<Item>,
    unused: BTreeMap<String, u32>,
    device_instances: u32,
    core_instances: u32,
    /// False while re-translating a schematic for an alternate cell.
    active: bool,
}

impl Acc {
    fn out(&mut self, file: u32, n: u32) {
        assert!(
            (file as usize) < self.out.len(),
            "file row {file} out of range"
        );
        if self.active {
            self.out[file as usize] += n;
        }
    }

    fn item(
        &mut self,
        file: u32,
        line: u32,
        class: Class,
        records: u32,
        detail: impl Into<String>,
    ) {
        if self.active {
            self.items.push(Item {
                file,
                line,
                class,
                records,
                detail: detail.into(),
            });
        }
    }
}

struct Tables<'o> {
    opts: &'o Options<'o>,
    srcs: Vec<Src>,
    index: HashMap<String, u32>,
    syms: Vec<Sym>,
    sym_of_src: Vec<Option<u32>>,
    plans: Vec<Option<Plan>>,
    sym_ifaces: Vec<Option<Iface>>,
    targets: Vec<Option<Target>>,
    core_ifaces: HashMap<&'static str, Iface>,
    /// (symbol, key) pairs some instance sets to text that is not a number expression
    /// (`value="port=1 z0=50 ac 1"`): such parameters are strings whatever their default.
    textual: HashSet<(u32, String)>,
    cells: Vec<Cell>,
    /// (base cell, alternate schematic src) → alternate cell.
    alt_cells: HashMap<(u32, u32), u32>,
    /// (schematic src, instance row) → its variant cell (instance netlist text, spec §3).
    variants: HashMap<(u32, u32), u32>,
    /// Inputs that are neither `.sym` nor `.sch`, by normalised path: files netlist text
    /// includes (`spice_sym_def=".include f.cir"`).
    side: HashMap<String, String>,
    lib_names: Vec<String>,
}

/// Normalises a `/` path lexically (`.`/`..`/empty components).
fn normalize(path: &str) -> String {
    let abs = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            ".." if abs => {}
            c => parts.push(c),
        }
    }
    let j = parts.join("/");
    if abs { format!("/{j}") } else { j }
}

fn dir_of(p: &str) -> &str {
    p.rsplit_once('/').map_or("", |(d, _)| d)
}

fn file_of(p: &str) -> &str {
    p.rsplit_once('/').map_or(p, |(_, f)| f)
}

fn stem_of(p: &str) -> &str {
    let f = file_of(p);
    f.rsplit_once('.').map_or(f, |(s, _)| s)
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() || rel.starts_with('/') {
        normalize(rel)
    } else {
        normalize(&format!("{dir}/{rel}"))
    }
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truthy(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Some(true),
        "false" | "off" | "no" | "0" | "" => Some(false),
        _ => None,
    }
}

/// Parses a core cell into the interface instances need.
fn iface_of_cell_bytes(bytes: &[u8]) -> Iface {
    let d = schemify_format::parse(bytes).expect("core cells parse");
    let mut f = Iface::default();
    for r in 1..d.record_count() {
        let field = |i| d.field(r, i).unwrap_or("");
        match d.record_kind(r) {
            RecordKind::Pin => {
                let inherit = d.find_attr(r, "inherit").and_then(|a| d.attr_value(a, 0));
                match inherit {
                    Some(net) => f.implicit.push((field(0).to_owned(), net.to_owned())),
                    None => f.pins.push((
                        field(0).to_owned(),
                        [
                            parse_coord(field(2)).unwrap_or(0),
                            parse_coord(field(3)).unwrap_or(0),
                        ],
                    )),
                }
            }
            RecordKind::Param => {
                let kind = match d.find_attr(r, "kind").and_then(|a| d.attr_value(a, 0)) {
                    Some("bool") => Kind::Bool,
                    Some("string") => Kind::Str,
                    Some("choice") => Kind::Choice,
                    _ => Kind::Number,
                };
                f.params
                    .push((field(0).to_owned(), field(1).to_owned(), kind));
            }
            _ => {}
        }
    }
    assert!(!f.pins.is_empty(), "core cells have pins");
    f
}

impl<'o> Tables<'o> {
    // ------------------------------------------------------------------ 1. read

    fn read_sources(inputs: &[(&str, &[u8])], opts: &'o Options<'o>) -> (Tables<'o>, Acc) {
        let (mut sorted, others): (Vec<_>, Vec<_>) = inputs
            .iter()
            .map(|(p, b)| (normalize(p), *b))
            .partition(|(p, _)| p.ends_with(".sym") || p.ends_with(".sch"));
        let side: HashMap<String, String> = others
            .into_iter()
            .map(|(p, b)| (p, String::from_utf8_lossy(b).into_owned()))
            .collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        sorted.dedup_by(|a, b| a.0 == b.0);
        let mut srcs = Vec::with_capacity(sorted.len());
        let mut items = Vec::new();
        for (path, bytes) in sorted {
            let text = String::from_utf8_lossy(bytes);
            let file = match xschem::read(&text) {
                Ok(f) => f,
                Err(e) => {
                    items.push(Item {
                        file: srcs.len() as u32,
                        line: 0,
                        class: Class::Dropped(Drop::Malformed),
                        records: 0,
                        detail: format!("not read: {e:?}"),
                    });
                    XschemFile::default()
                }
            };
            let version = file.global(GlobalTag::Version).unwrap_or("");
            let v10 = !version.contains("file_version=") || version.contains("file_version=1.0");
            let ext = if path.ends_with(".sym") {
                Ext::Sym
            } else {
                Ext::Sch
            };
            srcs.push(Src {
                path,
                ext,
                file,
                v10,
            });
        }
        let index: HashMap<String, u32> = srcs
            .iter()
            .enumerate()
            .map(|(i, s)| (s.path.clone(), i as u32))
            .collect();
        assert!(
            srcs.windows(2).all(|w| w[0].path < w[1].path),
            "sources sorted and unique"
        );
        assert_eq!(index.len(), srcs.len());
        let n = srcs.len();
        let acc = Acc {
            out: vec![0; n],
            items,
            unused: BTreeMap::new(),
            device_instances: 0,
            core_instances: 0,
            active: true,
        };
        let t = Tables {
            opts,
            srcs,
            index,
            syms: Vec::new(),
            sym_of_src: vec![None; n],
            plans: Vec::new(),
            sym_ifaces: Vec::new(),
            targets: Vec::new(),
            core_ifaces: HashMap::new(),
            textual: HashSet::new(),
            cells: Vec::new(),
            alt_cells: HashMap::new(),
            variants: HashMap::new(),
            side,
            lib_names: Vec::new(),
        };
        (t, acc)
    }

    /// Resolves a symbol or schematic reference made from source `from`.
    fn resolve(&self, from: u32, symref: &str) -> Option<u32> {
        assert!((from as usize) < self.srcs.len());
        let mut r = symref.trim().to_owned();
        if r.is_empty() || r.contains('@') || r.contains("tcleval(") || r.contains('(') {
            return None;
        }
        if self.srcs[from as usize].v10 && !r.ends_with(".sym") && !r.ends_with(".sch") {
            r.push_str(".sym");
        }
        let here = dir_of(&self.srcs[from as usize].path);
        let look = |p: String| self.index.get(&p).copied();
        if r.starts_with('/') {
            return look(normalize(&r));
        }
        if r.starts_with("./") || r.starts_with("../") {
            return look(join(here, &r));
        }
        self.opts
            .roots
            .iter()
            .find_map(|root| look(join(&normalize(root), &r)))
            .or_else(|| look(join(here, &r)))
    }

    // ------------------------------------------------------------------ 2. classify

    fn classify_symbols(&mut self) {
        for s in 0..self.srcs.len() {
            if self.srcs[s].ext != Ext::Sym {
                continue;
            }
            let iface = symbol::extract(&self.srcs[s].file);
            let raw = iface.attrs.get("type").unwrap_or("").trim().to_owned();
            let ty = if raw.starts_with("tcleval(") && raw.contains("[symtype]") {
                "subcircuit".to_owned() // the known default of IHP's `symtype` proc
            } else {
                raw
            };
            let has_format = iface
                .attrs
                .get("format")
                .is_some_and(|f| !f.trim().is_empty());
            let class = match ty.as_str() {
                "label" => SymClass::Label,
                "ipin" | "opin" | "iopin" => SymClass::Port,
                "noconn" => SymClass::Noconn,
                "netlist_commands" | "spice_parameters" => SymClass::Code,
                "show_label" | "scope" => SymClass::Drop(Drop::Display),
                "launcher"
                | "logo"
                | "architecture"
                | "arch_declarations"
                | "attributes"
                | "package"
                | "port_attributes"
                | "use"
                | "timescale"
                | "verilog_preprocessor"
                | "generic"
                | "annotator" => SymClass::Drop(Drop::NonElectrical),
                "netlist_options" | "bus_tap" => SymClass::Review(Reason::Unsupported),
                "probe" if !has_format => SymClass::Drop(Drop::Display),
                // A pinless symbol whose format uses `extra` nets is a device with only
                // inherited pins (sky130 `fill_1`, `tap_1`: LVS goldens list them).
                // (`plan_symbol`'s pin rule: without one, a `.shprim` with no pin, E025.)
                _ if iface.pins.name.is_empty()
                    && has_format
                    && !implicit_nets(&iface).is_empty() =>
                {
                    SymClass::Cell
                }
                _ if iface.pins.name.is_empty() && has_format => SymClass::Code,
                _ if iface.pins.name.is_empty() => SymClass::Drop(Drop::NonElectrical),
                t if t.starts_with("tcleval(") => SymClass::Review(Reason::Tcl),
                _ => SymClass::Cell,
            };
            self.sym_of_src[s] = Some(self.syms.len() as u32);
            self.syms.push(Sym {
                src: s as u32,
                iface,
                ty,
                class,
            });
        }
        let n = self.syms.len();
        self.plans = (0..n).map(|_| None).collect();
        self.sym_ifaces = (0..n).map(|_| None).collect();
        self.targets = vec![None; n];
        assert!(
            self.syms
                .iter()
                .all(|s| self.srcs[s.src as usize].ext == Ext::Sym)
        );
    }

    /// Marks parameters that instances in the inputs use as free text, so their kind
    /// is `string` (an override there would otherwise be lost to review).
    fn scan_overrides(&mut self) {
        for x in 0..self.srcs.len() as u32 {
            let f = &self.srcs[x as usize].file;
            for row in 0..f.instances.symref.len() {
                let Some(s) = self
                    .resolve(x, f.str(f.instances.symref[row]))
                    .and_then(|src| self.sym_of_src[src as usize])
                else {
                    continue;
                };
                let props = Attrs::parse(f.str(f.instances.props[row]));
                for i in 0..props.len() {
                    let v = props.value(i).trim();
                    if v.is_empty()
                        || v.contains("tcleval(")
                        || override_text(v, Kind::Number, &[], &[]).is_ok()
                    {
                        continue;
                    }
                    self.textual.insert((s, props.key(i).to_owned()));
                }
            }
        }
    }

    // ------------------------------------------------------------------ 3. plan symbols

    fn plan_symbols(&mut self, acc: &mut Acc) {
        for s in 0..self.syms.len() {
            if self.syms[s].class != SymClass::Cell {
                continue;
            }
            if let Some(map) = self.core_map(s) {
                self.targets[s] = Some(Target::Core(map));
                continue;
            }
            let plan = self.plan_symbol(s, acc);
            self.sym_ifaces[s] = Some(iface_of_plan(&plan));
            self.plans[s] = Some(plan);
        }
    }

    /// The core row for symbol `s` when it is a mapped `devices/` symbol whose pins
    /// (renamed) sit exactly where the core cell's pins are.
    fn core_map(&mut self, s: usize) -> Option<&'static DeviceMap> {
        let path = &self.srcs[self.syms[s].src as usize].path;
        if !self.opts.map_core || file_of(dir_of(path)) != "devices" {
            return None;
        }
        let map = core::device_map(stem_of(path))?;
        let iface = self.core_ifaces.entry(map.cell).or_insert_with(|| {
            iface_of_cell_bytes(core::core_file(map.cell).expect("mapped cell exists"))
        });
        let sym = &self.syms[s].iface;
        let same = sym.pins.name.len() == iface.pins.len()
            && (0..sym.pins.name.len()).all(|p| {
                let name = map.pin(sym.pin_name(p));
                iface
                    .pins
                    .iter()
                    .any(|(n, xy)| n == name && *xy == sym.pins.center[p])
            });
        same.then_some(map)
    }

    fn k_line(&self, s: usize) -> u32 {
        let g = &self.srcs[self.syms[s].src as usize].file.globals;
        (0..g.tag.len())
            .find(|&i| matches!(g.tag[i], GlobalTag::K | GlobalTag::G))
            .map_or(0, |i| g.line[i])
    }

    fn plan_symbol(&mut self, s: usize, acc: &mut Acc) -> Plan {
        let file_id = self.syms[s].src;
        let line = self.k_line(s);
        let sym = &self.syms[s];
        let iface = &sym.iface;
        let tpl = &iface.template;
        let attrs = &iface.attrs;
        let fmt = attrs.get("format");
        let mut reviews: Vec<Review> = Vec::new();
        let mut tokens: Vec<String> = Vec::new();
        let implicit = implicit_nets(iface);
        // Pins in netlist order.
        let mut pins: Vec<PinOut> = Vec::new();
        for p in 0..iface.pins.name.len() {
            let mut name = iface.pin_name(p).trim().to_owned();
            if name.is_empty() || name.contains(char::is_whitespace) {
                reviews.push(review(Reason::Name, format!("pin {p} has no usable name")));
                name = format!("pin{p}");
            }
            // A repeated name is the same port at a second position (xschem shorts them:
            // goldens `doublepin`, `xcross`, `short.sym`): an alias pin `<base>_<k>[…]`.
            let xs = name.clone();
            let alias = pins.iter().any(|q| q.xs == name);
            if alias {
                let (base, bus) = name.split_at(name.find('[').unwrap_or(name.len()));
                let mut k = 2;
                while pins.iter().any(|q| q.name == format!("{base}_{k}{bus}")) {
                    k += 1;
                }
                name = format!("{base}_{k}{bus}");
            }
            let dir = match iface.pins.dir[p] {
                PinDir::In => "in",
                PinDir::Out => "out",
                _ => "inout",
            };
            pins.push(PinOut {
                xs,
                name,
                dir,
                xy: iface.pins.center[p],
                inherit: None,
                alias,
            });
        }
        // A format that names its pins with `@@P` instead of `@pinlist` fixes the port
        // order: xschem's `.subckt` lists ports in that order (IHP std cells: `@@Y @@A`
        // over symbol order A, Y). Pins it does not name keep their order after.
        if let Some(f) = fmt.filter(|f| !f.contains("@pinlist")) {
            let mut order: Vec<String> = Vec::new();
            for t in crate::attr::at_tokens(f).filter(|t| t.kind == crate::attr::AtKind::PinNet) {
                if pins.iter().any(|q| q.name == t.name) && !order.iter().any(|o| o == t.name) {
                    order.push(t.name.to_owned());
                }
            }
            let rank = |q: &PinOut| {
                order
                    .iter()
                    .position(|o| *o == q.name)
                    .unwrap_or(order.len())
            };
            pins.sort_by_key(rank); // stable: unnamed pins keep file order
        }
        for (k, net) in &implicit {
            if pins.iter().any(|q| &q.name == k) {
                reviews.push(review(
                    Reason::Name,
                    format!("net parameter {k} is also a pin"),
                ));
                continue;
            }
            pins.push(PinOut {
                name: k.clone(),
                xs: k.clone(),
                dir: "inout",
                xy: [0, 0],
                inherit: Some(net.clone()),
                alias: false,
            });
        }
        // Parameters from the template.
        let spiceprefix = tpl.get("spiceprefix").map(|v| v.trim().to_owned());
        let avail: Vec<&str> = attrs
            .get("available_models")
            .unwrap_or("")
            .split_whitespace()
            .collect();
        let keys: Vec<String> = (0..tpl.len()).map(|i| tpl.key(i).to_owned()).collect();
        let mut params: Vec<ParamOut> = Vec::new();
        let mut dropped_keys: Vec<String> = Vec::new();
        for i in 0..tpl.len() {
            let k = tpl.key(i);
            let skip = matches!(
                k,
                "name" | "spiceprefix" | "device_model" | "spice_ignore" | "lvs_ignore"
            ) || implicit.iter().any(|(n, _)| n == k);
            if skip {
                continue;
            }
            if params.iter().any(|p| p.name == k) {
                continue; // xschem uses the first occurrence
            }
            if !is_ident(k) || !k.starts_with(|c: char| c.is_ascii_alphabetic()) {
                // Not a name any `@token` can reach (`+` continuation artefacts, `300`).
                dropped_keys.push(k.to_owned());
                continue;
            }
            let reserved = template::RESERVED.contains(&k)
                || matches!(k, "at" | "ignore" | "inherit")
                || implicit.iter().any(|(n, _)| n == k);
            if reserved {
                reviews.push(review(
                    Reason::Name,
                    format!("parameter `{k}` cannot be represented"),
                ));
                continue;
            }
            let v = tpl.value(i);
            let (mut kind, mut default, review) = infer(v, &avail, &keys);
            if kind == Kind::Number && self.textual.contains(&(s as u32, k.to_owned())) {
                (kind, default) = (Kind::Str, literal(v.trim()));
            }
            reviews.extend(review);
            params.push(ParamOut {
                name: k.to_owned(),
                default,
                kind,
                choices: if kind == Kind::Choice {
                    avail.iter().map(|a| a.to_string()).collect()
                } else {
                    Vec::new()
                },
                attrs: Vec::new(),
            });
        }
        // Templates per dialect.
        // Geometric pins named like a parameter are renamed (templates resolve pin names
        // before parameters; xschem keeps the two namespaces apart).
        let mut rename: Vec<(String, String)> = Vec::new();
        for k in 0..pins.len() {
            if pins[k].inherit.is_none() && params.iter().any(|p| p.name == pins[k].name) {
                let mut n = format!("{}_pin", pins[k].xs);
                while pins.iter().any(|q| q.name == n) || params.iter().any(|q| q.name == n) {
                    n.push('_');
                }
                rename.push((pins[k].xs.clone(), n.clone()));
                pins[k].name = n;
            }
            // `D[5..0]` is the bracket-less bundle `D5,…,D0` (as in labels).
            let bundle = dotted_bus(&pins[k].name);
            if bundle != pins[k].name && !pins[k].alias {
                rename.retain(|(x, _)| *x != pins[k].xs);
                rename.push((pins[k].xs.clone(), bundle.clone()));
                pins[k].name = bundle;
            }
        }
        let pin_names: Vec<String> = pins
            .iter()
            .filter(|p| p.inherit.is_none())
            .map(|p| p.xs.clone())
            .collect();
        let implicit_names: Vec<String> = implicit.iter().map(|(n, _)| n.clone()).collect();
        let subcircuit = sym.ty == "subcircuit";
        let mut leaf_formats = Vec::new();
        let mut schematic_formats = Vec::new();
        let mut spice_failed = false;
        for (key, dialect) in [
            ("format", "spice"),
            ("lvs_format", "cdl"),
            ("spectre_format", "spectre"),
            ("verilog_format", "verilog"),
        ] {
            let Some(f) = attrs.get(key).filter(|f| !f.is_empty()) else {
                continue;
            };
            if f.trim().is_empty() {
                // A blank format (`lvs_format=" "`) netlists nothing (golden
                // sky130_mismatch: the instance is absent from the LVS netlist).
                leaf_formats.push((dialect, String::new()));
                schematic_formats.push((dialect, String::new()));
                continue;
            }
            for schematic in [false, true] {
                let names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let ctx = FormatCtx {
                    params: &names,
                    pins: &pin_names,
                    implicit: &implicit_names,
                    spiceprefix: spiceprefix.as_deref(),
                    schematic,
                    rename: &rename,
                };
                match template::convert_format(f, &ctx) {
                    Ok(c) => {
                        for u in &c.unknown {
                            if !params.iter().any(|p| &p.name == u) {
                                tokens
                                    .push(format!("@{u} (declared as an empty string parameter)"));
                                params.push(ParamOut {
                                    name: u.clone(),
                                    default: String::new(),
                                    kind: Kind::Str,
                                    choices: Vec::new(),
                                    attrs: Vec::new(),
                                });
                            }
                        }
                        if !schematic {
                            tokens
                                .extend(c.dropped.iter().map(|d| format!("@{d} (rendered empty)")));
                        }
                        // Re-convert when unknown tokens were declared, so the template sees them.
                        let text = if c.unknown.is_empty() {
                            c.text
                        } else {
                            let names: Vec<String> =
                                params.iter().map(|p| p.name.clone()).collect();
                            template::convert_format(
                                f,
                                &FormatCtx {
                                    params: &names,
                                    ..ctx
                                },
                            )
                            .map(|c| c.text)
                            .unwrap_or(c.text)
                        };
                        if schematic {
                            schematic_formats.push((dialect, text));
                        } else {
                            leaf_formats.push((dialect, text));
                        }
                    }
                    Err(r) => {
                        if !schematic {
                            spice_failed |= dialect == "spice";
                            reviews.push(review(r.reason, format!("{key}: {}", r.detail)));
                        }
                    }
                }
            }
        }
        // Impl language and body.
        let device_model = attrs
            .get("device_model")
            .or_else(|| tpl.get("device_model"))
            .filter(|d| !d.trim().is_empty());
        let lang = match device_model {
            Some(d) if d.contains("pre_osdi") => "veriloga",
            Some(d) if d.contains("d_cosim") => "verilog",
            _ => "spice",
        };
        let mut body = Vec::new();
        for text in [
            device_model,
            attrs.get("spice_sym_def").filter(|d| !d.trim().is_empty()),
        ]
        .into_iter()
        .flatten()
        {
            match netlist_text(text, self.opts.pdk_vars) {
                Ok(t) => body.extend(lines(&t)),
                Err(detail) => reviews.push(review(Reason::Tcl, detail)),
            }
        }
        // `spice_sym_def` is the cell's netlist text (it replaces the schematic in xschem),
        // and xschem orders `@pinlist` like its `.subckt` line, inline or in the file an
        // `.include` names (golden tb_symbol_include): spell the pins out in that order.
        let sym_def = attrs
            .get("spice_sym_def")
            .filter(|d| !d.trim().is_empty())
            .and_then(|d| netlist_text(d, self.opts.pdk_vars).ok())
            .map(|t| lines(&t));
        let stem = stem_of(&self.srcs[sym.src as usize].path).to_owned();
        let ports = sym_def
            .as_deref()
            .and_then(|l| self.subckt_ports(sym.src, l, &stem));
        if let Some(ports) = ports {
            match pin_order(&pins, &ports) {
                Some(holes) => {
                    for (_, t) in &mut leaf_formats {
                        *t = t.replace("{pins}", &holes);
                    }
                }
                None => reviews.push(review(
                    Reason::PinToken,
                    format!(
                        "spice_sym_def .subckt ports `{}` do not match the pins",
                        ports.join(" ")
                    ),
                )),
            }
        }
        let ignore_src = attrs
            .get("spice_ignore")
            .or_else(|| tpl.get("spice_ignore"));
        let ignore = match ignore_src.map(str::trim) {
            Some(v) if v.starts_with("tcleval(") => {
                reviews.push(review(Reason::Tcl, format!("spice_ignore={v}")));
                None
            }
            Some(v @ ("open" | "short")) => Some(v.to_owned()),
            Some(v) if truthy(v) == Some(true) => Some("true".to_owned()),
            _ => None,
        };
        let has_spice = leaf_formats.iter().any(|(d, _)| *d == "spice");
        // Without a usable format xschem does not netlist the device: never guess one.
        let leaf_ignore = if spice_failed || (!has_spice && !subcircuit) {
            Some("true".to_owned())
        } else {
            ignore.clone()
        };
        // Device limits (D14).
        let xs_params: Vec<(String, String)> = (0..tpl.len())
            .map(|i| (tpl.key(i).to_owned(), tpl.value(i).to_owned()))
            .collect();
        let file_name = file_of(&self.srcs[sym.src as usize].path).to_owned();
        let (lims, lim_review) = limits::derive(&limits::Inputs {
            params: &xs_params,
            format: fmt,
            drc: attrs.get("drc"),
            file_name: &file_name,
            models: self.opts.models,
            procs: self.opts.drc,
            grid_pm: self.opts.grid_pm,
        });
        for l in lims {
            if let Some(p) = params
                .iter_mut()
                .find(|p| p.name == l.param && p.kind == Kind::Number)
            {
                p.attrs.push((l.attr, l.expr));
            }
        }
        reviews.extend(lim_review.into_iter().map(|d| review(Reason::Drc, d)));
        // CDF choices.
        if let Some(cdf) = self.opts.cdf {
            apply_cdf(cdf, stem_of(&file_name), &mut params, &mut reviews);
        }
        let prefix = tpl
            .get("name")
            .map(|n| {
                n.trim()
                    .chars()
                    .take_while(char::is_ascii_alphabetic)
                    .collect::<String>()
            })
            .filter(|p| !p.is_empty());
        for r in reviews {
            acc.item(file_id, line, Class::Review(r.reason), 0, r.detail);
        }
        for t in tokens {
            acc.item(file_id, line, Class::Token, 0, t);
        }
        for k in dropped_keys {
            acc.item(
                file_id,
                line,
                Class::Dropped(Drop::Malformed),
                0,
                format!("template key `{k}`"),
            );
        }
        assert!(pins.iter().all(|p| !p.name.is_empty()), "pins are named");
        assert!(
            (1..pins.len()).all(|i| !pins[..i].iter().any(|q| q.name == pins[i].name)),
            "pin names are unique"
        );
        assert!(
            (1..params.len()).all(|i| !params[..i].iter().any(|q| q.name == params[i].name)),
            "param names are unique"
        );
        Plan {
            pins,
            params,
            leaf: ImplOut {
                name: lang,
                lang,
                formats: leaf_formats,
                ignore: leaf_ignore,
                body,
            },
            ignore,
            schematic_formats,
            subcircuit,
            sym_def: sym_def.is_some(),
            prefix,
            spiceprefix,
        }
    }

    /// The port names of the `.subckt` that netlist text `text` of symbol source `src`
    /// defines, or of the file its `.include` names (a side input, resolved beside the
    /// symbol); the subckt named `stem` (case-insensitively) if several, else the first.
    /// `None` when there is no such line or the file is not an input.
    fn subckt_ports(&self, src: u32, text: &[String], stem: &str) -> Option<Vec<String>> {
        let mut found = spice::subckt_lines(&text.join("\n"));
        if found.is_empty() {
            let inc = text.iter().find_map(|l| {
                let w: Vec<&str> = l.split_whitespace().collect();
                (w.len() == 2 && w[0].eq_ignore_ascii_case(".include"))
                    .then(|| w[1].trim_matches(['"', '\'']))
            })?;
            let path = join(dir_of(&self.srcs[src as usize].path), inc);
            found = spice::subckt_lines(self.side.get(&path)?);
        }
        let pick = found
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(stem))
            .unwrap_or(0);
        found.into_iter().nth(pick).map(|(_, ports)| ports)
    }

    fn iface_of(&self, t: Target) -> &Iface {
        match t {
            Target::Core(m) => &self.core_ifaces[m.cell],
            Target::Cell(c) => {
                let s = self.cells[c as usize]
                    .sym
                    .expect("cell targets have a symbol");
                self.sym_ifaces[s as usize]
                    .as_ref()
                    .expect("cell symbols are planned")
            }
        }
    }

    // ------------------------------------------------------------------ 4. cells

    /// The schematic a cell symbol is implemented by, if any: `default_schematic=ignore`
    /// → none; `schematic=` → that file (IHP `hierarchy_config` → its known default, the
    /// same-named schematic in the sibling `sch` tree); else `<stem>.sch` beside it.
    fn schematic_of(&self, s: usize, acc: &mut Acc) -> Option<u32> {
        let sym = &self.syms[s];
        let path = &self.srcs[sym.src as usize].path;
        let (dir, stem) = (dir_of(path), stem_of(path));
        if sym.iface.attrs.get("default_schematic").map(str::trim) == Some("ignore") {
            return None;
        }
        match sym
            .iface
            .attrs
            .get("schematic")
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            Some(v) if v.contains("[hierarchy_config") => {
                let sch_dir = dir.replace("/sym/", "/sch/");
                self.index.get(&format!("{sch_dir}/{stem}.sch")).copied()
            }
            Some(v) if v.starts_with("tcleval(") => {
                acc.item(
                    sym.src,
                    0,
                    Class::Review(Reason::Tcl),
                    0,
                    format!("schematic={v}"),
                );
                None
            }
            Some(v) => {
                let v = v.replace("@symname", stem);
                self.resolve(sym.src, &v)
                    .or_else(|| self.resolve(sym.src, &format!("{v}.sch")))
            }
            None => self.index.get(&format!("{dir}/{stem}.sch")).copied(),
        }
    }

    fn plan_cells(&mut self, acc: &mut Acc) {
        let mut consumed: HashSet<u32> = HashSet::new();
        for s in 0..self.syms.len() {
            if self.plans[s].is_none() {
                continue;
            }
            let sch = self.schematic_of(s, acc);
            let path = self.srcs[self.syms[s].src as usize].path.clone();
            let shared = sch.is_some_and(|x| !consumed.insert(x));
            let c = self.cells.len() as u32;
            self.cells.push(Cell {
                dir: dir_of(&path).to_owned(),
                name: stem_of(&path).replace(':', "_"),
                kind: if sch.is_some() {
                    FileKind::Shcomp
                } else {
                    FileKind::Shprim
                },
                sym: Some(s as u32),
                sch,
                tally_sym: true,
                tally_sch: !shared,
                copy_of: None,
                lib: 0,
            });
            self.targets[s] = Some(Target::Cell(c));
        }
        // A schematic as a symbol: xschem netlists a schematic's `ipin`/`opin`/`iopin`
        // instances as its interface (the LVS `.subckt` ports of a symbol-less schematic,
        // and the pins of a `.sch` placed as a symbol, LCC). One derived symbol per such
        // schematic that no cell symbol owns (the cell `<stem>`), or that is owned but
        // placed as a symbol (a second cell `<stem>__lcc`: its pins sit elsewhere).
        let placed = self.placed_schematics();
        for x in 0..self.srcs.len() as u32 {
            let owned = consumed.contains(&x);
            if self.srcs[x as usize].ext != Ext::Sch || (owned && !placed.contains(&x)) {
                continue;
            }
            let owner: Vec<String> = self
                .cells
                .iter()
                .find(|c| c.sch == Some(x) && c.sym.is_some())
                .and_then(|c| self.plans[c.sym.expect("checked") as usize].as_ref())
                .map(|p| p.pins.iter().map(|q| q.xs.clone()).collect())
                .unwrap_or_default();
            let Some((iface, ty)) = self.derived_iface(x, &owner) else {
                continue;
            };
            let s = self.syms.len();
            self.syms.push(Sym {
                src: x,
                iface,
                ty,
                class: SymClass::Cell,
            });
            self.plans.push(None);
            self.sym_ifaces.push(None);
            self.targets.push(None);
            let plan = self.plan_symbol(s, acc);
            self.sym_ifaces[s] = Some(iface_of_plan(&plan));
            self.plans[s] = Some(plan);
            self.sym_of_src[x as usize] = Some(s as u32);
            let path = &self.srcs[x as usize].path;
            let stem = stem_of(path).replace(':', "_");
            let c = self.cells.len() as u32;
            let name = if owned { format!("{stem}__lcc") } else { stem };
            if owned {
                acc.item(
                    x,
                    0,
                    Class::AltCell,
                    0,
                    format!("{name} (the schematic placed as a symbol)"),
                );
            }
            self.cells.push(Cell {
                dir: dir_of(path).to_owned(),
                name,
                kind: FileKind::Shcomp,
                sym: Some(s as u32),
                sch: Some(x),
                tally_sym: false,
                tally_sch: !owned,
                copy_of: None,
                lib: 0,
            });
            self.targets[s] = Some(Target::Cell(c));
            consumed.insert(x);
        }
        for x in 0..self.srcs.len() as u32 {
            if self.srcs[x as usize].ext != Ext::Sch || consumed.contains(&x) {
                continue;
            }
            let path = &self.srcs[x as usize].path;
            self.cells.push(Cell {
                dir: dir_of(path).to_owned(),
                name: stem_of(path).replace(':', "_"),
                kind: FileKind::Shbench,
                sym: None,
                sch: Some(x),
                tally_sym: false,
                tally_sch: true,
                copy_of: None,
                lib: 0,
            });
        }
        // A component schematic carrying netlist text (simulation commands, models) is
        // also a test bench in xschem: emit a bench copy `<cell>__top` for that text.
        for c in 0..self.cells.len() {
            let cell = &self.cells[c];
            let Some(x) = cell
                .sch
                .filter(|_| cell.kind == FileKind::Shcomp && cell.tally_sch)
            else {
                continue;
            };
            if !self.has_netlist_text(x) {
                continue;
            }
            let copy = Cell {
                dir: cell.dir.clone(),
                name: format!("{}__top", cell.name),
                kind: FileKind::Shbench,
                sym: None,
                sch: Some(x),
                tally_sym: false,
                tally_sch: false,
                copy_of: Some(c as u32),
                lib: 0,
            };
            acc.item(
                x,
                0,
                Class::AltCell,
                0,
                format!("{} (bench copy carrying the netlist text)", copy.name),
            );
            self.cells.push(copy);
        }
        // Alternate schematics requested by instances (`schematic=` on a `C` record).
        for x in 0..self.srcs.len() as u32 {
            let f = &self.srcs[x as usize].file;
            for row in 0..f.instances.symref.len() {
                let props = Attrs::parse(f.str(f.instances.props[row]));
                let Some(alt) = props
                    .get("schematic")
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                else {
                    continue;
                };
                let target = self
                    .resolve(x, f.str(f.instances.symref[row]))
                    .and_then(|src| self.sym_of_src[src as usize])
                    .and_then(|s| self.targets[s as usize]);
                let Some(Target::Cell(c)) = target else {
                    continue;
                };
                let Some(a) = self.alt_source(x, c, alt) else {
                    continue;
                };
                if Some(a) == self.cells[c as usize].sch || self.alt_cells.contains_key(&(c, a)) {
                    continue;
                }
                let base = &self.cells[c as usize];
                let name = format!(
                    "{}__{}",
                    base.name,
                    stem_of(&self.srcs[a as usize].path).replace(':', "_")
                );
                let new = Cell {
                    dir: base.dir.clone(),
                    name,
                    kind: FileKind::Shcomp,
                    sym: base.sym,
                    sch: Some(a),
                    tally_sym: false,
                    tally_sch: false,
                    copy_of: None,
                    lib: 0,
                };
                acc.item(
                    x,
                    f.instances.line[row],
                    Class::AltCell,
                    0,
                    format!("{} for schematic={alt}", new.name),
                );
                self.alt_cells.insert((c, a), self.cells.len() as u32);
                self.cells.push(new);
            }
        }
        assert!(
            self.cells
                .iter()
                .all(|c| c.sym.is_some() || c.sch.is_some())
        );
        assert!(
            self.cells
                .iter()
                .all(|c| (c.kind == FileKind::Shbench) == c.sym.is_none())
        );
    }

    /// Instance netlist text that differs from the symbol's: `(key, value)` per key of
    /// [`INSTANCE_TEXT`] the instance sets (a blank value counts: `lvs_format=" "`
    /// netlists nothing) to other text than symbol `s`, unless the key is a parameter
    /// of `target`.
    fn overrides_of(&self, s: u32, target: Target, props: &Attrs) -> Vec<(&'static str, String)> {
        let sym = &self.syms[s as usize];
        let params = &self.iface_of(target).params;
        let mut out = Vec::new();
        for key in INSTANCE_TEXT {
            let Some(v) = props.get(key).filter(|v| !v.is_empty()) else {
                continue;
            };
            let differs = sym.iface.attrs.get(key).map(collapse_ws) != Some(collapse_ws(v));
            if differs && !params.iter().any(|p| p.0 == key) {
                out.push((key, v.to_owned()));
            }
        }
        assert!(out.len() <= INSTANCE_TEXT.len());
        out
    }

    /// Per-instance variant cells (spec §3): an instance whose `format`, `lvs_format` or
    /// `spice_sym_def` differs from its symbol's netlists through `<cell>__<instance>`,
    /// a copy of its target (its `schematic=` alternate included) with those attributes;
    /// instances with the same symbol, schematic and overrides share one. A
    /// `spice_sym_def` makes the copy a leaf netlisted by that text. A leaf copy renders
    /// `@symname` as the subckt the instance still calls: the `schematic=` stem when the
    /// instance sets one (xschem names the subckt after it), else the symbol's stem.
    fn plan_variants(&mut self, acc: &mut Acc) {
        // (symbol, schematic, symname, overrides) → variant cell.
        let mut seen: HashMap<(u32, Option<u32>, String, String), u32> = HashMap::new();
        let mut wanted = Vec::new();
        for x in 0..self.srcs.len() as u32 {
            if self.srcs[x as usize].ext != Ext::Sch {
                continue;
            }
            let f = &self.srcs[x as usize].file;
            for row in 0..f.instances.symref.len() {
                let Some(s) = self
                    .resolve(x, f.str(f.instances.symref[row]))
                    .and_then(|src| self.sym_of_src[src as usize])
                    .filter(|&s| self.syms[s as usize].class == SymClass::Cell)
                else {
                    continue;
                };
                let Some(target) = self.targets[s as usize] else {
                    continue;
                };
                let props = Attrs::parse(f.str(f.instances.props[row]));
                let overrides = self.overrides_of(s, target, &props);
                if overrides.is_empty() {
                    continue;
                }
                let alt = props
                    .get("schematic")
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned);
                let name = props.get("name").unwrap_or("").trim().to_owned();
                wanted.push((x, row, s, target, overrides, alt, name));
            }
        }
        for (x, row, s, target, overrides, alt, inst) in wanted {
            let line = self.srcs[x as usize].file.instances.line[row];
            let src = self.syms[s as usize].src;
            let base_stem = stem_of(&self.srcs[src as usize].path).to_owned();
            let sym_def = overrides.iter().any(|o| o.0 == "spice_sym_def");
            let base = match target {
                Target::Cell(c) => Some(c),
                Target::Core(_) => None,
            };
            let alt_src = alt
                .as_deref()
                .zip(base)
                .and_then(|(a, c)| self.alt_source(x, c, a));
            let sch = match base {
                Some(c) if !sym_def => alt_src.or(self.cells[c as usize].sch),
                _ => None,
            };
            let symname = match alt.as_deref() {
                Some(a) if sch.is_none() => stem_of(a).to_owned(),
                _ => base_stem.clone(),
            };
            let text: Vec<String> = overrides.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let key = (s, sch, symname.clone(), text.join("\n"));
            if let Some(&c) = seen.get(&key) {
                self.variants.insert((x, row as u32), c);
                continue;
            }
            // The copied symbol: the instance's text first, so every lookup sees it.
            let mut iface = self.syms[s as usize].iface.clone();
            for (k, v) in &overrides {
                iface.attrs.prepend(k, v);
            }
            if sch.is_none() {
                for k in ["format", "lvs_format", "spectre_format", "verilog_format"] {
                    let named = iface
                        .attrs
                        .get(k)
                        .filter(|v| v.contains("@symname"))
                        .map(|v| v.replace("@symname", &symname));
                    if let Some(v) = named {
                        iface.attrs.prepend(k, &v);
                    }
                }
            }
            let s2 = self.syms.len() as u32;
            let ty = self.syms[s as usize].ty.clone();
            self.syms.push(Sym {
                src,
                iface,
                ty,
                class: SymClass::Cell,
            });
            self.plans.push(None);
            self.sym_ifaces.push(None);
            self.targets.push(None);
            let textual: Vec<String> = self
                .textual
                .iter()
                .filter(|(t, _)| *t == s)
                .map(|(_, k)| k.clone())
                .collect();
            self.textual.extend(textual.into_iter().map(|k| (s2, k)));
            // The symbol's own findings were reported once, with the symbol.
            let active = std::mem::replace(&mut acc.active, false);
            let plan = self.plan_symbol(s2 as usize, acc);
            acc.active = active;
            if plan.pins.is_empty() {
                // The instance's text leaves a pinless symbol without its inherited pins:
                // no valid cell (`E025`), so the instance keeps its symbol's netlisting.
                acc.item(
                    x,
                    line,
                    Class::Review(Reason::InstanceText),
                    0,
                    format!("{}: the variant would have no pin", text.join(" ")),
                );
                continue;
            }
            if sch.is_none() && plan.leaf.ignore.as_deref() == Some("true") {
                acc.item(
                    x,
                    line,
                    Class::Review(Reason::InstanceText),
                    0,
                    format!("{}: the variant does not netlist", text.join(" ")),
                );
            }
            self.sym_ifaces[s2 as usize] = Some(iface_of_plan(&plan));
            self.plans[s2 as usize] = Some(plan);
            let (dir, base_name) = match base {
                Some(c) => (
                    self.cells[c as usize].dir.clone(),
                    self.cells[c as usize].name.clone(),
                ),
                None => (
                    dir_of(&self.srcs[x as usize].path).to_owned(),
                    base_stem.replace(':', "_"),
                ),
            };
            let inst: String = inst
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let name = format!("{base_name}__{inst}");
            acc.item(
                x,
                line,
                Class::AltCell,
                0,
                format!("{name} for {}", text.join(" ")),
            );
            let c = self.cells.len() as u32;
            self.cells.push(Cell {
                dir,
                name,
                kind: if sch.is_some() {
                    FileKind::Shcomp
                } else {
                    FileKind::Shprim
                },
                sym: Some(s2),
                sch,
                tally_sym: false,
                tally_sch: false,
                copy_of: None,
                lib: 0,
            });
            self.targets[s2 as usize] = Some(Target::Cell(c));
            self.variants.insert((x, row as u32), c);
            seen.insert(key, c);
        }
        assert!(
            self.variants
                .values()
                .all(|&c| (c as usize) < self.cells.len())
        );
    }

    /// Schematic sources some instance places as a symbol (`C {x.sch}`).
    fn placed_schematics(&self) -> HashSet<u32> {
        let mut out = HashSet::new();
        for x in 0..self.srcs.len() as u32 {
            let f = &self.srcs[x as usize].file;
            for row in 0..f.instances.symref.len() {
                if let Some(t) = self.resolve(x, f.str(f.instances.symref[row]))
                    && self.srcs[t as usize].ext == Ext::Sch
                {
                    out.insert(t);
                }
            }
        }
        out
    }

    /// The interface of schematic `x` used as a symbol: its own `K` attributes (type
    /// `subcircuit` when it sets none) and one pin per `ipin`/`opin`/`iopin` instance in
    /// file order, at that instance's pin, named by its `lab` (first occurrence wins),
    /// direction `in`/`out`/`inout`. `None` when it has no port instance.
    fn derived_iface(&self, x: u32, order: &[String]) -> Option<(SymbolInterface, String)> {
        let f = &self.srcs[x as usize].file;
        let mut iface = symbol::extract(f);
        iface.pins = symbol::Pins::default();
        iface.names.clear();
        // Port rows ranked by the owning symbol's pin order (stable: others after, in
        // file order): xschem's `.subckt` of a placed schematic follows its `.sym`
        // (golden poweramp_lcc `mos_power_ampli`, loading `switch_rreal`).
        let mut rows: Vec<usize> = (0..f.instances.symref.len()).collect();
        let lab_of = |row: usize| {
            Attrs::parse(f.str(f.instances.props[row]))
                .get("lab")
                .map(|l| l.trim().to_owned())
        };
        rows.sort_by_cached_key(|&row| {
            lab_of(row)
                .and_then(|l| order.iter().position(|o| *o == l))
                .unwrap_or(order.len())
        });
        for row in rows {
            let Some(p) = self
                .resolve(x, f.str(f.instances.symref[row]))
                .and_then(|src| self.sym_of_src[src as usize])
                .filter(|&p| self.syms[p as usize].class == SymClass::Port)
            else {
                continue;
            };
            let port = &self.syms[p as usize];
            let props = Attrs::parse(f.str(f.instances.props[row]));
            let lab = props
                .get("lab")
                .or_else(|| port.iface.template.get("lab"))
                .unwrap_or("")
                .trim();
            let taken = (0..iface.pins.name.len()).any(|k| iface.pin_name(k) == lab);
            let Some(at) = self.anchor(f, row, p).filter(|_| !lab.is_empty() && !taken) else {
                continue;
            };
            iface
                .pins
                .name
                .push(crate::push_span(&mut iface.names, lab));
            iface.pins.dir.push(match port.ty.as_str() {
                "ipin" => PinDir::In,
                "opin" => PinDir::Out,
                _ => PinDir::InOut,
            });
            iface.pins.center.push(at);
        }
        if iface.pins.name.is_empty() {
            return None;
        }
        let ty = match iface.attrs.get("type").map(str::trim) {
            Some(t) if !t.is_empty() => t.to_owned(),
            _ => "subcircuit".to_owned(),
        };
        Some((iface, ty))
    }

    /// Whether schematic `x` holds netlist text: code instances or `S {}` text.
    fn has_netlist_text(&self, x: u32) -> bool {
        let f = &self.srcs[x as usize].file;
        let s_text = (0..f.globals.tag.len()).any(|i| {
            f.globals.tag[i] == GlobalTag::S && !f.str(f.globals.text[i]).trim().is_empty()
        });
        s_text
            || (0..f.instances.symref.len()).any(|row| {
                self.resolve(x, f.str(f.instances.symref[row]))
                    .and_then(|src| self.sym_of_src[src as usize])
                    .is_some_and(|s| self.syms[s as usize].class == SymClass::Code)
                    || top_only(f, row)
            })
    }

    fn alt_source(&self, from: u32, c: u32, value: &str) -> Option<u32> {
        let base = &self.cells[c as usize];
        let v = value.replace("@symname", &base.name);
        let sym_src = base.sym.map(|s| self.syms[s as usize].src)?;
        [from, sym_src].into_iter().find_map(|f| {
            self.resolve(f, &v)
                .or_else(|| self.resolve(f, &format!("{v}.sch")))
                .filter(|&src| self.srcs[src as usize].ext == Ext::Sch)
        })
    }

    // ------------------------------------------------------------------ 5. libraries

    /// One library per cell directory, named after the directory (walking up past the
    /// generic `xschem`/`sym`/`sch` components); clashes get `_2`, `_3`, … in path order.
    fn name_libs(&mut self) {
        let dirs: BTreeSet<String> = self.cells.iter().map(|c| c.dir.clone()).collect();
        let mut names: Vec<String> = Vec::with_capacity(dirs.len());
        let mut of_dir: HashMap<String, u16> = HashMap::new();
        for d in &dirs {
            let base = d
                .rsplit('/')
                .find(|c| !c.is_empty() && !matches!(*c, "xschem" | "sym" | "sch"))
                .unwrap_or("lib")
                .replace(':', "_");
            let mut name = base.clone();
            let mut k = 2;
            while name == "core" || names.contains(&name) {
                name = format!("{base}_{k}");
                k += 1;
            }
            assert!(names.len() < usize::from(u16::MAX), "too many libraries");
            of_dir.insert(d.clone(), names.len() as u16);
            names.push(name);
        }
        let mut taken: HashSet<(u16, String)> = HashSet::new();
        for c in &mut self.cells {
            c.lib = of_dir[&c.dir];
            let base = c.name.clone();
            let mut k = 2;
            while !taken.insert((c.lib, c.name.clone())) {
                c.name = format!("{base}_{k}");
                k += 1;
            }
        }
        assert!(
            !names
                .iter()
                .any(|n| n == "core" || n.is_empty() || n.contains([':', '/']))
        );
        self.lib_names = names;
    }

    // ------------------------------------------------------------------ 6. emit

    fn emit_cell(&self, c: usize, acc: &mut Acc) -> (String, Vec<u8>) {
        let cell = &self.cells[c];
        let lib = &self.lib_names[usize::from(cell.lib)];
        let ext = match cell.kind {
            FileKind::Shprim => "shprim",
            FileKind::Shcomp => "shcomp",
            _ => "shbench",
        };
        let path = format!("{lib}/{}.{ext}", cell.name);
        assert!(
            !cell.name.is_empty() && !cell.name.contains('/'),
            "cell names are file stems"
        );
        let mut doc = Document::new(cell.kind);
        let plan = cell.sym.map(|s| {
            self.plans[s as usize]
                .as_ref()
                .expect("cell symbols are planned")
        });
        assert!(
            cell.kind != FileKind::Shprim || plan.is_some_and(|p| !p.pins.is_empty()),
            "{}: a .shprim has at least one pin (E025)",
            cell.name
        );
        // Header.
        let impls = self.impls_of(cell, plan);
        if impls.len() > 1 {
            doc.push_attr("default");
            doc.push_value(impls[0].0);
        }
        if let Some(p) = plan.and_then(|p| p.prefix.as_deref()) {
            doc.push_attr("prefix");
            doc.push_value(p);
        }
        if let Some(s) = cell.sym {
            let ty = &self.syms[s as usize].ty;
            if !ty.is_empty() && !ty.contains(char::is_whitespace) {
                doc.push_attr("category");
                doc.push_value(ty);
            }
        }
        let main_src = cell
            .sym
            .map(|s| self.syms[s as usize].src)
            .or(cell.sch)
            .expect("a cell has a source");
        doc.push_attr("provenance");
        doc.push_value(&format!(
            "xschem {lib}/{}",
            file_of(&self.srcs[main_src as usize].path)
        ));
        // Ranks 1–2: the symbol (a schematic placed as a symbol has no drawing of its own).
        if let (Some(s), Some(plan)) = (cell.sym, plan) {
            acc.active = cell.tally_sym;
            emit_interface(&mut doc, plan, &impls);
            if self.srcs[self.syms[s as usize].src as usize].ext == Ext::Sym {
                self.emit_graphics(&mut doc, s, plan, acc);
            }
            acc.active = true;
        }
        // Rank 3: the schematic.
        let mut out = SchOut::default();
        if let Some(x) = cell.sch {
            let seed = format!("{lib}/{}", file_of(&self.srcs[x as usize].path));
            let mut ids = Ids {
                seed,
                taken: HashSet::new(),
            };
            let parents: Vec<String> = plan.map_or_else(Vec::new, |p| {
                p.params.iter().map(|q| q.name.clone()).collect()
            });
            // Alias pins are no ports: their target's port serves both positions.
            let pins: Vec<(String, String)> = plan.map_or_else(Vec::new, |p| {
                p.pins
                    .iter()
                    .filter(|q| !q.alias)
                    .map(|q| (q.xs.clone(), q.name.clone()))
                    .collect()
            });
            let sc = SchCtx {
                file: x,
                lib: cell.lib,
                bench: cell.kind == FileKind::Shbench,
                parents,
                pins,
            };
            // A bench copy defines the component's numeric parameters as bench variables
            // (xschem leaves them undefined at the top level; these are its defaults),
            // those the schematic's values mention: an unused one (`m`) would be a
            // `.param` xschem's netlist does not have.
            if let Some(base) = cell.copy_of {
                let plan = self.cells[base as usize]
                    .sym
                    .and_then(|s| self.plans[s as usize].as_ref());
                let words = self.value_words(x);
                for p in plan
                    .into_iter()
                    .flat_map(|p| &p.params)
                    .filter(|p| p.kind == Kind::Number)
                    .filter(|p| words.contains(&p.name.to_ascii_lowercase()))
                {
                    out.vars.push((p.name.clone(), p.default.clone()));
                    out.default_vars.push(p.name.clone());
                }
            }
            acc.active = cell.tally_sch;
            self.emit_schematic(&mut doc, &mut ids, &sc, &mut out, acc);
            acc.active = true;
        }
        // Rank 4: bench variables from `.param` lines.
        for (n, e) in &out.vars {
            doc.begin_record(RecordKind::Var);
            doc.push_field(n);
            doc.push_field(e);
        }
        // Rank 5: impl bodies.
        if let Some(plan) = plan
            && !plan.leaf.body.is_empty()
            && impls.iter().any(|i| i.0 == plan.leaf.name)
        {
            doc.begin_record(RecordKind::Body);
            doc.push_field(plan.leaf.name);
            plan.leaf.body.iter().for_each(|l| doc.push_verbatim(l));
        }
        if !out.body.is_empty() && impls.iter().any(|i| i.0 == "schematic") {
            doc.begin_record(RecordKind::Body);
            doc.push_field("schematic");
            out.body.iter().for_each(|l| doc.push_verbatim(l));
        }
        // Rank 6: bench code blocks.
        for (name, lang, text) in &out.codes {
            doc.begin_record(RecordKind::Code);
            doc.push_field(name);
            doc.push_field(lang);
            text.iter().for_each(|l| doc.push_verbatim(l));
        }
        (path, write(&doc).into_bytes())
    }

    /// The impls a cell gets, default first: `(name, lang, formats, ignore)`.
    fn impls_of<'p>(&self, cell: &Cell, plan: Option<&'p Plan>) -> Vec<ImplRow<'p>> {
        let Some(p) = plan else { return Vec::new() };
        let leaf = (
            p.leaf.name,
            p.leaf.lang,
            p.leaf.formats.as_slice(),
            p.leaf.ignore.as_deref(),
        );
        let schematic = (
            "schematic",
            "schematic",
            p.schematic_formats.as_slice(),
            p.ignore.as_deref(),
        );
        match (cell.kind, p.subcircuit && !p.sym_def) {
            (FileKind::Shcomp, true) => vec![schematic],
            (FileKind::Shcomp, false) => vec![leaf, schematic],
            _ => vec![leaf],
        }
    }

    fn emit_graphics(&self, doc: &mut Document, s: u32, plan: &Plan, acc: &mut Acc) {
        let sym = &self.syms[s as usize];
        let fid = sym.src;
        let f = &self.srcs[fid as usize].file;
        tally_common(f, fid, acc, false);
        let layer_ok = |l: u8| u32::from(l) < CAD_LAYERS;
        for i in 0..f.lines.layer.len() {
            if !layer_ok(f.lines.layer[i]) {
                acc.item(
                    fid,
                    f.lines.line[i],
                    Class::Dropped(Drop::Layer),
                    1,
                    format!("L layer {}", f.lines.layer[i]),
                );
                continue;
            }
            let [x1, y1, x2, y2] = f.lines.xy[i];
            doc.begin_record(RecordKind::Line);
            [x1, y1, x2, y2]
                .into_iter()
                .for_each(|v| doc.push_field_coord(v));
            push_layer(doc, f.lines.layer[i]);
            let props = Attrs::parse(f.str(f.lines.props[i]));
            if let Some(d) = props
                .get("dash")
                .filter(|d| d.trim().parse::<u32>().is_ok_and(|n| n > 0))
            {
                doc.push_attr("dash");
                doc.push_value(d.trim());
            }
            acc.out(fid, 1);
        }
        for i in 0..f.boxes.layer.len() {
            if f.boxes.layer[i] == symbol::PIN_LAYER {
                acc.out(fid, 1); // folded into a `pin` record
                continue;
            }
            if !layer_ok(f.boxes.layer[i]) {
                acc.item(
                    fid,
                    f.boxes.line[i],
                    Class::Dropped(Drop::Layer),
                    1,
                    format!("B layer {}", f.boxes.layer[i]),
                );
                continue;
            }
            let [x1, y1, x2, y2] = f.boxes.xy[i];
            doc.begin_record(RecordKind::Rect);
            [x1, y1, x2, y2]
                .into_iter()
                .for_each(|v| doc.push_field_coord(v));
            push_layer(doc, f.boxes.layer[i]);
            let props = Attrs::parse(f.str(f.boxes.props[i]));
            doc.push_attr("fill");
            doc.push_value(if props.get("fill").map(str::trim) == Some("false") {
                "false"
            } else {
                "true"
            });
            acc.out(fid, 1);
        }
        for i in 0..f.polygons.layer.len() {
            let (first, count) = (f.polygons.first[i] as usize, f.polygons.count[i] as usize);
            let mut pts = f.points[first..first + count].to_vec();
            let closed = pts.len() > 2 && pts.first() == pts.last();
            if closed {
                pts.pop();
            }
            if pts.len() < 2 || !layer_ok(f.polygons.layer[i]) {
                acc.item(
                    fid,
                    f.polygons.line[i],
                    Class::Dropped(Drop::Layer),
                    1,
                    "P with < 2 points or layer >= 22",
                );
                continue;
            }
            doc.begin_record(RecordKind::Poly);
            pts.iter().for_each(|p| {
                doc.push_field_coord(p[0]);
                doc.push_field_coord(p[1]);
            });
            push_layer(doc, f.polygons.layer[i]);
            let props = Attrs::parse(f.str(f.polygons.props[i]));
            if matches!(props.get("fill").map(str::trim), Some("true" | "full")) {
                doc.push_attr("fill");
                doc.push_value("true");
            }
            if closed {
                doc.push_attr("closed");
                doc.push_value("true");
            }
            acc.out(fid, 1);
        }
        for i in 0..f.arcs.layer.len() {
            if !layer_ok(f.arcs.layer[i]) {
                acc.item(
                    fid,
                    f.arcs.line[i],
                    Class::Dropped(Drop::Layer),
                    1,
                    format!("A layer {}", f.arcs.layer[i]),
                );
                continue;
            }
            let [cx, cy, r] = f.arcs.circle[i];
            let [start, sweep] = f.arcs.angles[i];
            if sweep.abs() >= 360_000 {
                doc.begin_record(RecordKind::Circle);
                [cx, cy, r]
                    .into_iter()
                    .for_each(|v| doc.push_field_coord(v));
            } else {
                doc.begin_record(RecordKind::Arc);
                [cx, cy, r, start, sweep]
                    .into_iter()
                    .for_each(|v| doc.push_field_coord(v));
            }
            push_layer(doc, f.arcs.layer[i]);
            let props = Attrs::parse(f.str(f.arcs.props[i]));
            if matches!(props.get("fill").map(str::trim), Some("true" | "full")) {
                doc.push_attr("fill");
                doc.push_value("true");
            }
            acc.out(fid, 1);
        }
        let names: Vec<String> = plan.params.iter().map(|p| p.name.clone()).collect();
        for i in 0..f.texts.text.len() {
            let props = Attrs::parse(f.str(f.texts.props[i]));
            let hidden = props.get("hide").is_some_and(|h| truthy(h) != Some(false));
            let tpl = stext_template(f.str(f.texts.text[i]), &names, plan.spiceprefix.as_deref());
            let (Some(tpl), false) = (tpl, hidden) else {
                acc.item(
                    fid,
                    f.texts.line[i],
                    Class::Dropped(Drop::Display),
                    1,
                    f.str(f.texts.text[i]),
                );
                continue;
            };
            let [x, y] = f.texts.xy[i];
            doc.begin_record(RecordKind::Stext);
            doc.push_field_coord(x);
            doc.push_field_coord(y);
            doc.push_field(orient(f.texts.rot[i], f.texts.flip[i]).as_str());
            doc.push_field(&tpl);
            doc.push_attr("size");
            doc.push_value_coord(f.texts.scale[i][0]);
            for flag in ["hcenter", "vcenter"] {
                if props.get(flag).and_then(truthy) == Some(true) {
                    doc.push_attr(flag);
                }
            }
            let layer = props
                .get("layer")
                .and_then(|l| l.trim().parse::<u8>().ok())
                .unwrap_or(3);
            push_layer(doc, layer);
            acc.out(fid, 1);
        }
        let netlist_records = f.wires.xy.len() + f.instances.symref.len();
        if netlist_records > 0 {
            acc.item(
                fid,
                0,
                Class::Dropped(Drop::SymbolNetlistRecord),
                netlist_records as u32,
                "N/C records drawn inside a symbol",
            );
        }
    }

    fn emit_schematic(
        &self,
        doc: &mut Document,
        ids: &mut Ids,
        sc: &SchCtx,
        out: &mut SchOut,
        acc: &mut Acc,
    ) {
        let fid = sc.file;
        let f = &self.srcs[fid as usize].file;
        tally_common(f, fid, acc, true);
        // `S {}`: SPICE text emitted verbatim by xschem, inside the component's subckt when
        // the schematic is one (D17 as amended: a `.shcomp` carries `code` too).
        let g = &f.globals;
        for i in (0..g.tag.len()).filter(|&i| g.tag[i] == GlobalTag::S) {
            let text = f.str(g.text[i]);
            if text.trim().is_empty() {
                acc.out(fid, 1);
            } else {
                match netlist_text(text, self.opts.pdk_vars) {
                    Ok(t) => {
                        out.codes.push((
                            "schematic_spice".to_owned(),
                            "spice".to_owned(),
                            lines(&t),
                        ));
                        acc.out(fid, 1);
                    }
                    Err(d) => acc.item(fid, g.line[i], Class::Review(Reason::Tcl), 1, d),
                }
            }
        }
        // Pass 1: netlist text instances (their `.param`s become bench variables).
        let n = f.instances.symref.len();
        let mut sym_of_row: Vec<Option<u32>> = Vec::with_capacity(n);
        for row in 0..n {
            let s = self
                .resolve(fid, f.str(f.instances.symref[row]))
                .and_then(|x| self.sym_of_src[x as usize]);
            sym_of_row.push(s);
            let Some(s) = s.filter(|&s| self.syms[s as usize].class == SymClass::Code) else {
                continue;
            };
            let line = f.instances.line[row];
            let props = Attrs::parse(f.str(f.instances.props[row]));
            let ignored = props
                .get("spice_ignore")
                .or_else(|| self.syms[s as usize].iface.template.get("spice_ignore"))
                .and_then(truthy);
            if ignored == Some(true) {
                let detail = format!("netlist text instance {}", f.str(f.instances.symref[row]));
                acc.item(fid, line, Class::Dropped(Drop::SpiceIgnore), 1, detail);
                continue;
            }
            // In a component the text is the subckt's (code.sym and its kin default to
            // `only_toplevel=false`); `only_toplevel=true` text is the bench copy's.
            let top_only = props
                .get("only_toplevel")
                .or_else(|| self.syms[s as usize].iface.template.get("only_toplevel"))
                .and_then(truthy)
                == Some(true);
            if !sc.bench && top_only {
                let detail = format!("netlist text instance {}", f.str(f.instances.symref[row]));
                acc.item(fid, line, Class::Dropped(Drop::BenchCopy), 1, detail);
                continue;
            }
            match self.render_code(s, &props) {
                Ok((lang, text)) => {
                    let base = props
                        .get("name")
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .unwrap_or("code");
                    let name = unique(base, out.codes.iter().map(|c| c.0.as_str()));
                    out.codes.push((name, lang, lines(&text)));
                    acc.out(fid, 1);
                }
                Err(r) => acc.item(fid, line, Class::Review(r.reason), 1, r.detail),
            }
        }
        // A bench's `.param` lines become its variables; a component's stay in its
        // subckt text, where xschem writes them.
        for code in out.codes.iter_mut().filter(|_| sc.bench) {
            extract_params(&mut code.2, &mut out.vars, &mut out.default_vars);
        }
        let mut known: Vec<String> = sc.parents.clone();
        known.extend(out.vars.iter().map(|v| v.0.clone()));
        // Wires.
        for i in 0..f.wires.xy.len() {
            let [x1, y1, x2, y2] = f.wires.xy[i];
            if (x1, y1) == (x2, y2) {
                acc.item(fid, f.wires.line[i], Class::Dropped(Drop::ZeroWire), 1, "");
                continue;
            }
            doc.begin_record(RecordKind::Wire);
            doc.push_field(&ids.next(f.wires.line[i], 0));
            [x1, y1, x2, y2]
                .into_iter()
                .for_each(|v| doc.push_field_coord(v));
            out.extend_bbox([x1, y1]);
            out.extend_bbox([x2, y2]);
            let props = Attrs::parse(f.str(f.wires.props[i]));
            if props.get("bus").and_then(truthy) == Some(true) {
                doc.push_attr("bus");
                doc.push_value("true");
            }
            acc.out(fid, 1);
        }
        // Pass 2: everything else.
        for (row, sym_row) in sym_of_row.iter().enumerate() {
            let line = f.instances.line[row];
            let symref = f.str(f.instances.symref[row]);
            let Some(s) = *sym_row else {
                let lcc = self.resolve(fid, symref).is_some();
                let generator = symref.contains('(');
                let reason = if lcc || generator {
                    Reason::Unsupported
                } else {
                    Reason::MissingSymbol
                };
                let what = match (lcc, generator) {
                    (true, _) => "a schematic used as a symbol",
                    (_, true) => "a symbol generator",
                    _ => "symbol not found",
                };
                acc.item(
                    fid,
                    line,
                    Class::Review(reason),
                    1,
                    format!("{what}: {symref}"),
                );
                continue;
            };
            out.extend_bbox(f.instances.xy[row]);
            if !sc.bench && self.syms[s as usize].class == SymClass::Cell && top_only(f, row) {
                // xschem netlists `only_toplevel=true` devices only when this schematic
                // is the top (golden test_analog: n_diffamp's stimuli are not in its
                // subckt): the bench copy carries them.
                let detail = format!("only_toplevel instance {symref}");
                acc.item(fid, line, Class::Dropped(Drop::BenchCopy), 1, detail);
                continue;
            }
            match self.syms[s as usize].class {
                SymClass::Cell => self.emit_instance(doc, ids, sc, row, s, &known, out, acc),
                SymClass::Label => self.emit_label(doc, ids, sc, row, s, false, out, acc),
                SymClass::Port => self.emit_label(doc, ids, sc, row, s, true, out, acc),
                SymClass::Noconn => self.emit_noconn(doc, ids, sc, row, s, acc),
                SymClass::Code => {} // pass 1
                SymClass::Drop(d) => acc.item(fid, line, Class::Dropped(d), 1, symref),
                SymClass::Review(r) => acc.item(fid, line, Class::Review(r), 1, symref.to_owned()),
            }
        }
        // Texts.
        for i in 0..f.texts.text.len() {
            let text = f.str(f.texts.text[i]);
            let props = Attrs::parse(f.str(f.texts.props[i]));
            let display = text.trim().is_empty()
                || text.contains("tcleval(")
                || text.contains("@spice_get")
                || text.contains("@#")
                || props.find("name").is_some()
                || props.find("floater").is_some();
            if display {
                acc.item(fid, f.texts.line[i], Class::Dropped(Drop::Display), 1, text);
                continue;
            }
            doc.begin_record(RecordKind::Text);
            doc.push_field(&ids.next(f.texts.line[i], 0));
            let [x, y] = f.texts.xy[i];
            doc.push_field_coord(x);
            doc.push_field_coord(y);
            doc.push_field(orient(f.texts.rot[i], f.texts.flip[i]).as_str());
            doc.push_field(text);
            doc.push_attr("size");
            doc.push_value_coord(f.texts.scale[i][0]);
            acc.out(fid, 1);
        }
        // Drawings.
        let drawings =
            (f.lines.xy.len() + f.boxes.xy.len() + f.polygons.layer.len() + f.arcs.layer.len())
                as u32;
        if drawings > 0 {
            acc.item(
                fid,
                0,
                Class::Dropped(Drop::Graphic),
                drawings,
                format!("{drawings} L/B/P/A records"),
            );
        }
        // Interface pins bound by name only (xschem binds subcircuit ports by net name):
        // a port at a same-named label, else at a free point outside the drawing.
        if !sc.bench {
            let [mut fx, fy] = out.bbox_min();
            for (k, (xs, pin)) in sc.pins.iter().enumerate() {
                if out.ports.contains(pin) {
                    continue;
                }
                let at = out
                    .labels
                    .iter()
                    .find(|(n, _)| n == xs)
                    .map(|(_, xy)| *xy)
                    .unwrap_or_else(|| {
                        fx -= 20_000;
                        [fx, fy - 20_000]
                    });
                doc.begin_record(RecordKind::Port);
                doc.push_field(&ids.next(0, k as u32));
                doc.push_field_coord(at[0]);
                doc.push_field_coord(at[1]);
                doc.push_field(Orient::R0.as_str());
                doc.push_field(pin);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_instance(
        &self,
        doc: &mut Document,
        ids: &mut Ids,
        sc: &SchCtx,
        row: usize,
        s: u32,
        known: &[String],
        out: &mut SchOut,
        acc: &mut Acc,
    ) {
        let fid = sc.file;
        let f = &self.srcs[fid as usize].file;
        let line = f.instances.line[row];
        let props = Attrs::parse(f.str(f.instances.props[row]));
        let sym = &self.syms[s as usize];
        let xs_tpl = &sym.iface.template;
        assert_eq!(self.syms[s as usize].class, SymClass::Cell);
        let mut target = self.targets[s as usize].expect("cell symbols have targets");
        // An instance with netlist text of its own instantiates its variant cell, which
        // already carries any `schematic=` alternate.
        let variant = self.variants.get(&(fid, row as u32)).copied();
        if let Some(v) = variant {
            target = Target::Cell(v);
        }
        let in_devices = file_of(dir_of(&self.srcs[sym.src as usize].path)) == "devices";
        if in_devices && acc.active {
            acc.device_instances += 1;
            acc.core_instances += u32::from(matches!(target, Target::Core(_)));
        }
        if let Some(alt) = props
            .get("schematic")
            .map(str::trim)
            .filter(|v| !v.is_empty() && variant.is_none())
        {
            let resolved = match target {
                Target::Cell(c) => self.alt_source(fid, c, alt).and_then(|a| {
                    if Some(a) == self.cells[c as usize].sch {
                        Some(c)
                    } else {
                        self.alt_cells.get(&(c, a)).copied()
                    }
                }),
                Target::Core(_) => None,
            };
            match resolved {
                Some(a) => target = Target::Cell(a),
                None => acc.item(
                    fid,
                    line,
                    Class::Review(Reason::Unsupported),
                    0,
                    format!("schematic={alt}"),
                ),
            }
        }
        let name = match props.get("name").map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => dotted_bus(n),
            None => {
                acc.item(
                    fid,
                    line,
                    Class::Review(Reason::Name),
                    0,
                    "instance without name",
                );
                format!("x{line}")
            }
        };
        let cellref = match target {
            Target::Core(m) => format!("core:{}", m.cell),
            Target::Cell(c) => {
                let cl = &self.cells[c as usize];
                if cl.lib == sc.lib {
                    format!("./{}", cl.name)
                } else {
                    format!("{}:{}", self.lib_names[usize::from(cl.lib)], cl.name)
                }
            }
        };
        let o = orient(f.instances.rot[row], f.instances.flip[row]);
        let [x, y] = f.instances.xy[row];
        doc.begin_record(RecordKind::Inst);
        doc.push_field(&ids.next(line, 0));
        doc.push_field(&cellref);
        doc.push_attr("at");
        doc.push_value_coord(x);
        doc.push_value_coord(y);
        doc.push_value(o.as_str());
        doc.push_attr("name");
        doc.push_value(&name);
        let mut used: Vec<&str> = vec!["name", "schematic", "spice_ignore", "lvs_ignore", "embed"];
        match props.get("spice_ignore").map(str::trim) {
            Some(v) if v.starts_with("tcleval(") => acc.item(
                fid,
                line,
                Class::Review(Reason::Tcl),
                0,
                format!("spice_ignore={v}"),
            ),
            Some(v @ ("open" | "short")) => {
                doc.push_attr("ignore");
                doc.push_value(v);
            }
            Some(v) if truthy(v) == Some(true) => {
                doc.push_attr("ignore");
                doc.push_value("true");
            }
            _ => {}
        }
        let iface = self.iface_of(target);
        let map = match target {
            Target::Core(m) => Some(m),
            Target::Cell(_) => None,
        };
        let xs_key = |core_name: &str| -> Vec<String> {
            match map {
                Some(m) => (0..xs_tpl.len())
                    .map(|i| xs_tpl.key(i))
                    .chain((0..props.len()).map(|i| props.key(i)))
                    .filter(|k| m.param(k) == core_name)
                    .map(str::to_owned)
                    .collect(),
                None => vec![core_name.to_owned()],
            }
        };
        for (pin, default) in &iface.implicit {
            used.push(pin);
            let v = props
                .get(pin)
                .or_else(|| xs_tpl.get(pin))
                .unwrap_or(default)
                .trim();
            if v.is_empty() || v.contains(['@', ' ', '\t', '\n', '{', '[']) {
                acc.item(
                    fid,
                    line,
                    Class::Review(Reason::Expression),
                    0,
                    format!("{pin}={v}"),
                );
                continue;
            }
            doc.push_attr("inherit");
            doc.push_value(pin);
            doc.push_value(v);
        }
        for (pname, pdefault, kind) in &iface.params {
            let keys = xs_key(pname);
            let inst_v = keys.iter().find_map(|k| props.get(k));
            keys.iter()
                .filter_map(|k| props.find(k))
                .for_each(|i| used.push(props.key(i)));
            let value = match map {
                Some(_) => inst_v.or_else(|| keys.iter().find_map(|k| xs_tpl.get(k))),
                None => inst_v
                    .filter(|v| collapse_ws(v) != collapse_ws(xs_tpl.get(pname).unwrap_or(""))),
            };
            let Some(v) = value else { continue };
            match override_text(v, *kind, known, &sc.parents) {
                Ok(text) if map.is_some() && text == *pdefault => {}
                Ok(text) => {
                    doc.push_attr(pname);
                    doc.push_value(&text);
                }
                Err(r) => acc.item(
                    fid,
                    line,
                    Class::Review(r.reason),
                    0,
                    format!("{pname}={}", r.detail),
                ),
            }
        }
        // Per-instance netlist text.
        let plan = match target {
            Target::Cell(c) => self.cells[c as usize]
                .sym
                .and_then(|s| self.plans[s as usize].as_ref()),
            Target::Core(_) => None,
        };
        let sym_dm = sym.iface.attrs.get("device_model");
        // A mapped device's *template* `device_model` is not emitted: goldens whose
        // `switch_ngspice` instances set another `model` (flip_flop_ngspice, led_driver,
        // solar_panel) carry no `.MODEL SW1` from its template.
        let dm = match map {
            Some(_) => props.get("device_model").or(sym_dm),
            None => props
                .get("device_model")
                .filter(|d| Some(*d) != xs_tpl.get("device_model").or(sym_dm)),
        };
        used.push("device_model");
        if let Some(dm) = dm.filter(|d| !d.trim().is_empty()) {
            // Emitted once per netlist, like xschem's device_model table: a bench code
            // block, or the component's schematic-impl body.
            match netlist_text(dm, self.opts.pdk_vars) {
                Ok(t) if sc.bench => {
                    let t = lines(&t);
                    if !out.codes.iter().any(|c| c.2 == t) {
                        let n = unique(
                            &format!("model_{name}"),
                            out.codes.iter().map(|c| c.0.as_str()),
                        );
                        out.codes.push((n, "spice".to_owned(), t));
                    }
                }
                Ok(t) => {
                    let t = lines(&t);
                    if !out.body.windows(t.len().max(1)).any(|w| w == t.as_slice()) {
                        out.body.extend(t);
                    }
                }
                Err(d) => acc.item(fid, line, Class::Review(Reason::Tcl), 0, d),
            }
        }
        for key in ["format", "lvs_format", "spice_sym_def", "spiceprefix"] {
            // An empty value is the same as an absent one in xschem.
            let Some(v) = props.get(key).filter(|v| !v.trim().is_empty()) else {
                used.push(key);
                continue;
            };
            used.push(key);
            if variant.is_some() && INSTANCE_TEXT.contains(&key) {
                continue; // the variant cell netlists it
            }
            let sym_v = match key {
                "spiceprefix" => plan
                    .and_then(|p| p.spiceprefix.as_deref())
                    .or_else(|| xs_tpl.get(key)),
                _ => sym.iface.attrs.get(key),
            };
            let is_param = iface.params.iter().any(|p| p.0 == key);
            // spiceprefix only matters where it was folded into the template.
            let inert = key == "spiceprefix" && plan.is_none_or(|p| p.spiceprefix.is_none());
            if !is_param && !inert && sym_v.map(collapse_ws) != Some(collapse_ws(v)) {
                acc.item(
                    fid,
                    line,
                    Class::Review(Reason::InstanceText),
                    0,
                    format!("{key}={v}"),
                );
            }
        }
        for i in 0..props.len() {
            let k = props.key(i);
            if !used.contains(&k) && acc.active {
                *acc.unused.entry(k.to_owned()).or_insert(0) += 1;
            }
        }
        acc.out(fid, 1);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_label(
        &self,
        doc: &mut Document,
        ids: &mut Ids,
        sc: &SchCtx,
        row: usize,
        s: u32,
        port: bool,
        out: &mut SchOut,
        acc: &mut Acc,
    ) {
        let fid = sc.file;
        let f = &self.srcs[fid as usize].file;
        let line = f.instances.line[row];
        let props = Attrs::parse(f.str(f.instances.props[row]));
        let sym = &self.syms[s as usize];
        let lab = props
            .get("lab")
            .or_else(|| sym.iface.template.get("lab"))
            .unwrap_or("")
            .trim();
        let Some(anchor) = self.anchor(f, row, s) else {
            acc.item(
                fid,
                line,
                Class::Review(Reason::Unsupported),
                1,
                "label symbol without one pin",
            );
            return;
        };
        if lab.is_empty() || lab.contains(['@', '\n']) || lab.contains("tcleval(") {
            acc.item(
                fid,
                line,
                Class::Review(Reason::Expression),
                1,
                format!("lab={lab}"),
            );
            return;
        }
        let o = orient(f.instances.rot[row], f.instances.flip[row]);
        let bound = sc
            .pins
            .iter()
            .find(|(xs, _)| xs == lab)
            .map(|(_, p)| p.as_str());
        let is_port = port && !sc.bench && bound.is_some();
        if port && !sc.bench && !is_port {
            acc.item(
                fid,
                line,
                Class::Review(Reason::Port),
                0,
                format!("{lab} is not a symbol pin; kept as a label"),
            );
        }
        doc.begin_record(if is_port {
            RecordKind::Port
        } else {
            RecordKind::Label
        });
        doc.push_field(&ids.next(line, 0));
        doc.push_field_coord(anchor[0]);
        doc.push_field_coord(anchor[1]);
        doc.push_field(o.as_str());
        let field = match (is_port, bound) {
            (true, Some(pin)) => pin.to_owned(),
            _ => dotted_bus(lab),
        };
        doc.push_field(&field);
        let global = props
            .get("global")
            .or_else(|| sym.iface.attrs.get("global"))
            .and_then(truthy)
            .unwrap_or(false);
        if global && !is_port {
            doc.push_attr("global");
            doc.push_value("true");
        }
        if is_port {
            out.ports.push(field);
        } else {
            out.labels.push((lab.to_owned(), anchor));
        }
        acc.out(fid, 1);
    }

    fn emit_noconn(
        &self,
        doc: &mut Document,
        ids: &mut Ids,
        sc: &SchCtx,
        row: usize,
        s: u32,
        acc: &mut Acc,
    ) {
        let f = &self.srcs[sc.file as usize].file;
        let line = f.instances.line[row];
        let Some(at) = self.anchor(f, row, s) else {
            acc.item(
                sc.file,
                line,
                Class::Review(Reason::Unsupported),
                1,
                "noconn without one pin",
            );
            return;
        };
        doc.begin_record(RecordKind::Noconn);
        doc.push_field(&ids.next(line, 0));
        doc.push_field_coord(at[0]);
        doc.push_field_coord(at[1]);
        acc.out(sc.file, 1);
    }

    /// The schematic position of a one-pin symbol's pin for instance `row`.
    fn anchor(&self, f: &XschemFile, row: usize, s: u32) -> Option<[i32; 2]> {
        let pins = &self.syms[s as usize].iface.pins;
        let [p] = pins.center.as_slice() else {
            return None;
        };
        place(
            *p,
            orient(f.instances.rot[row], f.instances.flip[row]),
            f.instances.xy[row],
        )
    }

    /// Renders a netlist-text instance (`code.sym`, `simulator_commands`, pinless
    /// devices such as `k.sym`) to `(lang, text)`.
    fn render_code(&self, s: u32, props: &Attrs) -> Result<(String, String), Review> {
        let sym = &self.syms[s as usize];
        let tpl = &sym.iface.template;
        let get = |k: &str| props.get(k).or_else(|| tpl.get(k));
        let fmt = props
            .get("format")
            .or_else(|| sym.iface.attrs.get("format"))
            .unwrap_or("@value");
        let tcl_err = |detail: String| review(Reason::Tcl, detail);
        if fmt.contains("sim_is_") {
            let lang = get("simulator")
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .unwrap_or("ngspice");
            let text =
                netlist_text(get("value").unwrap_or(""), self.opts.pdk_vars).map_err(tcl_err)?;
            return Ok((lang.to_owned(), text));
        }
        let stem = stem_of(&self.srcs[sym.src as usize].path);
        let mut text = String::new();
        let mut last = 0;
        for t in at_tokens(fmt) {
            push_unescaped(&mut text, &fmt[last..t.offset as usize]);
            let value = match t.name {
                "symname" => Some(stem),
                n => get(n),
            };
            match (t.kind, value) {
                (crate::attr::AtKind::Percent, None) => text.push_str(t.name),
                (_, Some(v)) => text.push_str(v),
                _ => {}
            }
            last = t.offset as usize
                + 1
                + t.name.len()
                + usize::from(matches!(
                    t.kind,
                    crate::attr::AtKind::PinNet | crate::attr::AtKind::PinAttr
                ));
        }
        push_unescaped(&mut text, &fmt[last.min(fmt.len())..]);
        let text = netlist_text(&text, self.opts.pdk_vars).map_err(tcl_err)?;
        Ok(("spice".to_owned(), text))
    }

    /// Lowercase words of the attribute values of schematic `x`'s cell and netlist-text
    /// instances, and of its `S {}` text: the names its netlist can mention (graphs and
    /// launchers excluded).
    fn value_words(&self, x: u32) -> HashSet<String> {
        let f = &self.srcs[x as usize].file;
        let mut words = HashSet::new();
        let mut add = |text: &str| {
            words.extend(
                text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .filter(|w| !w.is_empty())
                    .map(str::to_ascii_lowercase),
            );
        };
        for row in 0..f.instances.props.len() {
            let class = self
                .resolve(x, f.str(f.instances.symref[row]))
                .and_then(|src| self.sym_of_src[src as usize])
                .map(|s| self.syms[s as usize].class);
            if matches!(class, Some(SymClass::Cell | SymClass::Code)) {
                let a = Attrs::parse(f.str(f.instances.props[row]));
                (0..a.len()).for_each(|i| add(a.value(i)));
            }
        }
        let g = &f.globals;
        for i in (0..g.tag.len()).filter(|&i| g.tag[i] == GlobalTag::S) {
            add(f.str(g.text[i]));
        }
        words
    }

    // ------------------------------------------------------------------ 7. outputs

    fn manifest(&self) -> String {
        let mut d = Document::new(FileKind::Project);
        for name in &self.lib_names {
            d.begin_record(RecordKind::Lib);
            d.push_field(name);
            d.push_field("path");
            d.push_field(name);
        }
        for (k, v) in self.opts.pdk_vars {
            d.begin_record(RecordKind::Set);
            d.push_field(k);
            d.push_field(v);
        }
        write(&d)
    }

    /// `path` relative to the shallowest search-path root that contains it (as given when
    /// none does), so a report reads the same on every machine (D32): the roots are
    /// where the checkout or PDK sits.
    fn report_path(&self, path: &str) -> String {
        let root = self
            .opts
            .roots
            .iter()
            .map(|r| normalize(r))
            .filter(|r| {
                !r.is_empty()
                    && path.len() > r.len()
                    && path.starts_with(r.as_str())
                    && path.as_bytes()[r.len()] == b'/'
            })
            .min_by_key(String::len);
        let rel = root.as_ref().map_or(path, |r| &path[r.len() + 1..]);
        assert!(root.is_none() || !rel.starts_with('/'));
        rel.to_owned()
    }

    fn finish(&self, files: &[(String, Vec<u8>)], mut acc: Acc) -> Report {
        // Symbols that became no cell: their records are carried by the records
        // their instances became (labels, ports, code) or by the core cell.
        for (si, sym) in self.syms.iter().enumerate() {
            let fid = sym.src as usize;
            if self.sym_of_src[fid] != Some(si as u32) {
                continue; // a variant's copy of a symbol: the canonical row reports it
            }
            if self.plans[si].is_some() {
                continue;
            }
            let records = records_in(&self.srcs[fid].file);
            let drop = if matches!(
                self.targets[self.sym_of_src[fid].expect("row") as usize],
                Some(Target::Core(_))
            ) {
                Drop::MappedToCore
            } else {
                Drop::NotACell
            };
            acc.item(
                sym.src,
                0,
                Class::Dropped(drop),
                records - acc.out[fid],
                format!("type={}", sym.ty),
            );
        }
        let mut outputs: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
        outputs.push("import-report.md".into());
        outputs.push("import-report.txt".into());
        outputs.sort();
        Report {
            files: self
                .srcs
                .iter()
                .map(|s| self.report_path(&s.path))
                .collect(),
            records_in: self.srcs.iter().map(|s| records_in(&s.file)).collect(),
            records_out: acc.out,
            items: acc.items,
            outputs,
            unused_attrs: acc.unused,
            device_instances: acc.device_instances,
            core_instances: acc.core_instances,
        }
    }
}

/// xschem's default number of layers (`cadlayers`); objects on higher layers are not
/// loaded by a default xschem, so they are dropped and reported.
const CAD_LAYERS: u32 = 22;

type ImplRow<'p> = (
    &'static str,
    &'static str,
    &'p [(&'static str, String)],
    Option<&'p str>,
);

/// Per-document id generator: `IdRng::for_import(seed, line·8 + sub)`, redrawn on
/// collision within the document.
struct Ids {
    seed: String,
    taken: HashSet<String>,
}

impl Ids {
    fn next(&mut self, line: u32, sub: u32) -> String {
        let mut rng = IdRng::for_import(&self.seed, line.wrapping_mul(8).wrapping_add(sub));
        let id = fresh_id(&mut rng, |c| self.taken.contains(c))
            .as_str()
            .to_owned();
        let fresh = self.taken.insert(id.clone());
        assert!(fresh);
        id
    }
}

struct SchCtx {
    file: u32,
    lib: u16,
    bench: bool,
    /// The cell's parameters (empty for a bench): overrides referencing them become
    /// expressions or holes evaluated in this cell's scope.
    parents: Vec<String>,
    /// Interface pin names (incl. inherited) a port may bind.
    pins: Vec<(String, String)>,
}

#[derive(Default)]
struct SchOut {
    vars: Vec<(String, String)>,
    /// Bench-copy variables taken from the component's defaults: a bench `.param` of
    /// the same name replaces them (that is the value xschem's netlist defines).
    default_vars: Vec<String>,
    codes: Vec<(String, String, Vec<String>)>,
    ports: Vec<String>,
    labels: Vec<(String, [i32; 2])>,
    /// Model text for the schematic impl's `body` (component instances' device_model).
    body: Vec<String>,
    bbox: Option<[i32; 2]>,
}

impl SchOut {
    fn extend_bbox(&mut self, p: [i32; 2]) {
        self.bbox = Some(match self.bbox {
            Some([x, y]) => [x.min(p[0]), y.min(p[1])],
            None => p,
        });
    }

    fn bbox_min(&self) -> [i32; 2] {
        let [x, y] = self.bbox.unwrap_or([0, 0]);
        [x.saturating_sub(100_000), y.saturating_sub(100_000)]
    }
}

fn records_in(f: &XschemFile) -> u32 {
    f.records_read + f.embedded.iter().map(|e| e.body.records_read).sum::<u32>()
}

/// Tallies the records every file kind treats alike: header records, comments,
/// malformed records, embedded symbols and roundings.
fn tally_common(f: &XschemFile, fid: u32, acc: &mut Acc, schematic: bool) {
    let g = &f.globals;
    for i in 0..g.tag.len() {
        let text = f.str(g.text[i]).trim();
        let other = match g.tag[i] {
            GlobalTag::Version | GlobalTag::K => false,
            GlobalTag::G => schematic, // VHDL text in a schematic; symbol attributes in a symbol
            GlobalTag::S => continue,  // the schematic pass decides; in a symbol: below
            GlobalTag::V | GlobalTag::E => true,
        };
        if other && !text.is_empty() {
            acc.item(
                fid,
                g.line[i],
                Class::Dropped(Drop::OtherNetlister),
                1,
                text,
            );
        } else {
            acc.out(fid, 1);
        }
    }
    if !schematic {
        for i in (0..g.tag.len()).filter(|&i| g.tag[i] == GlobalTag::S) {
            let text = f.str(g.text[i]).trim();
            if text.is_empty() {
                acc.out(fid, 1);
            } else {
                acc.item(
                    fid,
                    g.line[i],
                    Class::Dropped(Drop::OtherNetlister),
                    1,
                    text,
                );
            }
        }
    }
    acc.out(fid, f.comments);
    for is in &f.issues {
        acc.item(
            fid,
            is.line,
            Class::Dropped(Drop::Malformed),
            1,
            format!("{:?} ({})", is.kind, is.tag as char),
        );
    }
    for e in &f.embedded {
        acc.item(
            fid,
            e.line,
            Class::Dropped(Drop::Embedded),
            1 + e.body.records_read,
            "embedded symbol body",
        );
    }
    for r in &f.roundings {
        let what = match r.field {
            NumField::Coord => "coordinate",
            NumField::Scale => "text scale",
            NumField::Angle => "arc angle",
        };
        acc.item(
            fid,
            r.line,
            Class::Rounding,
            0,
            format!(
                "{what} col {} → {} milli (error {}/1000 milli)",
                r.col, r.milli, r.error_micro
            ),
        );
    }
}

fn push_layer(doc: &mut Document, layer: u8) {
    doc.push_attr("layer");
    doc.push_value(&layer.to_string());
}

fn emit_interface(doc: &mut Document, plan: &Plan, impls: &[ImplRow]) {
    for p in &plan.pins {
        doc.begin_record(RecordKind::Pin);
        doc.push_field(&p.name);
        doc.push_field(p.dir);
        doc.push_field_coord(p.xy[0]);
        doc.push_field_coord(p.xy[1]);
        if let Some(net) = &p.inherit {
            doc.push_attr("inherit");
            doc.push_value(net);
        }
        if let Some(target) = p
            .alias
            .then(|| plan.pins.iter().find(|q| !q.alias && q.xs == p.xs))
            .flatten()
        {
            doc.push_attr("alias");
            doc.push_value(&target.name);
        }
    }
    for p in &plan.params {
        doc.begin_record(RecordKind::Param);
        doc.push_field(&p.name);
        doc.push_field(&p.default);
        if let Some(k) = p.kind.word() {
            doc.push_attr("kind");
            doc.push_value(k);
        }
        if !p.choices.is_empty() {
            doc.push_attr("choices");
            p.choices.iter().for_each(|c| doc.push_value(c));
        }
        for (k, v) in &p.attrs {
            doc.push_attr(k);
            doc.push_value(v);
        }
    }
    for (name, lang, formats, ignore) in impls {
        doc.begin_record(RecordKind::Impl);
        doc.push_field(name);
        doc.push_field(lang);
        for (dialect, tpl) in formats.iter() {
            doc.push_attr("format");
            doc.push_value(dialect);
            doc.push_value(tpl);
        }
        if let Some(v) = ignore {
            doc.push_attr("ignore");
            doc.push_value(v);
        }
    }
}

/// A symbol text → `stext` template, or `None` for display-only text (OP annotation,
/// Tcl, pin attributes). Tokens that are not parameters, `name` or `cell` render
/// empty, as in xschem.
fn stext_template(text: &str, params: &[String], spiceprefix: Option<&str>) -> Option<String> {
    let display = [
        "tcleval(",
        "@spice_get",
        "@#",
        "ngspice::",
        "@path",
        "@schname",
    ];
    if text.trim().is_empty() || display.iter().any(|d| text.contains(d)) {
        return None;
    }
    let ctx = FormatCtx {
        params,
        pins: &[],
        implicit: &[],
        spiceprefix: Some(spiceprefix.unwrap_or("")),
        schematic: false,
        rename: &[],
    };
    let c = template::convert_format(text, &ctx).ok()?;
    let mut t = c.text;
    for u in c.unknown.iter().chain(["pins".to_owned()].iter()) {
        t = t.replace(&format!("{{{u}}}"), "");
    }
    let parsed = schemify_expr::Template::parse(&t).ok()?;
    let ok = parsed
        .program()
        .idents()
        .all(|i| i == "name" || i == "cell" || params.iter().any(|p| p == i));
    (ok && !t.trim().is_empty()).then_some(t)
}

/// An instance override value → the text stored for a parameter of `kind`.
fn override_text(
    v: &str,
    kind: Kind,
    known: &[String],
    parents: &[String],
) -> Result<String, Review> {
    let t = v.trim();
    let fail = |reason| review(reason, t);
    if t.contains("tcleval(") {
        return Err(fail(Reason::Tcl));
    }
    match kind {
        Kind::Number => {
            if let Some(n) = spice_number(t) {
                return Ok(n);
            }
            let inner = t
                .strip_prefix('\'')
                .and_then(|x| x.strip_suffix('\''))
                .or_else(|| t.strip_prefix('{').and_then(|x| x.strip_suffix('}')))
                .unwrap_or(t);
            let e = template::translate_expr(inner, ExprMode::Spice, known)
                .ok_or_else(|| fail(Reason::Expression))?;
            debug_assert!(schemify_expr::Program::compile(&e, schemify_expr::Mode::Scalar).is_ok());
            Ok(e)
        }
        Kind::Bool => truthy(t)
            .map(|b| b.to_string())
            .ok_or_else(|| fail(Reason::Expression)),
        Kind::Str | Kind::Choice => Ok(string_value(v, parents)),
    }
}

/// Moves closed `.param name=value …` lines (outside `.control`) into bench variables
/// when every value translates and only references names defined the same way.
fn extract_params(
    text: &mut Vec<String>,
    vars: &mut Vec<(String, String)>,
    defaults: &mut Vec<String>,
) {
    let mut keep = Vec::with_capacity(text.len());
    let mut control = false;
    for (k, l) in text.iter().enumerate() {
        let low = l.trim().to_ascii_lowercase();
        if low.starts_with(".control") {
            control = true;
        } else if low.starts_with(".endc") {
            control = false;
        }
        let continued = text
            .get(k + 1)
            .is_some_and(|n| n.trim_start().starts_with('+'));
        if control || continued || !low.starts_with(".param ") {
            keep.push(l.clone());
            continue;
        }
        // Component defaults give way to the bench's own definition.
        let replaced: Vec<(String, String)> = vars
            .iter()
            .filter(|v| !defaults.iter().any(|d| d.eq_ignore_ascii_case(&v.0)))
            .cloned()
            .collect();
        match param_pairs(l.trim()[6..].trim(), &replaced, vars) {
            Some(pairs) => {
                vars.retain(|v| !pairs.iter().any(|p| p.0.eq_ignore_ascii_case(&v.0)));
                defaults.retain(|d| !pairs.iter().any(|p| p.0.eq_ignore_ascii_case(d)));
                vars.extend(pairs);
            }
            None => keep.push(l.clone()),
        }
    }
    assert!(
        (1..vars.len()).all(|i| !vars[..i].iter().any(|v| v.0 == vars[i].0)),
        "bench variables are unique"
    );
    *text = keep;
}

/// The `k=v` pairs of one `.param` line, values translated; `None` unless every value
/// translates and names only `known` variables or earlier pairs. A name in `fixed`
/// (or repeated in the line) cannot be redefined.
fn param_pairs(
    s: &str,
    fixed: &[(String, String)],
    known: &[(String, String)],
) -> Option<Vec<(String, String)>> {
    let words = spice::words(s);
    let mut out: Vec<(String, String)> = Vec::with_capacity(words.len() / 3);
    for w in words.chunks(3) {
        let [name, "=", value] = w else { return None };
        if !is_ident(name) || !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return None;
        }
        let value = value
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .or_else(|| value.strip_prefix('{').and_then(|v| v.strip_suffix('}')))
            .unwrap_or(value);
        out.push(((*name).to_owned(), value.to_owned()));
    }
    let mut names: Vec<String> = known.iter().map(|v| v.0.clone()).collect();
    names.extend(out.iter().map(|p| p.0.clone()));
    let mut res = Vec::with_capacity(out.len());
    for (n, v) in out {
        if fixed.iter().any(|x| x.0.eq_ignore_ascii_case(&n))
            || res
                .iter()
                .any(|x: &(String, String)| x.0.eq_ignore_ascii_case(&n))
        {
            return None;
        }
        let e = template::translate_expr(&v, ExprMode::Spice, &names)?;
        let p = schemify_expr::Program::compile(&e, schemify_expr::Mode::Scalar).ok()?;
        if !p.idents().all(|id| names.iter().any(|n| n == id)) {
            return None;
        }
        res.push((n, e));
    }
    (!res.is_empty()).then_some(res)
}

/// The inherited pins of a symbol (spec §2): `extra=` nets in `extra` order, then the
/// `@body` idiom, each an identifier with a template value that `format` or `lvs_format`
/// uses as a standalone `@x` word, with its default net (`0` when the value is empty).
/// Goldens append extra pins to `.subckt` lines in that order.
fn implicit_nets(iface: &SymbolInterface) -> Vec<(String, String)> {
    let (attrs, tpl) = (&iface.attrs, &iface.template);
    let (fmt, lvs) = (attrs.get("format"), attrs.get("lvs_format"));
    let standalone = |f: Option<&str>, k: &str| {
        f.is_some_and(|f| f.split_whitespace().any(|w| w.strip_prefix('@') == Some(k)))
    };
    let mut implicit: Vec<(String, String)> = Vec::new();
    for k in attrs
        .get("extra")
        .unwrap_or("")
        .split_whitespace()
        .chain(["body"])
    {
        let Some(net) = tpl.get(k) else { continue };
        let net_param = standalone(fmt, k) || standalone(lvs, k);
        if net_param && is_ident(k) && !implicit.iter().any(|(n, _)| n == k) {
            let net = net.trim();
            let net = if net.is_empty() { "0" } else { net };
            implicit.push((k.to_owned(), net.to_owned()));
        }
    }
    assert!(implicit.iter().all(|(k, n)| !k.is_empty() && !n.is_empty()));
    implicit
}

/// Whether instance `row` of `f` sets `only_toplevel=true`.
fn top_only(f: &XschemFile, row: usize) -> bool {
    Attrs::parse(f.str(f.instances.props[row]))
        .get("only_toplevel")
        .and_then(truthy)
        == Some(true)
}

/// xschem's bracket-less bus form in a net or instance name: `A[3..0]` is the nets
/// `A3,A2,A1,A0` and `A[6..0..2]` steps by 2 (research: xschem-feature-inventory, bus
/// syntax; goldens name `G2[9..0]`'s devices `G29`…`G20`), as a Schemify bundle. Any
/// other text is returned unchanged.
pub(crate) fn dotted_bus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']').map(|c| open + c) else {
            break;
        };
        let base_start = rest[..open]
            .rfind(|c: char| c == ',' || c == '*' || c == ']' || c.is_whitespace())
            .map_or(0, |i| i + 1);
        let nums: Vec<i64> = rest[open + 1..close]
            .split("..")
            .map(|n| n.trim().parse::<i64>())
            .collect::<Result<_, _>>()
            .unwrap_or_default();
        let (from, to, step) = match nums[..] {
            [a, b] => (a, b, 1),
            [a, b, c] if c > 0 => (a, b, c),
            _ => {
                out.push_str(&rest[..=close]);
                rest = &rest[close + 1..];
                continue;
            }
        };
        let base = &rest[base_start..open];
        out.push_str(&rest[..base_start]);
        let dir = if to >= from { 1 } else { -1 };
        let count = ((to - from).abs() / step + 1).min(65_536);
        let bits: Vec<String> = (0..count)
            .map(|k| format!("{base}{}", from + dir * k * step))
            .collect();
        out.push_str(&bits.join(","));
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

fn unique<'a>(base: &str, taken: impl Iterator<Item = &'a str> + Clone) -> String {
    let mut name = base.to_owned();
    let mut k = 2;
    while taken.clone().any(|t| t == name) {
        name = format!("{base}_{k}");
        k += 1;
    }
    name
}

/// Appends `s` with `\x` → `x` (attribute-level escapes in netlist text).
fn push_unescaped(out: &mut String, s: &str) {
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
}

fn iface_of_plan(p: &Plan) -> Iface {
    Iface {
        pins: p
            .pins
            .iter()
            .filter(|q| q.inherit.is_none())
            .map(|q| (q.name.clone(), q.xy))
            .collect(),
        implicit: p
            .pins
            .iter()
            .filter_map(|q| q.inherit.as_ref().map(|n| (q.name.clone(), n.clone())))
            .collect(),
        params: p
            .params
            .iter()
            .map(|q| (q.name.clone(), q.default.clone(), q.kind))
            .collect(),
    }
}

/// Infers a parameter's kind and default text from its xschem template value.
fn infer(v: &str, avail: &[&str], keys: &[String]) -> (Kind, String, Option<Review>) {
    let t = v.trim();
    if t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
        return (Kind::Bool, t.to_ascii_lowercase(), None);
    }
    if let Some(n) = spice_number(t) {
        return (Kind::Number, n, None);
    }
    if let Some(body) = t
        .strip_prefix("expr(")
        .or_else(|| t.strip_prefix("expr_eng("))
        .or_else(|| t.strip_prefix("expr_eng4("))
        .and_then(|b| b.strip_suffix(')'))
    {
        let body = body.trim();
        let body = body
            .strip_prefix('\'')
            .and_then(|b| b.strip_suffix('\''))
            .unwrap_or(body);
        return match template::translate_expr(body, ExprMode::Xschem, keys) {
            Some(e) => (Kind::Number, e, None),
            None => (
                Kind::Str,
                String::new(),
                Some(review(Reason::Expression, t.to_owned())),
            ),
        };
    }
    if t.starts_with("tcleval(") {
        return (
            Kind::Str,
            String::new(),
            Some(review(Reason::Tcl, t.to_owned())),
        );
    }
    if !avail.is_empty() && avail.contains(&t) {
        return (Kind::Choice, t.to_owned(), None);
    }
    (Kind::Str, literal(t), None)
}

fn apply_cdf(cdf: &CdfDump, cell: &str, params: &mut [ParamOut], reviews: &mut Vec<Review>) {
    let Some(c) = (0..cdf.cells.cell.len()).find(|&c| cdf.str(cdf.cells.cell[c]) == cell) else {
        return;
    };
    for p in (0..cdf.params.name.len()).filter(|&p| cdf.params.cell[p] as usize == c) {
        let name = cdf.str(cdf.params.name[p]);
        let Some(out) = params.iter_mut().find(|q| q.name == name) else {
            continue;
        };
        if matches!(cdf.params.ty[p], CdfType::Radio | CdfType::Cyclic) && out.kind == Kind::Str {
            out.kind = Kind::Choice;
            out.choices = cdf.choices_of(p).map(str::to_owned).collect();
        }
        let callback = cdf.str(cdf.params.callback[p]);
        if !callback.trim().is_empty() {
            reviews.push(review(
                Reason::Tcl,
                format!("CDF callback on {name}: {callback}"),
            ));
        }
    }
}

/// Netlist text that may be `tcleval(…)`-wrapped → code/body text: PDK variables as
/// `${VAR}`, every literal `$` doubled (netlist spec §7.1 expansion syntax).
fn netlist_text(text: &str, vars: &[(String, String)]) -> Result<String, String> {
    let t = text.trim();
    match t.strip_prefix("tcleval(").and_then(|b| b.strip_suffix(')')) {
        Some(inner) => tcl::subst(inner, vars).or_else(|e| {
            // Tcl inside SPICE comment lines (`* path: $path`, `[xschem get …]`) has no
            // netlist effect: drop those lines rather than the whole block.
            let tcl_comment = |l: &&str| l.trim_start().starts_with('*') && l.contains(['[', '$']);
            if !inner.lines().any(|l| tcl_comment(&l)) {
                return Err(e);
            }
            let kept: Vec<&str> = inner.lines().filter(|l| !tcl_comment(l)).collect();
            tcl::subst(&kept.join("\n"), vars)
        }),
        None => Ok(text.replace('$', "$$")),
    }
}

/// Splits text into verbatim lines (no CR/LF), dropping leading and trailing blank lines.
fn lines(text: &str) -> Vec<String> {
    let v: Vec<String> = text
        .lines()
        .map(|l| l.trim_end_matches('\r').replace(['\r', '\0'], ""))
        .collect();
    let start = v
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(v.len());
    let end = v
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(start, |e| e + 1);
    v[start..end].to_vec()
}

/// The bit names of a pin name: `A[2:0]` → `A[2] A[1] A[0]`, `A[3]`, a bundle `a,b`,
/// else the name itself.
fn pin_bits(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for t in name.split(',') {
        let range = t
            .strip_suffix(']')
            .and_then(|r| r.split_once('['))
            .and_then(|(base, r)| {
                let (a, b) = r.split_once(':').unwrap_or((r, r));
                Some((
                    base,
                    a.trim().parse::<i64>().ok()?,
                    b.trim().parse::<i64>().ok()?,
                ))
            });
        match range {
            Some((base, a, b)) if a.abs_diff(b) < 65_536 => {
                let step = if b >= a { 1 } else { -1 };
                let mut i = a;
                loop {
                    out.push(format!("{base}[{i}]"));
                    if i == b {
                        break;
                    }
                    i += step;
                }
            }
            _ => out.push(t.to_owned()),
        }
    }
    out
}

/// The `@pinlist` replacement `{P} {Q} …` ordering the ordinary pins like `ports` (a
/// `.subckt` line's bit names, matched case-insensitively): each pin's bits must appear
/// together and in its own order, and every pin exactly once. `None` otherwise.
fn pin_order(pins: &[PinOut], ports: &[String]) -> Option<String> {
    let ordinary: Vec<&PinOut> = pins
        .iter()
        .filter(|p| p.inherit.is_none() && !p.alias)
        .collect();
    let bits: Vec<Vec<String>> = ordinary.iter().map(|p| pin_bits(&p.name)).collect();
    let mut used = vec![false; ordinary.len()];
    let mut holes = String::new();
    let mut i = 0;
    while i < ports.len() {
        let rest = &ports[i..];
        let k = (0..ordinary.len()).find(|&k| {
            !used[k]
                && bits[k].len() <= rest.len()
                && bits[k]
                    .iter()
                    .zip(rest)
                    .all(|(b, p)| b.eq_ignore_ascii_case(p))
        })?;
        let base = ordinary[k].name.split('[').next().unwrap_or("");
        if !is_ident(base) {
            return None;
        }
        used[k] = true;
        if !holes.is_empty() {
            holes.push(' ');
        }
        holes.push('{');
        holes.push_str(base);
        holes.push('}');
        i += bits[k].len();
    }
    assert!(i == ports.len());
    used.iter().all(|u| *u).then_some(holes)
}

#[cfg(test)]
mod tests {
    use super::{PinOut, dotted_bus, pin_order};
    use crate::spice::subckt_lines;

    fn pin(name: &str) -> PinOut {
        PinOut {
            name: name.to_owned(),
            xs: name.to_owned(),
            dir: "inout",
            xy: [0, 0],
            inherit: None,
            alias: false,
        }
    }

    #[test]
    fn subckt_ports_order_the_pinlist() {
        // golden tb_symbol_include: `.subckt symbol_include z y[5] … VCC VSS A[2] … B C`.
        let text = "* a file\n.subckt symbol_include z y[5] y[4] y[3] y[2]\n+ VCC VSS\n* note\n+ A[2] A[1] A[0] B C W=10 L=1\n...\n.ends\n";
        let found = subckt_lines(text);
        assert_eq!(found.len(), 1);
        let (name, ports) = &found[0];
        assert_eq!(name, "symbol_include");
        assert_eq!(ports.len(), 12);
        let pins: Vec<PinOut> = ["A[2:0]", "Z", "B", "Y[5:2]", "C", "vcc", "VSS"]
            .into_iter()
            .map(pin)
            .collect();
        assert_eq!(
            pin_order(&pins, ports).as_deref(),
            Some("{Z} {Y} {vcc} {VSS} {A} {B} {C}")
        );
        // Negative space: a bus whose bits are split, a missing pin, an unknown port.
        let split: Vec<String> = ["z", "y[5]", "VCC", "y[4]"].map(String::from).to_vec();
        assert_eq!(pin_order(&pins, &split), None);
        assert_eq!(pin_order(&pins[..2], &ports[..1]), None);
        let unknown: Vec<String> = ["q"].map(String::from).to_vec();
        assert_eq!(pin_order(&pins[1..2], &unknown), None);
        assert!(subckt_lines("R1 a b 1k\n.ends\n").is_empty());
        // An inline comment is no port.
        assert_eq!(subckt_lines(".subckt x a b ; c\n")[0].1, ["a", "b"]);
    }

    #[test]
    fn dotted_buses_become_bundles() {
        assert_eq!(dotted_bus("G2[9..7]"), "G29,G28,G27");
        assert_eq!(dotted_bus("~D[1..3]"), "~D1,~D2,~D3");
        assert_eq!(dotted_bus("A[6..0..2],B"), "A6,A4,A2,A0,B");
        // Untouched: colon ranges, single bits, malformed ranges.
        for s in ["A[3:0]", "A[3]", "A[x..1]", "A[3..", "plain"] {
            assert_eq!(dotted_bus(s), s);
        }
    }
}
