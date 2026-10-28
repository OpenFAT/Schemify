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
//! - `pub(crate) struct DefBlock`
//! - `pub(crate) struct Writer`: `Writer::new(context, order) -> Writer`,
//!   `Writer::def_block(&mut self, def: u32) -> DefBlock`,
//!   `Writer::finish(self) -> (Uses, NameMap)`

use crate::inst;
use crate::inst::{Context, RowScratch, Uses};
use crate::name_map::{Builder, NameMap};
use crate::{Lines, NONE, Respelling, deck};
use schemify_elab::{Dialect, Elab, Ignore, Name, Target};
use spicerack::codegen::xschem;
use spicerack::ir::Subcircuit;
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::thread::ScopedJoinHandle;

/// One emitted definition, as [`crate::Lowered`] holds it.
pub(crate) struct DefBlock {
    /// Its ports, its cell's own code, D17 as amended, spec §7.6.15, and for CDL its
    /// parameter tuple.
    pub(crate) subcircuit: Subcircuit,
    /// Its rows as packed lines, one run per rendered part (at least one).
    pub(crate) lines: Vec<Lines>,
    /// Its Verilog `wire`s (empty for other dialects).
    pub(crate) nets: Vec<String>,
}

/// The lowering state of one [`crate::lower`] call: per-row scratch and the name map
/// under construction. Blocks must be written in name-map order (the top, then `order`),
/// the order `new` numbers them in.
pub(crate) struct Writer<'elab> {
    context: Context<'elab>,
    scratch: RowScratch,
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
struct Rendered {
    /// One run per rendered part, in row order.
    runs: Vec<Lines>,
    /// Per line, in row order.
    kept: Vec<Kept>,
}

/// One rendered line's row.
#[derive(Clone, Copy, Debug)]
struct Kept {
    row: u32,
    /// The template rendered; a failed row is an error comment line with no name-map row.
    rendered: bool,
}

/// Renders every block on one thread (`true`) or lets large blocks use several (`false`,
/// the default). The text is identical either way; tests compare the two. Process-wide.
#[doc(hidden)]
pub fn set_serial(serial: bool) {
    SERIAL.store(serial, Ordering::Relaxed);
}

impl<'elab> Writer<'elab> {
    /// Numbers the emitted definitions (the top first when there is one, then `order`)
    /// and sizes the name map for their rows.
    pub(crate) fn new(context: Context<'elab>, order: &[u32]) -> Writer<'elab> {
        let elab = context.elab;
        let mut def_map = vec![NONE; elab.defs.cell.len()];
        let top = (elab.top != NONE).then_some(elab.top);
        let emitted = top.into_iter().chain(order.iter().copied());
        let mut rows = 0;
        let mut defs = 0;
        for def in emitted {
            assert_eq!(def_map[def as usize], NONE, "definition {def} listed once");
            def_map[def as usize] = defs;
            defs += 1;
            rows += elab.insts_of(def).len();
        }
        assert_eq!(defs as usize, order.len() + usize::from(top.is_some()));
        if let Some(top) = top {
            assert_eq!(
                def_map[top as usize], 0,
                "the name map's definition 0 is the top"
            );
        }
        let respellings = deck::xspice_spellings(elab, context.plans, context.dialect);
        Writer {
            context,
            scratch: RowScratch::new(elab),
            def_map,
            map: Builder::new(elab, context.dialect, defs as usize, rows, respellings),
        }
    }

    /// Definition `def`: its subcircuit, its rows as lines and its Verilog `wire`s. The
    /// deck sorts the rows by element name.
    pub(crate) fn def_block(&mut self, def: u32) -> DefBlock {
        let Context { elab, dialect, .. } = self.context;
        assert!(
            self.def_map[def as usize] != NONE,
            "definition {def} is emitted"
        );
        self.map.def(elab, def, &self.def_map);
        let lines = self.rows(def);
        let code = elab.defs.code_start[def as usize] as usize
            ..elab.defs.code_start[def as usize + 1] as usize;
        let raw_spice: Vec<String> = elab.defs.code[code]
            .iter()
            .map(|l| elab.names.get(*l).to_owned())
            .collect();
        let subcircuit = Subcircuit {
            name: elab.names.get(elab.defs.name[def as usize]).to_owned(),
            ports: deck::ports(elab, def, &self.map.respellings),
            parameters: if dialect == Dialect::Cdl {
                deck::cdl_params(elab, def)
            } else {
                Vec::new()
            },
            raw_spice,
            ..Subcircuit::default()
        };
        let nets = if dialect == Dialect::Verilog {
            wires(elab, def)
        } else {
            Vec::new()
        };
        assert!(!lines.is_empty(), "a block has at least one run");
        assert_eq!(
            subcircuit.ports.len(),
            elab.ports(def).len(),
            "one port per port net"
        );
        DefBlock {
            subcircuit,
            lines,
            nets,
        }
    }

    /// What the emitted rows used, and the finished name map.
    pub(crate) fn finish(self) -> (Uses, NameMap) {
        (self.scratch.uses, self.map.finish())
    }
}

// ---- private ----

impl Writer<'_> {
    /// Instance rows of definition `def` in row order as packed lines, each run marked
    /// sorted when the whole block is in element-name order, and their name-map rows.
    fn rows(&mut self, def: u32) -> Vec<Lines> {
        let mut block = self.render_parts(self.context.elab.insts_of(def));
        let sorted = self.record_names(&block);
        // The deck then copies the runs without reading their names again.
        for run in &mut block.runs {
            run.sorted = sorted;
        }
        block.runs
    }

    /// Renders `rows`: the first part here into `self.scratch`, every other part on its
    /// own thread with its own scratch, whose uses are merged after, in row order.
    fn render_parts(&mut self, rows: Range<usize>) -> Rendered {
        let context = self.context;
        let chunks = if rows.len() >= PARALLEL_ROWS && !SERIAL.load(Ordering::Relaxed) {
            thread::available_parallelism().map_or(1, |n| n.get().min(THREADS_MAX))
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
        let respellings: &[Respelling] = &self.map.respellings;
        let all = thread::scope(|scope| {
            let mut workers: Vec<ScopedJoinHandle<'_, (Rendered, Uses)>> =
                Vec::with_capacity(parts.len() - 1);
            for part in &parts[1..] {
                let part = part.clone();
                workers.push(scope.spawn(move || {
                    let mut scratch = RowScratch::new(context.elab);
                    let rendered = render(context, respellings, part, &mut scratch);
                    (rendered, scratch.uses)
                }));
            }
            let mut all = render(context, respellings, parts[0].clone(), &mut self.scratch);
            for worker in workers {
                let (part, uses) = worker.join().expect("rows render");
                self.scratch.uses.merge(uses);
                all.runs.extend(part.runs);
                all.kept.extend(part.kept);
            }
            all
        });
        assert_eq!(all.runs.len(), parts.len(), "one run per part");
        debug_assert!(
            all.kept.windows(2).all(|w| w[0].row < w[1].row),
            "parts append in row order"
        );
        all
    }

    /// Records the name-map row of every rendered line of `block` under the element name
    /// the deck writes; returns whether those names are in order.
    fn record_names(&mut self, block: &Rendered) -> bool {
        let elab = self.context.elab;
        let dialect = crate::spicerack_dialect(self.context.dialect);
        assert_eq!(
            block.kept.len(),
            block.runs.iter().map(Lines::len).sum::<usize>()
        );
        let (mut run, mut line) = (0, 0);
        let (mut sorted, mut prev) = (true, "");
        for kept in &block.kept {
            while line == block.runs[run].len() {
                (run, line) = (run + 1, 0); // the next part's run
            }
            let name = xschem::element_name(block.runs[run].get(line), dialect);
            sorted &= prev <= name;
            prev = name;
            if kept.rendered {
                let child = match elab.insts.target[kept.row as usize] {
                    Target::Def(def) => self.def_map[def as usize],
                    Target::Leaf(_) => NONE,
                };
                self.map.row(elab, kept.row as usize, name, child);
            }
            line += 1;
        }
        if let Some(last) = block.runs.get(run) {
            assert_eq!(line, last.len(), "every line is recorded");
        }
        sorted
    }
}

/// Verilog: every net of definition `def`'s emitted rows that is not a port, sorted by
/// name (spec §7.5).
fn wires(elab: &Elab, def: u32) -> Vec<String> {
    let ports = elab.ports(def);
    let mut wires: Vec<Name> = Vec::new();
    let emitted = elab
        .insts_of(def)
        .filter(|&r| elab.insts.ignore[r] == Ignore::No);
    for r in emitted {
        wires.extend(elab.conns(r).iter().filter(|n| **n != Name::NONE));
    }
    wires.sort_unstable();
    wires.dedup();
    wires.retain(|n| !ports.contains(n));
    elab.names.sort(&mut wires);
    assert!(!wires.contains(&Name::NONE));
    let texts: Vec<String> = wires
        .iter()
        .map(|n| elab.names.get(*n).to_owned())
        .collect();
    assert_eq!(texts.len(), wires.len());
    texts
}

/// Renders rows `rows` of one block straight into one run of packed lines (spec §7.6.1:
/// a failed template becomes an error comment, an empty rendering no element). The
/// impls and models used accumulate in `scratch`.
fn render(
    context: Context<'_>,
    respellings: &[Respelling],
    rows: Range<usize>,
    scratch: &mut RowScratch,
) -> Rendered {
    let Context { elab, dialect, .. } = context;
    assert!(
        rows.end <= elab.insts.name.len(),
        "rows of this elaboration"
    );
    let mut lines = Lines {
        text: String::with_capacity(48 * rows.len()),
        ends: Vec::with_capacity(rows.len()),
        sorted: false,
    };
    let mut kept = Vec::with_capacity(rows.len());
    let emitted = rows.filter(|&r| elab.insts.ignore[r] == Ignore::No);
    for r in emitted {
        let local = elab.names.get(elab.insts.name[r]);
        let start = lines.text.len();
        let net = |n: Name| crate::spelled(elab, respellings, n);
        let rendered = inst::line(context, r, local, net, &mut lines.text, scratch);
        match &rendered {
            Ok(()) if lines.text.len() == start => continue, // renders nothing
            Ok(()) => {}
            Err(message) => {
                lines.text.truncate(start);
                push_error(&mut lines.text, dialect, local, message);
            }
        }
        // The deck copies a part's lines whole, newlines included.
        if !lines.text.ends_with('\n') {
            lines.text.push('\n');
        }
        lines.end_line();
        kept.push(Kept {
            row: r as u32,
            rendered: rendered.is_ok(),
        });
    }
    assert_eq!(kept.len(), lines.len());
    assert!(lines.text.is_empty() || lines.text.ends_with('\n'));
    Rendered {
        runs: vec![lines],
        kept,
    }
}

/// Appends the comment a failed row is written as: `* error in <name>: <message>`.
fn push_error(out: &mut String, dialect: Dialect, local: &str, message: &str) {
    use std::fmt::Write;

    let start = out.len();
    let comment = xschem::comment(crate::spicerack_dialect(dialect));
    let message = message.replace('\n', " ");
    write!(out, "{comment} error in {local}: {message}").expect("a String takes any text");
    assert!(out.len() > start);
    assert!(
        !out[start..].contains('\n'),
        "the error is one comment line"
    );
}
