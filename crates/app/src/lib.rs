//! Schemify's editor (binary `schemify-gui`, D39): the `frame` spine over the core's spines.
//! The headless CLI, project loading and the OS boundary live in
//! `schemify-cli` (binary `schemify`) and are re-exported here as [`libs`], [`platform`] and
//! [`spines`].
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
//!   OP back-annotation, the optimizer's staged edit (D15–D18, D23).
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

pub use schemify_cli::{libs, platform, spines};

pub mod checks;
pub mod commands;
pub mod docs;
pub mod edit;
pub mod editor;
pub mod geometry;
pub mod plugins;
pub mod scene;
pub mod session;
pub mod shell;
#[cfg(not(target_arch = "wasm32"))]
pub mod sim;
pub mod waves;
