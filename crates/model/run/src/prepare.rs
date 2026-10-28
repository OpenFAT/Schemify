//! The spine's design steps (run spec §4, §5): the gate and design hash of a run, and
//! per job `elaborate(overrides) → netlist::emit`, plus the sources the simulator needs
//! compiled (Verilog-A → OSDI, Verilog → VVP) or copied (Verilog-A for ESPice's `.hdl`)
//! first, and the relative corner paths the runner makes absolute. In: the project, the
//! bench handle and the `jobs` table; out: the design [`Digest`] or [`Blocked`], and one
//! [`Prepared`] netlist per job (var values and `op(…)` vectors recorded in `results`).
//!
//! Contents:
//! - `pub enum Blocked`, `pub struct Source`, `pub enum Origin`, `pub enum Tool`,
//!   `pub struct Prepared` (`Prepared::resolved`)
//! - `pub fn design<F: Funcs>(project, top, jobs, env, dialect, funcs) -> Result<Digest, Blocked>`
//! - `pub fn prepare<F: Funcs>(project, top, jobs, j, env, dialect, funcs, results) -> Option<Prepared>`
//! - `pub fn sources(project: &Project, e: &Elab, dialect: Dialect) -> Vec<Source>`

use crate::jobs::Jobs;
use crate::results::Results;
use schemify_doc::{Digest, DocHandle, Project, Sha256};
use schemify_elab::{CornerLine, Dialect, Elab, Funcs, Name, Request, Severity};
use schemify_netlist as netlist;
use std::ops::Range;
use std::path::Path;

/// Why a run cannot start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Elaboration needs these cell rows opened (`Project::open`); then call again.
    NeedsOpen(Vec<u32>),
    /// Blocking violations (D13), one line each: `E005 lib:cell /X1 W: detail`.
    Violations(Vec<String>),
}

/// A source a job's simulator needs compiled (or copied) into the job directory first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Compiler.
    pub tool: Tool,
    /// Where the source text is.
    pub origin: Origin,
    /// File name the netlist expects in the job directory (`<stem>.osdi|.vvp|.va`: the
    /// `file` stem, else the cell stem).
    pub out: String,
}

/// Where a [`Source`]'s text comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The impl's `file`.
    File {
        /// Library row of the cell that names the file (`Project::libs`).
        lib: u8,
        /// Path relative to the library directory (the cell's directory joined with
        /// `file`).
        path: String,
    },
    /// The impl's `body` (run spec §5: the runner writes it to the build cache as
    /// `<stem>.va|.v` and compiles that file).
    Body(String),
}

/// How a source reaches the job directory: an external compiler (its path from the user
/// config, D7), or a plain copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Tool {
    /// OpenVAF: Verilog-A → OSDI (`pre_osdi`).
    Openvaf,
    /// Icarus Verilog: Verilog → VVP (ngspice `d_cosim`).
    Iverilog,
    /// No compiler: the Verilog-A source itself, copied as `<stem>.va` for ESPice's
    /// `.hdl` (ESPice compiles it, netlist spec §7.2a).
    Copy,
}

/// What a runner needs to simulate one job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    /// Netlist text.
    pub netlist: String,
    /// Sources to compile first, deduplicated by `out`.
    pub sources: Vec<Source>,
    /// Byte ranges in `netlist` of relative corner `lib`/`include` paths, in order: the
    /// native runner rewrites them against [`Prepared::base`] (run spec §5).
    pub relative: Vec<Range<u32>>,
    /// Parallel to [`Prepared::relative`]: the path came from a project variable
    /// (`${PDK_MODELS}/…`), so it is joined onto the project root instead.
    pub project: Vec<bool>,
    /// The bench's library row and its directory relative to that library.
    pub base: (u8, String),
}

/// One relative corner path in a netlist: its byte range, and whether it came from a
/// project variable.
struct RelativePath {
    range: Range<u32>,
    from_project: bool,
}

impl Prepared {
    /// Returns the netlist with every [`Prepared::relative`] path joined onto `base_dir`
    /// (the bench's directory on disk), or onto `project_dir` for a path from a project
    /// variable ([`Prepared::project`]); quoted when it holds whitespace.
    ///
    /// # Panics
    ///
    /// Panics if [`Prepared::relative`] and [`Prepared::project`] differ in length, or a
    /// range is empty, out of order, past the netlist or at its first byte (as `prepare`
    /// never builds them).
    #[must_use]
    pub fn resolved(&self, base_dir: &Path, project_dir: &Path) -> String {
        assert_eq!(self.relative.len(), self.project.len(), "parallel columns");
        let mut out = String::with_capacity(self.netlist.len() + 64 * self.relative.len());
        let mut at = 0;
        for (r, &from_project) in self.relative.iter().zip(&self.project) {
            let (start, end) = (r.start as usize, r.end as usize);
            assert!(at <= start, "ranges are sorted and disjoint");
            assert!(start < end, "a path is never empty");
            assert!(start >= 1, "a path follows its directive");
            assert!(end <= self.netlist.len());
            out.push_str(&self.netlist[at..start]);
            let dir = if from_project { project_dir } else { base_dir };
            let abs = dir.join(&self.netlist[start..end]);
            let abs = abs.to_string_lossy();
            // A Spectre path is quoted already (`include "<path>"`).
            let quoted = self.netlist.as_bytes()[start - 1] == b'"';
            if abs.contains(char::is_whitespace) && !quoted {
                out.push('"');
                out.push_str(&abs);
                out.push('"');
            } else {
                out.push_str(&abs);
            }
            at = end;
        }
        out.push_str(&self.netlist[at..]);
        assert!(out.len() >= self.netlist.len());
        out
    }
}

/// Gates the run and returns its design hash (run spec §4): elaborates `top` for
/// `dialect` once per distinct config of `jobs` (the bench default when `jobs` is empty)
/// at the default corner, refuses on blocking violations, and hashes the lockfile, the
/// project settings, `env`, the canonical bytes of the bench and of every cell those
/// elaborations reached, the job list, and the dialect unless it is ngspice. Any edit
/// that can change a job's netlist changes the hash, so results tagged with an older one
/// are stale.
///
/// Cost: one elaboration per config plus one canonical write per reached cell.
///
/// # Errors
///
/// [`Blocked`] when a reached cell is not open or a violation blocks the run.
///
/// # Panics
///
/// Panics if `top` is a stale handle.
pub fn design<F: Funcs>(
    project: &Project,
    top: DocHandle,
    jobs: &Jobs,
    env: &[(&str, &str)],
    dialect: Dialect,
    funcs: &mut F,
) -> Result<Digest, Blocked> {
    let bench = project.doc(top).expect("design needs an open top document");
    let mut configs: Vec<Option<&str>> = (0..jobs.len()).map(|j| jobs.config(j)).collect();
    configs.sort_unstable();
    configs.dedup();
    if configs.is_empty() {
        configs.push(None);
    }
    // Reached cells by display name (`lib:dir/stem`, unique per project): row indices
    // follow the app's listing order, which differs between platforms.
    let mut reached: Vec<(String, u32)> = Vec::new();
    for config in configs {
        let request = Request {
            config,
            corner: None,
            overrides: &[],
            env,
            dialect,
        };
        let e = schemify_elab::elaborate(project, top, &request, funcs);
        gate(&e)?;
        let rows = e.cells.iter().filter(|c| c.row != u32::MAX);
        reached.extend(rows.map(|c| (e.names.get(c.cell).to_owned(), c.row)));
    }
    reached.sort_unstable();
    reached.dedup();

    let mut hasher = Sha256::default();
    let mut part = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    part(b"schemify design 1");
    part(project.write_lock().as_bytes());
    for (k, v) in project.settings() {
        part(k.as_bytes());
        part(v.as_bytes());
    }
    for (k, v) in env {
        part(k.as_bytes());
        part(v.as_bytes());
    }
    part(&schemify_doc::sha256(bench.canonical().as_bytes()));
    for (name, row) in &reached {
        let doc = project
            .interface(*row as usize)
            .expect("elaboration reached it");
        part(name.as_bytes());
        part(&schemify_doc::sha256(doc.canonical().as_bytes()));
    }
    let mut key = String::new();
    for j in 0..jobs.len() {
        key.clear();
        jobs.write_key(j, &mut key);
        part(key.as_bytes());
    }
    // Another simulator's results are another run (ngspice adds nothing: the hashes of
    // results saved before dialects were hashed stay current).
    if dialect != Dialect::Ngspice {
        part(dialect.as_str().as_bytes());
    }
    let digest = hasher.finish();
    assert_ne!(digest, [0; 32]);
    Ok(digest)
}

/// Elaborates job `j` with its config, corner and assignments, gates it, emits its
/// netlist for `dialect` and records its bench var values in `results`. A job that
/// cannot be netlisted is marked failed (message in `results`) and `None` returned.
///
/// Cost: one elaboration and one emission.
///
/// # Panics
///
/// Panics if `top` is a stale handle or `j` is not a job of `jobs`.
#[allow(
    clippy::too_many_arguments,
    reason = "every argument is a table or input the step reads"
)]
pub fn prepare<F: Funcs>(
    project: &Project,
    top: DocHandle,
    jobs: &mut Jobs,
    j: u32,
    env: &[(&str, &str)],
    dialect: Dialect,
    funcs: &mut F,
    results: &mut Results,
) -> Option<Prepared> {
    assert!(j < jobs.len(), "job {j} of {}", jobs.len());
    let overrides: Vec<(&str, &str)> = jobs.assignments(j).collect();
    let request = Request {
        config: jobs.config(j),
        corner: jobs.corner(j),
        overrides: &overrides,
        env,
        dialect,
    };
    let e = schemify_elab::elaborate(project, top, &request, funcs);
    drop(overrides);
    if let Err(b) = gate(&e) {
        let msg = match b {
            Blocked::NeedsOpen(rows) => format!("cells not open: rows {rows:?}"),
            Blocked::Violations(v) => format!("blocked: {}", v.join("; ")),
        };
        results.fail(jobs, j, msg);
        return None;
    }
    results.set_vars(j, &e);
    let emitted = netlist::emit(&e, dialect);
    let mut text = emitted.text;
    // Lines before `.end`: the device vectors `op(…)` outputs read (run spec §4), then a
    // Monte Carlo sample's simulator seed (§3; ngspice reads `.option seed` before it
    // evaluates `.param`, wherever the line stands).
    // ponytail: the seed for ngspice and espice only; Xyce takes it on the command line
    // (`-randseed`).
    let mut tail = results.set_ops(j, &emitted.name_map, dialect);
    if let (Some(seed), Dialect::Ngspice | Dialect::Espice) = (jobs.seed(j), dialect) {
        use std::fmt::Write;

        writeln!(tail, ".option seed={seed}").expect("String write");
    }
    if !tail.is_empty() {
        assert!(
            text.ends_with(".end\n"),
            "an ngspice netlist ends with .end"
        );
        text.insert_str(text.len() - ".end\n".len(), &tail);
    }
    let paths = relative_paths(&e, dialect, &text);
    let relative: Vec<Range<u32>> = paths.iter().map(|p| p.range.clone()).collect();
    let from_project: Vec<bool> = paths.iter().map(|p| p.from_project).collect();
    let top = &e.cells[0];
    let base = if top.row == u32::MAX {
        (0, String::new())
    } else {
        let row = top.row as usize;
        let dir = project.strings().get(project.cells().dir[row]).to_owned();
        (project.cells().lib[row], dir)
    };
    let p = Prepared {
        netlist: text,
        sources: sources(project, &e, dialect),
        relative,
        project: from_project,
        base,
    };
    assert!(!p.netlist.is_empty());
    assert_eq!(p.relative.len(), p.project.len(), "parallel columns");
    Some(p)
}

/// Returns the Verilog-A and Verilog sources of the leaf impls `e` uses (a `file`, or a
/// `body` that is source: `netlist::compiled_source`; run spec §5), deduplicated by
/// output name. For [`Dialect::Espice`] a Verilog-A source is [`Tool::Copy`] to
/// `<stem>.va` (ESPice compiles it); otherwise it compiles with OpenVAF to `<stem>.osdi`.
/// `e` must be elaborated for `dialect`.
///
/// # Panics
///
/// Panics if `e`'s impl body offsets are not one more than its impls (an `Elab` that
/// `elaborate` did not build).
#[must_use]
pub fn sources(project: &Project, e: &Elab, dialect: Dialect) -> Vec<Source> {
    assert_eq!(
        e.impls.body_start.len(),
        e.impls.cell.len() + 1,
        "CSR offsets: one more than the impls"
    );
    let mut out: Vec<Source> = Vec::new();
    let cells = project.cells();
    for i in 0..e.impls.cell.len() {
        if !netlist::compiled_source(e, i) {
            continue;
        }
        let interface = &e.cells[e.impls.cell[i] as usize];
        let k = e.impls.index[i] as usize;
        let (tool, ext) = match (e.names.get(interface.impl_lang[k]), dialect) {
            ("veriloga", Dialect::Espice) => (Tool::Copy, "va"),
            ("veriloga", _) => (Tool::Openvaf, "osdi"),
            _ => (Tool::Iverilog, "vvp"),
        };
        let (origin, stem) = if interface.impl_file[k] == Name::NONE {
            let body = e.impls.body_start[i] as usize..e.impls.body_start[i + 1] as usize;
            let mut text = String::new();
            for &l in &e.impls.body[body] {
                text.push_str(e.names.get(l));
                text.push('\n');
            }
            (Origin::Body(text), e.names.get(interface.stem))
        } else {
            if interface.row == u32::MAX {
                continue; // not a project cell: no directory to resolve `file` against
            }
            let file = e.names.get(interface.impl_file[k]);
            // A derived `tech` cell's impls are its PDK cell's: `file` is relative to that.
            let row = project
                .tech_source(interface.row as usize)
                .unwrap_or(interface.row as usize);
            let dir = project.strings().get(cells.dir[row]);
            let path = if dir.is_empty() {
                file.to_owned()
            } else {
                format!("{dir}/{file}")
            };
            let stem = file.rsplit(['/', '\\']).next().unwrap_or(file);
            let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
            let lib = cells.lib[row];
            (Origin::File { lib, path }, stem)
        };
        let src = Source {
            tool,
            origin,
            out: format!("{stem}.{ext}"),
        };
        if !out.iter().any(|s| s.out == src.out) {
            out.push(src);
        }
    }
    crate::plan::sort_by_name(&mut out, |s| &s.out);
    // Provable from the dedup and the tool match above.
    debug_assert!(out.windows(2).all(|w| w[0].out < w[1].out));
    if dialect != Dialect::Espice {
        debug_assert!(out.iter().all(|s| s.tool != Tool::Copy));
    }
    out
}

// ---- private ----

/// Turns `e`'s elaboration-needs-open rows and blocking (`E`) violations into [`Blocked`].
fn gate(e: &Elab) -> Result<(), Blocked> {
    if !e.needs_open.is_empty() {
        return Err(Blocked::NeedsOpen(e.needs_open.clone()));
    }
    let blocking: Vec<String> = e
        .violations
        .rows
        .iter()
        .filter(|v| v.code.severity() == Severity::Error)
        .map(|v| {
            let n = |x: Name| if x == Name::NONE { "-" } else { e.names.get(x) };
            format!(
                "{} {} {} {}: {}",
                v.code.as_str(),
                n(v.cell),
                n(v.path),
                n(v.param),
                v.detail
            )
        })
        .collect();
    assert_eq!(blocking.is_empty(), !e.violations.blocking());
    if blocking.is_empty() {
        Ok(())
    } else {
        Err(Blocked::Violations(blocking))
    }
}

/// Byte ranges of the relative corner `lib`/`include` paths in `text`, the netlist `e`
/// emitted for `dialect` (the simulators: SPICE `.lib <path> <section>`, `.include
/// <path>`; Spectre `include "<path>" section=<section>`, `include "<path>"`). Corner
/// lines follow the header, globals and `.param` lines in file order, so each one's first
/// match after the previous one is the line itself.
fn relative_paths(e: &Elab, dialect: Dialect, text: &str) -> Vec<RelativePath> {
    let mut out = Vec::new();
    let b = &e.bench;
    if !crate::SIMULATORS.contains(&dialect) || b.corner_kind.is_empty() {
        return out;
    }
    assert_eq!(b.corner_start.len(), b.corner_kind.len() + 1, "CSR offsets");
    assert_eq!(
        b.corner_project.len(),
        b.corner_kind.len(),
        "parallel columns"
    );
    let spectre = dialect == Dialect::Spectre;
    let (lib, include, section) = if spectre {
        ("\ninclude \"", "\ninclude \"", "\" section=")
    } else {
        ("\n.lib ", "\n.include ", " ")
    };
    let mut from = 0;
    let mut line = String::new();
    for k in 0..b.corner_kind.len() {
        let args = &b.corner_args[b.corner_start[k] as usize..b.corner_start[k + 1] as usize];
        let arg = |i: usize| args.get(i).map_or("", |n| e.names.get(*n));
        let path = arg(0);
        line.clear();
        match b.corner_kind[k] {
            CornerLine::Lib => line.push_str(lib),
            CornerLine::Include => line.push_str(include),
            CornerLine::Temp => continue,
        }
        let prefix = line.len();
        line.push_str(path);
        if b.corner_kind[k] == CornerLine::Lib {
            line.push_str(section);
            line.push_str(arg(1));
        } else if spectre {
            line.push('"');
        }
        line.push('\n');
        let Some(at) = text[from..].find(line.as_str()).map(|i| from + i) else {
            continue;
        };
        from = at + line.len() - 1;
        if relative(path) {
            let start = (at + prefix) as u32;
            out.push(RelativePath {
                range: start..start + path.len() as u32,
                from_project: b.corner_project[k],
            });
        }
    }
    assert!(out.windows(2).all(|w| w[0].range.end <= w[1].range.start));
    out
}

/// Whether a corner path is relative to the bench: not `/…`, `\…`, `~…`, `C:…`, quoted,
/// or a `${NAME}` left unexpanded.
fn relative(path: &str) -> bool {
    let b = path.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    !path.is_empty() && !drive && !path.starts_with(['/', '\\', '~', '$', '"'])
}
