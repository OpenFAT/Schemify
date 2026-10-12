//! The spine's design steps (run spec §4, §5): the gate and design hash of a run, and
//! per job `elaborate(overrides) → netlist::emit`, plus the sources the simulator needs
//! compiled first (Verilog-A → OSDI, Verilog → VVP) and the relative corner paths the
//! runner makes absolute.

use crate::jobs::Jobs;
use crate::results::Results;
use schemify_doc::{Digest, DocHandle, Project, Sha256, sha256};
use schemify_elab::{CornerLine, Dialect, Elab, Funcs, Name, Request, Severity, elaborate};
use schemify_netlist as netlist;
use std::ops::Range;

/// Why a run cannot start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Elaboration needs these cell rows opened (`Project::open`); then call again.
    NeedsOpen(Vec<u32>),
    /// Blocking violations (D13), one line each: `E005 lib:cell /X1 W: detail`.
    Violations(Vec<String>),
}

/// A source a job's simulator needs compiled into the job directory first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Compiler.
    pub tool: Tool,
    /// Where the source text is.
    pub origin: Origin,
    /// File name the netlist expects in the job directory (`<stem>.osdi|.vvp`: the `file`
    /// stem, else the cell stem).
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

/// External compilers (their paths come from the user config, D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Tool {
    /// OpenVAF: Verilog-A → OSDI (`pre_osdi`).
    Openvaf,
    /// Icarus Verilog: Verilog → VVP (ngspice `d_cosim`).
    Iverilog,
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
    /// The bench's library row and its directory relative to that library.
    pub base: (u8, String),
}

impl Prepared {
    /// Returns the netlist with every [`Prepared::relative`] path joined onto `base_dir`
    /// (the bench's directory on disk; quoted when it holds whitespace).
    pub fn resolved(&self, base_dir: &std::path::Path) -> String {
        let mut out = String::with_capacity(self.netlist.len() + 64 * self.relative.len());
        let mut at = 0;
        for r in &self.relative {
            let (start, end) = (r.start as usize, r.end as usize);
            assert!(at <= start && start < end && end <= self.netlist.len());
            out.push_str(&self.netlist[at..start]);
            let abs = base_dir.join(&self.netlist[start..end]);
            let abs = abs.to_string_lossy();
            if abs.contains(char::is_whitespace) {
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

/// Gates the run and returns its design hash (run spec §4): elaborates `top` once per
/// distinct config of `jobs` (the bench default when `jobs` is empty) at the default
/// corner, refuses on blocking violations, and hashes the lockfile, the project
/// settings, `env`, the canonical bytes of the bench and of every cell those
/// elaborations reached, and the job list. Any edit that can change a job's netlist
/// changes the hash, so results tagged with an older one are stale.
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
        let req = Request {
            config,
            corner: None,
            overrides: &[],
            env,
            dialect: Dialect::Ngspice,
        };
        let e = elaborate(project, top, &req, funcs);
        gate(&e)?;
        let rows = e.cells.iter().filter(|c| c.row != u32::MAX);
        reached.extend(rows.map(|c| (e.names.get(c.cell).to_owned(), c.row)));
    }
    reached.sort_unstable();
    reached.dedup();

    let mut h = Sha256::default();
    let mut part = |bytes: &[u8]| {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
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
    part(&sha256(bench.canonical().as_bytes()));
    for (name, row) in &reached {
        let doc = project
            .interface(*row as usize)
            .expect("elaboration reached it");
        part(name.as_bytes());
        part(&sha256(doc.canonical().as_bytes()));
    }
    let mut key = String::new();
    for j in 0..jobs.len() {
        key.clear();
        jobs.write_key(j, &mut key);
        part(key.as_bytes());
    }
    let digest = h.finish();
    assert_ne!(digest, [0; 32]);
    Ok(digest)
}

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

/// Elaborates job `j` with its config, corner and assignments, gates it, emits its
/// netlist for `dialect` and records its bench var values in `results`. A job that
/// cannot be netlisted is marked failed (message in `results`) and `None` returned.
///
/// Cost: one elaboration and one emission.
///
/// # Panics
///
/// Panics if `top` is a stale handle or `j` is not a job of `jobs`.
#[allow(clippy::too_many_arguments)] // every argument is a table or input the step reads
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
    let req = Request {
        config: jobs.config(j),
        corner: jobs.corner(j),
        overrides: &overrides,
        env,
        dialect,
    };
    let e = elaborate(project, top, &req, funcs);
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
    let mut text = netlist::emit(&e, dialect).text;
    let relative = relative_paths(&e, dialect, &text);
    // A Monte Carlo sample fixes the simulator's random seed (run spec §3). ngspice reads
    // `.option seed` before it evaluates `.param` wherever the line stands: before `.end`.
    // ponytail: ngspice only; Xyce takes its seed on the command line (`-randseed`).
    if let (Some(seed), Dialect::Ngspice) = (jobs.seed(j), dialect) {
        assert!(
            text.ends_with(".end\n"),
            "an ngspice netlist ends with .end"
        );
        text.insert_str(
            text.len() - ".end\n".len(),
            &format!(".option seed={seed}\n"),
        );
    }
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
        sources: sources(project, &e),
        relative,
        base,
    };
    assert!(!p.netlist.is_empty());
    Some(p)
}

/// Byte ranges of the relative corner `lib`/`include` paths in `text`, the netlist `e`
/// emitted for `dialect` (SPICE dialects only: `.lib <path> <section>`, `.include
/// <path>`). Corner lines follow the header, globals and `.param` lines in file order, so
/// each one's first match after the previous one is the line itself.
fn relative_paths(e: &Elab, dialect: Dialect, text: &str) -> Vec<Range<u32>> {
    let mut out = Vec::new();
    let b = &e.bench;
    if !matches!(dialect, Dialect::Ngspice | Dialect::Xyce) || b.corner_kind.is_empty() {
        return out;
    }
    let mut from = 0;
    let mut line = String::new();
    for k in 0..b.corner_kind.len() {
        let args = &b.corner_args[b.corner_start[k] as usize..b.corner_start[k + 1] as usize];
        let arg = |i: usize| args.get(i).map_or("", |n| e.names.get(*n));
        let path = arg(0);
        line.clear();
        match b.corner_kind[k] {
            CornerLine::Lib => line.push_str("\n.lib "),
            CornerLine::Include => line.push_str("\n.include "),
            CornerLine::Temp => continue,
        }
        let prefix = line.len();
        line.push_str(path);
        if b.corner_kind[k] == CornerLine::Lib {
            line.push(' ');
            line.push_str(arg(1));
        }
        line.push('\n');
        let Some(at) = text[from..].find(line.as_str()).map(|i| from + i) else {
            continue;
        };
        from = at + line.len() - 1;
        if relative(path) {
            let start = (at + prefix) as u32;
            out.push(start..start + path.len() as u32);
        }
    }
    assert!(out.windows(2).all(|w| w[0].end <= w[1].start));
    out
}

/// Whether a corner path is relative to the bench: not `/…`, `\…`, `~…`, `C:…`, quoted,
/// or a `${NAME}` left unexpanded.
fn relative(path: &str) -> bool {
    let b = path.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    !path.is_empty() && !drive && !path.starts_with(['/', '\\', '~', '$', '"'])
}

/// Returns the Verilog-A and Verilog sources of the leaf impls `e` uses (a `file`, or a
/// `body` that is source: `netlist::compiled_source`; run spec §5), deduplicated by
/// output name.
pub fn sources(project: &Project, e: &Elab) -> Vec<Source> {
    let mut out: Vec<Source> = Vec::new();
    let cells = project.cells();
    for i in 0..e.impls.cell.len() {
        if !netlist::compiled_source(e, i) {
            continue;
        }
        let iface = &e.cells[e.impls.cell[i] as usize];
        let k = e.impls.index[i] as usize;
        let (tool, ext) = match e.names.get(iface.impl_lang[k]) {
            "veriloga" => (Tool::Openvaf, "osdi"),
            _ => (Tool::Iverilog, "vvp"),
        };
        let (origin, stem) = if iface.impl_file[k] == Name::NONE {
            let body = e.impls.body_start[i] as usize..e.impls.body_start[i + 1] as usize;
            let mut text = String::new();
            for &l in &e.impls.body[body] {
                text.push_str(e.names.get(l));
                text.push('\n');
            }
            (Origin::Body(text), e.names.get(iface.stem))
        } else {
            if iface.row == u32::MAX {
                continue; // not a project cell: no directory to resolve `file` against
            }
            let file = e.names.get(iface.impl_file[k]);
            // A derived `tech` cell's impls are its PDK cell's: `file` is relative to that.
            let row = project
                .tech_source(iface.row as usize)
                .unwrap_or(iface.row as usize);
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
    assert!(out.windows(2).all(|w| w[0].out < w[1].out));
    out
}
