//! The only crate that touches the GPU and the window (D5 as amended 2026-10-30): egui-winit
//! feeds the window's events to egui, egui-wgpu draws each frame's egui output, and the
//! schematic canvas is drawn among it by Schemify's own instanced pipelines ([`gpu`]) as egui
//! paint callbacks, so a pan or zoom over 10^5 objects writes one camera uniform and egui
//! tessellates nothing for it (D39).
//!
//! A frame:
//!
//! ```text
//! egui_winit::State::take_egui_input(window)       -> RawInput
//! App::frame(input, canvas rows)                    -> FullOutput  (the app packs the rows)
//! egui_winit::State::handle_platform_output         cursor, clipboard, IME, accessibility
//! ctx.tessellate(shapes); CanvasGpu::prepare        meshes; rows and camera uploaded
//! egui_wgpu::Renderer::render(pass)                 one pass: meshes and canvas stages in order
//! ```
//!
//! [`Raster`] draws the same output offscreen (goldens, PNG export).
//!
//! Contents:
//! - module `gpu`
//! - re-exports: `App`, `RunError`, `Waker`, `run`, `block_on` (`window`); `Raster`, `Image`
//!   (`raster`); `egui`, `egui_wgpu`, `kurbo`, `wgpu`

#![forbid(unsafe_code)]

mod device;
pub mod gpu;
mod raster;
mod window;

pub use raster::{Image, Raster};
#[cfg(not(target_arch = "wasm32"))]
pub use window::block_on;
pub use window::{App, RunError, Waker, run};

/// Re-exported so callers name the same egui, geometry and GPU types.
pub use {egui, egui_wgpu, kurbo, wgpu};
