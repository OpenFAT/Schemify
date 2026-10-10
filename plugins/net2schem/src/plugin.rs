//! Guest glue (wasm32 only): contributions and the import job.
//!
//! `net2schem.import <path>` (File ▸ Import SPICE netlist…, `Ctrl+Shift+I`) runs one job
//! through inverted I/O (plugin ABI §5), one phase per completion event:
//!
//! ```text
//! command        → fs_read(path)                                read
//! io_done(read)  → convert → fs_read(each output name ×3 kinds) probe   (refuse to overwrite)
//! io_done(probe) → … all answered → fs_write(each new file)      write
//! io_done(write) → … all answered → log summary                  idle
//! ```
//!
//! Without a path argument the command shows a small panel with a path field whose Enter
//! re-issues the command with the typed path. A new command abandons a job in flight
//! (late completions of its handles are ignored), so a trap mid-job never wedges the
//! plugin.

use std::cell::RefCell;

use schemify_abi::guest::{self, Host};
use schemify_abi::{Code, Event, Kind, Level, Out, text};

use crate::OutFile;

guest::export!(on_event);

/// The menu item; the path panel is appended while it is shown.
const UI: &str = "node file.net2schem_import item
  parent menu.file
  label \"Import SPICE netlist…\"
  command net2schem.import
  accel Ctrl+Shift+I
";
const PANEL: &str = "node net2schem.panel panel
  parent window
node net2schem.prompt label
  parent net2schem.panel
  label \"SPICE netlist to import (project-relative .sp/.cir/.spice path), Enter to convert:\"
node net2schem.path text_input
  parent net2schem.panel
  command net2schem.import
node net2schem.close button
  parent net2schem.panel
  label Close
  command net2schem.close
";
const COMMANDS: &str = "command net2schem.import \"Import SPICE netlist…\"
command net2schem.close \"Close the SPICE import panel\"
";
const KEYMAP: &str = "key Ctrl+Shift+I net2schem.import\n";
/// I/O requests kept in flight (the host's queue holds 256 per frame).
const WINDOW: usize = 32;
/// Warnings echoed to the log per import.
const WARNINGS_SHOWN: usize = 20;
/// Element cards converted within the default fuel budget (~0.3M fuel per card drawn
/// with labels, more when routed); larger netlists are refused with a log instead of
/// trapping mid-conversion.
// ponytail: split the work over several events (one cell per completion) to lift it.
const CARDS_MAX: usize = 250;

/// What a handle in flight stands for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Read,
    /// Probe of path `probes[i]`.
    Probe(u32),
    /// Write of `files[i]`.
    Write(u32),
}

/// One import. Lifetime: from the command to the last completion.
#[derive(Default)]
struct Job {
    source: String,
    dir: String,
    files: Vec<OutFile>,
    summary: String,
    /// (path, file index) to probe, issued in order.
    probes: Vec<(String, u32)>,
    /// Per file: some probed name exists.
    exists: Vec<bool>,
    /// Files to write, issued in order.
    writes: Vec<u32>,
    issued: usize,
    failed: Vec<String>,
    in_flight: Vec<(u32, Op)>,
}

thread_local! {
    /// The job in flight. Taken out while an event handles it, so a trap leaves `None`.
    static JOB: RefCell<Option<Job>> = const { RefCell::new(None) };
}

fn contribute(out: &mut Out, panel: bool) {
    let ui = if panel { format!("{UI}{PANEL}") } else { UI.to_owned() };
    out.contribute(Kind::Ui, &ui);
    out.contribute(Kind::Commands, COMMANDS);
    out.contribute(Kind::Keymap, KEYMAP);
}

fn on_event(ev: Event<'_>, host: &Host, out: &mut Out) {
    match ev {
        Event::Load => contribute(out, false),
        Event::Command { name: "net2schem.close", .. } => contribute(out, false),
        Event::Command { name: "net2schem.import", args } => {
            let mut words = Vec::new();
            let mut values = text::values(args);
            while let Ok(v) = values.need() {
                words.push(v.into_owned());
            }
            let path = words.join(" ");
            if path.is_empty() {
                contribute(out, true);
                return;
            }
            contribute(out, false);
            JOB.set(None); // abandon any job in flight
            start(&path, host, out);
        }
        Event::IoDone { handle, status, bytes } => {
            let Some(mut job) = JOB.take() else {
                return;
            };
            let Some(at) = job.in_flight.iter().position(|f| f.0 == handle) else {
                JOB.set(Some(job)); // not ours (an abandoned job)
                return;
            };
            let (_, op) = job.in_flight.swap_remove(at);
            if step(&mut job, op, status, bytes, host, out) {
                JOB.set(Some(job));
            }
        }
        Event::Rejected { code, record } => {
            let record = String::from_utf8_lossy(record);
            out.log_at(Level::Warn, &format!("net2schem: output rejected ({code}): {record}"));
        }
        _ => {}
    }
}

/// Starts an import of `path` (project-relative).
fn start(path: &str, host: &Host, out: &mut Out) {
    let lower = path.to_ascii_lowercase();
    if ![".sp", ".cir", ".spice", ".net", ".cdl"].iter().any(|e| lower.ends_with(e)) {
        out.log_at(Level::Warn, &format!("net2schem: {path}: not a .sp/.cir/.spice file; importing anyway"));
    }
    match host.fs_read(path) {
        Ok(h) => {
            let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_owned();
            let job = Job { source: path.to_owned(), dir, in_flight: vec![(h, Op::Read)], ..Job::default() };
            JOB.set(Some(job));
        }
        Err(c) => out.log_at(Level::Error, &format!("net2schem: cannot read {path}: {c}{}", hint(c))),
    }
}

fn hint(c: Code) -> &'static str {
    match c {
        Code::Denied => " (grant read_project/write_project in schemify.project, and use a project-relative path)",
        _ => "",
    }
}

fn out_path(job: &Job, name: &str) -> String {
    if job.dir.is_empty() { name.to_owned() } else { format!("{}/{name}", job.dir) }
}

/// Handles completion `op`; returns whether the job continues.
fn step(job: &mut Job, op: Op, status: i32, bytes: &[u8], host: &Host, out: &mut Out) -> bool {
    match op {
        Op::Read => {
            if status < 0 {
                let c = Code::from_raw(status.unsigned_abs()).unwrap_or(Code::Gone);
                out.log_at(Level::Error, &format!("net2schem: cannot read {}: {c}{}", job.source, hint(c)));
                return false;
            }
            if bytes.len() > crate::spice::TEXT_MAX {
                out.log_at(Level::Error, &format!("net2schem: {} is over {} bytes", job.source, crate::spice::TEXT_MAX));
                return false;
            }
            let text = String::from_utf8_lossy(bytes);
            let cards = text.lines().filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_alphabetic())).count();
            if cards > CARDS_MAX {
                out.log_at(
                    Level::Error,
                    &format!("net2schem: {}: {cards} element cards; the plugin converts at most {CARDS_MAX} (split the netlist)", job.source),
                );
                return false;
            }
            let conv = crate::convert(&job.source, &text);
            if conv.files.is_empty() {
                out.log_at(Level::Warn, &format!("net2schem: {}: no subckt or top-level card to import", job.source));
                return false;
            }
            let s = conv.stats;
            job.summary = format!(
                "net2schem: {} → {} files ({} cells); {} devices, {} wires, {} labels/ports; {}/{} nets fully wired; {} warnings",
                job.source,
                conv.files.len(),
                s.cells,
                s.devices,
                s.wires,
                s.markers,
                s.nets_wired,
                s.nets_multi,
                conv.warnings.len()
            );
            for w in conv.warnings.iter().take(WARNINGS_SHOWN) {
                out.log_at(Level::Warn, &format!("net2schem: {}: {w}", job.source));
            }
            for (i, f) in conv.files.iter().enumerate() {
                let stem = f.name.rsplit_once('.').map_or(f.name.as_str(), |(s, _)| s);
                for ext in ["shcomp", "shprim", "shbench"] {
                    job.probes.push((out_path(job, &format!("{stem}.{ext}")), i as u32));
                }
            }
            job.exists = vec![false; conv.files.len()];
            job.files = conv.files;
            pump(job, host, out)
        }
        Op::Probe(i) => {
            if status >= 0 {
                let file = job.probes[i as usize].1 as usize;
                job.exists[file] = true;
            }
            if job.issued < job.probes.len() || !job.in_flight.is_empty() {
                return pump(job, host, out);
            }
            // Every name answered: refuse to overwrite a cell; keep shared leaves.
            let clash: Vec<&str> = (0..job.files.len())
                .filter(|&k| job.exists[k] && !job.files[k].shared)
                .map(|k| job.files[k].name.as_str())
                .collect();
            if !clash.is_empty() {
                out.log_at(
                    Level::Error,
                    &format!("net2schem: {}: not imported, these cells already exist: {}", job.source, clash.join(" ")),
                );
                return false;
            }
            job.writes = (0..job.files.len() as u32).filter(|&k| !job.exists[k as usize]).collect();
            job.issued = 0;
            pump(job, host, out)
        }
        Op::Write(i) => {
            if status < 0 {
                job.failed.push(job.files[i as usize].name.clone());
            }
            if job.issued < job.writes.len() || !job.in_flight.is_empty() {
                return pump(job, host, out);
            }
            if job.failed.is_empty() {
                out.log(&job.summary);
            } else {
                out.log_at(Level::Error, &format!("net2schem: {}: could not write {}", job.source, job.failed.join(" ")));
            }
            false
        }
    }
}

/// Issues queued probes or writes up to [`WINDOW`] in flight; returns whether the job
/// continues (`false` after a refused request, which is logged).
fn pump(job: &mut Job, host: &Host, out: &mut Out) -> bool {
    let writing = !job.writes.is_empty();
    let total = if writing { job.writes.len() } else { job.probes.len() };
    while job.issued < total && job.in_flight.len() < WINDOW {
        let r = if writing {
            let k = job.writes[job.issued];
            let f = &job.files[k as usize];
            host.fs_write(&out_path(job, &f.name), &f.bytes).map(|h| (h, Op::Write(k)))
        } else {
            let (path, _) = &job.probes[job.issued];
            host.fs_read(path).map(|h| (h, Op::Probe(job.issued as u32)))
        };
        match r {
            Ok(entry) => {
                job.in_flight.push(entry);
                job.issued += 1;
            }
            Err(Code::Space) if !job.in_flight.is_empty() => break, // resume on the next completion
            Err(c) => {
                out.log_at(Level::Error, &format!("net2schem: {}: I/O refused: {c}{}", job.source, hint(c)));
                return false;
            }
        }
    }
    true
}
