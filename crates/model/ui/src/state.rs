//! Interaction state egui does not keep, owned by `ui` and keyed by stable node ids so it
//! survives recomposition: the open dialog and where it sits, the focused field's edit buffer,
//! scroll offsets (the host windows long lists by them), split divider positions and the
//! loaded docs images. Hover, focus, open menus and caret live in egui's memory.
//!
//! Contents:
//! - `pub struct UiState`: `MAX_SCROLL`, `MAX_SPLIT`, `SCROLL_TRIES`, `WHEEL_PX`, `MAX_IMAGES`,
//!   `scroll_of`, `set_scroll`, `split_of`, `set_split`, `set_dialog`, `set_dialog_at`, `dialog`,
//!   `dialog_at`, `focus`, `focus_text`, `set_keys`, `set_image`, `image`; crate-only
//!   `scroll_request`, `keep_scroll`, `icon`, `texture`
//! - `pub struct Image`; crate-only `struct NodeValue`

use std::collections::HashMap;
use std::sync::Arc;

use crate::math::MathList;
use crate::strings::Sym;

/// Interaction state the [`crate::show`] view reads and writes between frames.
#[derive(Default)]
pub struct UiState {
    pub(crate) dialog: Sym,
    /// Top-left corner of the open dialog (a context menu at the pointer); `None` centers it.
    pub(crate) dialog_at: Option<(f32, f32)>,
    /// Bumped whenever a dialog opens, so a reopened dialog forgets where it was dragged.
    pub(crate) dialog_serial: u32,
    /// The serial of the dialog last drawn (a new one is sized in a pass of its own).
    pub(crate) dialog_drawn: u32,
    /// The focused text field and its edit buffer (the field shows the node's value otherwise).
    pub(crate) edit: Option<(Sym, String)>,
    /// A text field to focus the next time it is shown.
    pub(crate) focus_next: Sym,
    /// Key chords go elsewhere this frame (another view of the same window has the keyboard).
    pub(crate) keys_off: bool,
    /// Offset of each scrolled `scroll` node, by node id.
    pub(crate) scroll: Vec<NodeValue>,
    /// Second-pane size of each dragged `split` node, by node id.
    pub(crate) split: Vec<NodeValue>,
    /// Scroll nodes the host moved ([`UiState::set_scroll`]), with the frames left to reach
    /// the offset (egui clamps it to the content it measured last frame).
    pub(crate) scroll_requests: Vec<(Sym, u8)>,
    /// Docs images by project-relative path, uploaded to egui on first use.
    pub(crate) images: HashMap<String, (Image, Option<egui::TextureHandle>)>,
    /// Laid-out formulas by (source, px bits).
    pub(crate) math: HashMap<(String, u32), Result<Arc<MathList>, String>>,
    /// Parsed button icons by their data's symbol (a button's `icon` prop).
    pub(crate) icons: HashMap<Sym, Option<Arc<crate::icon::Icon>>>,
}

/// One row of a per-node table of [`UiState`]: a node id and its remembered pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NodeValue {
    pub(crate) id: Sym,
    pub(crate) value: f32,
}

/// A decoded docs image: straight RGBA8 rows, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for UiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiState")
            .field("dialog", &self.dialog)
            .field("edit", &self.edit)
            .field("images", &self.images.len())
            .finish_non_exhaustive()
    }
}

impl UiState {
    /// Scroll nodes whose offsets are remembered; the oldest is forgotten beyond this.
    pub const MAX_SCROLL: usize = 64;
    /// Split nodes whose divider positions are remembered; the oldest is forgotten beyond this.
    pub const MAX_SPLIT: usize = 64;
    /// Frames a host's scroll request is retried while egui's content grows toward it.
    pub const SCROLL_TRIES: u8 = 4;
    /// Logical px per wheel line (a backend's pixel scroll is divided by it).
    pub const WHEEL_PX: f32 = 40.0;
    /// Docs images kept; registering more forgets none but refuses the new one.
    pub const MAX_IMAGES: usize = 256;

    /// Scroll offset of `scroll` node `id` in pixels (0 when never scrolled).
    #[must_use]
    pub fn scroll_of(&self, id: Sym) -> f32 {
        let v = self
            .scroll
            .iter()
            .find(|s| s.id == id)
            .map_or(0.0, |s| s.value);
        debug_assert!(v >= 0.0, "offsets are clamped at zero");
        v
    }

    /// Sets the scroll offset of node `id`, clamped to `[0, max]`; the view scrolls there the
    /// next time it shows the node.
    pub fn set_scroll(&mut self, id: Sym, offset: f32, max: f32) {
        let v = offset.clamp(0.0, max.max(0.0));
        remember(&mut self.scroll, Self::MAX_SCROLL, id, v);
        self.scroll_requests.retain(|r| r.0 != id);
        if self.scroll_requests.len() < Self::MAX_SCROLL {
            self.scroll_requests.push((id, Self::SCROLL_TRIES));
        }
        debug_assert!(self.scroll.len() <= Self::MAX_SCROLL);
    }

    /// The offset the host asked `id` to scroll to, while it is pending.
    pub(crate) fn scroll_request(&self, id: Sym) -> Option<f32> {
        self.scroll_requests
            .iter()
            .any(|r| r.0 == id)
            .then(|| self.scroll_of(id))
    }

    /// Records where the view left `id` scrolled. A pending request the view did not reach
    /// yet (its content still growing) keeps its offset for a few more frames.
    pub(crate) fn keep_scroll(&mut self, id: Sym, offset: f32, max: f32) {
        if let Some(k) = self.scroll_requests.iter().position(|r| r.0 == id) {
            let want = self.scroll_of(id);
            let tries = &mut self.scroll_requests[k].1;
            *tries = tries.saturating_sub(1);
            if (offset - want).abs() > 0.5 && *tries > 0 {
                return;
            }
            self.scroll_requests.swap_remove(k);
        }
        remember(
            &mut self.scroll,
            Self::MAX_SCROLL,
            id,
            offset.clamp(0.0, max.max(0.0)),
        );
    }

    /// Size in pixels of `split` node `id`'s second pane along the split's axis, once its
    /// divider was dragged (`None`: the panes keep their own sizes).
    #[must_use]
    pub fn split_of(&self, id: Sym) -> Option<f32> {
        self.split.iter().find(|s| s.id == id).map(|s| s.value)
    }

    /// Sets the second-pane size of `split` node `id`.
    ///
    /// # Panics
    ///
    /// Panics if `px` is negative or not finite.
    pub fn set_split(&mut self, id: Sym, px: f32) {
        assert!(px.is_finite() && px >= 0.0, "split size must be finite");
        remember(&mut self.split, Self::MAX_SPLIT, id, px);
        debug_assert!(self.split.len() <= Self::MAX_SPLIT);
    }

    /// Shows dialog `id` modally, centered; `Sym::NONE` closes it.
    pub fn set_dialog(&mut self, id: Sym) {
        self.set_dialog_at(id, None);
        debug_assert!(self.dialog_at.is_none());
    }

    /// Shows dialog `id` modally with its top-left corner at `at` (logical px, kept inside
    /// the window), e.g. a context menu at the pointer; `None` centers it. A dialog shown at a
    /// point closes on a click outside it, like a menu.
    ///
    /// # Panics
    ///
    /// Panics if `at` is not finite.
    pub fn set_dialog_at(&mut self, id: Sym, at: Option<(f32, f32)>) {
        assert!(
            at.is_none_or(|(x, y)| x.is_finite() && y.is_finite()),
            "dialog position must be finite"
        );
        self.dialog = id;
        self.dialog_at = at.filter(|_| !id.is_none());
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        self.edit = None;
        debug_assert!(!id.is_none() || self.dialog_at.is_none());
    }

    /// The open dialog (`Sym::NONE` when none).
    #[must_use]
    pub fn dialog(&self) -> Sym {
        self.dialog
    }

    /// Where the open dialog was asked to appear (`None`: centered).
    #[must_use]
    pub fn dialog_at(&self) -> Option<(f32, f32)> {
        self.dialog_at
    }

    /// The text field being edited (`Sym::NONE` when none).
    #[must_use]
    pub fn focus(&self) -> Sym {
        self.edit.as_ref().map_or(Sym::NONE, |e| e.0)
    }

    /// Gives keyboard focus to text field `node`, caret at the end of its value, the next time
    /// the view shows it (as a dialog does for its first field when it opens).
    pub fn focus_text(&mut self, node: Sym) {
        debug_assert!(!node.is_none(), "a field to focus");
        self.focus_next = node;
    }

    /// Whether this view's keymap takes the frame's keys (default on): a window showing two
    /// views gives them to the one under the pointer.
    pub fn set_keys(&mut self, on: bool) {
        self.keys_off = !on;
        debug_assert_eq!(self.keys_off, !on);
    }

    /// Registers the decoded image at project-relative `path` for `prose` image lines.
    /// Returns false (and keeps nothing) when [`UiState::MAX_IMAGES`] are registered.
    ///
    /// # Panics
    ///
    /// Panics if `image.rgba` is not `width * height * 4` bytes.
    pub fn set_image(&mut self, path: &str, image: Image) -> bool {
        assert_eq!(
            image.rgba.len() as u64,
            u64::from(image.width) * u64::from(image.height) * 4,
            "RGBA8 rows"
        );
        if self.images.len() >= Self::MAX_IMAGES && !self.images.contains_key(path) {
            return false;
        }
        self.images.insert(path.to_owned(), (image, None));
        debug_assert!(self.images.len() <= Self::MAX_IMAGES);
        true
    }

    /// The image registered at `path`.
    #[must_use]
    pub fn image(&self, path: &str) -> Option<&Image> {
        self.images.get(path).map(|i| &i.0)
    }

    /// The parsed icon of data `src` (`None`: not an icon), parsed once per string.
    pub(crate) fn icon(
        &mut self,
        strings: &crate::strings::Strings,
        src: Sym,
    ) -> Option<Arc<crate::icon::Icon>> {
        if src.is_none() {
            return None;
        }
        if self.icons.len() >= Self::MAX_IMAGES * 4 {
            self.icons.clear(); // bounded: live state text could intern new data each frame
        }
        self.icons
            .entry(src)
            .or_insert_with(|| crate::icon::parse(strings.get(src)).map(Arc::new))
            .clone()
    }

    /// The egui texture of the image at `path`, uploaded on first use.
    pub(crate) fn texture(&mut self, ctx: &egui::Context, path: &str) -> Option<egui::TextureId> {
        let (image, tex) = self.images.get_mut(path)?;
        let tex = tex.get_or_insert_with(|| {
            let size = [image.width as usize, image.height as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, &image.rgba);
            ctx.load_texture(path, color, egui::TextureOptions::LINEAR)
        });
        Some(tex.id())
    }
}

// ---- private ----

/// Sets `id`'s value in a per-node table of at most `cap` rows, forgetting the oldest row to
/// make room.
fn remember(rows: &mut Vec<NodeValue>, cap: usize, id: Sym, value: f32) {
    assert!(cap > 0, "a table with room");
    match rows.iter_mut().find(|s| s.id == id) {
        Some(e) => e.value = value,
        None => {
            if rows.len() >= cap {
                rows.remove(0);
            }
            rows.push(NodeValue { id, value });
        }
    }
    debug_assert!(rows.len() <= cap);
}
