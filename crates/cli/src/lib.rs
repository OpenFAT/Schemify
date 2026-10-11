//! The headless core (D39 as amended 2026-10-11): what the `schemify` binary runs and the
//! editor (`schemify-app`, binary `schemify-gui`) reuses. Links no GPU, window, text-shaping,
//! accessibility or LaTeX crate (`tests/deps.rs`).
//!
//! - [`platform`] — the OS boundary, split by `cfg` (native `std::fs`; on the web OPFS behind
//!   a synchronous in-memory table, the Clipboard API, downloads; D31, `docs/spec/web.md`);
//! - [`libs`] — the `load` spine: manifest + lockfile → library directories → index;
//! - [`spines`] — the netlist/run spine steps shared by the CLI and the editor;
//! - [`pack`] — the user package manager (`schemify pack`, config spec §5);
//! - [`rpc`] — the JSON-RPC command stream (D27, `docs/spec/rpc.md`): codec, framing,
//!   sockets and the headless server of `schemify rpc`;
//! - [`mcp`] — `schemify mcp`, D27's MCP bridge: MCP tools over the same headless methods.
//! - [`zip`] — project archives in and out (web spec §3).

#![forbid(unsafe_code)]

pub mod libs;
pub mod mcp;
pub mod pack;
pub mod platform;
pub mod rpc;
pub mod spines;
pub mod zip;
