//! `schemify annotate`: an extracted netlist becomes a leaf impl of its cell, its ports
//! checked against the cell's pins by name, and every bench that reaches the cell gets a
//! config that selects it (docs/guide/cli.md `annotate`).
//!
//! In: a cell file and the extractor's SPICE text (Magic `ext2spice`, GPurify `--format
//! spice`). Out: one edit transaction on the cell (`impl <name> spice` and `body <name>`
//! holding the text, after a wrapper `.subckt` in the cell's pin order when the ports come
//! in another order; the header's `default` kept) and one per bench (`config <name>` with
//! `cell <ref> <impl>`), applied by `doc` and written canonically, or printed as op text
//! with `--dry-run`. The devices the text calls that a bench corner's model files do not
//! define are reported. The text is read in `netlist`, the corners' model files in `kit`.
//!
//! Cites: format spec §4 (rank 1 `impl` and its kind rules, rank 5 `body`, rank 6 `config`),
//! netlist spec §1 (cell references), §2 (impl selection), §7.1 (default templates, bodies,
//! `${NAME}`), run spec §5 (corner paths), edits spec §1 (ops and their text).
//!
//! Data: one cell, one text (at most the format's 2^30-byte file), its ports at most
//! `schemify_elab::PORTS_MAX`; benches at most the project's indexed cells (2^16); model
//! files per corner at most `kit::KIT_FILES_MAX`. Everything lives for one command.
//!
//! Contents: `pub fn main(&[String]) -> u8`.

mod kit;
mod netlist;

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use schemify_doc::{
    Assignment, DocHandle, Document, Edit, EditRequest, FileKind, LibSource, Loc, Mode, Op,
    Outcome, Project, RecordBuf, RecordKind, Records, Strings, TableId, Target, Transaction,
    apply_edits, ops_to_text,
};
use schemify_elab::{Dialect, Request, elaborate, impl_ports, no_funcs};

use crate::libs;
use crate::platform;
use crate::spines::{Cells, OpenError, Opened, elaborate_all, env_pairs, open_file};

use kit::Kit;
use netlist::Plan;

const USAGE: &str = "usage: schemify annotate <cell> <extracted.spice> [--impl NAME] \
                     [--subckt NAME] [--config NAME] [--dry-run]";

/// Why the command stopped: exit 1 (a refusal, nothing written) or 2 (usage, I/O).
enum Fail {
    Refused(String),
    Usage(String),
}

/// The command line.
struct Args {
    cell: String,
    netlist: String,
    imp: String,
    subckt: Option<String>,
    config: String,
    dry_run: bool,
}

/// What a bench's config gets.
enum Config {
    /// The op that adds the rule.
    Add(Op),
    /// The config already has a `cell` rule for the cell: this one, kept as it is.
    Kept(String),
}

/// `schemify annotate <cell> <extracted.spice> [--impl NAME] [--subckt NAME] [--config NAME]
/// [--dry-run]`: writes the impl and the configs, prints one line per file (with
/// `--dry-run`, the op text per file instead, writing nothing) and returns the exit code:
/// 0 annotated (warnings included), 1 refused with nothing written, 2 usage or I/O errors.
#[must_use]
pub fn main(args: &[String]) -> u8 {
    let Some(a) = Args::parse(args) else {
        eprintln!("{USAGE}");
        return 2;
    };
    match annotate(&a) {
        Ok(()) => 0,
        Err(Fail::Refused(m)) => {
            eprintln!("{m}");
            1
        }
        Err(Fail::Usage(m)) => {
            eprintln!("{m}");
            2
        }
    }
}

// ---- private ----

impl Args {
    /// `None` for a usage error. `--impl` and `--config` name SPICE identifiers, so the
    /// wrapper's name (`<cell>__<impl>`) is one too.
    fn parse(args: &[String]) -> Option<Args> {
        let mut files = Vec::new();
        let (mut imp, mut config) = ("pex".to_owned(), "post".to_owned());
        let (mut subckt, mut dry_run) = (None, false);
        let mut it = args.iter();
        // Bounded by `args`: every pass consumes at least one argument.
        while let Some(a) = it.next() {
            match a.as_str() {
                "--impl" => imp.clone_from(it.next()?),
                "--config" => config.clone_from(it.next()?),
                "--subckt" => subckt = Some(it.next()?.clone()),
                "--dry-run" => dry_run = true,
                s if s.starts_with('-') => return None,
                _ => files.push(a.clone()),
            }
        }
        let ident = |s: &str| {
            s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        };
        let [cell, netlist] = <[String; 2]>::try_from(files).ok()?;
        if !ident(&imp) || !ident(&config) {
            return None;
        }
        assert!(!cell.starts_with('-'));
        assert!(!netlist.starts_with('-'));
        Some(Args {
            cell,
            netlist,
            imp,
            subckt,
            config,
            dry_run,
        })
    }
}

/// The annotate spine: open → read the netlist → pick its `.subckt` → match ports to pins
/// → plan the impl → ops on the cell → per bench reaching the cell: its config, its kit →
/// commit every transaction in memory → print or write.
fn annotate(a: &Args) -> Result<(), Fail> {
    let mut o = open_file(Path::new(&a.cell), platform::seed()).map_err(|e| match e {
        OpenError::Io(m) => Fail::Usage(format!("{}: {m}", a.cell)),
        OpenError::Design(m) => Fail::Refused(format!("{}: {m}", a.cell)),
    })?;
    for n in &o.notes {
        eprintln!("note: {n}");
    }
    let kind = o
        .project
        .doc(o.top)
        .map(Document::kind)
        .expect("open_file opened it");
    if kind == FileKind::Shbench {
        return Err(Fail::Usage(format!(
            "{}: a bench has no pins to annotate",
            a.cell
        )));
    }
    let bytes = platform::read(Path::new(&a.netlist)).map_err(Fail::Usage)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| Fail::Refused(format!("{}: not UTF-8 text", a.netlist)))?;
    let refuse = |m: String| Fail::Refused(format!("{}: {m}", a.netlist));
    let scanned = netlist::scan(&text).map_err(refuse)?;

    let env = env_pairs();
    let env: Vec<Assignment<&str>> = env.iter().map(Assignment::as_deref).collect();
    let cell = elaborate(&o.project, o.top, &request(None, None, &env), &mut no_funcs);
    let row = cell.cells[0].row as usize;
    assert!(
        row < o.project.cells().lib.len(),
        "open_file opens an indexed cell"
    );
    let stem = cell.names.get(cell.cells[0].stem).to_owned();

    let want = a.subckt.as_deref().unwrap_or(&stem);
    let (pname, at, line) = netlist::pick(&scanned, want).map_err(refuse)?;
    let ports = impl_ports("spice", &text[at..])
        .map_err(|e| refuse(format!("line {}: {}", e.line - 1 + line, e.why)))?;
    let ports: Vec<String> = ports.into_iter().map(|(n, _)| n).collect();
    let pins = netlist::port_bits(&cell);
    netlist::match_ports(&pins, &ports)
        .map_err(|m| refuse(format!(".subckt {pname} does not match {stem}'s pins: {m}")))?;
    let defined: Vec<&str> = scanned.subckts.iter().map(|s| s.0.as_str()).collect();
    let plan = netlist::plan(&stem, &a.imp, &pins, &ports, &pname, &defined, &text);

    let doc = o.project.doc(o.top).expect("open");
    let (cell_ops, replaced) =
        cell_ops(doc, &a.imp, &plan).map_err(|m| Fail::Refused(format!("{}: {m}", a.cell)))?;
    let how = match &plan.wrapper {
        None => "in pin order, used as is".to_owned(),
        Some(w) => format!("ports reordered by the wrapper {w}"),
    };
    let verb = if replaced { "replaced" } else { "added" };
    let mut lines = vec![format!(
        "{}: impl {} {verb} from {} (.subckt {pname}, {how})",
        shown(Path::new(&a.cell)),
        a.imp,
        a.netlist
    )];
    let mut txs: Vec<(DocHandle, PathBuf, Vec<Op>)> =
        vec![(o.top, PathBuf::from(&a.cell), cell_ops)];

    let root = libs::project_root(Path::new(&a.cell));
    let mut kits: BTreeMap<Vec<(PathBuf, Option<String>)>, Kit> = BTreeMap::new();
    for (bench, path) in benches(&o) {
        let h = if let Some(h) = o.project.open_handle(bench) {
            h
        } else {
            if let Err(e) = o.open_row(bench as u32) {
                eprintln!("warning: {e}: skipped");
                continue;
            }
            o.project.open_handle(bench).expect("just opened")
        };
        let shown_bench = shown(&path);
        // The cells the config will select reach (its own rules; none yet for a new one: a
        // config the bench lacks elaborates with no rules, its E035 aside).
        let probe = request(Some(&a.config), None, &env);
        let reach = match elaborate_all(&mut o, h, &probe, &mut no_funcs) {
            Ok(e) => e.cells.iter().any(|f| f.row as usize == row),
            Err(e) => {
                eprintln!("warning: {shown_bench}: {e}: skipped");
                continue;
            }
        };
        if !reach {
            continue;
        }
        let Some(cref) = cellref(&o.project, h, row) else {
            eprintln!(
                "warning: {shown_bench}: reaches {stem} but cannot name it (neither lib:{stem} \
                 nor ./{stem} resolves from there): add `config {}` by hand",
                a.config
            );
            continue;
        };
        match config_op(&o.project, h, &a.config, &cref, &a.imp, row) {
            Config::Add(op) => {
                lines.push(format!(
                    "{shown_bench}: config {}: cell {cref} {}",
                    a.config, a.imp
                ));
                txs.push((h, path.clone(), vec![op]));
            }
            Config::Kept(rule) => {
                lines.push(format!("{shown_bench}: config {} kept: {rule}", a.config));
            }
        }
        if !scanned.needs.is_empty() {
            let dirs = (path.parent().unwrap_or(&root), root.as_path());
            let found = kit::check_kit(&o, h, &a.config, &env, &scanned.needs, dirs, &mut kits);
            for (level, m) in found {
                eprintln!("{level}: {shown_bench}: {m}");
            }
        }
    }

    let mut writes: Vec<(PathBuf, String)> = Vec::new();
    let mut ops_text = String::new();
    for (h, path, ops) in txs {
        let file = shown(&path);
        ops_text.push_str(&format!("== {file}\n{}", ops_to_text(&ops)));
        if let Some(bytes) =
            commit(&mut o.project, h, ops).map_err(|m| Fail::Refused(format!("{file}: {m}")))?
        {
            writes.push((path, bytes));
        }
    }
    if a.dry_run {
        print!("{ops_text}");
        return Ok(());
    }
    for (path, bytes) in &writes {
        platform::write_atomic(path, bytes.as_bytes()).map_err(Fail::Usage)?;
    }
    for l in &lines {
        println!("{l}");
    }
    Ok(())
}

/// An elaboration of annotate's own: the impl selection of `config` and the model lines of
/// `corner` (`None`: the bench's defaults), no overrides.
fn request<'a>(
    config: Option<&'a str>,
    corner: Option<&'a str>,
    env: &'a [Assignment<&'a str>],
) -> Request<'a> {
    Request {
        config,
        corner,
        overrides: &[],
        env,
        dialect: Dialect::Ngspice,
    }
}

/// `path` relative to the working directory when it lies below it, with `/` separators.
fn shown(path: &Path) -> String {
    let abs = libs::absolute(path);
    let cwd = env::current_dir().ok().map(|d| libs::absolute(&d));
    let rel = cwd.and_then(|c| libs::rel_path(&c, &abs));
    rel.unwrap_or_else(|| abs.to_string_lossy().replace('\\', "/"))
}

/// The project's benches with their files, in path order: those of the project and of
/// `path` libraries (a `git` or `url` library is a fetched dependency, never written).
fn benches(o: &Opened) -> Vec<(usize, PathBuf)> {
    let p = &o.project;
    let c = p.cells();
    let mut out: Vec<(usize, PathBuf)> = (0..c.lib.len())
        .filter(|&r| c.kind[r] == FileKind::Shbench)
        .filter(|&r| {
            let lib = usize::from(c.lib[r]);
            matches!(
                p.libs().source[lib],
                LibSource::Project | LibSource::Path { .. }
            )
        })
        .filter_map(|r| {
            let dir = o.lib_dir.get(usize::from(c.lib[r]))?.as_ref()?;
            Some((r, dir.join(libs::cell_relpath(p, r))))
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    assert!(out.len() <= c.lib.len());
    assert!(out.iter().all(|(r, _)| c.kind[*r] == FileKind::Shbench));
    out
}

/// The ops that put `plan` on cell `doc` as impl `imp`, and whether they replace one: an
/// insert, or a `put` of the spice impl of that name keeping its comments; the same for its
/// body; and the header's `default` when the cell had one impl and none named, so the
/// default stays (format spec §4: a cell with more than one impl names its default).
///
/// # Errors
///
/// `imp` names an impl of another language: annotate replaces only spice impls.
fn cell_ops(doc: &Document, imp: &str, plan: &Plan) -> Result<(Vec<Op>, bool), String> {
    let s = doc.strings();
    let impls = doc.impls();
    let old = named(impls, s, imp);
    if let Some(r) = old {
        let lang = s.get(impls.fields[r as usize][1]);
        if lang != "spice" {
            return Err(format!(
                "impl {imp} is a {lang} impl; annotate replaces only spice impls (pass --impl)"
            ));
        }
    }
    let mut rec = RecordBuf::new(RecordKind::Impl).field(imp).field("spice");
    if let Some(f) = &plan.format {
        rec = rec.attr("format", &["spice", f]);
    }
    let mut body = RecordBuf::new(RecordKind::Body).field(imp);
    body.verbatim.clone_from(&plan.body);
    let mut ops = Vec::new();
    if old.is_none() && impls.kind.len() == 1 && doc.header_value("default").is_none() {
        let only = s.get(impls.fields[0][0]).to_owned();
        ops.push(Op::SetAttr {
            target: Target::header(),
            key: "default".to_owned(),
            values: vec![only],
        });
    }
    for (t, table, kind, mut rec) in [
        (impls, TableId::Impls, RecordKind::Impl, rec),
        (doc.bodies(), TableId::Bodies, RecordKind::Body, body),
    ] {
        ops.push(match named(t, s, imp) {
            Some(row) => {
                rec.comments = doc.record(Loc { table, row }).comments;
                Op::Put {
                    target: Target::new(kind, imp),
                    rec,
                }
            }
            None => Op::Insert { rec, at: None },
        });
    }
    assert!((2..=3).contains(&ops.len()));
    Ok((ops, old.is_some()))
}

/// The row of the record named `name` in table `t` (field 0), if any.
fn named(t: &Records, s: &Strings, name: &str) -> Option<u32> {
    let row = (0..t.kind.len()).find(|&r| s.get(t.fields[r][0]) == name);
    assert!(row.is_none_or(|r| r < t.kind.len()));
    row.map(|r| r as u32)
}

/// The reference bench `h` writes for cell `row`: `lib:stem` for a cell at the root of a
/// named library, else `./stem` beside the bench; `None` when neither resolves to the cell
/// from the bench (netlist spec §1).
fn cellref(p: &Project, h: DocHandle, row: usize) -> Option<String> {
    let c = p.cells();
    assert!(row < c.lib.len());
    let stem = p.strings().get(c.stem[row]);
    let lib = &p.libs().name[usize::from(c.lib[row])];
    let named = (!lib.is_empty()).then(|| format!("{lib}:{stem}"));
    let found = named
        .into_iter()
        .chain([format!("./{stem}")])
        .find(|r| p.resolve(h, r) == Ok(row));
    assert!(found.as_ref().is_none_or(|r| r.ends_with(stem)));
    found
}

/// The op that makes `config <cfg>` of bench `h` select impl `imp` of the cell at `row`
/// (reference `cref`): the config inserted, or a `cell` line appended to it; nothing when
/// it has a `cell` rule for the cell already, whatever impl that names.
fn config_op(p: &Project, h: DocHandle, cfg: &str, cref: &str, imp: &str, row: usize) -> Config {
    let doc = p.doc(h).expect("an open bench");
    let (s, t) = (doc.strings(), doc.configs());
    let rule = RecordBuf::new(RecordKind::Config)
        .field(cfg)
        .attr("cell", &[cref, imp]);
    let Some(r) = named(t, s, cfg) else {
        return Config::Add(Op::Insert {
            rec: rule,
            at: None,
        });
    };
    for (k, vals) in doc.pool().attrs(t.attrs[r as usize]) {
        if let ("cell", [c, i]) = (s.get(k), vals)
            && p.resolve(h, s.get(*c)) == Ok(row)
        {
            return Config::Kept(format!("cell {} {}", s.get(*c), s.get(*i)));
        }
    }
    assert_eq!(rule.attrs.len(), 1);
    Config::Add(Op::SetAttr {
        target: Target::new(RecordKind::Config, cfg),
        key: "cell".to_owned(),
        values: vec![cref.to_owned(), imp.to_owned()],
    })
}

/// Commits `ops` to document `h` and returns its canonical bytes (`None`: unchanged).
///
/// # Errors
///
/// `doc` refused the transaction (each failing op by index and code) or the save.
fn commit(p: &mut Project, h: DocHandle, ops: Vec<Op>) -> Result<Option<String>, String> {
    assert!(!ops.is_empty());
    let req = EditRequest {
        doc: h,
        edit: Edit::Tx(Transaction {
            label: "annotate".to_owned(),
            mode: Mode::Commit,
            ops,
        }),
        unix_ms: platform::unix_ms(),
    };
    let outcome = apply_edits(p, &[req]);
    assert_eq!(outcome.len(), 1, "one outcome per request");
    match &outcome[0] {
        Outcome::Committed { .. } => {}
        Outcome::Rejected(errs) => {
            let why: Vec<String> = errs
                .iter()
                .map(|e| format!("op {}: {}", e.op, e.code))
                .collect();
            return Err(format!("edit refused: {}", why.join(", ")));
        }
        Outcome::Refused(code) => return Err(format!("edit refused: {code}")),
        Outcome::Staged { .. }
        | Outcome::Undone { .. }
        | Outcome::Redone { .. }
        | Outcome::Dropped
        | Outcome::Nothing => unreachable!("a committed transaction commits or is refused"),
    }
    Ok(p.save(h)
        .map_err(|c| format!("save refused: {c}"))?
        .map(|s| s.bytes))
}

#[cfg(test)]
mod tests {
    use super::Args;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn args_take_two_files_and_identifiers() {
        let a = |v: &[&str]| Args::parse(&strings(v));
        let ok = a(&[
            "c.shcomp",
            "x.spice",
            "--impl",
            "lay",
            "--config",
            "pl",
            "--dry-run",
        ])
        .unwrap();
        assert_eq!(
            (ok.imp.as_str(), ok.config.as_str(), ok.dry_run),
            ("lay", "pl", true)
        );
        let ok = a(&["c.shcomp", "x.spice", "--subckt", "top"]).unwrap();
        assert_eq!((ok.imp.as_str(), ok.config.as_str()), ("pex", "post"));
        assert_eq!(ok.subckt.as_deref(), Some("top"));
        for bad in [
            &["c.shcomp"][..],
            &["c.shcomp", "x.spice", "y.spice"],
            &["c.shcomp", "x.spice", "--impl"],
            &["c.shcomp", "x.spice", "--impl", "1x"],
            &["c.shcomp", "x.spice", "--config", "a.b"],
            &["c.shcomp", "x.spice", "--frob"],
        ] {
            assert!(a(bad).is_none(), "{bad:?}");
        }
    }
}
