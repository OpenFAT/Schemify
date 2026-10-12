//! Simulation runs (D15–D18): job expansion (configs × corners × sweep points × Monte
//! Carlo samples), the native ngspice runner, results (outputs and specs over each
//! job's raw file), the Monte Carlo yield summary and the ask-tell optimizer. Owns the
//! `jobs` and `results` tables. Normative spec: `docs/spec/run.md`.
//!
//! Everything but [`Runner`] is pure and builds for `wasm32-unknown-unknown`; on the
//! web a runner plugin (D16) receives [`Prepared::netlist`] and the app hands the raw
//! bytes it returns to [`Results::record`].
//!
//! # Spine
//!
//! The app's `run` spine, one line per transform; branches (gate, failures) stay in the
//! caller:
//!
//! ```text
//! Plan::read(bench_doc)                              → plan     bench tables, compiled outputs/specs
//! expand(&plan, &select)                             → jobs     pure, deterministic order
//! design(project, top, &jobs, env, funcs)            → digest   gate (D13) + design hash
//! Results::new(&plan, digest)                        → results
//! per job: prepare(project, top, &mut jobs, j, env, dialect, funcs, &mut results)
//!                                                    → netlist  elaborate(overrides) → emit
//!          Runner::submit(&mut jobs, &mut results, j, &prepared)
//! per frame: Runner::poll(&plan, &mut jobs, &mut results, &mut finished)
//!                                                    → results  raw → outputs → specs
//! Runner::save(&jobs, &results) / write_results      → `.schemify/runs/<design>/results`
//! ```
//!
//! The optimizer wraps the per-job lines: [`OptRun::step`] tells the finished
//! candidate's goal outputs and pushes the next candidate as a job. A plugin `run`
//! request (plugin ABI §7: `run <bench> <config> <corner>` + `vary` lines) is one
//! [`Jobs::push`] followed by the same per-job lines.
//!
//! # Example
//!
//! ```
//! use schemify_doc::Project;
//! use schemify_run::{Plan, Select, expand};
//!
//! let bench = "schemify 1 shbench\n\nvar R 1k\n\ncorner ff\n  set C 0.5u\n\ncorner tt\n  \
//!              set C 1u\n\nsweep rsw\n  var R\n  values 1k 2k 3k\n";
//! let mut p = Project::new(None, None, 1).unwrap();
//! p.index_library("", "", &[("tb.shbench", bench.as_bytes())]).unwrap();
//! let top = p.open("", "tb.shbench", bench.as_bytes(), None).unwrap().handle;
//! let plan = Plan::read(p.doc(top).unwrap()).unwrap();
//! let select = Select { corners: &["ff", "tt"], sweeps: &["rsw"], ..Select::default() };
//! # #[cfg(all(feature = "sweep", feature = "corners"))] {
//! let jobs = expand(&plan, &select).unwrap();
//! assert_eq!(jobs.len(), 6);
//! assert_eq!((jobs.corner(1), jobs.assigned(1, "R")), (Some("ff"), Some("2k")));
//! # }
//! ```

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]

mod jobs;
#[cfg(feature = "mc")]
mod mc;
#[cfg(feature = "optimize")]
mod optimize;
mod plan;
mod prepare;
mod results;
#[cfg(not(target_arch = "wasm32"))]
mod runner;
#[cfg(feature = "sweep")]
mod sweep;

pub use jobs::{DEFAULT, JOBS_MAX, Jobs, NO_SAMPLE, Select, State, expand};
#[cfg(feature = "mc")]
pub use mc::{Yield, draw, sim_seed, wilson, yield_summary};
#[cfg(feature = "optimize")]
pub use optimize::{Algorithm, Evaluation, Objective, OptError, OptRun, Optimizer, Param};
pub use plan::{
    BUDGET_MAX, Corner, Dist, DistKind, Goal, MonteCarlo, Optimize, Output, Plan, PlanError,
    SAMPLES_MAX, SWEEP_POINTS_MAX, Spec, Sweep, TABLE_MAX, Var,
};
pub use prepare::{Blocked, Origin, Prepared, Source, Tool, design, prepare, sources};
pub use results::{JOB_LEVEL, Pass, Results, read_results, write_results};
#[cfg(not(target_arch = "wasm32"))]
pub use runner::{
    NETLIST, RAW, RESULTS, Runner, RunnerConfig, STDERR, STDOUT, SimCommand, Tools, WORKERS_MAX,
};
