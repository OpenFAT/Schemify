//! The frame batch (`docs/dev/research/render-backend.md` § 5, step 3): one frame's painter
//! calls in, instance bytes and draws out. Every [`crate::Painter`] call of a frame becomes
//! instances of one batch ([`Batch`]) that `shaders/batch.wgsl` draws in a single pass with
//! the canvas pipelines ([`crate::gpu::Pipelines::draw`]): rects, atlas masks, stroke
//! segments, fill triangles and images, each with its clip. Glyphs and small paths are coverage
//! masks rasterized once by `vello_cpu` (the golden painter's rasterizer, so a mask is the CPU
//! painter's own pixels) into the [`Atlas`]; long strokes are segments drawn analytically; the
//! canvas is the retained instance rows, drawn at its stage in the batch.
//!
//! Six questions:
//! 1. In: one frame's painter calls. Out: instance bytes ([`STRIDE`] each), the draws (a
//!    range of instances with its texture, or a canvas stage), the atlas's pending masks, the
//!    frame's images.
//! 2. 10^3–10^4 instances a frame (the UI's rects and glyphs, a wave pane's segments);
//!    [`INSTANCES_MAX`] at most.
//! 3. Device px as `f32` (a frame is under 2^24 px across); colours RGBA8; a kind and a draw
//!    index are small integers.
//! 4. Appended in call order (the order is the picture), read once by the upload.
//! 5. Instances, draws and images live one frame; the atlas, the rasterizer and the UI's rows
//!    ([`Batch::ui`]: the layout rows' instances, kept while their paint inputs hold) persist.
//! 6. Serial (call order is drawing order).
//!
//! Contents:
//! - `pub const STRIDE`, `RECT`, `MASK`, `SEG`, `TRI`, `IMAGE`, `MASK_SCALED`, `INSTANCES_MAX`
//! - `pub enum What`, `pub enum Stage`, `pub struct Draw`
//! - `pub struct Batch`, `Batch::begin_frame`, `Batch::settled`, `Batch::instances`,
//!   `Batch::len`, `Batch::is_empty`, `Batch::draws`, `Batch::images`
//! - `pub use painter::GpuPainter`
//! - `pub(crate) fn mask_row(inst: &[u8]) -> Option<u16>`

mod glyph;
mod painter;
mod path;

use std::collections::HashMap;
use std::ops::Range;

use peniko::{Color, ImageData};

use crate::atlas::{Atlas, Key};
use crate::ui_rows::UiRows;

pub use painter::GpuPainter;

/// Bytes per instance: box (4 × f32), aux (4 × f32), clip (4 × f32), colour, kind.
pub const STRIDE: usize = 56;
/// Instance kinds (`shaders/batch.wgsl`): a filled rect.
pub const RECT: u32 = 0;
/// An atlas mask drawn one texel per pixel.
pub const MASK: u32 = 1;
/// A round-capped stroke segment.
pub const SEG: u32 = 2;
/// A plain fill triangle.
pub const TRI: u32 = 3;
/// An image (premultiplied RGBA), sampled.
pub const IMAGE: u32 = 4;
/// An atlas mask drawn scaled (a glyph too large to rasterize at its size), sampled.
pub const MASK_SCALED: u32 = 5;
/// Instances one frame holds; past it calls draw nothing (counted in [`Batch::dropped`]).
pub const INSTANCES_MAX: usize = 1 << 18;

/// What a draw of the batch draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    /// Its instances, sampling the atlas.
    Ui,
    /// Its instances, sampling image `k` of [`Batch::images`].
    Image(u32),
    /// A canvas stage, from the canvas pipelines and the instance rows.
    Canvas(Stage),
    /// Slots of UI source `k`'s retained rows ([`UiRows`]), sampling the atlas.
    Retained(usize),
}

/// A stage of the canvas pass, in drawing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Background and grid.
    Background,
    /// The rows of one source (a canvas table, the marks, the gesture overlay).
    Rows(usize),
}

/// One draw: a range of instances and what draws them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draw {
    /// Instance indices (empty for a canvas stage).
    pub range: Range<u32>,
    /// The texture or canvas stage.
    pub what: What,
}

/// One frame's instances and draws, the coverage atlas behind them, and the rasterizer that
/// fills it. Owner: the window (or a benchmark); the [`GpuPainter`] fills it, the GPU side
/// ([`crate::gpu::Pipelines`]) uploads and draws it.
#[derive(Default)]
pub struct Batch {
    bytes: Vec<u8>,
    draws: Vec<Draw>,
    clips: Vec<[f32; 4]>,
    images: Vec<ImageData>,
    /// The masks every frame's glyphs and paths are drawn from.
    pub atlas: Atlas,
    raster: Raster,
    /// Instances past [`INSTANCES_MAX`], not drawn.
    pub dropped: u64,
    /// Masks rasterized so far (the atlas's misses).
    pub rastered: u64,
    /// Per glyph and direction (`[font blob, face and glyph id, direction hash]`), the key,
    /// em (device px) and subpixel phase of its last exact mask: the stand-in for its other
    /// sizes.
    latest: HashMap<Key, (Key, f32, [u8; 2])>,
    /// Exact glyph masks this frame may still rasterize where a stand-in exists.
    exact_left: u32,
    /// A stand-in was drawn this frame: the next frame rasterizes more exact masks.
    unsettled: bool,
    /// The UI's rows, kept across frames: the layout rows the GPU painter drew, each repainted
    /// only when its paint inputs change.
    pub ui: UiRows,
    /// While a UI row is painted for [`UiRows`]: its pane slot, and the instances it pushed
    /// (not this frame's) and whether one was an image (a frame's texture: drawn from the
    /// frame's instances instead).
    capture: Option<u32>,
    captured: Vec<u8>,
    captured_image: bool,
}

/// Largest side of one mask, device px: a larger fill is split into tiles of this side.
const MASK_MAX: i32 = 512;
/// No clip: far outside any target.
const UNCLIPPED: [f32; 4] = [-1.0e9, -1.0e9, 1.0e9, 1.0e9];
/// Glyph masks a frame rasterizes at most while a stand-in can take a glyph's place: a
/// cached mask of the same glyph in the same direction at another size, drawn scaled
/// ([`Batch::settled`] is false until the exact ones are in). A zoom with 200 labels in view
/// rasterized 100–180 masks a frame, 1.0–1.6 ms (`canvas_label_zoom_frames`).
const EXACT_PER_FRAME: u32 = 32;
/// Glyph and direction entries the stand-in index keeps before it starts over.
const LATEST_MAX: usize = 1 << 16;
/// Byte offsets in an instance of its aux box (a mask's atlas texels) and its kind word.
const AUX_AT: usize = 16;
const KIND_AT: usize = 52;

const _: () = assert!(STRIDE == 4 * (4 + 4 + 4 + 1 + 1), "three vec4s and two words");
const _: () = assert!(KIND_AT + 4 == STRIDE, "the kind is the last word");

/// `vello_cpu` state reused for every mask: the context, its resources, the target.
struct Raster {
    ctx: vello_cpu::RenderContext,
    res: vello_cpu::Resources,
    pix: vello_cpu::Pixmap,
    coverage: Vec<u8>,
}

/// The atlas texel row a mask instance (one [`STRIDE`]-byte instance) samples, `None` for any
/// other kind: what the retained UI rows pin ([`Atlas::pin`]).
///
/// # Panics
///
/// Panics unless `inst` is one instance.
pub(crate) fn mask_row(inst: &[u8]) -> Option<u16> {
    assert_eq!(inst.len(), STRIDE, "one instance");
    let kind = word(inst, KIND_AT) & 0xff;
    let y = f32::from_bits(word(inst, AUX_AT + 4));
    (kind == MASK || kind == MASK_SCALED).then_some(y as u16)
}

impl std::fmt::Debug for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Batch")
            .field("instances", &self.len())
            .field("draws", &self.draws.len())
            .field("atlas", &self.atlas.len())
            .finish()
    }
}

impl Batch {
    /// Starts a frame: no instance, draw, clip or image; the atlas's masks drawn from now on
    /// are this frame's.
    ///
    /// # Panics
    ///
    /// Panics while a UI row is being painted.
    pub fn begin_frame(&mut self) {
        assert!(self.capture.is_none(), "no row is being painted");
        self.bytes.clear();
        self.draws.clear();
        self.clips.clear();
        self.images.clear();
        self.atlas.begin_frame();
        self.exact_left = EXACT_PER_FRAME;
        self.unsettled = false;
        self.ui.begin_frame();
        // ponytail: the stand-in index starts over past the atlas's capacity in glyphs.
        if self.latest.len() > LATEST_MAX {
            self.latest.clear();
        }
        debug_assert!(self.is_empty(), "a fresh frame draws nothing");
    }

    /// False when the frame drew a glyph from a stand-in (another size, scaled): its owner
    /// asks for another frame, with no input, to rasterize the exact masks (a few dozen a
    /// frame), so a zoom costs little per frame and the picture settles exact.
    pub fn settled(&self) -> bool {
        !self.unsettled
    }

    /// Instances this frame, [`STRIDE`] bytes each.
    pub fn instances(&self) -> &[u8] {
        &self.bytes
    }

    /// Instances this frame.
    pub fn len(&self) -> usize {
        self.bytes.len() / STRIDE
    }

    /// True when the frame drew nothing.
    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }

    /// The draws, in order.
    pub fn draws(&self) -> &[Draw] {
        &self.draws
    }

    /// The images [`What::Image`] names.
    pub fn images(&self) -> &[ImageData] {
        &self.images
    }
}

impl Default for Raster {
    fn default() -> Self {
        Raster {
            ctx: vello_cpu::RenderContext::new(1, 1),
            res: vello_cpu::Resources::new(),
            pix: vello_cpu::Pixmap::new(1, 1),
            coverage: Vec::new(),
        }
    }
}

// ---- private ----

/// The native-endian `u32` at byte `at` of `b`.
fn word(b: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

/// RGBA8 bytes in memory order (R first), straight alpha, as `color_of` in
/// `shaders/instance.wgsl` reads it.
fn color_word(c: Color) -> u32 {
    let c = c.to_rgba8();
    u32::from_ne_bytes([c.r, c.g, c.b, c.a])
}

impl Batch {
    /// The current clip, device px.
    fn clip(&self) -> [f32; 4] {
        self.clips.last().copied().unwrap_or(UNCLIPPED)
    }

    /// Appends one instance drawn by `what`, under the current clip: to the row being captured
    /// for [`UiRows`] (its pane slot in the kind word), else to this frame's instances.
    fn push(&mut self, kind: u32, pos: [f32; 4], aux: [f32; 4], color: u32, what: What) {
        debug_assert!(kind <= 0xff, "a kind is the word's low byte");
        let clip = self.clip();
        let (out, kind) = match self.capture {
            Some(_) if matches!(what, What::Image(_)) => {
                self.captured_image = true;
                return;
            }
            Some(pane) => (&mut self.captured, kind | pane << 8),
            None if self.len() >= INSTANCES_MAX => {
                self.dropped += 1;
                return;
            }
            None => (&mut self.bytes, kind),
        };
        for v in pos.iter().chain(&aux).chain(&clip) {
            out.extend_from_slice(&v.to_ne_bytes());
        }
        out.extend_from_slice(&color.to_ne_bytes());
        out.extend_from_slice(&kind.to_ne_bytes());
        debug_assert_eq!(out.len() % STRIDE, 0, "whole instances");
        if self.capture.is_none() {
            let i = self.len() as u32 - 1;
            match self.draws.last_mut() {
                Some(d) if d.what == what && d.range.end == i => d.range.end = i + 1,
                _ => self.draws.push(Draw {
                    range: i..i + 1,
                    what,
                }),
            }
        }
    }

    /// Draws slots `range` of UI source `src`'s retained rows here (extending the last draw when
    /// it ends where `range` starts).
    fn retained(&mut self, src: usize, range: Range<u32>) {
        assert!(range.start <= range.end, "a slot range");
        match self.draws.last_mut() {
            Some(d) if d.what == What::Retained(src) && d.range.end == range.start => {
                d.range.end = range.end;
            }
            _ => self.draws.push(Draw {
                range,
                what: What::Retained(src),
            }),
        }
    }

    /// Marks where canvas stage `stage` is drawn among the instances.
    fn stage(&mut self, stage: Stage) {
        let i = self.len() as u32;
        self.draws.push(Draw {
            range: i..i,
            what: What::Canvas(stage),
        });
    }

    /// The draw index of `image` this frame (added on first use).
    fn image(&mut self, image: &ImageData) -> u32 {
        let id = image.data.id();
        let k = match self.images.iter().position(|d| d.data.id() == id) {
            Some(k) => k,
            None => {
                self.images.push(image.clone());
                self.images.len() - 1
            }
        };
        k as u32
    }
}

impl Raster {
    /// Rasterizes `draw` (white, into a `w × h` target at the origin) and returns its
    /// coverage, row-major.
    fn run(
        &mut self,
        w: u16,
        h: u16,
        draw: impl FnOnce(&mut vello_cpu::RenderContext, &mut vello_cpu::Resources),
    ) -> &[u8] {
        assert!(w > 0 && h > 0, "a mask has pixels");
        self.ctx.reset_and_resize(w, h);
        self.ctx.set_paint(Color::WHITE);
        draw(&mut self.ctx, &mut self.res);
        self.ctx.flush();
        self.pix.resize(w, h);
        self.pix
            .data_mut()
            .fill(vello_cpu::color::PremulRgba8::from_u32(0));
        self.ctx.render(&mut self.pix, &mut self.res);
        self.coverage.clear();
        self.coverage.extend(self.pix.data().iter().map(|p| p.a));
        assert_eq!(self.coverage.len(), usize::from(w) * usize::from(h));
        &self.coverage
    }
}
