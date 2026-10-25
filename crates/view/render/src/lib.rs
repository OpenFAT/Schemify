//! The only crate that touches the GPU (D5): draws `ui_layout` and the schematic canvas
//! tables, and owns the window/event loop glue (native and web).
//!
//! A frame, in spine order:
//!
//! ```text
//! widgets::draw_ui(Layer::Main)        bars, tabs, status
//! Painter::canvas(canvas rect)         grid, the tables' rows each with its labels, marks
//! Painter::overlay                     the gesture overlay
//! widgets::draw_ui(Layer::Overlay)     menu popups, dialogs
//! ```
//!
//! Every drawing function is generic over [`Painter`], so the same code fills the GPU
//! painter's frame batch ([`GpuPainter`]) or rasterizes on the CPU ([`CpuPainter`]: golden
//! images, and the web's frames when the browser has no WebGPU adapter). No `dyn`: the trait
//! is a compile-time bound only.
//!
//! On the GPU a frame is one render pass straight into the surface (D39): the batch's rects,
//! stroke segments, images and atlas masks ([`batch`]: glyphs, icons and math delimiters
//! rasterized once by `vello_cpu` into the coverage [`atlas`]) in painting order, and among
//! them the canvas's stages from its retained instance rows ([`gpu::InstanceRows`], patched
//! from the rows [`schemify_canvas::CanvasDiff`] reports changed), drawn by Schemify's own
//! instanced pipelines ([`gpu`]), so a pan or zoom writes one camera uniform. On the CPU,
//! [`canvas::draw_rows`] rasterizes the same rows, the goldens' oracle. Every frame is
//! complete: there is no tile cache and no vector renderer on the GPU.
//!
//! ```
//! use schemify_canvas::{Camera, CanvasPrims, Marks, Palette};
//! use schemify_render::{CpuPainter, Painter, canvas::*};
//! use schemify_render::kurbo::Rect;
//!
//! let mut prims = CanvasPrims::default();
//! prims.line(0, 0, 100_000, 0, 1);
//! let view = Rect::new(0.0, 0.0, 64.0, 64.0);
//! let mut cam = Camera { cx: 0.0, cy: 0.0, scale: 1.0 };
//! cam.fit(view, [0, -10_000, 100_000, 10_000]);
//! let mut p = CpuPainter::new(64, 64, 1.0);
//! let mut text = schemify_ui::TextSystem::new();
//! draw_canvas(&mut p, view, &cam, 20_000, &prims, &Palette::xschem_dark(),
//!     &CanvasPrims::default(), &Marks::default(), &mut text, &mut CanvasScratch::default());
//! let pixels = p.finish(schemify_render::peniko::Color::BLACK);
//! assert_eq!(pixels.width(), 64);
//! ```

#![forbid(unsafe_code)]

pub mod atlas;
pub mod batch;
pub mod canvas;
#[cfg(target_arch = "wasm32")]
mod cpu_view;
pub mod gpu;
mod painter;
pub mod waves;
pub mod widgets;
mod window;

pub use batch::{Batch, GpuPainter};
pub use painter::{Chains, CpuPainter, Painter, rgba};
/// CPU raster target returned by [`CpuPainter::finish`] (PNG encode/decode included).
pub use vello_cpu::Pixmap;
pub use window::{App, RunError, Waker, run};

/// Re-exported so callers name the same geometry, colour, glyph and GPU types.
pub use vello_cpu::Glyph;
pub use {kurbo, peniko, wgpu};

/// Re-exported for [`App::access`]'s return type.
pub use accesskit;
