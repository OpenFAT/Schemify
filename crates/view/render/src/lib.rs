//! The only crate that touches the GPU (D5): builds vello scenes from `ui_layout` and the
//! schematic canvas tables, and owns the window/event loop glue (native and web).
//!
//! A frame, in spine order:
//!
//! ```text
//! widgets::draw_ui(Layer::Main)        bars, tabs, status
//! canvas::draw_canvas(canvas rect)     grid, primitives, overlay, marks
//! widgets::draw_ui(Layer::Overlay)     menu popups, dialogs
//! ```
//!
//! Every drawing function is generic over [`Painter`], so the same code fills a GPU
//! [`vello::Scene`] ([`ScenePainter`]) or rasterizes on the CPU ([`CpuPainter`]: golden
//! images, and the web's frames when the browser has no WebGPU adapter). No `dyn`: the trait
//! is a compile-time bound only.
//!
//! The canvas is one retained instance row per primitive ([`gpu::InstanceRows`], patched from
//! the rows [`schemify_canvas::CanvasDiff`] reports changed), which both painters draw
//! ([`Painter::canvas`]): on the GPU, Schemify's own instanced pipelines ([`gpu`]) draw them
//! into the surface under the vello UI frame, which leaves a hole there, so a pan or zoom
//! writes one camera uniform; on the CPU, [`canvas::draw_rows`] rasterizes them, the goldens'
//! oracle. Every frame is complete: there is no tile cache. Canvas text stays with vello until
//! the glyph atlas. A frame whose vello scene did not change is composed without vello
//! ([`Layers::reusable`]).
//!
//! ```
//! use schemify_canvas::{Camera, CanvasPrims, Marks, Palette};
//! use schemify_render::{CpuPainter, Painter, canvas::*};
//! use vello::kurbo::Rect;
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
//! let pixels = p.finish(vello::peniko::Color::BLACK);
//! assert_eq!(pixels.width(), 64);
//! ```

#![forbid(unsafe_code)]

pub mod canvas;
#[cfg(target_arch = "wasm32")]
mod cpu_view;
pub mod gpu;
mod layers;
mod painter;
pub mod waves;
pub mod widgets;
mod window;

pub use layers::{LAYER_SLOTS, Layers};
pub use painter::{Chains, CpuPainter, Painter, ScenePainter, rgba};
/// CPU raster target returned by [`CpuPainter::finish`] (PNG encode/decode included).
pub use vello_cpu::Pixmap;
pub use window::{App, RunError, Waker, run};

/// Re-exported so callers name the same scene, geometry and color types.
pub use vello;

/// Re-exported for [`App::access`]'s return type.
pub use accesskit;
