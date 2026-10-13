//! Schemify → xschem `.sch`/`.sym` export (spec `interop.md` §11).
//!
//! Spine ([`export`]), one line per transform:
//! 1. `closure`: the cells reachable from the tops through instance references.
//! 2. `target_of` per reachable cell: written here, an xschem `devices/` symbol (a `core`
//!    cell, by inverting [`DEVICE_MAP`]), a global label symbol (`core:gnd`/`core:vdd`), or
//!    the original xschem symbol (a cell whose provenance is xschem).
//! 3. per written cell: `write_symbol` (`.shprim`, `.shcomp`) and `write_schematic`
//!    (`.shcomp`, `.shbench`).
//! 4. `xschemrc`: the fragment that puts the output directory on `XSCHEM_LIBRARY_PATH`.
//!
//! The translations at the end of the file are tables of spellings: values (§3.4, xschem
//! paragraph), templates (§3.5, import.md §4 inverted), orientations (import.md §3.1
//! inverted). Every construct without an xschem spelling becomes a [`Notes`] row and is
//! left at the symbol default; nothing is dropped silently.
//!
//! Data: in = a loaded [`Design`] (≤ 2^16 cells, each its own `Document`); out = one or
//! two text files per written cell. Every transform is one sequential pass over one cell's
//! records; everything lives for one call. Per-cell work is independent, but at a few
//! kilobytes per cell it stays sequential.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;

use schemify_format::{FileKind, Orient, RecordKind, format_coord, parse_coord};

use crate::core::{DEVICE_MAP, DeviceMap};
use crate::design::{Cell, Design, SymPins, sym_pins};
use crate::notes::Notes;
use crate::translate::ORIENT_TABLE;

/// The files of one export and what did not carry over.
#[derive(Clone, Debug, Default)]
pub struct XschemExport {
    /// `(path, bytes)` sorted by path: `<lib>/<cell>.sym`, `<lib>/<cell>.sch` and
    /// `xschemrc`, paths relative to the output directory.
    pub files: Vec<(String, Vec<u8>)>,
    /// One row per construct that has no xschem spelling (spec §12 classes).
    pub notes: Notes,
}

/// Output directory of cells in the project root (they have no library name); `_2`, `_3`, …
/// is appended while a library has the same name.
pub const ROOT_DIR: &str = "root";

/// Output directory of `core` cells that no xschem `devices/` symbol stands for.
pub const CORE_DIR: &str = "schemify_core";

/// The version record every written file starts with (xschem 3.4.7's file format).
const VERSION: &str = "v {xschem version=3.4.7 file_version=1.2}\n";

/// Nesting bound for inlining vars and computed params (a cycle is a load error already).
const INLINE_DEPTH_MAX: u32 = 16;

/// Functions a SPICE-quoted `'…'` value may call (ngspice's expression functions).
const SPICE_FUNCS: &[&str] = &[
    "abs", "sqrt", "exp", "ln", "log10", "sin", "cos", "tan", "asin", "acos", "atan", "sinh",
    "cosh", "tanh", "floor", "ceil", "int", "min", "max", "pow",
];

/// What an instance of a cell points at in xschem.
#[derive(Clone, Debug)]
enum Target {
    /// Written by this export into this output directory.
    Own(String),
    /// A `core` cell standing for an xschem `devices/` symbol.
    Device(&'static DeviceMap),
    /// `core:gnd`/`core:vdd`: xschem's global label symbol of that name.
    Label(&'static str),
    /// A cell imported from xschem: its original symbol reference.
    Original(String),
}

/// Exports cells `tops` (indices into `d.cells`) and every cell they instantiate as
/// xschem files.
///
/// Pure and deterministic: the same Design gives byte-identical files. `core` cells are
/// never written when an xschem `devices/` symbol stands for them; cells imported from
/// xschem (header `provenance xschem …`) are referenced by their original symbol unless
/// they are a top.
///
/// # Errors
///
/// No tops, an index outside `d.cells`, or a `core` cell named as a top: one line each.
pub fn export(d: &Design, tops: &[usize]) -> Result<XschemExport, Vec<String>> {
    let mut problems = Vec::new();
    if tops.is_empty() {
        problems.push("no cell to export".to_owned());
    }
    for &t in tops {
        match d.cells.get(t) {
            None => problems.push(format!("top {t}: no such cell")),
            Some(c) if c.lib == "core" => problems.push(format!("{}: a core cell", c.path)),
            Some(_) => {}
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let live = closure(d, tops);
    let mut root_dir = ROOT_DIR.to_owned();
    for k in 2..d.libs.len() + 3 {
        if !d.libs.contains_key(&root_dir) {
            break;
        }
        root_dir = format!("{ROOT_DIR}_{k}");
    }
    assert!(
        !d.libs.contains_key(&root_dir),
        "a free root directory name"
    );
    let targets: Vec<Option<Target>> = (0..d.cells.len())
        .map(|ci| live[ci].then(|| target_of(d, ci, tops.contains(&ci), &root_dir)))
        .collect();
    let mut notes = Notes::default();
    let mut files = Vec::new();
    let mut dirs = BTreeSet::new();
    for (ci, t) in targets.iter().enumerate() {
        let Some(Target::Own(dir)) = t else { continue };
        let c = &d.cells[ci];
        dirs.insert(dir.as_str());
        let sc = Scope::of(c, &mut notes);
        lost_attrs(c, &mut notes);
        if c.kind() != FileKind::Shbench {
            let text = write_symbol(c, &sc, &mut notes);
            files.push((format!("{dir}/{}.sym", c.name), text.into_bytes()));
        }
        if c.kind() != FileKind::Shprim {
            let text = write_schematic(d, ci, &targets, &sc, &mut notes);
            files.push((format!("{dir}/{}.sch", c.name), text.into_bytes()));
        }
    }
    files.push(("xschemrc".to_owned(), xschemrc(&dirs).into_bytes()));
    files.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(
        files.windows(2).all(|w| w[0].0 != w[1].0),
        "output paths are unique"
    );
    assert!(
        files.len() > tops.len(),
        "every top writes at least one file"
    );
    Ok(XschemExport { files, notes })
}

/// Marks every cell reachable from `tops`.
fn closure(d: &Design, tops: &[usize]) -> Vec<bool> {
    let mut live = vec![false; d.cells.len()];
    let mut queue: Vec<usize> = tops.to_vec();
    while let Some(ci) = queue.pop() {
        if std::mem::replace(&mut live[ci], true) {
            continue;
        }
        let c = &d.cells[ci];
        for r in c.records(RecordKind::Inst) {
            if let Some(m) = d.resolve(ci, c.field(r, 1)) {
                queue.push(m);
            }
        }
    }
    assert!(tops.iter().all(|&t| live[t]));
    live
}

fn target_of(d: &Design, ci: usize, top: bool, root_dir: &str) -> Target {
    let c = &d.cells[ci];
    if c.lib == "core" {
        return match c.name.as_str() {
            "gnd" => Target::Label("gnd.sym"),
            "vdd" => Target::Label("vdd.sym"),
            name => match DEVICE_MAP.iter().find(|m| m.cell == name) {
                Some(m) => Target::Device(m),
                None => Target::Own(CORE_DIR.to_owned()),
            },
        };
    }
    if let Some(p) = c
        .header("provenance")
        .and_then(|p| p.strip_prefix("xschem "))
        && !top
    {
        // ponytail: the importer names a library after its directory, so `<lib>/<file>`
        // is the reference relative to the library's parent; `devices` is on xschem's
        // default path itself. A library imported from elsewhere needs its parent on the
        // user's XSCHEM_LIBRARY_PATH.
        let r = p.strip_prefix("devices/").unwrap_or(p);
        return Target::Original(r.to_owned());
    }
    match c.lib.as_str() {
        "" => Target::Own(root_dir.to_owned()),
        lib => Target::Own(lib.to_owned()),
    }
}

// ---------------------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------------------

/// One `param` record.
struct Param<'a> {
    name: &'a str,
    default: &'a str,
    kind: &'a str,
    computed: Option<&'a str>,
}

fn params(c: &Cell) -> Vec<Param<'_>> {
    c.records(RecordKind::Param)
        .map(|r| Param {
            name: c.field(r, 0),
            default: c.field(r, 1),
            kind: c.attr(r, "kind").unwrap_or("number"),
            computed: c.attr(r, "computed"),
        })
        .collect()
}

/// The default impl record of `c` (header `default`, else the first impl).
fn default_impl(c: &Cell) -> Option<u32> {
    let want = c.header("default");
    let mut impls = c.records(RecordKind::Impl);
    match want {
        Some(w) => impls.find(|&r| c.field(r, 0) == w),
        None => impls.next(),
    }
}

/// True when instances of `m` netlist through Schemify's default template
/// `X{name} {pins} {cell}` (netlist.md §7.1): a schematic or SPICE-text default impl
/// without a `format spice`.
fn default_call(m: &Cell) -> bool {
    default_impl(m).is_some_and(|i| {
        matches!(m.field(i, 1), "schematic" | "spice")
            && !formats(m, i).iter().any(|f| f.0 == "spice")
    })
}

/// Formats of impl `r`: `(dialect, template)` in file order.
fn formats(c: &Cell, r: u32) -> Vec<(&str, &str)> {
    c.doc
        .attrs(r)
        .filter(|&a| c.doc.attr_key(a) == "format")
        .map(|a| {
            (
                c.doc.attr_value(a, 0).unwrap_or(""),
                c.doc.attr_value(a, 1).unwrap_or(""),
            )
        })
        .collect()
}

/// Writes the `.sym` of a `.shprim` or `.shcomp`.
fn write_symbol(c: &Cell, sc: &Scope, notes: &mut Notes) -> String {
    assert!(matches!(c.kind(), FileKind::Shprim | FileKind::Shcomp));
    let ps = params(c);
    let pins = sym_pins(c);
    let imp = default_impl(c);
    let lang = imp.map_or("", |r| c.field(r, 1));
    let subckt = lang == "schematic";
    let ctx = fmt_ctx(c, &pins);
    let xname = |p: usize| pins.alias[p].as_deref().unwrap_or(&pins.name[p]);
    let mut k: Vec<String> = vec![kv("type", if subckt { "subcircuit" } else { "primitive" })];
    // Netlist formats of the default impl.
    let mut spice = None;
    if let Some(r) = imp {
        for (dialect, tpl) in formats(c, r) {
            let key = match dialect {
                "spice" => "format",
                "cdl" => "lvs_format",
                "spectre" => "spectre_format",
                "verilog" => "verilog_format",
                other => {
                    notes.review("format", &c.path, format!("format {other}: no xschem key"));
                    continue;
                }
            };
            match convert_format(tpl, &ctx) {
                Ok(f) => {
                    if key == "format" {
                        spice = Some(f);
                    } else {
                        k.push(kv(key, &f));
                    }
                }
                Err(why) => notes.review("template", &c.path, format!("format {dialect}: {why}")),
            }
        }
    }
    if spice.is_none() && default_call(c) {
        // Schemify's default template `X{name} {pins} {cell}`; the `X` is the instance
        // name's (`instance` renames calls that lack it).
        let mut f = "@name @pinlist".to_owned();
        for p in &ctx.inherited {
            write!(f, " @{p}").unwrap();
        }
        f.push_str(" @symname");
        spice = Some(f);
    }
    if let (true, Some(f)) = (subckt, spice.as_mut()) {
        // Every parameter passed by name (the importer strips the `k=@k` pairs again).
        for p in &ctx.params {
            write!(f, " {p}=@{p}").unwrap();
        }
    } else if spice.is_none() && imp.is_some() {
        notes.review(
            "format",
            &c.path,
            format!("impl {lang}: no spice format, xschem will not netlist it"),
        );
    }
    if let Some(f) = spice {
        k.insert(1, kv("format", &f));
    }
    // Template: instance-name prefix, parameter defaults, inherited-pin default nets.
    let prefix = c.header("prefix").unwrap_or(if subckt { "x" } else { "U" });
    let mut tpl = vec![kv("name", &format!("{prefix}1"))];
    for p in ps.iter().filter(|p| p.computed.is_none()) {
        match xschem_value(p.kind, p.default, sc, ctx.raw.contains(&p.name)) {
            Ok(v) => tpl.push(kv(p.name, &v)),
            Err(why) => notes.review(
                why,
                &c.path,
                format!("param {} default {:?}: left empty", p.name, p.default),
            ),
        }
    }
    for p in 0..pins.name.len() {
        if let Some(net) = &pins.inherit[p] {
            tpl.push(kv(&pins.name[p], net));
        }
    }
    k.push(kv("template", &tpl.join(" ")));
    if !ctx.inherited.is_empty() {
        k.push(kv("extra", &ctx.inherited.join(" ")));
    }
    if let Some(r) = imp {
        if let Some(ig) = c.attr(r, "ignore") {
            k.push(kv("spice_ignore", ig));
        }
        if c.attr(r, "file").is_some() {
            notes.review(
                "format",
                &c.path,
                "impl file: not carried (xschem has no source file)",
            );
        }
        let name = c.field(r, 0);
        if let Some(b) = c.records(RecordKind::Body).find(|&b| c.field(b, 0) == name) {
            if lang == "spice" || lang == "schematic" {
                let text: Vec<&str> = c.doc.verbatim(b).collect();
                k.push(kv("device_model", &text.join("\n")));
            } else {
                notes.review(
                    "format",
                    &c.path,
                    format!("body {name} ({lang}): not carried"),
                );
            }
        }
    }
    let other_impls = c.records(RecordKind::Impl).count().saturating_sub(1);
    if other_impls > 0 {
        notes.review(
            "format",
            &c.path,
            format!("{other_impls} non-default impls: only the default one is exported"),
        );
    }
    let mut out = String::from(VERSION);
    out.push_str("G {}\n");
    writeln!(out, "K {}", brace(&k.join("\n"))).unwrap();
    out.push_str("V {}\nS {}\nE {}\n");
    let mut drew = false;
    for r in 1..c.doc.record_count() {
        let kind = c.doc.record_kind(r);
        if matches!(
            kind,
            RecordKind::Line
                | RecordKind::Rect
                | RecordKind::Arc
                | RecordKind::Circle
                | RecordKind::Poly
        ) {
            drew = true;
            let vals: Vec<i32> = (0..).map_while(|i| c.coord(r, i)).collect();
            shape(&mut out, c, r, kind, &vals, true, notes);
        } else if kind == RecordKind::Stext {
            let (text, dropped) = convert_stext(c.field(r, 3), &ctx);
            if dropped {
                notes.review(
                    "template",
                    &c.path,
                    format!(
                        "stext {:?}: holes other than name, cell, params dropped",
                        c.field(r, 3)
                    ),
                );
            }
            let o = Orient::parse(c.field(r, 2)).unwrap_or(Orient::R0);
            let mut props = Vec::new();
            if let Some(l) = c.attr(r, "layer") {
                props.push(kv("layer", l));
            }
            for flag in ["hcenter", "vcenter"] {
                if c.doc.find_attr(r, flag).is_some() {
                    props.push(kv(flag, "true"));
                }
            }
            let size = c.attr(r, "size").unwrap_or("0.2");
            text_record(
                &mut out,
                &text,
                [c.coord(r, 0), c.coord(r, 1)],
                o,
                size,
                &props,
            );
        }
    }
    if !drew {
        // A symbol without graphics would be invisible in xschem: a box around the pins.
        let (mut lo, mut hi) = ([-20_000, -20_000], [20_000, 20_000]);
        for p in pins
            .at
            .iter()
            .zip(&pins.inherit)
            .filter(|(_, i)| i.is_none())
            .map(|(a, _)| a)
        {
            for i in 0..2 {
                lo[i] = lo[i].min(p[i] + 10_000);
                hi[i] = hi[i].max(p[i] - 10_000);
            }
        }
        let mut line = String::from("B 4");
        for v in [lo[0], lo[1], hi[0], hi[1]] {
            line.push(' ');
            format_coord(v, &mut line);
        }
        writeln!(out, "{line} {{fill=false}}").unwrap();
        text_record(
            &mut out,
            "@symname",
            [Some(lo[0]), Some(lo[1] - 15_000)],
            Orient::R0,
            "0.2",
            &[],
        );
    }
    for p in 0..pins.name.len() {
        if pins.inherit[p].is_some() {
            continue;
        }
        let [x, y] = pins.at[p];
        let mut line = String::from("B 5");
        for v in [x - 2500, y - 2500, x + 2500, y + 2500] {
            line.push(' ');
            format_coord(v, &mut line);
        }
        let props = format!("{} {}", kv("name", xname(p)), kv("dir", &pins.dir[p]));
        writeln!(out, "{line} {}", brace(&props)).unwrap();
    }
    let drawn = pins.inherit.iter().filter(|i| i.is_none()).count();
    assert_eq!(ctx.pins.len() + ctx.inherited.len(), pins.name.len());
    assert_eq!(ctx.pins.len(), drawn, "a pin box per drawn pin");
    out
}

/// Notes the header, parameter and pin data xschem has no field for.
fn lost_attrs(c: &Cell, notes: &mut Notes) {
    for a in c.doc.attrs(0) {
        let reason = match c.doc.attr_key(a) {
            "default" | "prefix" | "corner" | "provenance" => continue,
            "doc" | "category" => "docs",
            "config" => "config",
            _ => "header",
        };
        notes.review(reason, &c.path, format!("header {}", c.doc.attr_key(a)));
    }
    let mut lost = BTreeSet::new();
    for r in c.records(RecordKind::Param) {
        for a in c.doc.attrs(r) {
            let key = c.doc.attr_key(a);
            if key != "kind" && key != "computed" {
                lost.insert(format!("param {} {key}", c.field(r, 0)));
            }
        }
    }
    for r in c.records(RecordKind::Pin) {
        for a in c.doc.attrs(r) {
            let key = c.doc.attr_key(a);
            if key != "inherit" && key != "alias" {
                lost.insert(format!("pin {} {key}", c.field(r, 0)));
            }
        }
    }
    if !lost.is_empty() {
        let list: Vec<String> = lost.into_iter().collect();
        notes.review("param-attr", &c.path, list.join(", "));
    }
}

// ---------------------------------------------------------------------------------------
// Schematics
// ---------------------------------------------------------------------------------------

/// Instance names already used in one schematic, lowercased (xschem keeps names unique
/// case-insensitively and would rename a clash); hands out fresh ones per prefix.
struct Names {
    taken: HashSet<String>,
}

impl Names {
    /// `name` itself when free, else `name2`, `name3`, ….
    fn unique(&mut self, name: &str) -> String {
        if self.taken.insert(name.to_ascii_lowercase()) {
            return name.to_owned();
        }
        let n = self.fresh(name);
        assert_ne!(n, name);
        n
    }

    /// `prefix1`, `prefix2`, …: the first free one.
    fn fresh(&mut self, prefix: &str) -> String {
        // Bounded: among `taken.len() + 1` candidates one is free.
        for k in 1..=self.taken.len() + 1 {
            let n = format!("{prefix}{k}");
            if self.taken.insert(n.to_ascii_lowercase()) {
                return n;
            }
        }
        unreachable!("pigeonhole: a candidate is free")
    }
}

/// Writes the `.sch` of a `.shcomp` or `.shbench`.
fn write_schematic(
    d: &Design,
    ci: usize,
    targets: &[Option<Target>],
    sc: &Scope,
    notes: &mut Notes,
) -> String {
    let c = &d.cells[ci];
    assert!(matches!(c.kind(), FileKind::Shcomp | FileKind::Shbench));
    let bench = c.kind() == FileKind::Shbench;
    let mut names = Names {
        taken: c
            .records(RecordKind::Inst)
            .map(|r| c.attr(r, "name").unwrap_or("").to_ascii_lowercase())
            .chain(
                c.records(RecordKind::Code)
                    .map(|r| c.field(r, 0).to_ascii_lowercase()),
            )
            .collect(),
    };
    let iface = sym_pins(c);
    let mut out = String::from(VERSION);
    out.push_str("G {}\nK {}\nV {}\nS {}\nE {}\n");
    let (mut lo, mut hi) = ([i32::MAX, i32::MAX], [i32::MIN, i32::MIN]);
    let mut grow = |p: [i32; 2]| {
        for i in 0..2 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    };
    for r in 1..c.doc.record_count() {
        let xy = |i| [c.coord(r, i).unwrap_or(0), c.coord(r, i + 1).unwrap_or(0)];
        let orient = |i| Orient::parse(c.field(r, i)).unwrap_or(Orient::R0);
        match c.doc.record_kind(r) {
            RecordKind::Shape => {
                let form = c.field(r, 1);
                let kind = match form {
                    "line" => RecordKind::Line,
                    "rect" => RecordKind::Rect,
                    "arc" => RecordKind::Arc,
                    "circle" => RecordKind::Circle,
                    _ => RecordKind::Poly,
                };
                let vals: Vec<i32> = (2..).map_while(|i| c.coord(r, i)).collect();
                for p in vals.as_chunks::<2>().0 {
                    grow(*p);
                }
                shape(&mut out, c, r, kind, &vals, false, notes);
            }
            RecordKind::Text => {
                grow(xy(1));
                let size = c.attr(r, "size").unwrap_or("0.4");
                let at = xy(1);
                text_record(
                    &mut out,
                    c.field(r, 4),
                    [Some(at[0]), Some(at[1])],
                    orient(3),
                    size,
                    &[],
                );
            }
            RecordKind::Wire => {
                grow(xy(1));
                grow(xy(3));
                let mut line = String::from("N");
                for i in 1..5 {
                    line.push(' ');
                    format_coord(c.coord(r, i).unwrap_or(0), &mut line);
                }
                let props = if c.attr(r, "bus") == Some("true") {
                    "bus=true"
                } else {
                    ""
                };
                writeln!(out, "{line} {}", brace(props)).unwrap();
            }
            RecordKind::Inst => {
                let a = c.doc.find_attr(r, "at").expect("inst has at (format E029)");
                let v = |i| c.doc.attr_value(a, i).unwrap_or("");
                let at = [
                    parse_coord(v(0)).unwrap_or(0),
                    parse_coord(v(1)).unwrap_or(0),
                ];
                grow(at);
                let m = d
                    .resolve(ci, c.field(r, 1))
                    .expect("loaded Designs resolve every reference");
                let target = targets[m].as_ref().expect("instantiated cells are live");
                let (symref, props) = instance(c, r, &d.cells[m], target, sc, notes);
                component(
                    &mut out,
                    &symref,
                    at,
                    Orient::parse(v(2)).unwrap_or(Orient::R0),
                    &props,
                );
            }
            RecordKind::Label => {
                grow(xy(1));
                let net = c.field(r, 4);
                let mut props = vec![kv("name", &names.fresh("l")), kv("lab", net)];
                if c.attr(r, "global") == Some("true") || net.ends_with('!') {
                    props.push(kv("global", "true"));
                }
                component(&mut out, "lab_pin.sym", xy(1), orient(3), &props);
            }
            RecordKind::Port => {
                grow(xy(1));
                let pin = c.field(r, 4);
                let p = iface.name.iter().position(|n| n == pin);
                let lab = p.and_then(|p| iface.alias[p].as_deref()).unwrap_or(pin);
                let sym = match p.map(|p| iface.dir[p].as_str()) {
                    Some("in") => "ipin.sym",
                    Some("out") => "opin.sym",
                    _ => "iopin.sym",
                };
                let props = [kv("name", &names.fresh("p")), kv("lab", lab)];
                component(&mut out, sym, xy(1), orient(3), &props);
            }
            RecordKind::Noconn => {
                grow(xy(1));
                let props = [kv("name", &names.fresh("nc"))];
                component(&mut out, "noconn.sym", xy(1), Orient::R0, &props);
            }
            RecordKind::Prose => notes.review("docs", &c.path, format!("prose {}", c.field(r, 0))),
            RecordKind::Constraint
            | RecordKind::Waive
            | RecordKind::Sweep
            | RecordKind::Montecarlo
            | RecordKind::Optimize
            | RecordKind::Output
            | RecordKind::Spec
            | RecordKind::Config => {
                let word = c.doc.record_word(r);
                notes.review(word, &c.path, format!("{word} {}", c.field(r, 0)));
            }
            RecordKind::Var if c.attr(r, "dist").is_some() => {
                notes.review("montecarlo", &c.path, format!("var {} dist", c.field(r, 0)));
            }
            _ => {}
        }
    }
    // Netlist text: code_shown instances in a column under the drawing.
    let mut blocks: Vec<(String, String)> = Vec::new();
    if bench {
        let (vars, corner) = bench_text(c, sc, notes);
        if !vars.is_empty() {
            blocks.push((names.unique("params"), vars));
        }
        if !corner.is_empty() {
            blocks.push((names.unique("corner"), corner));
        }
    }
    for r in c.records(RecordKind::Code) {
        let (name, lang) = (c.field(r, 0), c.field(r, 1));
        let text: Vec<&str> = c.doc.verbatim(r).collect();
        let text = text.join("\n");
        if !matches!(lang, "spice" | "ngspice") {
            notes.review(
                "code",
                &c.path,
                format!("code {name} {lang}: only SPICE text carries"),
            );
        } else if text.contains("${") {
            notes.review(
                "code",
                &c.path,
                format!("code {name}: ${{NAME}} expansion has no xschem form"),
            );
        } else {
            blocks.push((name.to_owned(), text.replace("$$", "$")));
        }
    }
    if !bench && let Some(imp) = default_impl(c) {
        let name = c.field(imp, 0);
        if let Some(b) = c.records(RecordKind::Body).find(|&b| c.field(b, 0) == name) {
            let text: Vec<&str> = c.doc.verbatim(b).collect();
            blocks.push((names.unique("body"), text.join("\n")));
        }
    }
    let (x0, mut y) = if lo[0] <= hi[0] {
        (lo[0], hi[1] + 60_000)
    } else {
        (0, 0)
    };
    for (name, text) in &blocks {
        let props = [
            kv("name", name),
            kv("only_toplevel", "false"),
            kv("value", text),
        ];
        component(&mut out, "code_shown.sym", [x0, y], Orient::R0, &props);
        let lines = i32::try_from(text.lines().count()).unwrap_or(i32::MAX / 40_000);
        y = y.saturating_add(60_000 + 20_000 * lines.min(10_000));
    }
    assert!(out.starts_with(VERSION) && out.ends_with('\n'));
    out
}

/// The `.param` lines of a bench's vars and the model lines of its default corner (its
/// `set`s replace var values); other corners are reviewed.
fn bench_text(c: &Cell, sc: &Scope, notes: &mut Notes) -> (String, String) {
    let default = c.header("corner");
    let mut sets: Vec<(&str, &str)> = Vec::new();
    let mut corner = String::new();
    for r in c.records(RecordKind::Corner) {
        let name = c.field(r, 0);
        if Some(name) != default {
            notes.review(
                "corner",
                &c.path,
                format!("corner {name}: only the default corner carries"),
            );
            continue;
        }
        for a in c.doc.attrs(r) {
            let v = |i| c.doc.attr_value(a, i).unwrap_or("");
            match c.doc.attr_key(a) {
                "lib" => writeln!(corner, ".lib {} {}", v(0), v(1)).unwrap(),
                "include" => writeln!(corner, ".include {}", v(0)).unwrap(),
                "set" => sets.push((v(0), v(1))),
                "temp" => match spice_value(v(0), sc, false) {
                    Ok(t) => writeln!(corner, ".temp {t}").unwrap(),
                    Err(why) => {
                        notes.review(why, &c.path, format!("corner {name} temp {:?}", v(0)))
                    }
                },
                key => notes.review("corner", &c.path, format!("corner {name} {key}")),
            }
        }
    }
    if corner.contains("${") {
        notes.review(
            "corner",
            &c.path,
            "${NAME} in a model path is written verbatim",
        );
    }
    let mut vars = String::new();
    for r in c.records(RecordKind::Var) {
        let name = c.field(r, 0);
        let expr = sets
            .iter()
            .rev()
            .find(|s| s.0 == name)
            .map_or(c.field(r, 1), |s| s.1);
        let value = spice_value(expr, sc, false).or_else(|why| {
            c.doc
                .find_attr(r, "cached")
                .and_then(|a| c.doc.attr_value(a, 0))
                .map(|v| {
                    notes.review(
                        "expression",
                        &c.path,
                        format!("var {name}: its cached value {v}"),
                    );
                    v.to_owned()
                })
                .ok_or(why)
        });
        match value {
            Ok(v) => writeln!(vars, ".param {name}={v}").unwrap(),
            Err(why) => notes.review(why, &c.path, format!("var {name} {expr:?}: not written")),
        }
    }
    (vars.trim_end().to_owned(), corner.trim_end().to_owned())
}

/// The symbol reference and attributes of instance record `r` of `c`, whose master is `m`.
fn instance(
    c: &Cell,
    r: u32,
    m: &Cell,
    target: &Target,
    sc: &Scope,
    notes: &mut Notes,
) -> (String, Vec<String>) {
    assert_eq!(c.doc.record_kind(r), RecordKind::Inst);
    let name = c.attr(r, "name").unwrap_or("");
    // Schemify's default template adds the subcircuit letter to a name without it
    // (netlist.md §7.6.5); xschem writes the name as is, so the name carries it.
    let xname =
        if matches!(target, Target::Own(_)) && default_call(m) && !name.starts_with(['x', 'X']) {
            notes.review(
                "name",
                &c.path,
                format!("{name} written X{name} (the subcircuit call letter)"),
            );
            format!("X{name}")
        } else {
            name.to_owned()
        };
    let mut props = vec![kv("name", &xname)];
    let mparams = params(m);
    let mpins = sym_pins(m);
    let mctx = fmt_ctx(m, &mpins);
    let over = |key: &str| c.attr(r, key);
    let symref = match target {
        Target::Own(dir) => format!("{dir}/{}.sym", m.name),
        Target::Original(s) => s.clone(),
        Target::Device(map) => format!("{}.sym", map.symbol),
        Target::Label(s) => (*s).to_owned(),
    };
    match target {
        Target::Label(_) => {
            let lab = over("lab")
                .or(mparams.first().map(|p| p.default))
                .unwrap_or("");
            props.push(kv("lab", lab));
        }
        // Every core parameter, defaults included: xschem's template defaults differ
        // (`nmos4` w=5u against core 1u), so nothing may fall back to them.
        Target::Device(map) => {
            for p in mparams.iter().filter(|p| p.computed.is_none()) {
                let key = map
                    .params
                    .iter()
                    .find(|(_, core)| *core == p.name)
                    .map_or(p.name, |(x, _)| x);
                let src = over(p.name).unwrap_or(p.default);
                match xschem_value(p.kind, src, sc, false) {
                    Ok(v) => props.push(kv(key, &v)),
                    Err(why) => notes.review(
                        why,
                        &c.path,
                        format!("{name}.{}={src:?}: xschem default kept", p.name),
                    ),
                }
            }
        }
        Target::Own(_) | Target::Original(_) => {
            for a in c.doc.attrs(r) {
                let key = c.doc.attr_key(a);
                if matches!(key, "at" | "name" | "ignore" | "inherit") {
                    continue;
                }
                let src = c.doc.attr_value(a, 0).unwrap_or("");
                let Some(p) = mparams.iter().find(|p| p.name == key) else {
                    notes.review(
                        "expression",
                        &c.path,
                        format!("{name}.{key}: not a parameter of {}", m.name),
                    );
                    continue;
                };
                match xschem_value(p.kind, src, sc, mctx.raw.contains(&key)) {
                    Ok(v) => props.push(kv(key, &v)),
                    Err(why) => notes.review(
                        why,
                        &c.path,
                        format!("{name}.{key}={src:?}: symbol default kept"),
                    ),
                }
            }
        }
    }
    if let Some(ig) = over("ignore") {
        props.push(kv("spice_ignore", ig));
    }
    for a in c.doc.attrs(r).filter(|&a| c.doc.attr_key(a) == "inherit") {
        let (pin, net) = (
            c.doc.attr_value(a, 0).unwrap_or(""),
            c.doc.attr_value(a, 1).unwrap_or(""),
        );
        props.push(kv(pin, net));
    }
    assert!(props[0].starts_with("name=") && !symref.is_empty());
    (symref, props)
}

// ---------------------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------------------

/// Writes `C {symref} x y rot flip {props}`.
fn component(out: &mut String, symref: &str, at: [i32; 2], o: Orient, props: &[String]) {
    let (rot, flip) = rot_flip(o);
    let mut line = format!("C {}", brace(symref));
    for v in at {
        line.push(' ');
        format_coord(v, &mut line);
    }
    writeln!(out, "{line} {rot} {flip} {}", brace(&props.join(" "))).unwrap();
}

/// Writes `T {text} x y rot flip size size {props}`.
fn text_record(
    out: &mut String,
    text: &str,
    at: [Option<i32>; 2],
    o: Orient,
    size: &str,
    props: &[String],
) {
    let (rot, flip) = rot_flip(o);
    let mut line = format!("T {}", brace(text));
    for v in at {
        line.push(' ');
        format_coord(v.unwrap_or(0), &mut line);
    }
    writeln!(
        out,
        "{line} {rot} {flip} {size} {size} {}",
        brace(&props.join(" "))
    )
    .unwrap();
}

/// Writes a line/rect/arc/circle/poly (`vals` = its coordinate values) as `L B A P`.
/// In a symbol, layer 5 (xschem's pin layer) moves to 4 so a box never becomes a pin.
fn shape(
    out: &mut String,
    c: &Cell,
    r: u32,
    kind: RecordKind,
    vals: &[i32],
    symbol: bool,
    notes: &mut Notes,
) {
    assert!(
        vals.len() >= 3,
        "every graphic form has at least three values"
    );
    let mut layer: u32 = c.attr(r, "layer").and_then(|l| l.parse().ok()).unwrap_or(4);
    if symbol && layer == 5 {
        notes.review(
            "layer",
            &c.path,
            "graphic on layer 5 (xschem's pin layer) moved to 4",
        );
        layer = 4;
    }
    let fill = c.attr(r, "fill") == Some("true");
    let mut props = Vec::new();
    if let Some(dash) = c.attr(r, "dash") {
        props.push(kv("dash", dash));
    }
    if c.attr(r, "width").is_some() {
        notes.review("shape", &c.path, "line width has no xschem form");
    }
    let (tag, coords): (char, Vec<i32>) = match kind {
        RecordKind::Line => ('L', vals.to_vec()),
        RecordKind::Rect => {
            props.push(kv("fill", if fill { "true" } else { "false" }));
            ('B', vals.to_vec())
        }
        RecordKind::Arc => ('A', vals.to_vec()),
        RecordKind::Circle => {
            let mut v = vals.to_vec();
            v.extend([0, 360_000]);
            ('A', v)
        }
        _ => {
            let mut v = vals.to_vec();
            // xschem closes a polygon whose last point is its first.
            let open = v.len() >= 4 && v[..2] != v[v.len() - 2..];
            if c.attr(r, "closed") == Some("true") && open {
                v.extend([v[0], v[1]]);
            }
            ('P', v)
        }
    };
    if fill && tag != 'B' {
        props.push(kv("fill", "true"));
    }
    let mut line = format!("{tag} {layer}");
    if tag == 'P' {
        write!(line, " {}", coords.len() / 2).unwrap();
    }
    for v in coords {
        line.push(' ');
        format_coord(v, &mut line);
    }
    writeln!(out, "{line} {}", brace(&props.join(" "))).unwrap();
}

/// xschem `(rot, flip)` of `o`: the inverse of the importer's [`ORIENT_TABLE`].
fn rot_flip(o: Orient) -> (usize, usize) {
    for (rot, row) in ORIENT_TABLE.iter().enumerate() {
        if let Some(flip) = row.iter().position(|x| *x == o) {
            return (rot, flip);
        }
    }
    unreachable!("ORIENT_TABLE holds all eight orientations")
}

/// The `xschemrc` fragment: the output directory on the symbol search path (so
/// `<lib>/<cell>.sym` references resolve), the libraries listed for the reader.
fn xschemrc(dirs: &BTreeSet<&str>) -> String {
    let mut out = String::from(
        "# Schemify export (interop.md §11). Source this file from your xschemrc, or pass it\n\
         # with --rcfile: it puts this directory on the symbol search path.\n",
    );
    for d in dirs {
        writeln!(out, "# library: {d}").unwrap();
    }
    out.push_str("append XSCHEM_LIBRARY_PATH :[file dirname [file normalize [info script]]]\n");
    out
}

// ---------------------------------------------------------------------------------------
// Attribute strings (feature inventory §5.3, §5.4)
// ---------------------------------------------------------------------------------------

/// `key=value`, the value quoted when it holds a separator, a quote, a backslash or `=`, or
/// is empty; `\` and `"` are escaped (xschem strips one level when it reads the value).
fn kv(key: &str, value: &str) -> String {
    assert!(
        !key.is_empty() && !key.contains(['=', ' ']),
        "attribute key {key:?}"
    );
    let quote = value.is_empty()
        || value.contains(|c: char| c.is_whitespace() || matches!(c, ';' | '"' | '\\' | '='));
    let mut out = format!("{key}=");
    if quote {
        out.push('"');
    }
    for ch in value.chars() {
        if matches!(ch, '"' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    if quote {
        out.push('"');
    }
    out
}

/// A brace string: `{…}` with `\`, `{` and `}` escaped.
fn brace(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('{');
    for ch in s.chars() {
        if matches!(ch, '\\' | '{' | '}') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('}');
    assert!(out.len() >= s.len() + 2);
    out
}

// ---------------------------------------------------------------------------------------
// Values and expressions (§3.4, xschem paragraph)
// ---------------------------------------------------------------------------------------

/// One token of a Schemify expression, as far as translation needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok<'a> {
    Num(&'a str),
    /// An identifier, possibly namespaced (`ns.name`).
    Ident(&'a str),
    /// One of `+ - * / ^ ( ) ,`.
    Op(&'a str),
}

/// Splits `s` into tokens; `None` for anything outside numbers, identifiers and the
/// arithmetic operators (strings, comparisons, `?:`, `&&`, …): no xschem spelling.
fn lex(s: &str) -> Option<Vec<Tok<'_>>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let run = |mut i: usize, f: fn(u8) -> bool| {
        while i < b.len() && f(b[i]) {
            i += 1;
        }
        i
    };
    while i < b.len() {
        let c = b[i];
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            i = run(i, |c| c.is_ascii_digit());
            if b.get(i) == Some(&b'.') {
                i = run(i + 1, |c| c.is_ascii_digit());
            }
            if matches!(b.get(i), Some(b'e' | b'E')) {
                let j = i + 1 + usize::from(matches!(b.get(i + 1), Some(b'+' | b'-')));
                if b.get(j).is_some_and(u8::is_ascii_digit) {
                    i = run(j, |c| c.is_ascii_digit());
                }
            }
            i = run(i, |c| c.is_ascii_alphabetic());
            out.push(Tok::Num(&s[start..i]));
        } else if c.is_ascii_alphabetic() || c == b'_' {
            i = run(i, |c| c.is_ascii_alphanumeric() || c == b'_');
            if b.get(i) == Some(&b'.')
                && b.get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
            {
                i = run(i + 1, |c| c.is_ascii_alphanumeric() || c == b'_');
            }
            out.push(Tok::Ident(&s[start..i]));
        } else if b"+-*/^(),".contains(&c) {
            i += 1;
            out.push(Tok::Op(&s[start..i]));
        } else {
            return None;
        }
    }
    Some(out)
}

/// How names resolve inside one cell when its expressions are exported.
struct Scope {
    /// Names the netlist resolves (cell parameters, bench vars): written as they are.
    keep: Vec<String>,
    /// Names xschem has no place for (a component's vars, computed params): their
    /// definition is inlined in parentheses; `None` when it does not translate.
    inline: Vec<(String, Option<String>)>,
}

impl Scope {
    /// The scope of `c`: a bench keeps its vars (they become `.param`s), a component or
    /// device inlines them, using a plugin-call var's cached value (noted).
    fn of(c: &Cell, notes: &mut Notes) -> Scope {
        let bench = c.kind() == FileKind::Shbench;
        let mut sc = Scope {
            keep: Vec::new(),
            inline: Vec::new(),
        };
        for p in params(c) {
            match p.computed {
                Some(e) => sc.inline.push((p.name.to_owned(), Some(e.to_owned()))),
                None => sc.keep.push(p.name.to_owned()),
            }
        }
        for r in c.records(RecordKind::Var) {
            let (name, expr) = (c.field(r, 0), c.field(r, 1));
            if bench {
                sc.keep.push(name.to_owned());
                continue;
            }
            let plugin = lex(expr).is_none_or(|t| {
                t.iter()
                    .any(|t| matches!(t, Tok::Ident(x) if x.contains('.')))
            });
            let def = if plugin {
                let cached = c
                    .doc
                    .find_attr(r, "cached")
                    .and_then(|a| c.doc.attr_value(a, 0));
                if let Some(v) = cached {
                    notes.review(
                        "expression",
                        &c.path,
                        format!("var {name}: inlined as its cached value {v}"),
                    );
                }
                cached.map(str::to_owned)
            } else {
                Some(expr.to_owned())
            };
            sc.inline.push((name.to_owned(), def));
        }
        sc
    }
}

/// `src` with every inlined name of `sc` replaced by its definition in parentheses.
fn inline(src: &str, sc: &Scope, depth: u32) -> Result<String, &'static str> {
    if depth > INLINE_DEPTH_MAX {
        return Err("inlined-var");
    }
    let Some(toks) = lex(src) else {
        return Ok(src.to_owned()); // fails translation later, with the right reason
    };
    if !toks
        .iter()
        .any(|t| matches!(t, Tok::Ident(x) if sc.inline.iter().any(|(n, _)| n == x)))
    {
        return Ok(src.to_owned());
    }
    let mut out = String::new();
    for (k, t) in toks.iter().enumerate() {
        match *t {
            Tok::Ident(x)
                if toks.get(k + 1) != Some(&Tok::Op("(")) && !sc.keep.iter().any(|n| n == x) =>
            {
                match sc.inline.iter().find(|(n, _)| n == x) {
                    Some((_, Some(def))) => {
                        let def = inline(def, sc, depth + 1)?;
                        if matches!(lex(&def).as_deref(), Some([Tok::Num(_)])) {
                            out.push_str(def.trim());
                        } else {
                            out.push('(');
                            out.push_str(&def);
                            out.push(')');
                        }
                    }
                    Some((_, None)) => return Err("expression"),
                    None => out.push_str(x),
                }
            }
            Tok::Num(s) | Tok::Ident(s) | Tok::Op(s) => out.push_str(s),
        }
    }
    Ok(out)
}

/// A number expression as a SPICE value: a number literal as written (Schemify's suffixes
/// are SPICE's), anything else as a SPICE expression with `^` → `**`, quoted `'…'` unless
/// `bare` (the format quotes it: a raw parameter, see [`FmtCtx::raw`]).
///
/// # Errors
///
/// The `needs_review` reason: `expression` for strings, conditionals, comparisons,
/// namespaced names and functions SPICE lacks.
fn spice_value(src: &str, sc: &Scope, bare: bool) -> Result<String, &'static str> {
    let e = inline(src.trim(), sc, 0)?;
    let toks = lex(&e).ok_or("expression")?;
    match toks.as_slice() {
        [] => return Err("expression"),
        [Tok::Num(_)] | [Tok::Op("-"), Tok::Num(_)] => return Ok(e.trim().to_owned()),
        _ => {}
    }
    let q = if bare { "" } else { "'" };
    let mut out = String::from(q);
    for (k, t) in toks.iter().enumerate() {
        match *t {
            Tok::Ident(x) => {
                let call = toks.get(k + 1) == Some(&Tok::Op("("));
                if x.contains('.')
                    || matches!(x, "true" | "false")
                    || (call && !SPICE_FUNCS.contains(&x))
                {
                    return Err("expression");
                }
                out.push_str(x);
            }
            Tok::Op("^") => out.push_str("**"),
            Tok::Num(s) | Tok::Op(s) => out.push_str(s),
        }
    }
    out.push_str(q);
    Ok(out)
}

/// A string-kind value (a template): literal text kept, a hole naming one identifier
/// written as that bare name (the inverse of the importer's parent-parameter holes).
fn string_value(src: &str) -> Result<String, &'static str> {
    let mut out = String::new();
    for piece in split_template(src).ok_or("template")? {
        match piece {
            Piece::Lit(s) => out.push_str(&s),
            Piece::Hole(h) if is_ident(h.trim()) => out.push_str(h.trim()),
            Piece::Hole(_) => return Err("template"),
        }
    }
    Ok(out)
}

/// A value of a parameter of kind `kind` in xschem spelling; `raw` for a parameter its
/// symbol's format quotes itself ([`FmtCtx::raw`]).
fn xschem_value(kind: &str, src: &str, sc: &Scope, raw: bool) -> Result<String, &'static str> {
    match kind {
        "string" | "choice" => string_value(src),
        "bool" => match src.trim() {
            v @ ("true" | "false") => Ok(v.to_owned()),
            _ => Err("expression"),
        },
        _ => spice_value(src, sc, raw),
    }
}

fn is_ident(s: &str) -> bool {
    let mut b = s.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

// ---------------------------------------------------------------------------------------
// Templates (§3.5, import.md §4 inverted)
// ---------------------------------------------------------------------------------------

/// A piece of a Schemify template.
enum Piece {
    /// Literal text, `{{`/`}}` resolved.
    Lit(String),
    /// A hole's expression text.
    Hole(String),
}

/// Splits a template (expr.md §6); `None` for an unclosed `{` or a lone `}`.
fn split_template(s: &str) -> Option<Vec<Piece>> {
    let b = s.as_bytes();
    let (mut out, mut lit) = (Vec::new(), String::new());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1)) {
            (b'{', Some(b'{')) | (b'}', Some(b'}')) => {
                lit.push(b[i] as char);
                i += 2;
            }
            (b'{', _) => {
                let mut j = i + 1;
                let mut in_str = false;
                while j < b.len() && (in_str || b[j] != b'}') {
                    match b[j] {
                        b'\\' if in_str => j += 1,
                        b'"' => in_str = !in_str,
                        _ => {}
                    }
                    j += 1;
                }
                if j >= b.len() {
                    return None;
                }
                if !lit.is_empty() {
                    out.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                out.push(Piece::Hole(s[i + 1..j].to_owned()));
                i = j + 1;
            }
            (b'}', _) => return None,
            _ => {
                let ch = s[i..].chars().next().expect("in bounds");
                lit.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    if !lit.is_empty() {
        out.push(Piece::Lit(lit));
    }
    Some(out)
}

/// What a format template's identifiers may name.
struct FmtCtx<'a> {
    /// Ordinary pins: Schemify name → xschem name.
    pins: Vec<(&'a str, &'a str)>,
    /// Inherited pins (xschem `extra` nets).
    inherited: Vec<&'a str>,
    /// Overridable parameters.
    params: Vec<&'a str>,
    /// Computed parameters and their expressions.
    computed: Vec<(&'a str, &'a str)>,
    /// The impl's `model` attribute.
    model: Option<&'a str>,
    /// Parameters a format uses as numbers (a hole with a glued scale suffix, `{W}u`, or
    /// inside an arithmetic hole): every hole of one is written as the SPICE expression
    /// `'(@W)'` and its values unquoted, so a parametric value (`wn*2`) still netlists
    /// (xschem pastes values as text: `'wn'u` would not).
    raw: Vec<&'a str>,
}

/// The format context of cell `c` with pins `pins` (its [`sym_pins`]).
fn fmt_ctx<'a>(c: &'a Cell, pins: &'a SymPins) -> FmtCtx<'a> {
    let ps = params(c);
    let imp = default_impl(c);
    // Pins: xschem shorts same-named pins, so an alias pin takes its target's name.
    let mut ctx = FmtCtx {
        pins: (0..pins.name.len())
            .filter(|&p| pins.inherit[p].is_none())
            .map(|p| {
                (
                    pins.name[p].as_str(),
                    pins.alias[p].as_deref().unwrap_or(&pins.name[p]),
                )
            })
            .collect(),
        inherited: (0..pins.name.len())
            .filter(|&p| pins.inherit[p].is_some())
            .map(|p| pins.name[p].as_str())
            .collect(),
        params: ps
            .iter()
            .filter(|p| p.computed.is_none())
            .map(|p| p.name)
            .collect(),
        computed: ps
            .iter()
            .filter_map(|p| Some((p.name, p.computed?)))
            .collect(),
        model: imp.and_then(|r| c.attr(r, "model")),
        raw: Vec::new(),
    };
    let mut raw = Vec::new();
    for (_, tpl) in imp.map(|r| formats(c, r)).unwrap_or_default() {
        let pieces = split_template(tpl).unwrap_or_default();
        for (k, piece) in pieces.iter().enumerate() {
            let Piece::Hole(h) = piece else { continue };
            let h = h.trim();
            let glued = matches!(pieces.get(k + 1), Some(Piece::Lit(l)) if glued_suffix(l) > 0);
            if let Some(p) = ctx.params.iter().find(|p| **p == h) {
                if glued {
                    raw.push(*p);
                }
            } else if !is_special(h, &ctx) {
                mark_raw(h, &ctx, &mut raw, 0);
            }
        }
    }
    raw.sort_unstable();
    raw.dedup();
    assert!(
        raw.iter().all(|r| ctx.params.contains(r)),
        "raw params are params"
    );
    ctx.raw = raw;
    ctx
}

/// True for holes `convert_format` spells as a token of their own (builtins, pins, the
/// `savecurrent` idiom), never as a number.
fn is_special(h: &str, ctx: &FmtCtx) -> bool {
    matches!(h, "name" | "cell" | "pins" | "params" | "file")
        || (h == "model" && ctx.model.is_some())
        || ctx.pins.iter().any(|(n, _)| *n == h)
        || ctx.inherited.contains(&h)
        || (h.starts_with("savecurrent") && h.contains('?'))
}

/// Adds every parameter arithmetic hole `h` reads (through computed params) to `raw`.
fn mark_raw<'a>(h: &str, ctx: &FmtCtx<'a>, raw: &mut Vec<&'a str>, depth: u32) {
    let Some(toks) = lex(h) else { return };
    for t in toks {
        let Tok::Ident(x) = t else { continue };
        if let Some(p) = ctx.params.iter().find(|p| **p == x) {
            raw.push(p);
        } else if let Some((_, e)) = ctx.computed.iter().find(|(n, _)| *n == x)
            && depth < INLINE_DEPTH_MAX
        {
            mark_raw(e, ctx, raw, depth + 1);
        }
    }
}

/// Length of the SPICE scale suffix `lit` starts with (`u` after `{W}` in `{W}u`), else 0.
fn glued_suffix(lit: &str) -> usize {
    let n = lit.bytes().take_while(u8::is_ascii_alphabetic).count();
    let word = lit[..n].to_ascii_lowercase();
    if matches!(
        word.as_str(),
        "f" | "p" | "n" | "u" | "m" | "k" | "meg" | "g" | "t" | "mil"
    ) {
        n
    } else {
        0
    }
}

/// Output of a template conversion: an xschem `@token` or literal text.
enum Out {
    Tok(String),
    Lit(String),
}

/// Writes token-layer text: `@tok`, and literal text with `@ % \` escaped; a literal that
/// directly follows a token is escaped too, since only white space or the next `@`/`%`
/// ends a token (`@W\u` is `@W` then `u`).
fn render(pieces: &[Out]) -> String {
    let mut out = String::new();
    let mut after_tok = false;
    for p in pieces {
        match p {
            Out::Tok(t) => {
                out.push('@');
                out.push_str(t);
                after_tok = true;
            }
            Out::Lit(s) => {
                for (k, ch) in s.chars().enumerate() {
                    let ends_tok = k == 0 && after_tok && !ch.is_whitespace();
                    if ends_tok || matches!(ch, '@' | '%' | '\\') {
                        out.push('\\');
                    }
                    out.push(ch);
                }
                after_tok &= s.is_empty();
            }
        }
    }
    out
}

/// A netlist template as an xschem format: `{name}` → `@name`, `{cell}` → `@symname`,
/// `{pins}` → `@pinlist` (then the inherited pins), a pin → `@@pin`, an inherited pin or a
/// parameter → `@p`, the `savecurrent` idiom → `@savecurrent`; a raw parameter
/// ([`FmtCtx::raw`]) and any other arithmetic hole → a SPICE expression `'(@W)*1u'`
/// (computed params inlined, a glued scale suffix folded in).
///
/// # Errors
///
/// Why the template has no xschem spelling.
fn convert_format(tpl: &str, ctx: &FmtCtx) -> Result<String, String> {
    let pieces = split_template(tpl).ok_or("not a template")?;
    let mut out = Vec::new();
    let mut skip = 0; // suffix bytes of the next literal folded into the previous hole
    for (k, piece) in pieces.iter().enumerate() {
        let h = match piece {
            Piece::Lit(s) => {
                out.push(Out::Lit(s[std::mem::take(&mut skip)..].to_owned()));
                continue;
            }
            Piece::Hole(h) => h.trim(),
        };
        match h {
            "name" => out.push(Out::Tok("name".into())),
            "cell" => out.push(Out::Tok("symname".into())),
            "pins" => {
                out.push(Out::Tok("pinlist".into()));
                for p in &ctx.inherited {
                    out.push(Out::Lit(" ".into()));
                    out.push(Out::Tok((*p).into()));
                }
            }
            "model" if ctx.model.is_some() => out.push(Out::Lit(ctx.model.unwrap_or("").into())),
            "params" | "file" => return Err(format!("{{{h}}} has no xschem token")),
            _ if let Some((_, x)) = ctx.pins.iter().find(|(n, _)| *n == h) => {
                out.push(Out::Tok(format!("@{x}")));
            }
            _ if ctx.inherited.contains(&h) => out.push(Out::Tok(h.into())),
            _ if ctx.params.contains(&h) && !ctx.raw.contains(&h) => out.push(Out::Tok(h.into())),
            _ if h.starts_with("savecurrent")
                && h.contains('?')
                && ctx.params.contains(&"savecurrent") =>
            {
                out.push(Out::Tok("savecurrent".into()));
            }
            _ => {
                out.push(Out::Lit("'".into()));
                spice_pieces(h, ctx, &mut out, 0)?;
                if let Some(Piece::Lit(l)) = pieces.get(k + 1) {
                    skip = glued_suffix(l);
                    if skip > 0 {
                        out.push(Out::Lit(format!("*1{}", &l[..skip])));
                    }
                }
                out.push(Out::Lit("'".into()));
            }
        }
    }
    Ok(render(&out))
}

/// An arithmetic hole as SPICE expression pieces, every parameter as `(@p)`.
fn spice_pieces(h: &str, ctx: &FmtCtx, out: &mut Vec<Out>, depth: u32) -> Result<(), String> {
    if depth > INLINE_DEPTH_MAX {
        return Err("computed parameters nest too deep".into());
    }
    let toks = lex(h).ok_or_else(|| format!("{{{h}}}: not arithmetic"))?;
    for (k, t) in toks.iter().enumerate() {
        match *t {
            Tok::Ident(x) if toks.get(k + 1) == Some(&Tok::Op("(")) => {
                if !SPICE_FUNCS.contains(&x) {
                    return Err(format!("{x}(): no SPICE function"));
                }
                out.push(Out::Lit(x.into()));
            }
            Tok::Ident(x) if ctx.params.contains(&x) => {
                out.push(Out::Lit("(".into()));
                out.push(Out::Tok(x.into()));
                out.push(Out::Lit(")".into()));
            }
            Tok::Ident(x) => match ctx.computed.iter().find(|(n, _)| *n == x) {
                Some((_, e)) => {
                    out.push(Out::Lit("(".into()));
                    spice_pieces(e, ctx, out, depth + 1)?;
                    out.push(Out::Lit(")".into()));
                }
                None => return Err(format!("{{{h}}}: {x} is no parameter")),
            },
            Tok::Op("^") => out.push(Out::Lit("**".into())),
            Tok::Num(s) | Tok::Op(s) => out.push(Out::Lit(s.into())),
        }
    }
    Ok(())
}

/// A symbol text template as an xschem `T` text: `{name}` → `@name`, `{cell}` →
/// `@symname`, `{p}` → `@p`; other holes are dropped (the flag says so).
fn convert_stext(tpl: &str, ctx: &FmtCtx) -> (String, bool) {
    let Some(pieces) = split_template(tpl) else {
        return (render(&[Out::Lit(tpl.into())]), true);
    };
    let (mut out, mut dropped) = (Vec::new(), false);
    for piece in pieces {
        match piece {
            Piece::Lit(s) => out.push(Out::Lit(s)),
            Piece::Hole(h) => match h.trim() {
                "name" => out.push(Out::Tok("name".into())),
                "cell" => out.push(Out::Tok("symname".into())),
                p if ctx.params.contains(&p) => out.push(Out::Tok(p.into())),
                _ => dropped = true,
            },
        }
    }
    (render(&out), dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(keep: &[&str], inline: &[(&str, Option<&str>)]) -> Scope {
        Scope {
            keep: keep.iter().map(|s| (*s).to_owned()).collect(),
            inline: inline
                .iter()
                .map(|(n, d)| ((*n).to_owned(), d.map(str::to_owned)))
                .collect(),
        }
    }

    #[test]
    fn values_follow_the_xschem_paragraph() {
        let sc = scope(
            &["wn", "vdd"],
            &[("w2", Some("wn*2")), ("p", None), ("c", Some("2"))],
        );
        assert_eq!(spice_value("1.5u", &sc, false), Ok("1.5u".into()));
        assert_eq!(spice_value("-40", &sc, false), Ok("-40".into()));
        assert_eq!(spice_value("vdd/2", &sc, false), Ok("'vdd/2'".into()));
        assert_eq!(spice_value("vdd/2", &sc, true), Ok("vdd/2".into()));
        assert_eq!(spice_value("wn^2", &sc, false), Ok("'wn**2'".into()));
        assert_eq!(spice_value("w2+c", &sc, false), Ok("'(wn*2)+2'".into()));
        assert_eq!(spice_value("sqrt(wn)", &sc, false), Ok("'sqrt(wn)'".into()));
        for bad in ["a ? b : c", "\"s\"", "gmid.w(1)", "if(a,b,c)", "p*2", "a<b"] {
            assert_eq!(spice_value(bad, &sc, false), Err("expression"), "{bad}");
        }
        assert_eq!(string_value("'{WP}*2'"), Ok("'WP*2'".into()));
        assert_eq!(string_value("{{x}}"), Ok("{x}".into()));
        assert_eq!(string_value("{a*2}"), Err("template"));
        assert_eq!(xschem_value("bool", "true", &sc, false), Ok("true".into()));
    }

    #[test]
    fn formats_and_texts_convert_by_the_token_rules() {
        let ctx = FmtCtx {
            pins: vec![("D", "D"), ("B_2", "B")],
            inherited: vec!["VDD"],
            params: vec!["W", "L", "savecurrent"],
            computed: vec![("ad", "W*0.29")],
            model: None,
            raw: vec!["W"],
        };
        assert_eq!(
            convert_format("M{name} {D} {B_2} {pins} W={W}u L={L} ad={ad}p {W}", &ctx).unwrap(),
            "M@name @@D @@B @pinlist @VDD W='(@W\\)*1u' L=@L ad='((@W\\)*0.29)*1p' '(@W\\)'"
        );
        assert_eq!(glued_suffix("meg x"), 3);
        assert_eq!(glued_suffix("um"), 0);
        assert_eq!(
            convert_format(
                "{name} {savecurrent ? \"\\n.save i(\" + name + \")\" : \"\"}",
                &ctx
            )
            .unwrap(),
            "@name @savecurrent"
        );
        assert_eq!(convert_format("a@b 50%", &ctx).unwrap(), "a\\@b 50\\%");
        assert!(convert_format("{params}", &ctx).is_err());
        assert!(convert_format("{Z}", &ctx).is_err());
        assert_eq!(
            convert_stext("W={W} {eng(W)}", &ctx),
            ("W=@W ".into(), true)
        );
        assert_eq!(kv("format", "@a\\b"), "format=\"@a\\\\b\"");
        assert_eq!(brace("a{b}\\"), "{a\\{b\\}\\\\}");
    }

    #[test]
    fn orientation_inverts_the_import_table() {
        for (rot, row) in ORIENT_TABLE.iter().enumerate() {
            for (flip, o) in row.iter().enumerate() {
                assert_eq!(rot_flip(*o), (rot, flip));
                assert_eq!(crate::translate::orient(rot as i16, flip as i16), *o);
            }
        }
    }
}
