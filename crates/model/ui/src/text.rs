//! Text shaping and measurement (parley): strings in, cached shaped layouts out, plus the
//! caches of math, prose and icon layouts and the images prose draws.
//!
//! The embedded UI font (a Noto Sans subset, `fonts/README.md`) is registered and named by
//! every style, so every string the UI ships shapes the same on every platform (D32) and the
//! golden images stay deterministic. System fonts (fontique) are consulted only for
//! characters outside the subset; the web build has none.
//!
//! Contents:
//! - `pub type TextId = u32`, `pub const NO_TEXT: TextId`
//! - `pub struct Image`: `new(path, width, height, rgba) -> Image`
//! - `pub struct TextSystem`: `CACHE_CAPACITY`, `IMAGES_MAX`, `new`, `begin_frame`, `epoch`,
//!   `shape`, `shape_into`, `prose`, `prose_list`, `set_image`, `image_id`, `image`, `layout`,
//!   `math`, `math_list`, `icon`, `icon_paths`, `math_fonts`; crate-only `family`, `mono`,
//!   `push_layout`, `math_uncached`

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use parley::fontique::Blob;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, Layout, LayoutContext, StyleProperty,
};

use crate::fonts::NOTO_SANS;
use crate::icon;
use crate::icon::Icon;
use crate::math;
use crate::math::{MathFonts, MathList};
use crate::prose;
use crate::prose::{CodeMetrics, ProseList};
use crate::strings::{Strings, Sym};

/// Index of a shaped layout in [`TextSystem`]; valid until the next [`TextSystem::begin_frame`]
/// that evicts.
pub type TextId = u32;

/// "No text" sentinel for [`TextId`] columns.
pub const NO_TEXT: TextId = u32::MAX;

/// A decoded image the app registered for prose (`![alt](path)`, [`TextSystem::set_image`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The path prose names it by (project-relative, as written in the source).
    pub path: String,
    /// Width in pixels (> 0).
    pub width: u32,
    /// Height in pixels (> 0).
    pub height: u32,
    /// `width × height × 4` bytes of premultiplied RGBA8, rows top to bottom. An `Arc`
    /// because the renderer's image type (`peniko::Blob`) takes one: it reaches the GPU
    /// without a copy.
    pub rgba: Arc<Vec<u8>>,
}

/// Font database, shaping scratch, the cache of shaped UI strings and math, and the images
/// prose draws.
///
/// Capacity: [`TextSystem::CACHE_CAPACITY`] cached layouts; beyond that the whole cache is
/// dropped at the next [`TextSystem::begin_frame`] (a phase boundary), so a session that keeps
/// producing new strings stays bounded. Images: [`TextSystem::IMAGES_MAX`], never evicted.
pub struct TextSystem {
    pub(crate) font_cx: FontContext,
    pub(crate) layout_cx: LayoutContext<()>,
    family: String,
    mono: String,
    layouts: Vec<Layout<()>>,
    cache: HashMap<(Sym, u16), TextId>,
    math_fonts: MathFonts,
    maths: Vec<MathList>,
    math_cache: HashMap<(Sym, u16), u32>,
    proses: Vec<ProseList>,
    prose_cache: HashMap<(Sym, u16, u16), u32>,
    images: Vec<Image>,
    icons: Vec<Icon>,
    /// Icon source → id, or `u32::MAX` for data that does not parse (parsed once either way).
    icon_cache: HashMap<Sym, u32>,
    /// Code-block metrics per prose font in [`prose::CODE_PX`] (measured once, kept
    /// through evictions).
    pub(crate) code_metrics: Vec<Option<CodeMetrics>>,
    /// Cache drops so far ([`TextSystem::epoch`]).
    epoch: u32,
}

impl Image {
    /// An image named `path` from premultiplied RGBA8 pixels.
    ///
    /// # Panics
    ///
    /// Panics if a side is 0 or `rgba` is not `width × height × 4` bytes.
    #[must_use]
    pub fn new(path: &str, width: u32, height: u32, rgba: Vec<u8>) -> Image {
        assert!(width > 0, "an image has columns");
        assert!(height > 0, "an image has rows");
        assert_eq!(
            rgba.len() as u64,
            u64::from(width) * u64::from(height) * 4,
            "four bytes per pixel"
        );
        Image {
            path: path.to_owned(),
            width,
            height,
            rgba: Arc::new(rgba),
        }
    }
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TextSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextSystem")
            .field("layouts", &self.layouts.len())
            .finish()
    }
}

impl TextSystem {
    /// Cached layouts kept across frames before the cache is reset.
    pub const CACHE_CAPACITY: usize = 1 << 14;
    /// Images registered at once ([`TextSystem::set_image`]).
    pub const IMAGES_MAX: usize = 64;

    /// Creates the system and registers the embedded fonts. UI text asks for the Noto Sans
    /// subset, then KaTeX Main (already embedded for math) for the arrows and relations Noto
    /// Sans lacks (→ ≤ ≥), and only then the system fallback.
    ///
    /// # Panics
    ///
    /// If an embedded font registers no family (a build defect, never input).
    #[must_use]
    pub fn new() -> Self {
        let mut font_cx = FontContext::default();
        let mut register = |bytes: &'static [u8]| {
            // Arc: `Blob::new` takes one (fontique's API); it wraps a `&'static` slice.
            let families = font_cx
                .collection
                .register_fonts(Blob::new(Arc::new(bytes)), None);
            families
                .first()
                .and_then(|(id, _)| font_cx.collection.family_name(*id))
                .expect("an embedded font registers a family")
                .to_owned()
        };
        let noto = register(NOTO_SANS);
        let symbols = register(math::face_bytes("Main-Regular"));
        // Monospace for code (prose code spans and blocks): KaTeX's, already embedded for math.
        let mono = register(math::face_bytes("Typewriter-Regular"));
        // A CSS family list: every builder parses it (`FontFamily::from(&str)`).
        let family = format!("\"{noto}\", \"{symbols}\"");
        assert_ne!(noto, mono, "code text is monospace, never the UI face");
        assert_ne!(
            symbols, mono,
            "code text is monospace, never the symbol face"
        );
        let mut ts = TextSystem {
            font_cx,
            layout_cx: LayoutContext::new(),
            family,
            mono,
            layouts: Vec::new(),
            cache: HashMap::new(),
            math_fonts: MathFonts::new(),
            maths: Vec::new(),
            math_cache: HashMap::new(),
            proses: Vec::new(),
            prose_cache: HashMap::new(),
            images: Vec::new(),
            icons: Vec::new(),
            icon_cache: HashMap::new(),
            code_metrics: Vec::new(),
            epoch: 0,
        };
        let code: Vec<Option<CodeMetrics>> = prose::CODE_PX
            .map(|px| prose::measure_code(&mut ts, px))
            .collect();
        // The probe layouts are not cache entries: drop them so ids start at 0 as before.
        ts.layouts.clear();
        ts.code_metrics = code;
        assert_eq!(ts.code_metrics.len(), prose::CODE_PX.len());
        ts
    }

    /// Marks a layout pass boundary: if the cache is over capacity it is emptied, which
    /// invalidates every [`TextId`] handed out before this call.
    pub fn begin_frame(&mut self) {
        let held = self.layouts.len() + self.maths.len() + self.proses.len() + self.icons.len();
        if held > Self::CACHE_CAPACITY {
            self.layouts.clear();
            self.cache.clear();
            self.maths.clear();
            self.math_cache.clear();
            self.proses.clear();
            self.prose_cache.clear();
            self.icons.clear();
            self.icon_cache.clear();
            self.epoch = self.epoch.wrapping_add(1);
        }
        let held = self.layouts.len() + self.maths.len() + self.proses.len() + self.icons.len();
        debug_assert!(
            held <= Self::CACHE_CAPACITY,
            "a frame starts under capacity"
        );
        debug_assert!(
            self.cache.len() <= self.layouts.len(),
            "every cached id names a layout"
        );
    }

    /// How many times the cache was dropped: a text, math, prose or icon id names the same
    /// layout for as long as this stays the same (a painter that keeps what an id drew
    /// compares it).
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Shapes `text` at `px` pixels (single line, no wrapping), cached by (string, size).
    ///
    /// # Panics
    ///
    /// If `px` is 0.
    pub fn shape(&mut self, strings: &Strings, text: Sym, px: u16) -> TextId {
        assert!(px > 0, "font size must be positive");
        if let Some(&id) = self.cache.get(&(text, px)) {
            debug_assert!((id as usize) < self.layouts.len(), "a cached id is live");
            return id;
        }
        let mut layout = Layout::new();
        self.shape_into(strings.get(text), f32::from(px), &mut layout);
        let id = self.layouts.len() as TextId;
        self.layouts.push(layout);
        self.cache.insert((text, px), id);
        debug_assert_ne!(id, NO_TEXT, "the cache never reaches the sentinel");
        id
    }

    /// Shapes uncached text into `out` (reusing its allocation): canvas labels, editors.
    ///
    /// # Panics
    ///
    /// If `px` is not finite and positive.
    pub fn shape_into(&mut self, text: &str, px: f32, out: &mut Layout<()>) {
        assert!(px.is_finite(), "font size must be finite");
        assert!(px > 0.0, "font size must be positive");
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(FontFamily::from(
            self.family.as_str(),
        )));
        builder.push_default(StyleProperty::FontSize(px));
        builder.build_into(out, text);
        out.break_all_lines(None);
        out.align(Alignment::Start, AlignmentOptions::default());
    }

    pub(crate) fn family(&self) -> &str {
        debug_assert!(!self.family.is_empty(), "the UI family list is registered");
        &self.family
    }

    pub(crate) fn mono(&self) -> &str {
        debug_assert!(!self.mono.is_empty(), "the monospace family is registered");
        &self.mono
    }

    /// Stores a layout built outside [`TextSystem::shape`] (prose runs) and returns its id.
    pub(crate) fn push_layout(&mut self, layout: Layout<()>) -> TextId {
        assert!(
            self.layouts.len() < NO_TEXT as usize,
            "layout ids stay below the sentinel"
        );
        self.layouts.push(layout);
        let id = (self.layouts.len() - 1) as TextId;
        debug_assert_ne!(id, NO_TEXT);
        id
    }

    /// Lays out math that has no interned source (prose formulas); `None` outside the subset.
    pub(crate) fn math_uncached(&mut self, src: &str, px: f32) -> Option<u32> {
        assert!(px > 0.0, "math size must be positive");
        let list = math::layout_math(src, px, &self.math_fonts).ok()?;
        self.maths.push(list);
        let id = (self.maths.len() - 1) as u32;
        debug_assert_ne!(id, u32::MAX, "math ids stay below the sentinel");
        Some(id)
    }

    /// Lays out markdown `src` (the `prose` widget, [`crate::prose`]) at `px` wrapped at
    /// `width` logical pixels, cached by (source, size, width). The id is valid until the next
    /// [`TextSystem::begin_frame`] that evicts.
    ///
    /// # Panics
    ///
    /// Panics if `px` or `width` is zero.
    pub fn prose(&mut self, strings: &Strings, src: Sym, px: u16, width: u16) -> u32 {
        assert!(px > 0, "prose needs a font size");
        assert!(width > 0, "prose needs a width");
        if let Some(&id) = self.prose_cache.get(&(src, px, width)) {
            debug_assert!((id as usize) < self.proses.len(), "a cached id is live");
            return id;
        }
        let list = prose::layout_prose(self, strings.get(src), f32::from(px), f32::from(width));
        self.proses.push(list);
        let id = (self.proses.len() - 1) as u32;
        self.prose_cache.insert((src, px, width), id);
        debug_assert!(
            self.prose_cache.len() <= self.proses.len(),
            "every cached id is live"
        );
        id
    }

    /// A prose layout from [`TextSystem::prose`].
    ///
    /// # Panics
    ///
    /// Panics on a stale id.
    pub fn prose_list(&self, id: u32) -> &ProseList {
        assert!((id as usize) < self.proses.len(), "prose id {id} is stale");
        &self.proses[id as usize]
    }

    /// Registers `image` for prose that names its path, replacing one of the same path.
    /// Prose is laid out again at its next [`TextSystem::prose`] (ids handed out before stay
    /// valid until the next evicting [`TextSystem::begin_frame`]). Returns false, registering
    /// nothing, when [`TextSystem::IMAGES_MAX`] other images are registered.
    pub fn set_image(&mut self, image: Image) -> bool {
        debug_assert_eq!(
            image.rgba.len() as u64,
            u64::from(image.width) * u64::from(image.height) * 4
        );
        match self.images.iter().position(|i| i.path == image.path) {
            Some(k) => self.images[k] = image,
            None if self.images.len() >= Self::IMAGES_MAX => return false,
            None => self.images.push(image),
        }
        self.prose_cache.clear();
        debug_assert!(
            self.images.len() <= Self::IMAGES_MAX,
            "images stay under capacity"
        );
        true
    }

    /// Id of the registered image named `path`.
    pub fn image_id(&self, path: &str) -> Option<u32> {
        debug_assert!(
            self.images.len() <= Self::IMAGES_MAX,
            "images stay under capacity"
        );
        let k = self.images.iter().position(|i| i.path == path)?;
        Some(k as u32)
    }

    /// A registered image.
    ///
    /// # Panics
    ///
    /// Panics on an id [`TextSystem::image_id`] did not return.
    pub fn image(&self, id: u32) -> &Image {
        assert!(
            (id as usize) < self.images.len(),
            "image id {id} not registered"
        );
        let image = &self.images[id as usize];
        debug_assert!(image.width > 0, "registered images have pixels");
        image
    }

    /// A shaped layout.
    ///
    /// # Panics
    ///
    /// Panics on [`NO_TEXT`] or a stale id.
    pub fn layout(&self, id: TextId) -> &Layout<()> {
        assert_ne!(id, NO_TEXT, "no layout behind the sentinel");
        assert!(
            (id as usize) < self.layouts.len(),
            "layout id {id} is stale"
        );
        &self.layouts[id as usize]
    }

    /// Lays out LaTeX `src` at `px` pixels per em, cached by (source, size).
    ///
    /// # Errors
    ///
    /// Returns the parser's message when `src` is not in the supported LaTeX subset.
    ///
    /// # Panics
    ///
    /// If `px` is 0.
    pub fn math(&mut self, strings: &Strings, src: Sym, px: u16) -> Result<u32, String> {
        assert!(px > 0, "math size must be positive");
        if let Some(&id) = self.math_cache.get(&(src, px)) {
            debug_assert!((id as usize) < self.maths.len(), "a cached id is live");
            return Ok(id);
        }
        let list = math::layout_math(strings.get(src), f32::from(px), &self.math_fonts)?;
        let id = self.maths.len() as u32;
        self.maths.push(list);
        self.math_cache.insert((src, px), id);
        Ok(id)
    }

    /// A math layout from [`TextSystem::math`].
    ///
    /// # Panics
    ///
    /// Panics on a stale id.
    pub fn math_list(&self, id: u32) -> &MathList {
        assert!((id as usize) < self.maths.len(), "math id {id} is stale");
        &self.maths[id as usize]
    }

    /// Parses icon data `src` (the `icon` prop, [`crate::icon`]) once per source and returns
    /// its id; `None` when the data does not parse. The id is valid until the next
    /// [`TextSystem::begin_frame`] that evicts.
    pub fn icon(&mut self, strings: &Strings, src: Sym) -> Option<u32> {
        if let Some(&id) = self.icon_cache.get(&src) {
            debug_assert!(
                id == u32::MAX || (id as usize) < self.icons.len(),
                "a cached id is live"
            );
            return (id != u32::MAX).then_some(id);
        }
        let id = match icon::parse(strings.get(src)) {
            Some(icon) => {
                self.icons.push(icon);
                (self.icons.len() - 1) as u32
            }
            None => u32::MAX,
        };
        self.icon_cache.insert(src, id);
        debug_assert!(id == u32::MAX || (id as usize) < self.icons.len());
        (id != u32::MAX).then_some(id)
    }

    /// An icon from [`TextSystem::icon`].
    ///
    /// # Panics
    ///
    /// Panics on a stale id.
    pub fn icon_paths(&self, id: u32) -> &Icon {
        assert!((id as usize) < self.icons.len(), "icon id {id} is stale");
        &self.icons[id as usize]
    }

    /// The embedded math fonts (render needs them to draw [`MathList`] glyphs).
    pub fn math_fonts(&self) -> &MathFonts {
        &self.math_fonts
    }
}

#[cfg(test)]
mod tests {
    use parley::PositionedLayoutItem;

    use super::*;
    use crate::fonts::NOTO_SANS;

    #[test]
    fn shapes_with_embedded_font_and_caches() {
        let mut strings = Strings::new();
        let mut text = TextSystem::new();
        let a = strings.intern("File");
        let id = text.shape(&strings, a, 13);
        assert_eq!(text.shape(&strings, a, 13), id);
        assert_ne!(text.shape(&strings, a, 20), id);
        let w13 = text.layout(id).width();
        let l = text.layout(id);
        assert!(l.width() > 15.0 && l.width() < 40.0, "width {}", l.width());
        assert!(l.height() > 10.0 && l.height() < 25.0);
        let wide = text.shape(&strings, strings.find("File").unwrap(), 20);
        assert!(text.layout(wide).width() > w13);
    }

    /// The embedded fonts cover every character the UI ships (D39 cut, `fonts/README.md`),
    /// so system fonts never shape UI text: each cluster is a real glyph of the Noto subset,
    /// or of KaTeX Main for the arrows and relations Noto Sans lacks.
    #[test]
    fn ui_characters_come_from_the_embedded_fonts() {
        // ASCII, the non-ASCII characters in the app/ui/render/default-UI sources, Greek.
        let probe = "AZaz09!~ µΩ±×°→≤≥−…§Δ—· αβγδΣπ € ½ ⁰¹²³⁴⁵⁶⁷⁸⁹";
        let mut text = TextSystem::new();
        let mut layout = Layout::new();
        text.shape_into(probe, 14.0, &mut layout);
        let mut glyphs = 0;
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let font = run.run().font().data.data().as_ptr();
                let symbols = math::face_bytes("Main-Regular").as_ptr();
                assert!(
                    font == NOTO_SANS.as_ptr() || font == symbols,
                    "a glyph from another font"
                );
                for g in run.glyphs() {
                    assert_ne!(g.id, 0, "notdef in the embedded font");
                    glyphs += 1;
                }
            }
        }
        assert!(glyphs >= probe.chars().filter(|c| !c.is_whitespace()).count());
        assert!(NOTO_SANS.len() < 200 * 1024, "the UI font is the subset");
    }
}
