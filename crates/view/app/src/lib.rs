//! Schemify's editor binary (`schemify-gui`, D39): `schemify-editor`'s frame spine in a
//! window, painted by `schemify-render`. The model, the commands and every table live in
//! `schemify-editor`, which links no GPU or window crate; this crate adds only the humble
//! view and the OS shells around it (architecture.md, crate `app`):
//!
//! - [`paint`]: `Editor::picture` → pixels (UI, canvas, graphs, waves dock), plus the PNG
//!   export raster; the canvas tables go through `canvas` (the diff, the label indexes);
//! - [`window`]: the window frame loop (`schemify_render::run`);
//! - [`serve`]: `--rpc <socket>`, the JSON-RPC command stream into the running window;
//! - [`headless`]: `cmd`, one command with no window.
//!
//! Contents:
//! - `pub mod headless`, `pub mod paint`, `pub mod serve`, `pub mod window`

#![forbid(unsafe_code)]

mod canvas;
#[cfg(not(target_arch = "wasm32"))]
pub mod headless;
pub mod paint;
#[cfg(not(target_arch = "wasm32"))]
pub mod serve;
pub mod window;
