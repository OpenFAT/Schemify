//! The headless core (D39 as amended 2026-10-11): what the `schemify` binary runs and the
//! editor (`schemify-app`, binary `schemify-gui`) reuses. Links no GPU, window, text-shaping,
//! accessibility or LaTeX crate (`tests/deps.rs`).
//!
//! - [`platform`] — the OS boundary, split by `cfg` (native `std::fs`; web OPFS is a TODO,
//!   D31);
//! - [`libs`] — the `load` spine: manifest + lockfile → library directories → index;
//! - [`spines`] — the netlist/run spine steps shared by the CLI and the editor;
//! - [`pack`] — the user package manager (`schemify pack`, config spec §5).

#![forbid(unsafe_code)]

pub mod libs;
pub mod pack;
pub mod platform;
pub mod spines;
