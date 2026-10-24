//! Schemify's editor binary (`schemify-gui`, D39): `schemify-editor`'s frame spine in a
//! window, painted by `schemify-render`. The model, the commands and every table live in
//! `schemify-editor`, which links no GPU or window crate; this crate adds only the humble
//! view and the OS shells around it:
//!
//! - [`paint`] — `Editor::picture` → pixels (canvas raster layer, UI, graphs, waves dock),
//!   plus the PNG export raster;
//! - [`window`] — the window frame loop (`render::run`);
//! - [`serve`] — `--rpc <socket>`: the JSON-RPC command stream into the running window;
//! - [`headless`] — `cmd`: one command, no window.

#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
pub mod headless;
pub mod paint;
#[cfg(not(target_arch = "wasm32"))]
pub mod serve;
pub mod window;
