//! schemify-wave — SPICE waveform data: `.raw` parsing, columnar storage,
//! math expressions, SI formatting. Pure data crate: no GUI dependency.
//!
//! Transformation: `.raw` bytes → `Vec<RawPlot>` (column-major f64 SoA)
//! → derived columns via `expr::eval`.

pub mod data;
pub mod expr;
pub mod raw;
pub mod si;

pub use data::{RawPlot, VarKind, Variable};
pub use expr::{EvalResult, Expr, ExprError, eval, parse_expr};
pub use raw::{RawError, parse_raw};
pub use si::{format_si, parse_si};
