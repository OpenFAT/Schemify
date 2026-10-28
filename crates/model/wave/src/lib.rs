//! Waveform data (D23, D26): the `.raw` reader, the column store (`raw_plots`), the calculator
//! binding to `schemify_expr`, display decimation, and the viewer tables. Pure data: no GUI
//! dependency, builds for every target including `wasm32`.
//!
//! Pipeline:
//!
//! ```text
//! parse_raw(bytes)            -> Vec<RawPlot>       one per analysis block, column-major f64
//! group_datasets(plots)       -> Vec<RawPlot>       consecutive `.step` blocks become steps
//! Waves::apply(Request)       -> traces, panes, x view, cursors   (the viewer's only writer;
//! Waves::reload(datasets)        Waves::reload swaps in new results keeping the traces)
//! decimate(x, y, scales)      -> polyline pixels    per trace step, per frame
//! logic_runs / bin_counts     -> digital lanes / histogram bars (per pane mode)
//! ```
//!
//! The calculator is `schemify_expr` in [`Mode::Calc`](schemify_expr::Mode::Calc): [`calc::eval`]
//! hands it borrowed columns, so `v(out)` reads the file's storage directly.
//!
//! ```
//! use schemify_wave::{parse_raw, Request, Waves, Applied};
//!
//! let raw = b"Title: t\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 2\n\
//! No. Points: 3\nVariables:\n 0 time time\n 1 v(out) voltage\nValues:\n\
//! 0 0.0\n 1.0\n1 1.0\n 3.0\n2 2.0\n 5.0\n";
//! let mut w = Waves::new(parse_raw(raw)?);
//! let r = w.apply(&Request::AddTrace { src: "2 * v(out)".into(), dataset: 0, pane: 0 })?;
//! assert_eq!(r, Applied::Trace(0));
//! assert_eq!(w.value_at(0, 1.5), Some(8.0));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Contents: the re-exports below; `pub mod calc`.

#![forbid(unsafe_code)]

mod apply;
mod axis;
pub mod calc;
mod decimate;
mod digital;
mod pane;
mod plot;
mod raw;
mod request;
mod si;
mod trace;
mod waves;

pub use axis::{Axis, Scale};
pub use decimate::decimate;
pub use digital::{Run, bin_counts, logic, logic_runs};
pub use pane::{
    Cursor, LineStyle, MAX_BINS, MAX_BUS_BITS, MAX_DATASETS, MAX_PANES, MAX_TRACES, PALETTE,
    PaneMode, PaneProps, Radix, TRACE_COLORS,
};
pub use plot::{RawPlot, VarKind, Variable, group_datasets};
pub use raw::{RawError, parse_raw};
pub use request::{Applied, Request, ViewError};
// The number text of typed axis ranges and cursor positions (spec waves §8): canonical out, SI
// suffixes in, so an unchanged field reads back the same value.
pub use schemify_expr::{parse_num, write_num};
pub use si::format_si;
pub use waves::Waves;
