//! Transient interaction state, owned by `ui` and keyed by stable node ids so it survives
//! recomposition: open menus and dropdowns, the open dialog and where it sits, hover and the
//! keyboard highlight, focus and the focused field's editor, scroll offsets, split divider
//! positions. Written by [`crate::route_input`] and the host's setters; read by layout.
//!
//! Contents:
//! - `pub struct UiState`: `MAX_OPEN`, `MAX_SCROLL`, `MAX_SPLIT`, `WHEEL_PX`, `scroll_of`,
//!   `set_scroll`, `split_of`, `set_split`, `open_menus`, `open`, `set_dialog`, `set_dialog_at`,
//!   `dialog`, `hover`, `focus`, `editor`, `ime_area`, `focus_text`; crate-only `blur`

use parley::PlainEditor;

use crate::field::focus_field;
use crate::kinds::{Kind, StyleProp};
use crate::layout::{Rect, UiLayout};
use crate::strings::{Strings, Sym};
use crate::tables::UiTables;
use crate::text::TextSystem;

/// Transient interaction state, owned by `ui` and keyed by node ids.
#[derive(Default)]
pub struct UiState {
    pub(crate) open: Vec<Sym>,
    pub(crate) dialog: Sym,
    /// Top-left corner of the open dialog (a context menu at the pointer); `None` centers it.
    pub(crate) dialog_at: Option<(f32, f32)>,
    pub(crate) hover: Sym,
    pub(crate) focus: Sym,
    pub(crate) capture: bool,
    pub(crate) editor: Option<PlainEditor<()>>,
    /// Wheel offset of each scrolled `scroll` node, by node id (layout clamps it).
    pub(crate) scroll: Vec<(Sym, f32)>,
    /// Second-pane size of each dragged `split` node, by node id.
    pub(crate) split: Vec<(Sym, f32)>,
    /// The `split` node whose divider the pointer is dragging (`Sym::NONE` when none).
    pub(crate) split_drag: Sym,
    /// Top-left corner the user dragged a centered dialog to (layout keeps it inside the
    /// viewport); reset when another dialog opens.
    pub(crate) dialog_pos: Option<(f32, f32)>,
    /// While a dialog is dragged: the pointer's offset from the dialog's top-left corner.
    pub(crate) dialog_grab: Option<(f32, f32)>,
}

impl std::fmt::Debug for UiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiState")
            .field("open", &self.open)
            .field("dialog", &self.dialog)
            .field("hover", &self.hover)
            .field("focus", &self.focus)
            .finish()
    }
}

impl UiState {
    /// Maximum menu nesting.
    pub const MAX_OPEN: usize = 16;
    /// Scroll nodes whose offsets are remembered; the oldest is forgotten beyond this.
    pub const MAX_SCROLL: usize = 64;
    /// Split nodes whose divider positions are remembered; the oldest is forgotten beyond this.
    pub const MAX_SPLIT: usize = 64;
    /// Pixels per wheel line.
    pub const WHEEL_PX: f32 = 40.0;

    /// Scroll offset of `scroll` node `id` in pixels (0 when never scrolled).
    pub fn scroll_of(&self, id: Sym) -> f32 {
        self.scroll
            .iter()
            .find(|(s, _)| *s == id)
            .map_or(0.0, |(_, o)| *o)
    }

    /// Sets the scroll offset of node `id`, clamped to `[0, max]`.
    pub fn set_scroll(&mut self, id: Sym, offset: f32, max: f32) {
        let v = offset.clamp(0.0, max.max(0.0));
        remember(&mut self.scroll, Self::MAX_SCROLL, id, v);
    }

    /// Size in pixels of `split` node `id`'s second pane along the split's axis, once its
    /// divider was dragged (`None`: the panes keep their own sizes).
    pub fn split_of(&self, id: Sym) -> Option<f32> {
        self.split.iter().find(|(s, _)| *s == id).map(|(_, v)| *v)
    }

    /// Sets the second-pane size of `split` node `id` (what a divider drag does; a host may
    /// restore a saved layout with it). Takes effect at the next layout.
    ///
    /// # Panics
    ///
    /// Panics if `px` is negative or not finite.
    pub fn set_split(&mut self, id: Sym, px: f32) {
        assert!(px.is_finite() && px >= 0.0, "split size must be finite");
        remember(&mut self.split, Self::MAX_SPLIT, id, px);
    }

    /// Opens a chain of menus (menubar menu first); used by tests and by keyboard menu access.
    pub fn open_menus(&mut self, chain: &[Sym]) {
        assert!(chain.len() <= Self::MAX_OPEN, "menu chain too deep");
        self.open.clear();
        self.open.extend_from_slice(chain);
    }

    /// Open menu chain.
    pub fn open(&self) -> &[Sym] {
        &self.open
    }

    /// Shows dialog `id` modally (closing menus); `Sym::NONE` closes it.
    pub fn set_dialog(&mut self, id: Sym) {
        self.set_dialog_at(id, None);
    }

    /// Shows dialog `id` modally with its top-left corner at `at` (logical px, moved inside
    /// the viewport by layout), e.g. a context menu at the pointer; `None` centers it as
    /// [`UiState::set_dialog`] does. A dialog shown at a point closes on a click outside it,
    /// like a menu.
    pub fn set_dialog_at(&mut self, id: Sym, at: Option<(f32, f32)>) {
        assert!(
            at.is_none_or(|(x, y)| x.is_finite() && y.is_finite()),
            "dialog position must be finite"
        );
        self.open.clear();
        self.dialog = id;
        self.dialog_at = at.filter(|_| !id.is_none());
        self.dialog_pos = None;
        self.dialog_grab = None;
        self.blur();
    }

    /// The open dialog (`Sym::NONE` when none).
    pub fn dialog(&self) -> Sym {
        self.dialog
    }

    /// Node under the pointer, or the menu row the keyboard highlights.
    pub fn hover(&self) -> Sym {
        self.hover
    }

    /// Focused node.
    pub fn focus(&self) -> Sym {
        self.focus
    }

    /// The focused text field's editor; its layout is fresh after [`route_input`].
    pub fn editor(&self) -> Option<&PlainEditor<()>> {
        self.editor.as_ref()
    }

    /// Where the platform should place an IME candidate window: the focused field's preedit
    /// text, else its caret line, in logical px (`None` without a focused, laid-out field).
    /// `layout` must be the layout of the current state.
    pub fn ime_area(&self, tables: &UiTables, layout: &UiLayout) -> Option<Rect> {
        let ed = self.editor.as_ref()?;
        let n = tables.nodes.find(self.focus)?;
        let r = layout.row_of(n)?;
        let l = ed.try_layout()?;
        let area = layout.rect[r];
        let style = |p| layout.style(tables, n, p);
        // Where the painter puts the text: top-left inside the padding (text areas) or
        // vertically centered (single-line fields).
        let top = if tables.nodes.kind[n as usize] == Kind::TextArea {
            area.y + style(StyleProp::PadY)
        } else {
            area.y + (area.h - l.height()) / 2.0
        };
        let b = ed.ime_cursor_area();
        let out = Rect {
            x: area.x + style(StyleProp::Pad) + b.x0 as f32,
            y: top + b.y0 as f32,
            w: (b.x1 - b.x0).max(1.0) as f32,
            h: (b.y1 - b.y0).max(1.0) as f32,
        };
        debug_assert!(out.w > 0.0 && out.h > 0.0);
        Some(out)
    }

    pub(crate) fn blur(&mut self) {
        self.focus = Sym::NONE;
        self.editor = None;
    }

    /// Gives keyboard focus to text field `node` with the caret at the end of its value, as
    /// a dialog does for its first field when it opens. `layout` must be the layout of the
    /// current state with the field visible.
    ///
    /// Returns false (and changes nothing) when `node` is not a laid-out `text_input`.
    pub fn focus_text(
        &mut self,
        tables: &UiTables,
        layout: &UiLayout,
        strings: &Strings,
        text: &mut TextSystem,
        node: Sym,
    ) -> bool {
        let Some(n) = tables.nodes.find(node) else {
            return false;
        };
        if !matches!(
            tables.nodes.kind[n as usize],
            Kind::TextInput | Kind::TextArea
        ) {
            return false;
        }
        let Some(r) = layout.row_of(n) else {
            return false;
        };
        let rect = layout.rect[r];
        // A point past the last glyph puts the caret at the end.
        let (x, y) = (rect.x + rect.w - 1.0, rect.y + rect.h / 2.0);
        focus_field(tables, layout, strings, text, self, r, x, y);
        assert_eq!(self.focus, node);
        true
    }
}

// ---- private ----

/// Sets `id`'s value in a per-node table of at most `cap` rows, forgetting the oldest row to
/// make room.
fn remember(rows: &mut Vec<(Sym, f32)>, cap: usize, id: Sym, v: f32) {
    match rows.iter_mut().find(|(s, _)| *s == id) {
        Some(e) => e.1 = v,
        None => {
            if rows.len() >= cap {
                rows.remove(0);
            }
            rows.push((id, v));
        }
    }
    debug_assert!(rows.len() <= cap);
}
