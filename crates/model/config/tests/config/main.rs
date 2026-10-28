//! Oracles for the user config (docs/dev/testing.md, config spec §2–§5, §7): Lua source → the
//! table written directly as the expected value, the sandbox's negative space (absent
//! libraries, budget, memory cap, capacities, errors located at `file:line`), and the
//! command lines.
//!
//! The same interpreter runs natively and on the web (web spec §5), so CI also runs this
//! target (`--test config`) for `wasm32-wasip1` under wasmtime. The browser target has no
//! test harness.
//!
//! Contents: `pub(crate) const SHA`, `pub(crate) fn run(src: &str)`,
//! `pub(crate) fn errors(out: &[Request]) -> Vec<&str>`; the tests live in `api`, `sandbox`
//! and `cli`.

#![cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]

mod api;
mod cli;
mod sandbox;

use schemify_config::{Config, Level, Request};

/// A well-formed `sha256` for `url` package specs.
pub(crate) const SHA: &str = "9f2c000000000000000000000000000000000000000000000000000000000001";

/// Runs `src` as an init file with no config root.
pub(crate) fn run(src: &str) -> (Config, Vec<Request>) {
    Config::from_source(src, None)
}

/// The texts of the error notifications in `out`.
pub(crate) fn errors(out: &[Request]) -> Vec<&str> {
    out.iter()
        .filter_map(|r| match r {
            Request::Notify {
                level: Level::Error,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
