//! The live Virtuoso kit, host side (`plugins/interop/live/README.md`; spec `interop.md`
//! §5.5, §7.4, §9, §10). A real Virtuoso runs two generated SKILL sessions; this file writes
//! their inputs and grades their outputs, step by step (`SCHEMIFY_LIVE_STEP`, driven by
//! `live/run.sh`):
//!
//! - `prepare`: the Rust exports of the fixture projects (core-cell and analogLib modes),
//!   Virtuoso-shaped dump fixtures, malformed files, and `session1.il`.
//! - `mid` (after session 1): the Rust imports of what Virtuoso dumped, a Schemify-side edit,
//!   the re-exports, and `session2.il`.
//! - `verify` (after session 2 and the shell's netlist and OCEAN runs): every check, graded
//!   into `out/report.md`.
//!
//! The oracles are the ones the mock-backed tests assert (`virtuoso_e2e.rs`, `ade.rs`), now on
//! a real database. `kit_passes_on_the_mock` runs the whole kit with the mock Virtuoso in
//! place of the real one, so a FAIL on a real Virtuoso is a difference between Virtuoso and
//! our reading of its manuals, not a bug in the kit.

mod common;
mod skill;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use common::{
    canonical, design_libs, is_cell, line_of, located, read_tree, record_of, root, strip_fp,
};
use interop::job::{Command, Job, Step};
use interop::sfyx::{self, Record};
use interop::virtuoso::{ExportOptions, ImportOptions};
use skill::Interp;
use skill::mockdb::MockDb;

const LIVE_IL: &str = include_str!("../live/sfy_live.il");
const BUILD: &str = include_str!("fixtures/skill/build.il");

/// Fixture projects every run carries: (name, repository-relative directory). Each is
/// self-contained (its devices are its own cells or `core:*`), so it loads on any Virtuoso.
const PROJECTS: [(&str, &str); 5] = [
    ("rc", "crates/app/tests/fixtures/rc"),
    ("inv", "crates/app/tests/fixtures/inv"),
    ("ota", "crates/app/tests/fixtures/ota"),
    ("elab", "crates/elab/tests/fixtures"),
    ("rclive", "plugins/interop/tests/fixtures/live/rclive"),
];

/// Dumps the SKILL dumper wrote (from `fixtures/skill/build.il`) whose masters are analogLib
/// and basic only: (name, fixture path, the design library in the file). The hand-written
/// `fixtures/virtuoso/*.sfyx` are left out: their 160 units per inch dump back as 160000.
const DUMPS: [(&str, &str, &str); 5] = [
    ("skill_inv", "skill/inv.sfyx", "work"),
    ("skill_bus", "skill/bus.sfyx", "work"),
    ("skill_netx", "skill/netx.sfyx", "work"),
    ("skill_symkit", "skill/symkit.sfyx", "work"),
    ("skill_vamod", "skill/vamod.sfyx", "work"),
];

/// The scratch library every self-test loads into.
const SCRATCH: &str = "sfy_scratch";
/// The library the refusal checks target.
const NEG_LIB: &str = "sfyn_rclive";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// A SKILL string literal.
fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn q_opt(s: Option<&str>) -> String {
    s.map_or("nil".into(), q)
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Runs a conversion job against the files of `project`, as `schemify-interop` does.
fn job(project: &Path, cmd: Command) -> Result<Vec<(String, Vec<u8>)>, Vec<String>> {
    let mut job = Job::new(cmd);
    for _ in 0..interop::job::ROUNDS_MAX {
        match job.step() {
            Step::Read(paths) => {
                for p in paths {
                    job.supply(&p, std::fs::read(project.join(&p)).ok());
                }
            }
            Step::Done { writes, .. } => return Ok(writes),
            Step::Failed(p) => return Err(p),
        }
    }
    Err(vec!["the job did not finish".into()])
}

// ---------------------------------------------------------------------------------------
// Options and naming

/// What the user asked for beyond the fixtures (`run.sh` flags, kept in `options.txt`).
#[derive(Clone, Debug, Default)]
struct Opts {
    /// `--cell lib/cell`: the user's own cells for the self-test and the round trip; `--lib
    /// lib` is the cell `*`, the whole library.
    cells: Vec<(String, String)>,
    /// `--project dir`: the user's own Schemify projects, round-tripped like the fixtures.
    projects: Vec<PathBuf>,
    /// `--pdk lib[/cell]`: a PDK library to mirror, and a transistor for the probe.
    pdk: Option<(String, Option<String>)>,
    /// `--retarget map`: a target map for the gm/Id retarget of the Exercise 1 benches.
    target: Option<PathBuf>,
    /// `--callbacks`: load the user's cells again with CDF callbacks on.
    callbacks: bool,
}

impl Opts {
    fn from_env() -> Opts {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Opts {
            cells: var("SCHEMIFY_LIVE_CELLS")
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|c| c.split_once('/'))
                .map(|(l, c)| (l.to_owned(), c.to_owned()))
                .collect(),
            pdk: var("SCHEMIFY_LIVE_PDK").map(|p| match p.split_once('/') {
                Some((l, c)) => (l.to_owned(), Some(c.to_owned())),
                None => (p, None),
            }),
            projects: var("SCHEMIFY_LIVE_PROJECTS")
                .unwrap_or_default()
                .split_whitespace()
                .map(PathBuf::from)
                .collect(),
            target: var("SCHEMIFY_LIVE_TARGET").map(PathBuf::from),
            callbacks: var("SCHEMIFY_LIVE_CALLBACKS").is_some_and(|v| v == "1"),
        }
    }

    fn save(&self, work: &Path) {
        let mut s = String::new();
        for (l, c) in &self.cells {
            writeln!(s, "cell {l}/{c}").unwrap();
        }
        if let Some((l, c)) = &self.pdk {
            writeln!(
                s,
                "pdk {l}{}",
                c.as_ref().map(|c| format!("/{c}")).unwrap_or_default()
            )
            .unwrap();
        }
        for d in &self.projects {
            writeln!(s, "project {}", path_str(d)).unwrap();
        }
        if let Some(t) = &self.target {
            writeln!(s, "target {}", path_str(t)).unwrap();
        }
        if self.callbacks {
            s.push_str("callbacks\n");
        }
        write(&work.join("options.txt"), s);
    }

    fn load(work: &Path) -> Opts {
        let mut o = Opts::default();
        for line in read(&work.join("options.txt")).unwrap_or_default().lines() {
            match line.split_once(' ').unwrap_or((line, "")) {
                ("cell", v) => {
                    if let Some((l, c)) = v.split_once('/') {
                        o.cells.push((l.into(), c.into()));
                    }
                }
                ("pdk", v) => {
                    o.pdk = Some(match v.split_once('/') {
                        Some((l, c)) => (l.into(), Some(c.into())),
                        None => (v.into(), None),
                    });
                }
                ("project", v) => o.projects.push(PathBuf::from(v)),
                ("target", v) => o.target = Some(PathBuf::from(v)),
                ("callbacks", _) => o.callbacks = true,
                _ => {}
            }
        }
        o
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Core devices as `schemify_core` cells (`--core-cells`): the exact round trip (§7.4).
    Core,
    /// Core devices onto analogLib masters (the CLI default, §6.5).
    Alib,
}

impl Mode {
    const ALL: [Mode; 2] = [Mode::Core, Mode::Alib];
    fn tag(self) -> &'static str {
        match self {
            Mode::Core => "core",
            Mode::Alib => "alib",
        }
    }
    fn letter(self) -> char {
        match self {
            Mode::Core => 'c',
            Mode::Alib => 'a',
        }
    }
}

/// The user-cell key in file and check names (`ALL` for a whole library).
fn ukey(lib: &str, cell: &str) -> String {
    format!("{lib}.{}", whole_or(cell))
}

fn whole_or(cell: &str) -> &str {
    if cell == "*" { "ALL" } else { cell }
}

/// The scratch library a user cell (or library) goes back to Virtuoso in; `kind` `u` for the
/// round trip, `b` for the callback load.
fn ulib(kind: char, lib: &str, cell: &str) -> String {
    format!("sfy{kind}_{lib}_{}", whole_or(cell))
}

/// The SKILL cell list of a user entry: `nil` (every cell) for a whole library.
fn cell_list(cell: &str) -> String {
    if cell == "*" {
        "nil".into()
    } else {
        format!("(list {})", q(cell))
    }
}

/// Every project a run carries: the fixtures, then the user's (`--project`), named `p_<dir>`.
fn all_projects(o: &Opts) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = PROJECTS
        .iter()
        .map(|(n, rel)| ((*n).to_owned(), project_dir(rel)))
        .collect();
    for d in &o.projects {
        let base: String = d
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        v.push((format!("p_{base}"), d.clone()));
    }
    v
}

/// Schemify library → Virtuoso library for project `name` in `mode`: every library gets a
/// `sfy<c|a>_<name>` prefix, so nothing collides with the user's libraries.
fn lib_map(name: &str, mode: Mode, project: &Path) -> BTreeMap<String, String> {
    let base = format!("sfy{}_{name}", mode.letter());
    let mut m = BTreeMap::new();
    m.insert(String::new(), base.clone());
    m.insert("core".to_owned(), format!("{base}_core"));
    if let Ok(text) = std::fs::read(project.join("schemify.project")) {
        for lib in interop::design::manifest_libs(&text)
            .unwrap_or_default()
            .into_keys()
        {
            m.insert(lib.clone(), format!("{base}_{lib}"));
        }
    }
    m
}

fn project_dir(rel: &str) -> PathBuf {
    root().join(rel)
}

/// The cell files of a project (every cell is a top: the closure adds nothing new).
fn cell_files(project: &Path) -> Vec<String> {
    read_tree(project)
        .into_keys()
        .filter(|p| is_cell(p))
        .collect()
}

/// (path, text) pairs: OCEAN scripts.
type Files = Vec<(String, String)>;

/// The Rust export of a whole project, as `schemify-interop virtuoso-export` writes it:
/// (`.sfyx`, OCEAN scripts as (path, text)).
fn export_project(
    project: &Path,
    mode: Mode,
    libs: &BTreeMap<String, String>,
) -> Result<(String, Files), Vec<String>> {
    let opts = ExportOptions {
        producer: "sfylive".into(),
        analoglib: mode == Mode::Alib,
        lib_map: libs.clone(),
        ..ExportOptions::default()
    };
    let writes = job(
        project,
        Command::VirtuosoExport {
            tops: cell_files(project),
            out: "out.sfyx".into(),
            opts,
        },
    )?;
    let mut sfyx = None;
    let mut ocean = Vec::new();
    for (p, b) in writes {
        let text = String::from_utf8(b).unwrap();
        if p == "out.sfyx" {
            sfyx = Some(text);
        } else if let Some(rest) = p.strip_prefix("ocean/") {
            ocean.push((rest.to_owned(), text));
        }
    }
    Ok((
        sfyx.ok_or_else(|| vec!["no .sfyx written".to_owned()])?,
        ocean,
    ))
}

/// The `"design"` part of a dump: every record of its design cells and maestro tests, the
/// library names `from` renamed `to` in every string field, without the header and the
/// `schemify*` properties (ids and stash, which only the Rust exporter writes).
fn design_records(text: &str, from: &[&str], to: &str) -> Result<Vec<String>, String> {
    fn rename(v: &sfyx::Value, from: &[&str], to: &str) -> sfyx::Value {
        match v {
            sfyx::Value::Str(s) if from.contains(&s.as_str()) => sfyx::Value::Str(to.to_owned()),
            sfyx::Value::List(items) => {
                sfyx::Value::List(items.iter().map(|x| rename(x, from, to)).collect())
            }
            other => other.clone(),
        }
    }
    let recs = sfyx::read(text).map_err(|e| format!("line {}: {}", e.line, e.msg))?;
    let mut out = Vec::new();
    for (at, r) in located(&recs) {
        let keep = at.design || at.maestro;
        if !keep
            || matches!(r.tag.as_str(), "sfyx" | "end" | "lib")
            || (r.tag == "prop" && r.str(1).is_ok_and(|n| n.starts_with("schemify")))
        {
            continue;
        }
        let mut fields: Vec<sfyx::Value> = r.fields.iter().map(|v| rename(v, from, to)).collect();
        // Which piece of a split wire a label stays glued to is Virtuoso's choice (a live run
        // reglued a label to the other piece): the parent ref of a label is not compared.
        if r.tag == "label" && fields.len() > 1 {
            fields[1] = sfyx::Value::Int(0);
        }
        out.push(line_of(&Record::new(&r.tag, fields)));
    }
    Ok(out)
}

/// The design cells of a dump, each with its records as [`design_records`] gives them
/// (libraries `from` renamed `to`), by cell name.
fn design_cells(text: &str, from: &[&str], to: &str) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut cur: Option<String> = None;
    for line in design_records(text, from, to).unwrap_or_default() {
        if let Some(rec) = record_of(&line).filter(|r| r.tag == "cell") {
            cur = rec.str(0).ok().map(str::to_owned);
        }
        if let Some(c) = &cur {
            out.entry(c.clone()).or_default().push(line);
        }
    }
    out
}

/// How a cell's records came back from a library round trip (`--lib`), CDF aside: the
/// first difference of the rest, the CDF params and siminfo keys lost, and those gained.
/// Gaining is expected where the import declared an empty string param for a name a symbol
/// label shows (spec `interop.md` §6.3), a netSet param for an inherited pin, or the export
/// added netlisting siminfo; a CDF without params or siminfo counts as none.
struct CellBack {
    diff: Option<String>,
    lost: Vec<String>,
    gained: Vec<String>,
}

fn cell_back(a: &[String], b: &[String]) -> CellBack {
    // A siminfo record splits into one entry per key, so an added key is an addition.
    let split = |recs: &[String]| {
        let (mut rest, mut cdf) = (Vec::new(), BTreeSet::new());
        for l in recs {
            match record_of(l) {
                Some(r) if r.tag == "cdf" => {}
                Some(r) if r.tag == "param" => {
                    cdf.insert(l.clone());
                }
                Some(r) if r.tag == "siminfo" => {
                    let Some(sfyx::Value::List(pairs)) = r.fields.get(1) else {
                        cdf.insert(l.clone());
                        continue;
                    };
                    for kv in pairs {
                        let one = Record::new(
                            &r.tag,
                            vec![r.fields[0].clone(), sfyx::Value::List(vec![kv.clone()])],
                        );
                        cdf.insert(line_of(&one));
                    }
                }
                _ => rest.push(l.clone()),
            }
        }
        (rest, cdf)
    };
    let ((ra, ca), (rb, cb)) = (split(a), split(b));
    // Expected additions: export's netlisting siminfo, an empty string param for a name a
    // symbol label shows, a netSet param for a pin with a net expression (§6.2).
    let declared = |l: &String| {
        record_of(l).is_some_and(|r| {
            r.tag == "siminfo"
                || r.str(1).is_ok_and(|t| t == "netSet")
                || (r.str(1).is_ok_and(|t| t == "string") && r.str(2).is_ok_and(str::is_empty))
        })
    };
    let gained: Vec<String> = cb.difference(&ca).cloned().collect();
    let mut lost: Vec<String> = ca.difference(&cb).cloned().collect();
    lost.extend(
        gained
            .iter()
            .filter(|l| !declared(l))
            .map(|l| format!("added {l}")),
    );
    CellBack {
        diff: first_diff(&ra, &rb),
        lost,
        gained: gained.into_iter().filter(declared).collect(),
    }
}

/// Every cell of a dump with its role (`design` or `ref`).
fn cell_roles(text: &str) -> Vec<(String, String)> {
    sfyx::read(text)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.tag == "cell")
        .filter_map(|r| Some((r.str(0).ok()?.to_owned(), r.str(1).ok()?.to_owned())))
        .collect()
}

/// The first difference of two record lists, or `None`.
fn first_diff(a: &[String], b: &[String]) -> Option<String> {
    for (k, (x, y)) in a.iter().zip(b).enumerate() {
        if x != y {
            return Some(format!("record {}: `{x}` became `{y}`", k + 1));
        }
    }
    match a.len().cmp(&b.len()) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some(format!(
            "record {} `{}` is missing",
            b.len() + 1,
            a[b.len()]
        )),
        std::cmp::Ordering::Less => Some(format!("extra record {} `{}`", a.len() + 1, b[a.len()])),
    }
}

// ---------------------------------------------------------------------------------------
// Session files

/// A generated SKILL session: loads, then the body, then `(exit)`. Lines starting with
/// `(load ` or `(exit` are what the mock run skips.
struct Session {
    text: String,
}

impl Session {
    fn new(work: &Path, n: u32) -> Session {
        let skill = work.join("skill");
        let mut text = format!(
            ";; session{n}.il: generated by plugins/interop/tests/virtuoso_live.rs; run by live/run.sh as\n\
             ;;   virtuoso -nograph -restore session{n}.il\n"
        );
        for f in ["schemify.il", "sfy_probe.il", "sfy_live.il"] {
            writeln!(text, "(load {})", q(&path_str(&skill.join(f)))).unwrap();
        }
        writeln!(
            text,
            "(SfyLiveBegin {})",
            q(&path_str(&work.join(format!("out/session{n}.txt"))))
        )
        .unwrap();
        Session { text }
    }

    fn call(&mut self, form: String) {
        self.text.push_str(&form);
        self.text.push('\n');
    }

    fn finish(mut self, path: &Path) {
        self.text.push_str("(SfyLiveEnd)\n(exit)\n");
        write(path, self.text);
    }
}

fn lib_path(work: &Path, lib: &str) -> String {
    path_str(&work.join("libs").join(lib))
}

// ---------------------------------------------------------------------------------------
// prepare

fn prepare(work: &Path, o: &Opts) {
    for d in ["in", "out", "projects", "skill"] {
        let _ = std::fs::remove_dir_all(work.join(d));
    }
    o.save(work);
    write(
        &work.join("skill/schemify.il"),
        interop::skill::schemify_il(),
    );
    write(&work.join("skill/sfy_probe.il"), interop::skill::PROBE);
    write(&work.join("skill/sfy_live.il"), LIVE_IL);
    let inp = work.join("in");
    let out = work.join("out");
    let mut libs: BTreeSet<String> = [SCRATCH.to_owned(), NEG_LIB.to_owned()].into();
    let mut s = Session::new(work, 1);
    let mut netlists: Vec<String> = Vec::new();
    let mut oceans: Vec<String> = Vec::new();
    let mut plan_fail: Vec<String> = Vec::new();

    s.call("(SfyLiveEntryPoints)".into());

    // Schemify → Virtuoso: every fixture project in both modes, and their dumps back.
    let mut loads = Vec::new();
    for (name, dir) in all_projects(o) {
        for mode in Mode::ALL {
            let map = lib_map(&name, mode, &dir);
            match export_project(&dir, mode, &map) {
                Ok((text, ocean)) => {
                    let file = inp.join(format!("{name}.{}.sfyx", mode.tag()));
                    write(&file, &text);
                    for (p, t) in ocean {
                        write(&inp.join(format!("ocean/{name}.{}/{p}", mode.tag())), t);
                    }
                    let dl = design_libs(&text);
                    libs.extend(dl.iter().cloned());
                    loads.push((name.clone(), mode, file, dl));
                }
                Err(e) => plan_fail.push(format!(
                    "FAIL export.{name}.{} {}",
                    mode.tag(),
                    e.join("; ")
                )),
            }
        }
    }
    // Refusals: the rclive export into a library of its own, truncated and malformed.
    let neg_map: BTreeMap<String, String> = [
        (String::new(), NEG_LIB.to_owned()),
        ("core".to_owned(), format!("{NEG_LIB}_core")),
    ]
    .into();
    libs.insert(format!("{NEG_LIB}_core"));
    let (neg, _) = export_project(&project_dir(PROJECTS[4].1), Mode::Core, &neg_map)
        .expect("the rclive fixture exports");
    let mut lines: Vec<&str> = neg.lines().collect();
    lines.pop(); // ("end")
    write(
        &inp.join("neg_truncated.sfyx"),
        format!("{}\n", lines.join("\n")),
    );
    let cv = neg.lines().position(|l| l.starts_with("(\"cv\"")).unwrap();
    let mut bad: Vec<&str> = neg.lines().collect();
    bad.insert(cv + 1, "(\"wire\" 1 \"not a point list\" 0)");
    write(
        &inp.join("neg_malformed.sfyx"),
        format!("{}\n", bad.join("\n")),
    );
    // Virtuoso-shaped dumps: each into a library of its own.
    for (name, rel, _) in DUMPS {
        write(
            &inp.join(format!("dump_{name}.sfyx")),
            std::fs::read(fixtures().join(rel)).unwrap(),
        );
        libs.insert(format!("sfyd_{name}"));
    }
    for (l, c) in &o.cells {
        libs.insert(ulib('u', l, c));
        if o.callbacks {
            libs.insert(ulib('b', l, c));
        }
    }
    // Retarget: the Exercise 1 benches against the user's target map.
    let mut retarget = None;
    if let Some(t) = &o.target {
        let ex = work.join("projects/exercise1");
        for (p, b) in read_tree(&fixtures().join("exercise1")) {
            write(&ex.join(p), b);
        }
        match std::fs::read(t) {
            Ok(b) => write(&ex.join("target.retarget"), b),
            Err(e) => plan_fail.push(format!(
                "FAIL retarget.map cannot read {}: {e}",
                t.display()
            )),
        }
        let opts = ExportOptions {
            producer: "sfylive".into(),
            analoglib: true,
            retarget: true,
            target: Some("target.retarget".into()),
            lib_map: [(String::new(), "sfyr_ex1".to_owned())].into(),
            ..ExportOptions::default()
        };
        let tops: Vec<String> = cell_files(&ex)
            .into_iter()
            .filter(|p| p.ends_with(".shbench"))
            .collect();
        match job(
            &ex,
            Command::VirtuosoExport {
                tops,
                out: "out.sfyx".into(),
                opts,
            },
        ) {
            Ok(writes) => {
                for (p, b) in writes {
                    if p == "out.sfyx" {
                        write(&inp.join("ex1.sfyx"), b);
                    } else if let Some(rest) = p.strip_prefix("ocean/") {
                        write(&inp.join(format!("ocean/ex1/{rest}")), b);
                    }
                }
                libs.insert("sfyr_ex1".into());
                retarget = Some(path_str(&inp.join("ex1.sfyx")));
                oceans.push("ex1".into());
            }
            Err(e) => plan_fail.push(format!("FAIL retarget.export {}", e.join("; "))),
        }
    }

    for l in &libs {
        s.call(format!("(SfyLiveLib {} {})", q(l), q(&lib_path(work, l))));
    }
    s.call(format!(
        "(SfyLiveExportLib \"analoglib.dump\" {} \"analogLib\")",
        q(&path_str(&out.join("analogLib.sfyx")))
    ));
    let (pdk_lib, pdk_cell) = match &o.pdk {
        Some((l, c)) => (Some(l.as_str()), c.as_deref()),
        None => (None, None),
    };
    s.call(format!(
        "(SfyLiveProbe {} {} {} {})",
        q(SCRATCH),
        q_opt(pdk_lib),
        q_opt(pdk_cell),
        q(&path_str(&out.join("sfy_probe.txt")))
    ));
    for (name, mode, file, dl) in &loads {
        let id = format!("{name}.{}", mode.tag());
        s.call(format!(
            "(SfyLiveImport {} {} t nil t nil t)",
            q(&format!("load.{id}")),
            q(&path_str(file))
        ));
        for lib in dl {
            s.call(format!(
                "(SfyLiveExport {} {} {} nil)",
                q(&format!("dump.{id}.{lib}")),
                q(&path_str(&out.join(format!("{id}.{lib}.sfyx")))),
                q(lib)
            ));
        }
    }
    for mode in Mode::ALL {
        let lib = format!("sfy{}_rclive", mode.letter());
        s.call(format!(
            "(SfyLivePresent {} {} \"rclive_tb\" \"maestro\")",
            q(&format!("maestro.rclive.{}", mode.tag())),
            q(&lib)
        ));
        netlists.push(format!("{lib} rclive_tb spectre"));
        oceans.push(format!("rclive.{}", mode.tag()));
    }
    netlists.push("sfya_rclive rcfilter spectre".into());
    netlists.push("sfya_rclive rcfilter auCdl".into());
    for (lib, cell) in [
        ("sfya_rclive", "rclive_tb"),
        ("sfyc_inv", "top"),
        ("sfya_elab", "opamp_tb"),
    ] {
        s.call(format!(
            "(SfyLiveSelfTest {} {} {} {} {})",
            q(&format!("selftest.{lib}.{cell}")),
            q(lib),
            q(cell),
            q(SCRATCH),
            q(&path_str(&out))
        ));
    }
    for (name, _, _) in DUMPS {
        let lib = format!("sfyd_{name}");
        s.call(format!(
            "(SfyLiveImport {} {} t {} t nil nil)",
            q(&format!("load.dump.{name}")),
            q(&path_str(&inp.join(format!("dump_{name}.sfyx")))),
            q(&lib)
        ));
        s.call(format!(
            "(SfyLiveExport {} {} {} nil)",
            q(&format!("dump.dump.{name}")),
            q(&path_str(&out.join(format!("dump_{name}.sfyx")))),
            q(&lib)
        ));
    }
    // Refusals: nothing may be created by a refused file; an existing view needs ?overwrite;
    // a cellview open for editing is never rewritten.
    for kind in ["truncated", "malformed"] {
        s.call(format!(
            "(SfyLiveImport {} {} nil nil t nil t)",
            q(&format!("refuse.{kind}")),
            q(&path_str(&inp.join(format!("neg_{kind}.sfyx"))))
        ));
        s.call(format!(
            "(SfyLiveAbsent {} {} \"rclive_tb\")",
            q(&format!("refuse.{kind}.nothing-created")),
            q(NEG_LIB)
        ));
    }
    for kind in ["exists", "open"] {
        if let Some(t) = read(&inp.join("rclive.core.sfyx")) {
            write(&inp.join(format!("neg_{kind}.sfyx")), t);
        }
    }
    s.call(format!(
        "(SfyLiveImport \"refuse.exists\" {} nil nil nil nil t)",
        q(&path_str(&inp.join("neg_exists.sfyx")))
    ));
    s.call(format!(
        "(SfyLiveOpenRefusal \"refuse.open-for-editing\" {} \"sfyc_rclive\" \"rclive_tb\")",
        q(&path_str(&inp.join("neg_open.sfyx")))
    ));
    // The user's own cells.
    for (l, c) in &o.cells {
        let k = ukey(l, c);
        if c != "*" {
            s.call(format!(
                "(SfyLiveSelfTest {} {} {} {} {})",
                q(&format!("user.selftest.{k}")),
                q(l),
                q(c),
                q(SCRATCH),
                q(&path_str(&out))
            ));
            netlists.push(format!("{l} {c} spectre"));
            netlists.push(format!("{l} {c} auCdl"));
        }
        let dump = path_str(&out.join(format!("user_{k}.sfyx")));
        s.call(format!(
            "(SfyLiveExport {} {} {} {})",
            q(&format!("user.dump.{k}")),
            q(&dump),
            q(l),
            cell_list(c)
        ));
        if o.callbacks {
            let cb = path_str(&out.join(format!("user_{k}.cb.sfyx")));
            s.call(format!("(SfyLiveCopy {} {})", q(&dump), q(&cb)));
            s.call(format!(
                "(SfyLiveImport {} {} t {} t t nil)",
                q(&format!("user.callbacks.{k}")),
                q(&cb),
                q(&ulib('b', l, c))
            ));
        }
    }
    if let Some(l) = pdk_lib {
        s.call(format!(
            "(SfyLiveExportLib \"pdk.dump\" {} {})",
            q(&path_str(&out.join("pdk.sfyx"))),
            q(l)
        ));
    }
    if let Some(file) = retarget {
        s.call(format!(
            "(SfyLiveImport \"retarget.load\" {} t nil t nil nil)",
            q(&file)
        ));
        s.call(format!(
            "(SfyLiveRetarget \"retarget.place\" {} {} nil)",
            q(&file),
            q(&path_str(&work.join("projects/exercise1/target.retarget")))
        ));
    }
    s.finish(&work.join("session1.il"));
    write(
        &work.join("libs.txt"),
        libs.iter().map(|l| format!("{l}\n")).collect::<String>(),
    );
    write(
        &work.join("netlists.txt"),
        netlists
            .iter()
            .map(|l| format!("{l}\n"))
            .collect::<String>(),
    );
    write(
        &work.join("oceans.txt"),
        oceans.iter().map(|l| format!("{l}\n")).collect::<String>(),
    );
    write(
        &out.join("prepare.txt"),
        plan_fail
            .iter()
            .map(|l| format!("{l}\n"))
            .collect::<String>(),
    );
}

// ---------------------------------------------------------------------------------------
// mid

/// Imports dump `sfyx` (an absolute path) into `project` (existing files merge, §7.3).
fn import_into(
    project: &Path,
    sfyx: &Path,
    opts: ImportOptions,
) -> Result<Vec<(String, Vec<u8>)>, Vec<String>> {
    job(
        project,
        Command::VirtuosoImport {
            sfyx: path_str(sfyx),
            opts,
        },
    )
}

fn inverse(map: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    map.iter().map(|(k, v)| (v.clone(), k.clone())).collect()
}

/// The Schemify-side edit of the teamwork check: C1's value in the RC section doubles.
const TEAM_EDIT: (&str, &str, &str) = ("rcfilter.shcomp", "  value C\n", "  value C*2\n");
/// The Virtuoso-side edit: V1's pulse high level (CDF `v2` of the `schemify_core` vsource,
/// §6.6) 1 → 2, which must come back as V1's source text `PULSE(0 2 …)`.
const TEAM_VIRTUOSO_EDIT: (&str, &str, &str) = ("v2", "2", "PULSE(0 2 ");

fn mid(work: &Path, o: &Opts) {
    let out = work.join("out");
    let inp = work.join("in");
    let mut lines: Vec<String> = Vec::new();
    let mut s = Session::new(work, 2);

    // Teamwork (§7.5): the Virtuoso dump of the RC bench (core-cell mode, where the round
    // trip is exact), imported over the project as a user would, edited in Schemify,
    // exported again.
    let rclive = project_dir(PROJECTS[4].1);
    let map = lib_map("rclive", Mode::Core, &rclive);
    let team = work.join("projects/team");
    let _ = std::fs::remove_dir_all(&team);
    for (p, b) in read_tree(&rclive) {
        write(&team.join(p), b);
    }
    let dump = out.join("rclive.core.sfyc_rclive.sfyx");
    let io = ImportOptions {
        lib_map: inverse(&map),
        ..ImportOptions::default()
    };
    match dump.exists().then(|| import_into(&team, &dump, io.clone())) {
        None => lines.push("FAIL team.import session 1 wrote no dump of sfyc_rclive".into()),
        Some(Err(e)) => lines.push(format!("FAIL team.import {}", e.join("; "))),
        Some(Ok(writes)) => {
            for (p, b) in writes {
                if is_cell(&p) || p == "schemify.project" {
                    write(&team.join(p), b);
                }
            }
            let (file, from, to) = TEAM_EDIT;
            let text = read(&team.join(file)).unwrap_or_default();
            if text.contains(from) {
                write(&team.join(file), text.replacen(from, to, 1));
                lines.push(format!(
                    "PASS team.schemify-edit {file}: `{}` -> `{}`",
                    from.trim(),
                    to.trim()
                ));
                let opts = ExportOptions {
                    producer: "sfylive".into(),
                    lib_map: map.clone(),
                    ..ExportOptions::default()
                };
                match job(
                    &team,
                    Command::VirtuosoExport {
                        tops: cell_files(&team),
                        out: "out.sfyx".into(),
                        opts,
                    },
                ) {
                    Ok(w) => {
                        let text = w
                            .iter()
                            .find(|(p, _)| p == "out.sfyx")
                            .map(|(_, b)| String::from_utf8(b.clone()).unwrap())
                            .unwrap_or_default();
                        let recs = sfyx::read(&text).unwrap_or_default();
                        let keep = recs
                            .iter()
                            .filter(|r| {
                                r.tag == "prop" && r.str(1).is_ok_and(|n| n == "schemifyKeep")
                            })
                            .count();
                        let cvs = recs.iter().filter(|r| r.tag == "cv").count();
                        lines.push(format!("INFO team.markers {keep} of {cvs}"));
                        write(&inp.join("team.sfyx"), text);
                        // Snapshot of what Schemify holds before session 2.
                        let snap = work.join("projects/team_before");
                        let _ = std::fs::remove_dir_all(&snap);
                        for (p, b) in read_tree(&team) {
                            write(&snap.join(p), b);
                        }
                        s.call(format!(
                            "(SfyLiveImport \"team.load\" {} t nil t nil t)",
                            q(&path_str(&inp.join("team.sfyx")))
                        ));
                        s.call(format!(
                            "(SfyLiveEditParam \"team.virtuoso-edit\" \"sfyc_rclive\" \"rclive_tb\" \"V1\" {} {})",
                            q(TEAM_VIRTUOSO_EDIT.0),
                            q(TEAM_VIRTUOSO_EDIT.1)
                        ));
                        s.call(format!(
                            "(SfyLiveExport \"team.dump\" {} \"sfyc_rclive\" nil)",
                            q(&path_str(&out.join("team.sfyx")))
                        ));
                    }
                    Err(e) => lines.push(format!("FAIL team.export {}", e.join("; "))),
                }
            } else {
                lines.push(format!(
                    "FAIL team.schemify-edit {file} has no `{}` after the import",
                    from.trim()
                ));
            }
        }
    }

    // The user's cells: Virtuoso dump → Rust import → Rust export → Virtuoso (a scratch
    // library) → dump again.
    for (l, c) in &o.cells {
        let k = ukey(l, c);
        let dump = out.join(format!("user_{k}.sfyx"));
        let proj = work.join(format!("projects/user_{k}"));
        let _ = std::fs::remove_dir_all(&proj);
        std::fs::create_dir_all(&proj).unwrap();
        if !dump.exists() {
            lines.push(format!("FAIL user.import.{k} session 1 wrote no dump"));
            continue;
        }
        let files = match import_into(&proj, &dump, ImportOptions::default()) {
            Ok(f) => f,
            Err(e) => {
                lines.push(format!("FAIL user.import.{k} {}", e.join("; ")));
                continue;
            }
        };
        let problems = interop::check_output(&files);
        lines.push(if problems.is_empty() {
            format!(
                "PASS user.import.{k} {} files, every cell loads",
                files.len()
            )
        } else {
            format!("FAIL user.import.{k} {}", problems.join("; "))
        });
        for (p, b) in &files {
            write(&proj.join(p), b);
        }
        let tops: Vec<String> = files
            .iter()
            .map(|(p, _)| p.clone())
            .filter(|p| is_cell(p) && p.starts_with(&format!("{l}/")))
            .filter(|p| {
                let t = read(&proj.join(p)).unwrap_or_default();
                !t.contains("\n  provenance virtuoso")
            })
            .collect();
        let copy = ulib('u', l, c);
        let opts = ExportOptions {
            producer: "sfylive".into(),
            lib_map: [(l.clone(), copy.clone())].into(),
            ..ExportOptions::default()
        };
        match job(
            &proj,
            Command::VirtuosoExport {
                tops,
                out: "out.sfyx".into(),
                opts,
            },
        ) {
            Ok(w) => {
                if let Some((_, b)) = w.iter().find(|(p, _)| p == "out.sfyx") {
                    let file = inp.join(format!("user_{k}.rt.sfyx"));
                    write(&file, b);
                    s.call(format!(
                        "(SfyLiveImport {} {} t nil t nil nil)",
                        q(&format!("user.load.{k}")),
                        q(&path_str(&file))
                    ));
                    s.call(format!(
                        "(SfyLiveExport {} {} {} {})",
                        q(&format!("user.dump2.{k}")),
                        q(&path_str(&out.join(format!("user_{k}.second.sfyx")))),
                        q(&copy),
                        cell_list(c)
                    ));
                }
            }
            Err(e) => lines.push(format!("FAIL user.export.{k} {}", e.join("; "))),
        }
    }

    // The PDK mirror (§8.1).
    if o.pdk.is_some() {
        let dump = out.join("pdk.sfyx");
        let proj = work.join("projects/pdk");
        let _ = std::fs::remove_dir_all(&proj);
        std::fs::create_dir_all(&proj).unwrap();
        match dump
            .exists()
            .then(|| import_into(&proj, &dump, ImportOptions::default()))
        {
            None => lines.push("FAIL pdk.import session 1 wrote no pdk.sfyx".into()),
            Some(Err(e)) => lines.push(format!("FAIL pdk.import {}", e.join("; "))),
            Some(Ok(files)) => {
                let n = files.iter().filter(|(p, _)| p.ends_with(".shprim")).count();
                let problems = interop::check_output(&files);
                lines.push(if problems.is_empty() && n > 0 {
                    format!("PASS pdk.import {n} device cells, every one loads")
                } else if n == 0 {
                    "FAIL pdk.import no device cell came out (cells without a symbol view are skipped)".into()
                } else {
                    format!("FAIL pdk.import {}", problems.join("; "))
                });
                for (p, b) in &files {
                    write(&proj.join(p), b);
                }
            }
        }
    }

    s.finish(&work.join("session2.il"));
    write(
        &out.join("mid.txt"),
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    );
}

// ---------------------------------------------------------------------------------------
// verify

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Grade {
    Fail,
    Skip,
    Warn,
    Pass,
    Info,
}

impl Grade {
    fn word(self) -> &'static str {
        match self {
            Grade::Fail => "FAIL",
            Grade::Skip => "SKIP",
            Grade::Warn => "WARN",
            Grade::Pass => "PASS",
            Grade::Info => "INFO",
        }
    }
}

struct Report {
    rows: Vec<(String, Grade, String)>,
}

impl Report {
    fn add(&mut self, id: impl Into<String>, g: Grade, detail: impl Into<String>) {
        self.rows.push((
            id.into(),
            g,
            detail.into().replace('|', "\\|").replace('\n', " "),
        ));
    }
    fn check(&mut self, id: impl Into<String>, ok: bool, detail: impl Into<String>) {
        self.add(id, if ok { Grade::Pass } else { Grade::Fail }, detail);
    }
    /// Lines `PASS|FAIL|INFO <id> <detail>` from a session or the mid step.
    fn take(&mut self, text: &str) {
        for l in text.lines() {
            let mut it = l.splitn(3, ' ');
            let (k, id, d) = (
                it.next().unwrap_or(""),
                it.next().unwrap_or(""),
                it.next().unwrap_or(""),
            );
            let g = match k {
                "PASS" => Grade::Pass,
                "FAIL" => Grade::Fail,
                "INFO" => Grade::Info,
                _ => continue,
            };
            self.add(id, g, d);
        }
    }
    fn fails(&self) -> Vec<&(String, Grade, String)> {
        self.rows.iter().filter(|r| r.1 == Grade::Fail).collect()
    }
}

/// The Rust round trip of one fixture project in core mode: the dumps of its libraries
/// import to the original files byte for byte (fingerprints aside, §7.4 with the stash).
fn check_exact_round_trip(r: &mut Report, work: &Path, name: &str, dir: &Path) {
    let map = lib_map(name, Mode::Core, dir);
    let id = format!("roundtrip.{name}.core");
    let Some(text) = read(&work.join(format!("in/{name}.core.sfyx"))) else {
        return r.add(id, Grade::Fail, "no export");
    };
    let proj = work.join(format!("projects/rt_{name}"));
    let _ = std::fs::remove_dir_all(&proj);
    std::fs::create_dir_all(&proj).unwrap();
    if let Ok(m) = std::fs::read(dir.join("schemify.project")) {
        write(&proj.join("schemify.project"), m);
    }
    let io = ImportOptions {
        lib_map: inverse(&map),
        ..ImportOptions::default()
    };
    let mut got = BTreeMap::new();
    for lib in design_libs(&text) {
        let dump = work.join(format!("out/{name}.core.{lib}.sfyx"));
        if !dump.exists() {
            return r.add(id, Grade::Fail, format!("Virtuoso wrote no dump of {lib}"));
        }
        match import_into(&proj, &dump, io.clone()) {
            Ok(files) => {
                for (p, b) in files {
                    if is_cell(&p) {
                        got.insert(p, strip_fp(&String::from_utf8_lossy(&b)));
                    }
                }
            }
            Err(e) => {
                return r.add(
                    id,
                    Grade::Fail,
                    format!("import of {lib}: {}", e.join("; ")),
                );
            }
        }
    }
    let want: BTreeMap<String, String> = read_tree(dir)
        .into_iter()
        .filter(|(p, _)| is_cell(p))
        .map(|(p, b)| (p, canonical(&b)))
        .collect();
    // Both versions of every file that differs, for `diff -u` (test.sh prints them).
    let diffs = work.join("out/diffs").join(name);
    for (p, w) in &want {
        if let Some(g) = got.get(p).filter(|g| *g != w) {
            write(&diffs.join(format!("{p}.want")), w);
            write(&diffs.join(format!("{p}.got")), g);
        }
    }
    for (p, w) in &want {
        match got.get(p) {
            None => return r.add(id, Grade::Fail, format!("{p} did not come back")),
            Some(g) if g != w => {
                let (k, (a, b)) = w
                    .lines()
                    .zip(g.lines())
                    .enumerate()
                    .find(|(_, (a, b))| a != b)
                    .unwrap_or((w.lines().count().min(g.lines().count()), ("<end>", "<end>")));
                return r.add(
                    id,
                    Grade::Fail,
                    format!("{p} line {}: `{a}` came back as `{b}`", k + 1),
                );
            }
            _ => {}
        }
    }
    r.add(
        id,
        Grade::Pass,
        format!("{} cells byte-identical after Virtuoso", want.len()),
    );
}

/// Elaborates `cell` of a project (its files plus the core library) for `dialect`.
fn netlist_of(
    files: &BTreeMap<String, Vec<u8>>,
    cell: &str,
    dialect: schemify_elab::Dialect,
) -> Result<String, String> {
    use schemify_doc::{Project, core_library};
    use schemify_elab::{Request, elaborate, no_funcs};
    let manifest = files.get("schemify.project").map(Vec::as_slice);
    let mut p = Project::new(manifest, None, 7).map_err(|e| format!("{e:?}"))?;
    let dirs = manifest
        .map(|m| interop::design::manifest_libs(m).unwrap_or_default())
        .unwrap_or_default();
    let mut libs: BTreeMap<String, Vec<(&str, &[u8])>> = BTreeMap::new();
    libs.insert("core".into(), core_library().to_vec());
    for (path, bytes) in files.iter().filter(|f| is_cell(f.0)) {
        let (dir, rel) = path.rsplit_once('/').unwrap_or(("", path));
        let lib = dirs
            .iter()
            .find(|(_, d)| d.as_str() == dir)
            .map_or(String::new(), |(l, _)| l.clone());
        libs.entry(lib).or_default().push((rel, bytes));
    }
    for (lib, listing) in &libs {
        p.index_library(lib, "live", listing)
            .map_err(|e| format!("{e:?}"))?;
    }
    let mut handles = BTreeMap::new();
    for (lib, listing) in &libs {
        if lib == "core" {
            continue;
        }
        for (rel, bytes) in listing {
            let o = p
                .open(lib, rel, bytes, None)
                .map_err(|e| format!("{lib}/{rel}: {e:?}"))?;
            let key = if lib.is_empty() {
                (*rel).to_owned()
            } else {
                format!("{}/{rel}", dirs[lib])
            };
            handles.insert(key, o.handle);
        }
    }
    let h = *handles
        .get(cell)
        .ok_or_else(|| format!("{cell} not found"))?;
    let req = Request {
        config: None,
        corner: None,
        overrides: &[],
        env: &[],
        dialect,
    };
    let e = elaborate(&p, h, &req, &mut no_funcs);
    let blocking: Vec<String> = e
        .violations
        .rows
        .iter()
        .filter(|v| v.code.severity() == schemify_elab::Severity::Error)
        .map(|v| format!("{} {}", v.code.as_str(), v.detail))
        .collect();
    if !blocking.is_empty() {
        return Err(blocking.join("; "));
    }
    Ok(schemify_netlist::emit(&e, dialect).text)
}

/// The analogLib-mode round trip of one fixture project: every bench (else every
/// component) netlists the same before and after Virtuoso.
fn check_alib_round_trip(r: &mut Report, work: &Path, name: &str, dir: &Path) {
    let map = lib_map(name, Mode::Alib, dir);
    let id = format!("roundtrip.{name}.alib");
    let Some(text) = read(&work.join(format!("in/{name}.alib.sfyx"))) else {
        return r.add(id, Grade::Fail, "no export");
    };
    let proj = work.join(format!("projects/rta_{name}"));
    let _ = std::fs::remove_dir_all(&proj);
    std::fs::create_dir_all(&proj).unwrap();
    if let Ok(m) = std::fs::read(dir.join("schemify.project")) {
        write(&proj.join("schemify.project"), m);
    }
    let io = ImportOptions {
        lib_map: inverse(&map),
        analoglib: true,
        ..ImportOptions::default()
    };
    for lib in design_libs(&text) {
        let dump = work.join(format!("out/{name}.alib.{lib}.sfyx"));
        if !dump.exists() {
            return r.add(id, Grade::Fail, format!("Virtuoso wrote no dump of {lib}"));
        }
        match import_into(&proj, &dump, io.clone()) {
            Ok(files) => {
                for (p, b) in files {
                    write(&proj.join(p), b);
                }
            }
            Err(e) => {
                return r.add(
                    id,
                    Grade::Fail,
                    format!("import of {lib}: {}", e.join("; ")),
                );
            }
        }
    }
    let before = read_tree(dir);
    let after = read_tree(&proj);
    let mut tops: Vec<&String> = before.keys().filter(|p| p.ends_with(".shbench")).collect();
    if tops.is_empty() {
        tops = before.keys().filter(|p| p.ends_with(".shcomp")).collect();
    }
    let mut compared = 0;
    for t in tops {
        let want = match netlist_of(&before, t, schemify_elab::Dialect::Ngspice) {
            Ok(n) => n,
            Err(e) => {
                r.add(
                    format!("{id}.{t}"),
                    Grade::Info,
                    format!("not netlistable without its plugins: {e}"),
                );
                continue;
            }
        };
        match netlist_of(&after, t, schemify_elab::Dialect::Ngspice) {
            Ok(got) => {
                let cmp =
                    schemify_spicecmp::compare(&want, &got, &schemify_spicecmp::Options::default());
                if !cmp.is_equal() {
                    return r.add(
                        id,
                        Grade::Fail,
                        format!("{t}: the ngspice netlist changed: {:?}", cmp.diffs.first()),
                    );
                }
                compared += 1;
            }
            Err(e) => return r.add(id, Grade::Fail, format!("{t} after Virtuoso: {e}")),
        }
    }
    r.add(
        id,
        Grade::Pass,
        format!("{compared} netlists equivalent after Virtuoso (analogLib masters)"),
    );
}

/// Pin-pattern alignment against the analogLib this Virtuoso has (spec §6.4, §6.5): the
/// shipped geometry table (`devices::alib_pins`) must equal the live symbols, since export
/// places re-mastered devices by it; per core device, whether its pins align with the
/// analogLib cell's (no stubs), partly, or not at all; and the analogLib-mode RC bench, all
/// of whose devices align, must come back from Virtuoso without a stub.
fn check_alib_pins(r: &mut Report, work: &Path) {
    use interop::virtuoso::devices;
    let Some(text) = read(&work.join("out/analogLib.sfyx")) else {
        return r.add("analoglib.table", Grade::Fail, "no dump of analogLib");
    };
    let Ok(recs) = sfyx::read(&text) else {
        return r.add(
            "analoglib.table",
            Grade::Fail,
            "the analogLib dump does not read",
        );
    };
    // analogLib cell → terminal → pin centre, milli-units, y down.
    let mut live: BTreeMap<String, BTreeMap<String, [i32; 2]>> = BTreeMap::new();
    for (at, rec) in located(&recs) {
        let cell = at.cell;
        match rec.tag.as_str() {
            "pinfig" if at.view == "symbol" => {
                let (Ok(t), Ok(x1), Ok(y1), Ok(x2), Ok(y2)) =
                    (rec.str(1), rec.int(2), rec.int(3), rec.int(4), rec.int(5))
                else {
                    continue;
                };
                // Dump units (1000 per DBU) are milli-units; y flips.
                let c = [(x1 + x2) / 2, -((y1 + y2) / 2)];
                live.entry(cell.clone())
                    .or_default()
                    .entry(t.to_owned())
                    .or_insert(c);
            }
            _ => {}
        }
    }
    let show = |p: [i32; 2]| format!("({},{})", p[0] / 1000, p[1] / 1000);
    let cells: BTreeSet<&str> = devices::DEVICES
        .iter()
        .flat_map(|d| d.alib.iter().copied())
        .chain(["gnd", "vdd"])
        .collect();
    for a in &cells {
        let id = format!("analoglib.table.{a}");
        let (Some(table), Some(got)) = (devices::alib_pins(a), live.get(*a)) else {
            r.add(id, Grade::Info, "not in this analogLib or not in the table");
            continue;
        };
        let off: Vec<String> = table
            .iter()
            .filter(|(t, p)| got.get(*t) != Some(p))
            .map(|(t, p)| {
                let g = got.get(*t).map_or("missing".to_owned(), |q| show(*q));
                format!("{t} table {} live {g}", show(*p))
            })
            .collect();
        if off.is_empty() {
            r.add(id, Grade::Pass, "the live symbol has the table's pins");
        } else {
            r.add(
                id,
                Grade::Fail,
                format!("re-mastered devices would misplace: {}", off.join("; ")),
            );
        }
    }
    // Core pins, symbol frame.
    let mut core: BTreeMap<String, Vec<[i32; 2]>> = BTreeMap::new();
    for (file, bytes) in schemify_doc::core_library() {
        let name = file.trim_end_matches(".shprim").to_owned();
        for l in String::from_utf8_lossy(bytes).lines() {
            let w: Vec<&str> = l.split_whitespace().collect();
            if let ["pin", _, _, x, y, ..] = w[..]
                && let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>())
            {
                core.entry(name.clone())
                    .or_default()
                    .push([(x * 1000.0).round() as i32, (y * 1000.0).round() as i32]);
            }
        }
    }
    for d in devices::DEVICES {
        let Some(cp) = core.get(d.core) else { continue };
        for a in d.alib {
            let Some(table) = devices::alib_pins(a) else {
                continue;
            };
            let new: Vec<Option<[i32; 2]>> = d
                .alib_pins
                .iter()
                .take(cp.len())
                .map(|t| table.iter().find(|x| x.0 == *t).map(|x| x.1))
                .collect();
            let old = &cp[..new.len()];
            let id = format!("align.{}.{a}", d.core);
            match interop::align::fit(old, &new, schemify_format::Orient::R0) {
                Some(f) if f.full() => r.add(
                    id,
                    Grade::Pass,
                    format!(
                        "pins line up with the analogLib origin at {}",
                        show(f.origin)
                    ),
                ),
                Some(f) => r.add(
                    id,
                    Grade::Info,
                    format!(
                        "{} of {} pins line up; the others get stubs",
                        f.hit.iter().filter(|h| **h).count(),
                        f.hit.len()
                    ),
                ),
                None => r.add(
                    id,
                    Grade::Info,
                    "another pin pattern: every pin gets a stub",
                ),
            }
        }
    }
    // The RC bench on analogLib, every device aligned: no stub came back.
    if let Some(dump) = read(&work.join("out/rclive.alib.sfya_rclive.sfyx")) {
        let stubs = sfyx::read(&dump)
            .unwrap_or_default()
            .iter()
            .filter(|x| x.tag == "prop" && x.str(1).is_ok_and(|n| n == "schemifyStub"))
            .count();
        r.check(
            "align.rclive.no-stubs",
            stubs == 0,
            format!("{stubs} stub wire(s) in the analogLib-mode RC bench"),
        );
    }
}

/// The last number in an OCEAN `ocnPrint` table.
fn last_number(text: &str) -> Option<f64> {
    text.split_whitespace()
        .rev()
        .find_map(|w| w.parse::<f64>().ok())
}

fn verify(work: &Path, o: &Opts) -> Report {
    let out = work.join("out");
    let mut r = Report { rows: Vec::new() };
    r.take(&read(&out.join("prepare.txt")).unwrap_or_default());
    for n in [1, 2] {
        match read(&out.join(format!("session{n}.txt"))) {
            Some(t) => {
                r.take(&t);
                if !t.contains("INFO session end") {
                    r.add(
                        format!("session{n}"),
                        Grade::Fail,
                        format!(
                            "stopped early: see out/CDS.session{n}.log for the dialog or error"
                        ),
                    );
                }
            }
            None if n == 1 => {
                // Nothing else can be graded: say so once instead of failing every check.
                let why = "Virtuoso did not start or did not load session1.il: see \
                           out/CDS.session1.log and run.sh's terminal output (a host that cannot \
                           run Virtuoso shows an OpenAccess or licence error there)";
                r.add("session1", Grade::Fail, why);
                let md = format!(
                    "# Virtuoso live test report\n\nVerdict: **NOT RUN**: Virtuoso did not start.\n\n\
                     | Check | Result | Detail |\n|---|---|---|\n| session1 | FAIL | {why} |\n"
                );
                write(&out.join("report.md"), md);
                return r;
            }
            None => r.add(
                format!("session{n}"),
                Grade::Fail,
                format!("did not run: see out/CDS.session{n}.log"),
            ),
        }
        if n == 1 {
            r.take(&read(&out.join("mid.txt")).unwrap_or_default());
        }
    }

    // Probe: every section that failed (GUESSes of spec §13 it could not settle).
    match read(&out.join("sfy_probe.txt")) {
        None => r.add(
            "probe.file",
            Grade::Fail,
            "no /tmp/sfy_probe.txt was written",
        ),
        Some(t) => {
            let errors: Vec<&str> = t.lines().filter(|l| l.contains(" ERROR")).collect();
            r.add(
                "probe.sections",
                if errors.is_empty() {
                    Grade::Pass
                } else {
                    Grade::Warn
                },
                if errors.is_empty() {
                    "every section ran".to_owned()
                } else {
                    errors.join("; ")
                },
            );
        }
    }

    // Round trips of the fixture projects.
    for (name, dir) in all_projects(o) {
        check_exact_round_trip(&mut r, work, &name, &dir);
        check_alib_round_trip(&mut r, work, &name, &dir);
    }
    // Virtuoso-shaped dumps: load → dump is the identity on the design records.
    for (name, rel, lib) in DUMPS {
        let id = format!("dumpfix.{name}");
        let want = design_records(
            &read(&fixtures().join(rel)).unwrap(),
            &[lib],
            &format!("sfyd_{name}"),
        );
        match (want, read(&out.join(format!("dump_{name}.sfyx")))) {
            (_, None) => r.add(id, Grade::Fail, "no dump"),
            (Err(e), _) => r.add(id, Grade::Fail, e),
            (Ok(w), Some(t)) => match design_records(&t, &[], "") {
                Err(e) => r.add(id, Grade::Fail, format!("the dump does not read: {e}")),
                Ok(g) => match first_diff(&w, &g) {
                    None => r.add(
                        id,
                        Grade::Pass,
                        format!("{} design records identical", w.len()),
                    ),
                    Some(d) => r.add(id, Grade::Fail, d),
                },
            },
        }
    }
    // Load logs: schCheck results of every loaded schematic.
    for (name, _) in all_projects(o) {
        for mode in Mode::ALL {
            let log =
                read(&work.join(format!("in/{name}.{}.sfyx.log", mode.tag()))).unwrap_or_default();
            let bad: Vec<&str> = log
                .lines()
                .filter(|l| l.starts_with("check ") && !l.contains(": 0 errors"))
                .collect();
            if !bad.is_empty() {
                r.add(
                    format!("schcheck.{name}.{}", mode.tag()),
                    Grade::Fail,
                    bad.join("; "),
                );
            }
        }
    }

    // Teamwork (§7.5).
    if let Some(t) = read(&out.join("mid.txt")) {
        let markers = t
            .lines()
            .find_map(|l| l.strip_prefix("INFO team.markers "))
            .and_then(|v| v.split(' ').next()?.parse::<usize>().ok());
        let log = read(&work.join("in/team.sfyx.log")).unwrap_or_default();
        let kept = log
            .lines()
            .find_map(|l| l.strip_prefix("count kept unchanged "))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if let Some(m) = markers {
            r.check(
                "team.kept-unchanged",
                m > 0 && kept == m,
                format!("{kept} cellviews kept as Virtuoso drew them, {m} unchanged in Schemify"),
            );
        }
        let before = work.join("projects/team_before");
        let dump = out.join("team.sfyx");
        if before.exists() {
            let id = "team.edits-arrive";
            if !dump.exists() {
                r.add(id, Grade::Fail, "session 2 wrote no dump");
            } else {
                let map = lib_map("rclive", Mode::Core, &project_dir(PROJECTS[4].1));
                let io = ImportOptions {
                    lib_map: inverse(&map),
                    ..ImportOptions::default()
                };
                match import_into(&before, &dump, io) {
                    Err(e) => r.add(id, Grade::Fail, e.join("; ")),
                    Ok(files) => {
                        let mut changed = Vec::new();
                        for (p, b) in files.iter().filter(|(p, _)| is_cell(p)) {
                            let old = strip_fp(&read(&before.join(p)).unwrap_or_default());
                            let new = strip_fp(&String::from_utf8_lossy(b));
                            for (a, b) in old.lines().zip(new.lines()).filter(|(a, b)| a != b) {
                                changed.push(format!("{p}: `{}` -> `{}`", a.trim(), b.trim()));
                            }
                            if old.lines().count() != new.lines().count() {
                                changed.push(format!(
                                    "{p}: {} lines became {}",
                                    old.lines().count(),
                                    new.lines().count()
                                ));
                            }
                        }
                        let schemify_edit = files.iter().any(|(p, b)| {
                            p == TEAM_EDIT.0 && String::from_utf8_lossy(b).contains(TEAM_EDIT.2)
                        });
                        let virtuoso_edit = changed.len() == 1
                            && changed[0].to_uppercase().contains(TEAM_VIRTUOSO_EDIT.2);
                        r.check(
                            id,
                            schemify_edit && virtuoso_edit,
                            format!(
                                "Schemify edit kept: {schemify_edit}; changes after Virtuoso: {}",
                                if changed.is_empty() {
                                    "none".into()
                                } else {
                                    changed.join("; ")
                                }
                            ),
                        );
                    }
                }
            }
        }
    }

    // The user's cells: dump → Rust → Virtuoso → dump is the identity on the design records.
    for (l, c) in &o.cells {
        let k = ukey(l, c);
        let id = format!("user.roundtrip.{k}");
        match (
            read(&out.join(format!("user_{k}.sfyx"))),
            read(&out.join(format!("user_{k}.second.sfyx"))),
        ) {
            (Some(a), Some(b)) => {
                // A cell that is only a symbol and CDF came into Schemify as a device mirror
                // and goes back as a reference to the original cell (§8.1), not a copy: the
                // cells the export sent as designs must dump the same; the others stay
                // where they were, and instances of them keep pointing at the original
                // library (both library names compare as one).
                let sent: BTreeSet<String> = read(&work.join(format!("in/user_{k}.rt.sfyx")))
                    .map(|t| cell_roles(&t))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|x| x.1 == "design")
                    .map(|x| x.0)
                    .collect();
                let copy = ulib('u', l, c);
                let (a, b) = (
                    design_cells(&a, &[l, &copy], "L"),
                    design_cells(&b, &[l, &copy], "L"),
                );
                let mut diffs = Vec::new();
                let mut lost = Vec::new();
                let mut gained = 0;
                for (cell, recs) in a.iter().filter(|x| sent.contains(x.0)) {
                    match b.get(cell) {
                        Some(got) => {
                            let back = cell_back(recs, got);
                            if let Some(d) = back.diff {
                                diffs.push(format!("{cell}: {d}"));
                            }
                            if !back.lost.is_empty() {
                                diffs.push(format!("{cell}: CDF lost {}", back.lost.join(", ")));
                            }
                            gained += usize::from(!back.gained.is_empty());
                        }
                        None => lost.push(cell.clone()),
                    }
                }
                if sent.is_empty() {
                    diffs.push("the export sent no design cell".into());
                }
                let as_refs = a.len() - a.keys().filter(|x| sent.contains(*x)).count();
                if diffs.is_empty() && lost.is_empty() {
                    r.add(
                        id,
                        Grade::Pass,
                        format!(
                            "{} cells dump the same ({gained} with label params declared or siminfo added); {as_refs} device cell(s) went back as references to the original",
                            a.len() - as_refs
                        ),
                    );
                } else {
                    let mut why = diffs;
                    if !lost.is_empty() {
                        why.push(format!("did not come back: {}", lost.join(", ")));
                    }
                    r.add(id, Grade::Fail, why.join("; "));
                }
            }
            _ => r.add(id, Grade::Fail, "a dump is missing"),
        }
        if o.callbacks {
            let log = read(&out.join(format!("user_{k}.cb.sfyx.log"))).unwrap_or_default();
            let failed: Vec<&str> = log
                .lines()
                .filter(|l| l.starts_with("callback failed"))
                .collect();
            r.check(
                format!("user.callbacks-run.{k}"),
                failed.is_empty(),
                if failed.is_empty() {
                    "every callback ran".into()
                } else {
                    failed.join("; ")
                },
            );
            for l in log
                .lines()
                .filter(|l| l.starts_with("clamped") || l.starts_with("derived"))
            {
                r.add(format!("user.callbacks.{k}"), Grade::Info, l);
            }
        }
    }
    if let Some(log) = read(&work.join("in/ex1.sfyx.retarget.log")) {
        r.check(
            "retarget.log",
            log.trim_end().ends_with("PASS"),
            log.lines()
                .filter(|l| l.starts_with("inst "))
                .collect::<Vec<_>>()
                .join("; "),
        );
    }

    // Netlists Virtuoso wrote (run.sh, si).
    let nl = out.join("netlist");
    if read(&nl.join("SKIPPED")).is_some() || !nl.exists() {
        r.add(
            "netlist",
            Grade::Skip,
            read(&nl.join("SKIPPED"))
                .unwrap_or_else(|| "si was not run".into())
                .trim()
                .to_owned(),
        );
    } else {
        for line in read(&work.join("netlists.txt")).unwrap_or_default().lines() {
            let v: Vec<&str> = line.split(' ').collect();
            let [lib, cell, sim] = v[..] else { continue };
            let id = format!("netlist.{sim}.{lib}.{cell}");
            let status = read(&nl.join(format!("{lib}.{cell}.{sim}.status"))).unwrap_or_default();
            let net = read(&nl.join(format!("{lib}.{cell}.{sim}.net"))).unwrap_or_default();
            // auCdl depends on the site's si setup more than on the plugin: a WARN.
            let ok = status.trim() == "0" && !net.trim().is_empty();
            let grade = match (ok, sim) {
                (true, _) => Grade::Pass,
                (false, "auCdl") => Grade::Warn,
                _ => Grade::Fail,
            };
            r.add(
                &id,
                grade,
                if net.trim().is_empty() {
                    format!(
                        "si exit {}: see out/netlist/{lib}.{cell}.{sim}.log",
                        status.trim()
                    )
                } else {
                    format!("{} lines", net.lines().count())
                },
            );
            if lib == "sfya_rclive"
                && cell == "rcfilter"
                && sim == "auCdl"
                && !net.trim().is_empty()
            {
                // G7: our CDL of the same cell against Virtuoso's auCdl.
                let files = read_tree(&project_dir(PROJECTS[4].1));
                match netlist_of(&files, "rcfilter.shcomp", schemify_elab::Dialect::Cdl) {
                    Ok(ours) => {
                        let c = schemify_spicecmp::compare(
                            &ours,
                            &net,
                            &schemify_spicecmp::Options::default(),
                        );
                        r.add(
                            "netlist.cdl-matches-aucdl",
                            if c.is_equal() {
                                Grade::Pass
                            } else {
                                Grade::Warn
                            },
                            if c.is_equal() {
                                "Schemify CDL = Virtuoso auCdl".into()
                            } else {
                                format!("differs (settles GUESS G7): {:?}", c.diffs.first())
                            },
                        );
                    }
                    Err(e) => r.add("netlist.cdl-matches-aucdl", Grade::Warn, e),
                }
            }
        }
    }

    // OCEAN runs of the exported scripts (run.sh): the RC bench against R*C.
    let oc = out.join("ocean");
    if read(&oc.join("SKIPPED")).is_some() || !oc.exists() {
        r.add(
            "ocean",
            Grade::Skip,
            read(&oc.join("SKIPPED"))
                .unwrap_or_else(|| "ocean was not run".into())
                .trim()
                .to_owned(),
        );
    } else {
        for d in read(&work.join("oceans.txt")).unwrap_or_default().lines() {
            let status = read(&oc.join(format!("{d}.status"))).unwrap_or_default();
            r.check(
                format!("ocean.{d}.ran"),
                status.trim() == "0",
                format!("ocean exit {}: see out/ocean/{d}.log", status.trim()),
            );
            if d.starts_with("rclive.") {
                for (o_name, want) in [("tau", 1e-3), ("v1m", 1.0 - (-1.0f64).exp())] {
                    let f = work.join(format!("ocean/{d}/rclive_tb/{o_name}.txt"));
                    match read(&f).as_deref().and_then(last_number) {
                        Some(v) => r.check(
                            format!("ocean.{d}.{o_name}"),
                            ((v - want) / want).abs() < 0.01,
                            format!("{v:e}, expected {want:e} within 1%"),
                        ),
                        None => r.add(
                            format!("ocean.{d}.{o_name}"),
                            Grade::Fail,
                            format!("no value in {}", path_str(&f)),
                        ),
                    }
                }
            }
        }
    }

    check_alib_pins(&mut r, work);

    // Optional areas left out.
    if o.cells.is_empty() {
        r.add(
            "user",
            Grade::Skip,
            "no --cell given: your own designs were not tested",
        );
    }
    if o.pdk.is_none() {
        r.add(
            "pdk",
            Grade::Skip,
            "no --pdk given: the PDK mirror and a PDK device in the probe were not tested",
        );
    }
    if o.target.is_none() {
        r.add(
            "retarget",
            Grade::Skip,
            "no --retarget given: gm/Id retarget was not tested",
        );
    }
    if o.cells.is_empty() || !o.callbacks {
        r.add(
            "callbacks",
            Grade::Skip,
            "no --callbacks with --cell: PDK CDF callbacks were not tested",
        );
    }

    let mut md = String::from("# Virtuoso live test report\n\n");
    let version = r
        .rows
        .iter()
        .find(|x| x.0 == "session" && x.2.starts_with("virtuoso"))
        .map(|x| x.2.clone())
        .unwrap_or_default();
    let fails = r.fails().len();
    let skips = r.rows.iter().filter(|x| x.1 == Grade::Skip).count();
    let verdict = if fails > 0 {
        format!("**NOT FULLY FUNCTIONAL**: {fails} check(s) failed")
    } else if skips > 0 {
        format!("**PASS on everything tested**, {skips} area(s) not tested (see SKIP rows)")
    } else {
        "**FULLY FUNCTIONAL**: every check passed".to_owned()
    };
    writeln!(
        md,
        "{version}\n\nVerdict: {verdict}\n\n| Check | Result | Detail |\n|---|---|---|"
    )
    .unwrap();
    let mut rows = r.rows.clone();
    rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    for (id, g, d) in rows
        .iter()
        .filter(|x| (x.0 != "session" && !x.0.starts_with("lib.")) || x.1 == Grade::Fail)
    {
        writeln!(md, "| {id} | {} | {d} |", g.word()).unwrap();
    }
    write(&out.join("report.md"), &md);
    r
}

// ---------------------------------------------------------------------------------------
// Entry points

/// One kit step against a real Virtuoso's outputs (`live/run.sh`): `SCHEMIFY_LIVE_DIR` is the
/// work directory, `SCHEMIFY_LIVE_STEP` one of `prepare`, `mid`, `verify`.
#[test]
#[ignore = "run by plugins/interop/live/run.sh"]
fn live_step() {
    let work = PathBuf::from(std::env::var("SCHEMIFY_LIVE_DIR").expect("SCHEMIFY_LIVE_DIR"));
    let step = std::env::var("SCHEMIFY_LIVE_STEP").expect("SCHEMIFY_LIVE_STEP");
    match step.as_str() {
        "prepare" => prepare(&work, &Opts::from_env()),
        "mid" => mid(&work, &Opts::load(&work)),
        "verify" => {
            let r = verify(&work, &Opts::load(&work));
            println!("{}", read(&work.join("out/report.md")).unwrap_or_default());
            assert!(
                r.fails().is_empty(),
                "{} check(s) failed: see {}",
                r.fails().len(),
                path_str(&work.join("out/report.md"))
            );
        }
        other => panic!("unknown step {other}"),
    }
}

/// Every procedure the generated sessions and sfy_live.il call reads in the SKILL subset.
#[test]
fn sfy_live_reads_in_the_subset() {
    skill::read_all(LIVE_IL).unwrap_or_else(|e| panic!("sfy_live.il: {e}"));
}

/// The mock Virtuoso a kit run talks to: every library the kit creates, analogLib with the
/// masters the analogLib-mode exports use, and the two functions the mock lacks.
fn mock(work: &Path) -> Interp<MockDb> {
    let libs = read(&work.join("libs.txt")).unwrap();
    let mut all = vec!["analogLib", "basic"];
    all.extend(libs.lines());
    let mut i = skill::virtuoso(&all);
    // Every new maestro test starts with a PDK's default model files, as GF22's libInit
    // gives them (live run): the loads must clear them.
    i.host.mae.default_env = vec![(
        "modelFiles".to_owned(),
        skill::V::list(vec![skill::V::list(vec![
            skill::V::str("/pdk/design_wrapper.lib.scs"),
            skill::V::str("tt_pre"),
        ])]),
    )];
    i.load(interop::skill::PROBE).unwrap();
    i.load(LIVE_IL).unwrap();
    i.load(BUILD).unwrap();
    i.load("(SfyFixStdlib)").unwrap();
    i.load(
        r#"
(procedure (ddCreateLib n p) (ddGetObj n))
;; analogLib two-terminal masters as the live ICADVM20.1 analogLib draws them
;; (devices::alib_pins): PLUS at the origin, MINUS 60 DBU below.
(procedure (SfyLiveFixTwo cell params)
  (let ((cdf nil))
    (SfyFixSymbol "analogLib" cell (list (list "PLUS" "inputOutput" 0 0) (list "MINUS" "inputOutput" 0 -60)))
    (setq cdf (or (cdfGetBaseCellCDF (ddGetObj "analogLib" cell)) (SfyFixCdf "analogLib" cell)))
    (foreach p params
      (cdfCreateParam cdf ?name p ?type "string" ?defValue "" ?prompt p ?parseAsNumber "yes" ?parseAsCEL "yes"))
    (setSGq cdf (list nil 'spectre (list nil 'termOrder '(PLUS MINUS))) simInfo)
    (cdfSaveCDF cdf)))
;; MOS as the live analogLib draws them (y up here): gate at the origin, D/S/B 40 DBU right.
(SfyFixSymbol "analogLib" "nmos4" (list (list "D" "inputOutput" 40 30) (list "G" "input" 0 0)
                                        (list "S" "inputOutput" 40 -30) (list "B" "inputOutput" 40 0)))
(SfyFixSymbol "analogLib" "pmos4" (list (list "D" "inputOutput" 40 -30) (list "G" "input" 0 0)
                                        (list "S" "inputOutput" 40 30) (list "B" "inputOutput" 40 0)))
(SfyLiveFixTwo "res" nil)
(SfyLiveFixTwo "cap" '("c" "m"))
(SfyLiveFixTwo "vdc" '("vdc" "acm" "acp"))
(SfyLiveFixTwo "vpulse" '("v1" "v2" "td" "tr" "tf" "pw" "per" "vdc" "acm" "acp"))
"#,
    )
    .unwrap();
    i
}

/// Runs a generated session on the mock: disk files in, the session's writes out.
fn run_on_mock(i: &mut Interp<MockDb>, work: &Path, session: &str) {
    for sub in ["in", "out", "skill", "projects"] {
        for (p, b) in read_tree(&work.join(sub)) {
            if let Ok(t) = String::from_utf8(b) {
                i.files.insert(path_str(&work.join(sub).join(p)), t);
            }
        }
    }
    let text = read(&work.join(session)).unwrap();
    let body: String = text
        .lines()
        .filter(|l| !l.starts_with("(load ") && !l.starts_with("(exit"))
        .map(|l| format!("{l}\n"))
        .collect();
    i.load(&body)
        .unwrap_or_else(|e| panic!("{session}: {e}\n{}", i.log));
    let prefix = path_str(work);
    for (p, t) in &i.files {
        if p.starts_with(&prefix) {
            write(Path::new(p), t);
        }
    }
}

/// Plugin bugs the kit finds on the mock, so a real Virtuoso shows them too: (check, why).
/// Remove a row when its bug is fixed; the test fails on a row that passes.
const KNOWN_BUGS: [(&str, &str); 0] = [];

/// The whole kit with the mock Virtuoso in place of a real one (no si or OCEAN): every
/// check the mock can answer passes but the [`KNOWN_BUGS`], so the kit itself is sound.
#[test]
fn kit_passes_on_the_mock() {
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("virtuoso-live-mock");
    let _ = std::fs::remove_dir_all(&work);
    let o = Opts::default();
    prepare(&work, &o);
    let mut i = mock(&work);
    run_on_mock(&mut i, &work, "session1.il");
    mid(&work, &o);
    run_on_mock(&mut i, &work, "session2.il");
    let r = verify(&work, &o);
    let fails: Vec<String> = r
        .fails()
        .iter()
        .filter(|(id, _, _)| !KNOWN_BUGS.iter().any(|(k, _)| k == id))
        .map(|(id, _, d)| format!("{id}: {d}"))
        .collect();
    assert!(fails.is_empty(), "{}", fails.join("\n"));
    for (id, why) in KNOWN_BUGS {
        assert!(
            r.fails().iter().any(|(f, _, _)| f == id),
            "{id} passes now ({why}): remove it from KNOWN_BUGS"
        );
    }
    let skipped: BTreeSet<&str> = r
        .rows
        .iter()
        .filter(|x| x.1 == Grade::Skip)
        .map(|x| x.0.as_str())
        .collect();
    assert_eq!(
        skipped,
        ["callbacks", "netlist", "ocean", "pdk", "retarget", "user"].into()
    );
}

/// `--lib` on the mock: the fixture library `work` (cells drawn the Virtuoso way: bare pin
/// shapes, pin masters of other views, alias labels, net expressions, symbols with IL
/// labels) goes Virtuoso → Schemify → a fresh Virtuoso library and must dump the same, as
/// `--lib sample` does on a live run.
#[test]
fn kit_lib_round_trip_on_the_mock() {
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("virtuoso-live-mock-lib");
    let _ = std::fs::remove_dir_all(&work);
    let o = Opts {
        cells: vec![("work".into(), "*".into())],
        ..Opts::default()
    };
    prepare(&work, &o);
    let mut i = mock(&work);
    for l in ["work", "pdk", "cb"] {
        i.host.add_lib(l);
    }
    i.load(
        "(SfyFixPdk) (SfyFixCb) (SfyFixInv) (SfyFixBus) (SfyFixSymkit) (SfyFixNetx) (SfyFixVamod)",
    )
    .unwrap();
    run_on_mock(&mut i, &work, "session1.il");
    mid(&work, &o);
    run_on_mock(&mut i, &work, "session2.il");
    let r = verify(&work, &o);
    let user: Vec<String> = r
        .rows
        .iter()
        .filter(|x| x.0.starts_with("user.") && x.1 != Grade::Skip)
        .map(|x| format!("{} {:?} {}", x.0, x.1, x.2))
        .collect();
    assert!(
        user.iter().any(|x| x.starts_with("user.roundtrip.")),
        "no round trip: {user:#?}"
    );
    let fails: Vec<&String> = user.iter().filter(|x| x.contains(" Fail ")).collect();
    assert!(fails.is_empty(), "{fails:#?}");
}
