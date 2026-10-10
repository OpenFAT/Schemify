//! Waveform data (D23, D26): the `.raw` reader, the column store (`raw_plots`), the calculator
//! binding to `schemify_expr`, display decimation, and the viewer tables. Pure data: no GUI
//! dependency, builds for every target including `wasm32`.
//!
//! Pipeline:
//!
//! ```text
//! parse_raw(bytes)            -> Vec<RawPlot>       one per analysis block, column-major f64
//! group_datasets(plots)       -> Vec<RawPlot>       consecutive `.step` blocks become steps
//! Waves::apply(Request)       -> traces, panes, x view, cursors   (the viewer's only writer)
//! decimate(x, y, scales)      -> polyline pixels    per trace step, per frame
//! ```
//!
//! The calculator is `schemify_expr` in [`Mode::Calc`](schemify_expr::Mode::Calc): [`calc::bind`]
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

#![forbid(unsafe_code)]

pub mod calc;
mod data;
mod decimate;
mod raw;
mod si;
mod view;

pub use data::{RawPlot, VarKind, Variable, group_datasets};
pub use decimate::{Scale, decimate, minmax, minmax_scalar};
pub use raw::{RawError, parse_raw};
pub use si::format_si;
pub use view::{
    Applied, Axis, Cursor, LineStyle, MAX_DATASETS, MAX_PANES, MAX_TRACES, Request, TRACE_COLORS,
    ViewError, Waves,
};
