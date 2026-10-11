//! Schemify's application crate: the spines (ARCHITECTURE.md § Spines) and the only crate
//! that talks to the OS. The `schemify` binary (`src/main.rs`) runs the headless CLI spines;
//! with the `gui` feature (default) this library also provides the interactive editor:
//!
//! - [`editor`] — the `frame` spine: input → commands → plugins → `doc` edits → UI
//!   composition → scene, plus the xschem editing gestures (tool state machine);
//! - [`commands`] — the command table (core enum + plugin rows, D27);
//! - [`docs`] — the docs page beside the canvas (D19 vars, D21 math, D20 cache write-back);
//! - [`scene`] — the document as canvas primitives, with hit-testing and box selection;
//! - [`edit`] — gestures as `doc` edit transactions (one per gesture, so one undo);
//! - [`geometry`] — grid symmetries applied to records;
//! - [`session`] — project load, open documents, tabs and hierarchy, saves and journals;
//! - [`shell`] — UI composition (core base ⊕ plugins ⊕ app state) and the canvas palette;
//! - [`plugins`] — plugin host glue (bundled default UI, D6);
//! - [`waves`] — the waveform dock (D23) and its cross-probe entry;
//! - [`checks`] — the live violation overlay, the checks panel and net highlight (D13);
//! - [`sim`] — the run spine in the editor: non-blocking runs, results table, plot slots,
//!   OP back-annotation, the optimizer's staged edit (D15–D18, D23);
//! - [`spines`] — the netlist/run spine steps shared with the headless CLI;
//! - [`platform`] — file system split by `cfg` (native `std::fs`; web OPFS is a TODO, D31).
//!
//! ```no_run
//! use schemify_app::editor::{Editor, Options};
//! use schemify_ui::InputEvents;
//!
//! let mut ed = Editor::new(Options { path: Some("design/top.shcomp".into()), ..Options::default() })?;
//! ed.update(&InputEvents::default(), (1280.0, 800.0), 0);
//! # Ok::<(), String>(())
//! ```

#![forbid(unsafe_code)]

#[cfg(feature = "gui")]
pub mod checks;
#[cfg(feature = "gui")]
pub mod commands;
#[cfg(feature = "gui")]
pub mod docs;
#[cfg(feature = "gui")]
pub mod edit;
#[cfg(feature = "gui")]
pub mod editor;
#[cfg(feature = "gui")]
pub mod geometry;
pub mod libs;
pub mod platform;
#[cfg(feature = "gui")]
pub mod plugins;
#[cfg(feature = "gui")]
pub mod scene;
#[cfg(feature = "gui")]
pub mod session;
#[cfg(feature = "gui")]
pub mod shell;
#[cfg(all(feature = "gui", not(target_arch = "wasm32")))]
pub mod sim;
pub mod spines;
#[cfg(feature = "gui")]
pub mod waves;
