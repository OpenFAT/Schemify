//! Blocks (netlist spec §7.1, §7.6.0–1, §7.6.15): one emitted definition in, its
//! SpiceRack subcircuit, its rows as packed element lines, its Verilog `wire`s and its
//! name-map rows out.
//!
//! A large block renders in chunks on scoped threads, appended in row order, so the
//! lines are the serial lines (D37; oracle `tests/metamorphic.rs`
//! `threads_change_nothing`). The name map is recorded on the calling thread.
//!
//! Contents:
//! - `pub fn set_serial(serial: bool)`
//! - `pub(crate) struct Writer`: `Writer::new(e, d, plans, order) -> Writer`,
//!   `Writer::def_block(&mut self, d: u32) -> (Subcircuit, Vec<Lines>, Vec<String>)`,
//!   `Writer::finish(self) -> (Uses, NameMap)`

use crate::inst::{self, Tmp, Uses};
use crate::name_map::{Builder, NameMap};
use crate::plan::Plans;
use crate::{Lines, NONE, deck, sr};
use schemify_elab::{Dialect, Elab, Ignore, Name, Target};
use spicerack::codegen::xschem;
use spicerack::ir::Subcircuit;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// The lowering state of one [`crate::lower`] call: the plans, per-row scratch and the
/// name map under construction. Blocks must be written in name-map order (the top, then
/// `order`), the order `new` numbers them in.
pub(crate) struct Writer<'e> {
    e: &'e Elab,
    d: Dialect,
    plans: &'e Plans,
    tmp: Tmp,
    /// Elab definition → name-map definition (`NONE` when not emitted).
    def_map: Vec<u32>,
    map: Builder,
}

/// Blocks with at least this many rows render on several threads.
const PARALLEL_ROWS: usize = 1 << 15;

/// Render threads per block, at most.
const THREADS_MAX: usize = 4;

/// Forces serial rendering (the D37 jobs = 1 vs N oracle).
// ponytail: a process-wide test knob, like elab's; a parameter if a caller ever needs it.
static SERIAL: AtomicBool = AtomicBool::new(false);

/// The rows of one block in row order as packed lines.
struct Block {
    /// One run per rendered part, in row order.
    runs: Vec<Lines>,
    /// Per line: its row, and whether it rendered (a failed row is an error comment line
    /// with no name-map row).
    kept: Vec<(u32, bool)>,
}

/// Renders every block on one thread (`true`) or lets large blocks use several (`false`,
/// the default). The text is identical either way; tests compare the two. Process-wide.
#[doc(hidden)]
pub fn set_serial(serial: bool) {
    SERIAL.store(serial, Relaxed);
}

impl<'e> Writer<'e> {
    /// Numbers the emitted definitions (the top first when there is one, then `order`)
    /// and sizes the name map for their rows.
    pub(crate) fn new(e: &'e Elab, d: Dialect, plans: &'e Plans, order: &[u32]) -> Writer<'e> {
        let mut def_map = vec![NONE; e.defs.cell.len()];
        let emitted = (e.top != NONE)
            .then_some(e.top)
            .into_iter()
            .chain(order.iter().copied());
        let mut rows = 0;
        for (k, def) in emitted.enumerate() {
            assert_eq!(def_map[def as usize], NONE, "definition {def} listed once");
            def_map[def as usize] = k as u32;
            rows += e.insts_of(def).len();
        }
        let defs = order.len() + usize::from(e.top != NONE);
        let respell = deck::xspice_spellings(e, plans, d);
        Writer {
            e,
            d,
            plans,
            tmp: Tmp::new(e),
            def_map,
            map: Builder::new(e, d, defs, rows, respell),
        }
    }

    /// Definition `d`: the subcircuit (its ports, its cell's own code, D17 as amended,
    /// spec §7.6.15, and for CDL its parameter tuple), its rows as lines and its Verilog
    /// `wire`s. The deck sorts the rows by element name.
    pub(crate) fn def_block(&mut self, d: u32) -> (Subcircuit, Vec<Lines>, Vec<String>) {
        let e = self.e;
        assert!(
            self.def_map[d as usize] != NONE,
            "definition {d} is emitted"
        );
        self.map.def(e, d, &self.def_map);
        let lines = self.rows(d);
        let code =
            e.defs.code_start[d as usize] as usize..e.defs.code_start[d as usize + 1] as usize;
        let sc = Subcircuit {
            name: e.names.get(e.defs.name[d as usize]).to_owned(),
            ports: deck::ports(e, d, &self.map.respell),
            parameters: if self.d == Dialect::Cdl {
                deck::cdl_params(e, d)
            } else {
                Vec::new()
            },
            raw_spice: e.defs.code[code]
                .iter()
                .map(|l| e.names.get(*l).to_owned())
                .collect(),
            ..Subcircuit::default()
        };
        let nets = if self.d == Dialect::Verilog {
            wires(e, d)
        } else {
            Vec::new()
        };
        assert!(!lines.is_empty(), "a block has at least one run");
        (sc, lines, nets)
    }

    /// What the emitted rows used, and the finished name map.
    pub(crate) fn finish(self) -> (Uses, NameMap) {
        (self.tmp.uses, self.map.finish())
    }
}

// ---- private ----

impl Writer<'_> {
    /// Instance rows of definition `d` in row order as packed lines, each run marked
    /// sorted when the whole block is in element-name order, and their name-map rows.
    fn rows(&mut self, d: u32) -> Vec<Lines> {
        let mut block = self.render_parts(self.e.insts_of(d));
        let sorted = self.record_names(&block);
        // The deck then copies the runs without reading their names again.
        block.runs.iter_mut().for_each(|l| l.sorted = sorted);
        block.runs
    }

    /// Renders `rows`: the first part here into `self.tmp`, every other part on its own
    /// thread with its own scratch, whose uses are merged after, in row order.
    fn render_parts(&mut self, rows: Range<usize>) -> Block {
        let e = self.e;
        let chunks = if rows.len() >= PARALLEL_ROWS && !SERIAL.load(Relaxed) {
            std::thread::available_parallelism().map_or(1, |n| n.get().min(THREADS_MAX))
        } else {
            1
        };
        let step = rows.len().div_ceil(chunks).max(1);
        // At least one part, so an empty block renders one empty run.
        let parts: Vec<Range<usize>> = (rows.start..rows.end.max(rows.start + 1))
            .step_by(step)
            .map(|a| a..(a + step).min(rows.end))
            .collect();
        assert!(!parts.is_empty());
        assert!(parts.len() <= THREADS_MAX);
        let (d, plans, respell) = (self.d, self.plans, &self.map.respell);
        std::thread::scope(|sc| {
            let workers: Vec<_> = parts[1..]
                .iter()
                .map(|p| {
                    sc.spawn(|| {
                        let mut tmp = Tmp::new(e);
                        (render(e, d, plans, respell, p.clone(), &mut tmp), tmp.uses)
                    })
                })
                .collect();
            let mut all = render(e, d, plans, respell, parts[0].clone(), &mut self.tmp);
            for w in workers {
                let (part, uses) = w.join().expect("rows render");
                self.tmp.uses.merge(uses);
                all.runs.extend(part.runs);
                all.kept.extend(part.kept);
            }
            all
        })
    }

    /// Records the name-map row of every rendered line of `block` under the element name
    /// the deck writes; returns whether those names are in order.
    fn record_names(&mut self, block: &Block) -> bool {
        let (e, xd) = (self.e, sr(self.d));
        assert_eq!(
            block.kept.len(),
            block.runs.iter().map(Lines::len).sum::<usize>()
        );
        let (mut run, mut line) = (0, 0);
        let (mut sorted, mut prev) = (true, "");
        for &(r, rendered) in &block.kept {
            while line == block.runs[run].len() {
                (run, line) = (run + 1, 0); // the next part's run
            }
            let name = xschem::element_name(block.runs[run].get(line), xd);
            sorted &= prev <= name;
            prev = name;
            if rendered {
                let child = match e.insts.target[r as usize] {
                    Target::Def(c) => self.def_map[c as usize],
                    Target::Leaf(_) => NONE,
                };
                self.map.row(e, r as usize, name, child);
            }
            line += 1;
        }
        sorted
    }
}

/// Verilog: every net of definition `d`'s emitted rows that is not a port, sorted by
/// name (spec §7.5).
fn wires(e: &Elab, d: u32) -> Vec<String> {
    let ports = e.ports(d);
    let mut wires: Vec<Name> = Vec::new();
    for r in e.insts_of(d).filter(|&r| e.insts.ignore[r] == Ignore::No) {
        wires.extend(e.conns(r).iter().filter(|n| **n != Name::NONE));
    }
    wires.sort_unstable();
    wires.dedup();
    wires.retain(|n| !ports.contains(n));
    e.names.sort(&mut wires);
    assert!(!wires.contains(&Name::NONE));
    wires.iter().map(|n| e.names.get(*n).to_owned()).collect()
}

/// Renders rows `rows` of one block straight into one run of packed lines (spec §7.6.1:
/// a failed template becomes an error comment, an empty rendering no element). The
/// impls and models used accumulate in `tmp`.
fn render(
    e: &Elab,
    d: Dialect,
    plans: &Plans,
    respell: &[(Name, String)],
    rows: Range<usize>,
    tmp: &mut Tmp,
) -> Block {
    let mut lines = Lines {
        text: String::with_capacity(48 * rows.len()),
        ends: Vec::with_capacity(rows.len()),
        sorted: false,
    };
    let mut kept = Vec::with_capacity(rows.len());
    for r in rows.filter(|&r| e.insts.ignore[r] == Ignore::No) {
        let local = e.names.get(e.insts.name[r]);
        let start = lines.text.len();
        let net = |n: Name| crate::spelled(e, respell, n);
        let rendered = inst::line(e, d, plans, r, local, net, &mut lines.text, tmp);
        match &rendered {
            Ok(()) if lines.text.len() == start => continue, // renders nothing
            Ok(()) => {}
            Err(msg) => {
                lines.text.truncate(start);
                push_error(&mut lines.text, d, local, msg);
            }
        }
        // The deck copies a part's lines whole, newlines included.
        if !lines.text.ends_with('\n') {
            lines.text.push('\n');
        }
        lines.end_line();
        kept.push((r as u32, rendered.is_ok()));
    }
    assert_eq!(kept.len(), lines.len());
    Block {
        runs: vec![lines],
        kept,
    }
}

/// Appends the comment a failed row is written as: `* error in <name>: <message>`.
fn push_error(out: &mut String, d: Dialect, local: &str, msg: &str) {
    let start = out.len();
    out.push_str(xschem::comment(sr(d)));
    out.push_str(" error in ");
    out.push_str(local);
    out.push_str(": ");
    out.push_str(&msg.replace('\n', " "));
    assert!(out.len() > start);
}
