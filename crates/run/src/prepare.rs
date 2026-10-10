//! The spine's design steps (run spec §4, §5): the gate and design hash of a run, and
//! per job `elaborate(overrides) → netlist::emit`, plus the source files the simulator
//! needs compiled first (Verilog-A → OSDI, Verilog → VVP).

use crate::jobs::Jobs;
use crate::results::Results;
use schemify_doc::{Digest, DocHandle, Project, Sha256, sha256};
use schemify_elab::{Dialect, Elab, Funcs, Name, Request, Severity, elaborate};
use schemify_netlist as netlist;

/// Why a run cannot start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Elaboration needs these cell rows opened (`Project::open`); then call again.
    NeedsOpen(Vec<u32>),
    /// Blocking violations (D13), one line each: `E005 lib:cell /X1 W: detail`.
    Violations(Vec<String>),
}

/// A source file a job's simulator needs compiled into the job directory first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Compiler.
    pub tool: Tool,
    /// Library row of the cell that names the file (`Project::libs`).
    pub lib: u8,
    /// Path relative to the library directory (the cell's directory joined with `file`).
    pub path: String,
    /// File name the netlist expects in the job directory (`<file stem>.osdi|.vvp`).
    pub out: String,
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
    /// Files to compile first, deduplicated by `out`.
    pub sources: Vec<Source>,
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
    let p = Prepared {
        netlist: netlist::emit(&e, dialect).text,
        sources: sources(project, &e),
    };
    assert!(!p.netlist.is_empty());
    Some(p)
}

/// Returns the Verilog-A and Verilog files of the leaf impls `e` uses (impls with a
/// `file` attribute; run spec §5), deduplicated by output name.
pub fn sources(project: &Project, e: &Elab) -> Vec<Source> {
    let mut out: Vec<Source> = Vec::new();
    let cells = project.cells();
    for i in 0..e.impls.cell.len() {
        let iface = &e.cells[e.impls.cell[i] as usize];
        let k = e.impls.index[i] as usize;
        let (tool, ext) = match e.names.get(iface.impl_lang[k]) {
            "veriloga" => (Tool::Openvaf, "osdi"),
            "verilog" => (Tool::Iverilog, "vvp"),
            _ => continue,
        };
        if iface.impl_file[k] == Name::NONE || iface.row == u32::MAX {
            continue; // ponytail: inline `body` sources are not compiled yet.
        }
        let file = e.names.get(iface.impl_file[k]);
        let dir = project.strings().get(cells.dir[iface.row as usize]);
        let path = if dir.is_empty() {
            file.to_owned()
        } else {
            format!("{dir}/{file}")
        };
        let stem = file.rsplit('/').next().unwrap_or(file);
        let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
        let src = Source {
            tool,
            lib: cells.lib[iface.row as usize],
            path,
            out: format!("{stem}.{ext}"),
        };
        if !out.iter().any(|s| s.out == src.out) {
            out.push(src);
        }
    }
    out.sort_by(|a, b| a.out.cmp(&b.out));
    assert!(out.windows(2).all(|w| w[0].out < w[1].out));
    out
}
