//! Input routing: a windowing-system-independent event table in, `command_requests` and
//! interaction-state changes out (spine: `ui::route_input`).
//!
//! The UI never edits `ui_nodes` in response to input. Clicking a checkbox emits its command;
//! whoever owns the checked state answers with a `set … checked` contribution. Only transient
//! interaction state (open menus and dropdowns, hover and the keyboard highlight, focus, the
//! focused field's editor, scroll offsets, split divider positions) lives in [`UiState`], keyed
//! by stable node ids so it survives recomposition.

use parley::{FontFamily, PlainEditor, StyleProperty};

use crate::layout::{Shown, UiLayout, area_scroll_max};
use crate::strings::{Strings, Sym};
use crate::tables::{Chord, Key, Kind, Mods, NO_NODE, NodeIx, Prop, StyleProp, UiTables};
use crate::text::TextSystem;

/// Pointer button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    /// Primary.
    Left,
    /// Wheel button (pans the canvas in xschem).
    Middle,
    /// Secondary.
    Right,
}

/// One input event in logical pixels. Text payloads are byte ranges into [`InputEvents::text`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InputEvent {
    /// Pointer moved.
    PointerMove {
        /// X.
        x: f32,
        /// Y.
        y: f32,
    },
    /// Button pressed.
    PointerDown {
        /// X.
        x: f32,
        /// Y.
        y: f32,
        /// Which button.
        button: Button,
    },
    /// Button released.
    PointerUp {
        /// X.
        x: f32,
        /// Y.
        y: f32,
        /// Which button.
        button: Button,
    },
    /// Pointer left the window.
    PointerLeave,
    /// Wheel or touchpad scroll, in lines (positive `dy` = away from the user).
    Wheel {
        /// X.
        x: f32,
        /// Y.
        y: f32,
        /// Horizontal lines.
        dx: f32,
        /// Vertical lines.
        dy: f32,
    },
    /// Key pressed (auto-repeat included), normalized as a [`Chord`].
    Key {
        /// The chord.
        chord: Chord,
    },
    /// Committed text (typing or IME commit).
    Text {
        /// Start byte in [`InputEvents::text`].
        start: u32,
        /// End byte.
        end: u32,
    },
    /// IME preedit (empty range clears it).
    Preedit {
        /// Start byte in [`InputEvents::text`].
        start: u32,
        /// End byte.
        end: u32,
    },
    /// Assistive-technology "click" on a node (AccessKit `Action::Click`).
    Activate {
        /// Target node id.
        node: Sym,
    },
    /// The held modifiers changed (pointer gestures such as Shift+wheel read them; key chords
    /// carry their own).
    Mods {
        /// Modifier bits now held.
        mods: Mods,
    },
}

/// The input event table for one frame.
#[derive(Clone, Debug, Default)]
pub struct InputEvents {
    /// Events in arrival order.
    pub events: Vec<InputEvent>,
    /// Text payloads.
    pub text: String,
}

impl InputEvents {
    /// Events kept per frame; extra events are dropped (a stalled frame must not grow memory).
    pub const CAPACITY: usize = 4096;

    /// Appends an event; returns false when the table is full and the event was dropped.
    pub fn push(&mut self, e: InputEvent) -> bool {
        if self.events.len() >= Self::CAPACITY {
            return false;
        }
        self.events.push(e);
        true
    }

    /// Appends a committed-text event.
    pub fn push_text(&mut self, s: &str) -> bool {
        let start = self.text.len() as u32;
        self.text.push_str(s);
        self.push(InputEvent::Text {
            start,
            end: self.text.len() as u32,
        })
    }

    /// Appends an IME preedit event.
    pub fn push_preedit(&mut self, s: &str) -> bool {
        let start = self.text.len() as u32;
        self.text.push_str(s);
        self.push(InputEvent::Preedit {
            start,
            end: self.text.len() as u32,
        })
    }

    /// Empties the table, keeping its allocation.
    pub fn clear(&mut self) {
        self.events.clear();
        self.text.clear();
    }

    /// True when no event is queued.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// `command_requests`: commands to run, consumed by the app's dispatcher.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandRequests {
    /// Command names.
    pub command: Vec<Sym>,
    /// Node that issued it (`Sym::NONE` for key bindings).
    pub node: Vec<Sym>,
    /// End byte of each argument in `args` (the text of a submitted field; usually empty).
    pub arg_end: Vec<u32>,
    /// Argument bytes.
    pub args: String,
}

impl CommandRequests {
    fn push(&mut self, command: Sym, node: Sym, arg: &str) {
        self.command.push(command);
        self.node.push(node);
        self.args.push_str(arg);
        self.arg_end.push(self.args.len() as u32);
    }

    /// Argument of request `i`.
    pub fn arg(&self, i: usize) -> &str {
        let start = if i == 0 {
            0
        } else {
            self.arg_end[i - 1] as usize
        };
        &self.args[start..self.arg_end[i] as usize]
    }

    /// Number of requests.
    pub fn len(&self) -> usize {
        self.command.len()
    }

    /// True when nothing was requested.
    pub fn is_empty(&self) -> bool {
        self.command.is_empty()
    }
}

/// Everything [`route_input`] produces for one frame.
#[derive(Clone, Debug, Default)]
pub struct Routed {
    /// Commands to dispatch.
    pub commands: CommandRequests,
    /// Events that belong to the canvas (pointer over it or captured by a drag started on it).
    pub canvas: Vec<InputEvent>,
    /// The layout is stale (menus opened/closed, focus changed size).
    pub relayout: bool,
    /// The picture changed (hover, editing).
    pub redraw: bool,
}

impl Routed {
    fn clear(&mut self) {
        self.commands = CommandRequests::default();
        self.canvas.clear();
        self.relayout = false;
        self.redraw = false;
    }
}

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
        match self.scroll.iter_mut().find(|(s, _)| *s == id) {
            Some(e) => e.1 = v,
            None => {
                if self.scroll.len() >= Self::MAX_SCROLL {
                    self.scroll.remove(0);
                }
                self.scroll.push((id, v));
            }
        }
        debug_assert!(self.scroll.len() <= Self::MAX_SCROLL);
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
        match self.split.iter_mut().find(|(s, _)| *s == id) {
            Some(e) => e.1 = px,
            None => {
                if self.split.len() >= Self::MAX_SPLIT {
                    self.split.remove(0);
                }
                self.split.push((id, px));
            }
        }
        debug_assert!(self.split.len() <= Self::MAX_SPLIT);
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
    pub fn ime_area(&self, tables: &UiTables, layout: &UiLayout) -> Option<crate::Rect> {
        let ed = self.editor.as_ref()?;
        let n = tables.nodes.find(self.focus)?;
        let r = layout.row_of(n)?;
        let l = ed.try_layout()?;
        let area = layout.rect[r];
        let style =
            |p| crate::layout::row_style(tables, layout.title_style, n, Shown::Normal, p) as f32;
        // Where the painter puts the text: top-left inside the padding (text areas) or
        // vertically centered (single-line fields).
        let top = if tables.nodes.kind[n as usize] == Kind::TextArea {
            area.y + style(StyleProp::PadY)
        } else {
            area.y + (area.h - l.height()) / 2.0
        };
        let b = ed.ime_cursor_area();
        let out = crate::Rect {
            x: area.x + style(StyleProp::Pad) + b.x0 as f32,
            y: top + b.y0 as f32,
            w: (b.x1 - b.x0).max(1.0) as f32,
            h: (b.y1 - b.y0).max(1.0) as f32,
        };
        debug_assert!(out.w > 0.0 && out.h > 0.0);
        Some(out)
    }

    fn blur(&mut self) {
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

fn interactive(kind: Kind, shown: Shown) -> bool {
    shown != Shown::Normal
        || matches!(
            kind,
            Kind::Item
                | Kind::Button
                | Kind::Tab
                | Kind::Checkbox
                | Kind::Dropdown
                | Kind::TextInput
                | Kind::TextArea
        )
}

/// Depth in the open chain of the popup that contains `node`, if any.
fn popup_depth(tables: &UiTables, state: &UiState, node: NodeIx) -> Option<usize> {
    let parent = tables.nodes.parent[node as usize];
    if parent == NO_NODE {
        return None;
    }
    let pid = tables.nodes.id[parent as usize];
    state.open.iter().position(|&m| m == pid)
}

/// True for a dropdown with choices (it opens a popup instead of running its command).
fn has_popup(tables: &UiTables, node: NodeIx) -> bool {
    tables.nodes.kind[node as usize] == Kind::Dropdown
        && tables.nodes.children(node).next().is_some()
}

/// Routes one frame of input: keymap chords, clicks on items and buttons, menu and dropdown
/// popups (pointer and keyboard), hover tracking, split dividers, text-field focus and editing
/// (including IME preedit), and canvas forwarding.
///
/// Reads `tables` and `layout` (which must be the layout of the current `state`); writes only
/// `state` and `out`. A text field's `input` command is pushed with its text before any other
/// command of the same event that follows the edit.
///
/// # Panics
///
/// Panics if an event's text range lies outside `events.text`.
pub fn route_input(
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &mut TextSystem,
    state: &mut UiState,
    events: &InputEvents,
    out: &mut Routed,
) {
    assert!(
        events.events.len() <= InputEvents::CAPACITY,
        "input table over capacity"
    );
    out.clear();
    let nodes = &tables.nodes;
    let ids = nodes.id();
    // The focused field's text changed since its last `input` command.
    let mut typed = false;
    for &ev in &events.events {
        let modal = nodes.find(state.dialog).is_some();
        let hit = |x: f32, y: f32| {
            layout
                .hit(x, y, modal)
                .map(|r| (r, layout.node[r], layout.shown[r]))
        };
        match ev {
            InputEvent::PointerMove { x, y } => {
                if !state.split_drag.is_none() {
                    drag_split(tables, layout, state, x, y);
                    out.relayout = true;
                    continue;
                }
                if state.capture {
                    out.canvas.push(ev);
                    continue;
                }
                let h = hit(x, y);
                let hover = match h {
                    Some((_, n, s)) if interactive(nodes.kind[n as usize], s) => ids[n as usize],
                    _ => Sym::NONE,
                };
                if hover != state.hover {
                    state.hover = hover;
                    out.redraw = true;
                }
                match h {
                    // Menu tracking: once a menubar menu is open, hovering moves the open chain
                    // (an open dropdown does not follow the pointer onto the menubar).
                    Some((_, n, Shown::Title))
                        if !state.open.is_empty()
                            && state.open[0] != ids[n as usize]
                            && nodes
                                .find(state.open[0])
                                .is_some_and(|m| nodes.kind[m as usize] == Kind::Menu) =>
                    {
                        state.open.clear();
                        state.open.push(ids[n as usize]);
                        out.relayout = true;
                    }
                    Some((_, n, s))
                        if !state.open.is_empty()
                            && matches!(s, Shown::Cascade | Shown::Normal) =>
                    {
                        if let Some(d) = popup_depth(tables, state, n) {
                            let before = state.open.len();
                            state.open.truncate(d + 1);
                            if s == Shown::Cascade && state.open.len() < UiState::MAX_OPEN {
                                state.open.push(ids[n as usize]);
                            }
                            out.relayout |= state.open.len() != before || s == Shown::Cascade;
                        }
                    }
                    Some((_, n, Shown::Normal)) if nodes.kind[n as usize] == Kind::Canvas => {
                        out.canvas.push(ev)
                    }
                    _ => {}
                }
            }
            InputEvent::PointerDown { x, y, button } => {
                let h = hit(x, y);
                let on_canvas = matches!(h, Some((_, n, Shown::Normal)) if nodes.kind[n as usize] == Kind::Canvas);
                if !state.open.is_empty() && (h.is_none() || on_canvas || button != Button::Left) {
                    state.open.clear(); // click outside closes menus and is consumed (Tk)
                    out.relayout = true;
                    continue;
                }
                if modal && h.is_none() && state.dialog_at.is_some() {
                    // A dialog at a point (the context menu) closes like a menu.
                    flush_typed(tables, state, &mut typed, out);
                    state.set_dialog(Sym::NONE);
                    out.relayout = true;
                    continue;
                }
                if on_canvas {
                    state.capture = true;
                    if !state.focus.is_none() {
                        flush_typed(tables, state, &mut typed, out);
                        state.blur();
                        out.redraw = true;
                    }
                    out.canvas.push(ev);
                    continue;
                }
                let Some((r, n, shown)) = h else { continue };
                if button != Button::Left {
                    continue;
                }
                let i = n as usize;
                let id = ids[i];
                let field = matches!(nodes.kind[i], Kind::TextInput | Kind::TextArea);
                let same = field && state.focus == id;
                if !same && !state.focus.is_none() {
                    flush_typed(tables, state, &mut typed, out);
                    state.blur();
                    out.redraw = true;
                    out.relayout = true; // a text_area shrinks back to its value
                }
                match (nodes.kind[i], shown) {
                    (_, Shown::Title) => {
                        let reopen = state.open.first() != Some(&id);
                        state.open.clear();
                        if reopen {
                            state.open.push(id);
                        }
                        out.relayout = true;
                    }
                    (_, Shown::Cascade) => {
                        if let Some(d) = popup_depth(tables, state, n) {
                            state.open.truncate(d + 1);
                            state.open.push(id);
                            out.relayout = true;
                        }
                    }
                    (Kind::Dropdown, Shown::Normal) if has_popup(tables, n) => {
                        toggle_popup(tables, state, n);
                        out.relayout = true;
                    }
                    (Kind::Split, Shown::Normal) => {
                        // Inside a split only its divider is not covered by a pane.
                        state.split_drag = id;
                        out.redraw = true;
                    }
                    (Kind::TextInput | Kind::TextArea, _) => {
                        focus_field(tables, layout, strings, text, state, r, x, y);
                        out.redraw = true;
                    }
                    (
                        Kind::Item
                        | Kind::Button
                        | Kind::Tab
                        | Kind::Checkbox
                        | Kind::Dropdown
                        | Kind::Prose
                        | Kind::Label,
                        _,
                    ) => {
                        let cmd = nodes.prop(Prop::Command)[i];
                        if !cmd.is_none() && !nodes.is_disabled(strings, n) {
                            out.commands.push(cmd, id, "");
                            out.relayout |= !state.open.is_empty();
                            state.open.clear();
                        }
                    }
                    _ => {}
                }
            }
            InputEvent::PointerUp { .. } => {
                if !state.split_drag.is_none() {
                    state.split_drag = Sym::NONE;
                    out.redraw = true;
                } else if state.capture {
                    state.capture = false;
                    out.canvas.push(ev);
                }
            }
            InputEvent::PointerLeave => {
                if !state.hover.is_none() {
                    state.hover = Sym::NONE;
                    out.redraw = true;
                }
            }
            InputEvent::Wheel { x, y, dy, .. } => {
                let h = hit(x, y);
                let on_canvas = matches!(h, Some((_, n, Shown::Normal)) if nodes.kind[n as usize] == Kind::Canvas);
                if on_canvas && state.open.is_empty() {
                    out.canvas.push(ev);
                    continue;
                }
                // A text area taller than its box scrolls inside itself (not the pane).
                if let Some((r, n, Shown::Normal)) = h
                    && nodes.kind[n as usize] == Kind::TextArea
                {
                    let max = area_scroll_max(tables, layout, text, state, r);
                    if max > 0.0 {
                        let id = ids[n as usize];
                        let now = state.scroll_of(id);
                        state.set_scroll(id, now - dy * UiState::WHEEL_PX, max);
                        out.redraw |= state.scroll_of(id) != now;
                        continue;
                    }
                }
                // Over a scroll pane (or anything inside one): scroll it. Wheel away from the
                // user (positive `dy`) moves the content down, like every text view.
                let pane = h.map(|(r, n, _)| {
                    if nodes.kind[n as usize] == Kind::Scroll {
                        n
                    } else {
                        layout.scroller(r)
                    }
                });
                if let Some(s) = pane.filter(|&s| s != NO_NODE)
                    && let Some(max) = layout.scroll_max(s)
                {
                    let id = ids[s as usize];
                    let now = state.scroll_of(id);
                    state.set_scroll(id, now - dy * UiState::WHEEL_PX, max);
                    out.relayout |= state.scroll_of(id) != now;
                }
            }
            InputEvent::Key { chord } => {
                if state.editor.is_some() {
                    let area = nodes
                        .find(state.focus)
                        .is_some_and(|n| nodes.kind[n as usize] == Kind::TextArea);
                    edit_key(tables, text, state, chord, area, &mut typed, out);
                    out.redraw = true;
                    out.relayout |= area; // the field grows and shrinks with its lines
                    if area {
                        follow_caret(tables, layout, text, state);
                    }
                } else if !state.open.is_empty() {
                    // An open popup has the keyboard: no key binding runs under it.
                    menu_key(tables, strings, state, chord, out);
                    out.relayout = true;
                    out.redraw = true;
                } else if chord == Chord::new(Mods::default(), Key::ESCAPE) && modal {
                    state.dialog = Sym::NONE;
                    state.dialog_at = None;
                    out.relayout = true;
                } else if let Some(cmd) = tables.keymap.lookup(chord) {
                    out.commands.push(cmd, Sym::NONE, "");
                } else if chord == Chord::new(Mods::default(), Key::f(10))
                    && !modal
                    && open_menubar(tables, strings, state)
                {
                    // F10 (when nothing binds it) opened the first menubar menu.
                    out.relayout = true;
                    out.redraw = true;
                }
            }
            InputEvent::Text { start, end } => {
                let s = &events.text[start as usize..end as usize];
                if let Some(ed) = state.editor.as_mut() {
                    let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
                    d.clear_compose();
                    d.insert_or_replace_selection(s);
                    d.refresh_layout();
                    typed = true;
                    out.redraw = true;
                    let area = nodes
                        .find(state.focus)
                        .is_some_and(|n| nodes.kind[n as usize] == Kind::TextArea);
                    out.relayout |= area;
                    if area {
                        follow_caret(tables, layout, text, state);
                    }
                }
            }
            InputEvent::Preedit { start, end } => {
                let s = &events.text[start as usize..end as usize];
                if let Some(ed) = state.editor.as_mut() {
                    let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
                    if s.is_empty() {
                        d.clear_compose();
                    } else {
                        d.set_compose(s, None);
                    }
                    d.refresh_layout();
                    out.redraw = true;
                }
            }
            InputEvent::Mods { .. } => {}
            InputEvent::Activate { node } => {
                let Some(n) = nodes.find(node) else { continue };
                let cmd = nodes.prop(Prop::Command)[n as usize];
                if nodes.kind[n as usize] == Kind::Menu || has_popup(tables, n) {
                    let chain_ok =
                        popup_depth(tables, state, n).map(|d| state.open.truncate(d + 1));
                    if chain_ok.is_none() {
                        state.open.clear();
                    }
                    if state.open.len() < UiState::MAX_OPEN {
                        state.open.push(node);
                    }
                    out.relayout = true;
                } else if !cmd.is_none() && !nodes.is_disabled(strings, n) {
                    out.commands.push(cmd, node, "");
                }
            }
        }
    }
    flush_typed(tables, state, &mut typed, out);
    debug_assert!(!typed);
    debug_assert!(state.open.len() <= UiState::MAX_OPEN);
}

/// Pushes the focused field's `input` command with its text when an edit changed it since the
/// last push. Called before anything that blurs the field and once at the end of a frame.
fn flush_typed(tables: &UiTables, state: &UiState, typed: &mut bool, out: &mut Routed) {
    if !std::mem::take(typed) {
        return;
    }
    let (Some(n), Some(ed)) = (tables.nodes.find(state.focus), state.editor.as_ref()) else {
        return;
    };
    let cmd = tables.nodes.prop(Prop::Input)[n as usize];
    if !cmd.is_none() {
        out.commands.push(cmd, state.focus, ed.raw_text());
    }
    debug_assert!(!*typed);
}

/// Opens or closes dropdown `n`'s popup: a dropdown inside an open popup keeps the chain above
/// it, anywhere else it starts a new chain.
fn toggle_popup(tables: &UiTables, state: &mut UiState, n: NodeIx) {
    let id = tables.nodes.id[n as usize];
    let reopen = state.open.last() != Some(&id);
    match popup_depth(tables, state, n) {
        Some(d) => state.open.truncate(d + 1),
        None => state.open.clear(),
    }
    if reopen && state.open.len() < UiState::MAX_OPEN {
        state.open.push(id);
    }
    debug_assert!(state.open.len() <= UiState::MAX_OPEN);
}

/// Moves the divider of the dragged `split` to the pointer: the second pane takes the space
/// from the pointer to the split's far edge, leaving each pane at least
/// [`crate::layout::SPLIT_MIN`].
fn drag_split(tables: &UiTables, layout: &UiLayout, state: &mut UiState, x: f32, y: f32) {
    let id = state.split_drag;
    let Some(r) = tables.nodes.find(id).and_then(|n| layout.row_of(n)) else {
        state.split_drag = Sym::NONE; // the split went away mid-drag
        return;
    };
    let n = layout.node[r];
    let style =
        |p| crate::layout::row_style(tables, layout.title_style, n, Shown::Normal, p) as f32;
    let gap = style(StyleProp::Gap).max(crate::layout::SPLIT_GAP);
    let rect = layout.rect[r];
    let (far, pos, pad, len) = if style(StyleProp::Dir) != 0.0 {
        (rect.x + rect.w, x, style(StyleProp::Pad), rect.w)
    } else {
        (rect.y + rect.h, y, style(StyleProp::PadY), rect.h)
    };
    let total = (len - 2.0 * pad - gap).max(0.0);
    let min = crate::layout::SPLIT_MIN.min(total / 2.0);
    let px = (far - pad - pos - gap / 2.0).clamp(min, total - min);
    state.set_split(id, px);
    debug_assert!(px >= 0.0 && px <= total);
}

/// Opens the first menu of the first menubar with the keyboard highlight on its first row.
/// Returns false when there is no menubar menu.
fn open_menubar(tables: &UiTables, strings: &Strings, state: &mut UiState) -> bool {
    let n = &tables.nodes;
    let Some(bar) = (0..n.len()).find(|&i| n.kind[i] == Kind::Menubar) else {
        return false;
    };
    let Some(first) = n
        .children(bar as NodeIx)
        .find(|&c| n.kind[c as usize] == Kind::Menu)
    else {
        return false;
    };
    state.open.clear();
    state.open.push(n.id[first as usize]);
    state.hover = first_row(tables, strings, first);
    debug_assert_eq!(state.open.len(), 1);
    true
}

/// Rows of popup `menu` the keyboard can land on: enabled items, cascades and dropdowns.
fn nav_rows(tables: &UiTables, strings: &Strings, menu: NodeIx) -> Vec<NodeIx> {
    let n = &tables.nodes;
    n.children(menu)
        .filter(|&c| {
            matches!(n.kind[c as usize], Kind::Item | Kind::Menu | Kind::Dropdown)
                && !n.is_disabled(strings, c)
        })
        .collect()
}

/// Id of the first keyboard row of `menu` (`Sym::NONE` when it has none).
fn first_row(tables: &UiTables, strings: &Strings, menu: NodeIx) -> Sym {
    nav_rows(tables, strings, menu)
        .first()
        .map_or(Sym::NONE, |&c| tables.nodes.id[c as usize])
}

/// Keyboard navigation of the open popup chain; the deepest popup has the keyboard and
/// [`UiState::hover`] is its highlighted row. Up/Down move the highlight (wrapping), Right
/// opens a cascade or moves to the next menubar menu, Left closes a cascade or moves to the
/// previous menubar menu, Enter or Space activates, Escape closes one level, and a letter
/// (mnemonic) jumps to the next row whose label starts with it, activating it when it is the
/// only one.
fn menu_key(
    tables: &UiTables,
    strings: &Strings,
    state: &mut UiState,
    chord: Chord,
    out: &mut Routed,
) {
    let n = &tables.nodes;
    let Some(menu) = state.open.last().and_then(|&m| n.find(m)) else {
        state.open.clear(); // the open popup's node went away
        return;
    };
    let rows = nav_rows(tables, strings, menu);
    let cur = rows.iter().position(|&c| n.id[c as usize] == state.hover);
    let key = chord.key();
    let plain = chord.mods().0 & (Mods::CTRL | Mods::ALT | Mods::SUPER) == 0;
    match key {
        Key::ESCAPE => {
            let closed = state.open.pop().expect("checked non-empty");
            state.hover = if state.open.is_empty() {
                Sym::NONE
            } else {
                closed
            };
        }
        Key::DOWN | Key::UP if !rows.is_empty() => {
            let len = rows.len();
            let next = match (cur, key == Key::DOWN) {
                (None, true) => 0,
                (None, false) => len - 1,
                (Some(i), true) => (i + 1) % len,
                (Some(i), false) => (i + len - 1) % len,
            };
            state.hover = n.id[rows[next] as usize];
        }
        Key::RIGHT => match cur {
            Some(i) if n.kind[rows[i] as usize] == Kind::Menu => {
                activate(tables, strings, state, rows[i], out);
            }
            _ => step_menubar(tables, strings, state, true),
        },
        Key::LEFT => {
            if state.open.len() > 1 {
                state.hover = state.open.pop().expect("more than one open");
            } else {
                step_menubar(tables, strings, state, false);
            }
        }
        Key::ENTER => {
            if let Some(i) = cur {
                activate(tables, strings, state, rows[i], out);
            }
        }
        k if k == Key(' ' as u32) && plain => {
            if let Some(i) = cur {
                activate(tables, strings, state, rows[i], out);
            }
        }
        k if plain && k.0 < Key::NAMED => {
            let Some(c) = char::from_u32(k.0) else { return };
            let starts = |row: NodeIx| {
                let label = strings.get(n.prop(Prop::Label)[row as usize]);
                label
                    .chars()
                    .next()
                    .is_some_and(|f| f.to_lowercase().eq(c.to_lowercase()))
            };
            let hits: Vec<usize> = (0..rows.len()).filter(|&j| starts(rows[j])).collect();
            match hits.as_slice() {
                [] => {}
                [only] => activate(tables, strings, state, rows[*only], out),
                many => {
                    // The next match after the highlight, wrapping.
                    let after = cur.map_or(0, |i| i + 1);
                    let j = many
                        .iter()
                        .copied()
                        .find(|&j| j >= after)
                        .unwrap_or(many[0]);
                    state.hover = n.id[rows[j] as usize];
                }
            }
        }
        _ => {}
    }
    debug_assert!(state.open.len() <= UiState::MAX_OPEN);
}

/// Runs a popup row from the keyboard: an item runs its command and closes the chain; a
/// cascade or dropdown opens its popup with the highlight on its first row.
fn activate(
    tables: &UiTables,
    strings: &Strings,
    state: &mut UiState,
    row: NodeIx,
    out: &mut Routed,
) {
    let n = &tables.nodes;
    let i = row as usize;
    let opens = n.kind[i] == Kind::Menu || has_popup(tables, row);
    if opens {
        if state.open.len() < UiState::MAX_OPEN {
            state.open.push(n.id[i]);
            state.hover = first_row(tables, strings, row);
        }
        return;
    }
    let cmd = n.prop(Prop::Command)[i];
    if !cmd.is_none() && !n.is_disabled(strings, row) {
        out.commands.push(cmd, n.id[i], "");
        state.open.clear();
        state.hover = Sym::NONE;
    }
}

/// Moves the open chain to the next (`forward`) or previous menu of the menubar holding the
/// open root menu, highlighting its first row. Does nothing for a popup not opened from a
/// menubar (a dropdown).
fn step_menubar(tables: &UiTables, strings: &Strings, state: &mut UiState, forward: bool) {
    let n = &tables.nodes;
    let Some(root) = state.open.first().and_then(|&m| n.find(m)) else {
        return;
    };
    let bar = n.parent[root as usize];
    if bar == NO_NODE || n.kind[bar as usize] != Kind::Menubar {
        return;
    }
    let menus: Vec<NodeIx> = n
        .children(bar)
        .filter(|&c| n.kind[c as usize] == Kind::Menu)
        .collect();
    let Some(at) = menus.iter().position(|&m| m == root) else {
        return;
    };
    let len = menus.len();
    let next = menus[if forward {
        (at + 1) % len
    } else {
        (at + len - 1) % len
    }];
    state.open.clear();
    state.open.push(n.id[next as usize]);
    state.hover = first_row(tables, strings, next);
    debug_assert_eq!(state.open.len(), 1);
}

/// Focuses text field at layout row `r` and places the caret under `(x, y)`.
#[allow(clippy::too_many_arguments)]
fn focus_field(
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    text: &mut TextSystem,
    state: &mut UiState,
    r: usize,
    x: f32,
    y: f32,
) {
    let n = layout.node[r];
    let i = n as usize;
    let rect = layout.rect[r];
    let style =
        |p| crate::layout::row_style(tables, layout.title_style, n, Shown::Normal, p) as f32;
    let (pad, font) = (style(StyleProp::Pad), style(StyleProp::Font).max(8.0));
    let area = tables.nodes.kind[i] == Kind::TextArea;
    let top = if area { style(StyleProp::PadY) } else { 0.0 };
    let id = tables.nodes.id[i];
    if state.focus != id || state.editor.is_none() {
        let mut ed = PlainEditor::new(font);
        let family = FontFamily::from(text.family());
        ed.edit_styles()
            .insert(StyleProperty::FontFamily(family.into_owned()));
        if area {
            ed.set_width(Some((rect.w - 2.0 * pad).max(font)));
        }
        ed.set_text(strings.get(tables.nodes.prop(Prop::Value)[i]));
        state.editor = Some(ed);
        state.focus = id;
    }
    // A text area may be scrolled inside itself: the point is in its text's coordinates.
    let scrolled = if area { state.scroll_of(id) } else { 0.0 };
    let ed = state.editor.as_mut().expect("editor just created");
    let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
    d.move_to_point(x - rect.x - pad, y - rect.y - top + scrolled);
    d.refresh_layout();
}

/// Scrolls the focused text area inside itself so the caret stays in view (after an edit or
/// a caret move). Uses the area's last laid-out box.
fn follow_caret(tables: &UiTables, layout: &UiLayout, text: &TextSystem, state: &mut UiState) {
    let Some(n) = tables.nodes.find(state.focus) else {
        return;
    };
    debug_assert_eq!(tables.nodes.kind[n as usize], Kind::TextArea);
    let (Some(r), Some(caret)) = (
        layout.row_of(n),
        state.editor.as_ref().and_then(|e| e.cursor_geometry(1.0)),
    ) else {
        return;
    };
    let pad_y = crate::layout::row_style(
        tables,
        layout.title_style,
        n,
        Shown::Normal,
        StyleProp::PadY,
    ) as f32;
    let view = layout.rect[r].h - 2.0 * pad_y;
    let max = area_scroll_max(tables, layout, text, state, r);
    let now = state.scroll_of(state.focus);
    let (top, bottom) = (caret.y0 as f32, caret.y1 as f32);
    let want = if bottom > now + view {
        bottom - view
    } else if top < now {
        top
    } else {
        now
    };
    state.set_scroll(state.focus, want, max);
    debug_assert!(state.scroll_of(state.focus) <= max.max(0.0));
}

/// Applies an editing key to the focused field; Enter submits (Ctrl+Enter in a `text_area`,
/// where Enter breaks the line), Escape cancels. Sets `typed` when the key changed the text;
/// flushes the field's `input` command before the field loses focus.
fn edit_key(
    tables: &UiTables,
    text: &mut TextSystem,
    state: &mut UiState,
    chord: Chord,
    area: bool,
    typed: &mut bool,
    out: &mut Routed,
) {
    let mods = chord.mods().0;
    let shift = mods & Mods::SHIFT != 0;
    let word = mods & Mods::CTRL != 0;
    let key = chord.key();
    if area && key == Key::ENTER && !word {
        let ed = state.editor.as_mut().expect("caller checked editor");
        let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
        d.insert_or_replace_selection("\n");
        d.refresh_layout();
        *typed = true;
        return;
    }
    if area && (key == Key::UP || key == Key::DOWN) {
        let ed = state.editor.as_mut().expect("caller checked editor");
        let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
        match (key == Key::UP, shift) {
            (true, false) => d.move_up(),
            (true, true) => d.select_up(),
            (false, false) => d.move_down(),
            (false, true) => d.select_down(),
        }
        d.refresh_layout();
        return;
    }
    if key == Key::ENTER || key == Key::ESCAPE {
        flush_typed(tables, state, typed, out);
        if key == Key::ENTER
            && let (Some(n), Some(ed)) = (tables.nodes.find(state.focus), state.editor.as_ref())
        {
            let cmd = tables.nodes.prop(Prop::Command)[n as usize];
            if !cmd.is_none() {
                out.commands.push(cmd, state.focus, ed.raw_text());
            }
        }
        state.blur();
        return;
    }
    let ed = state.editor.as_mut().expect("caller checked editor");
    let mut d = ed.driver(&mut text.font_cx, &mut text.layout_cx);
    match key {
        Key::LEFT if shift => d.select_left(),
        Key::LEFT if word => d.move_word_left(),
        Key::LEFT => d.move_left(),
        Key::RIGHT if shift => d.select_right(),
        Key::RIGHT if word => d.move_word_right(),
        Key::RIGHT => d.move_right(),
        Key::HOME if shift => d.select_to_line_start(),
        Key::HOME => d.move_to_line_start(),
        Key::END if shift => d.select_to_line_end(),
        Key::END => d.move_to_line_end(),
        Key::BACKSPACE if word => d.backdelete_word(),
        Key::BACKSPACE => d.backdelete(),
        Key::DELETE if word => d.delete_word(),
        Key::DELETE => d.delete(),
        k if k == Key('a' as u32) && word => d.select_all(),
        _ => {} // printable keys arrive as Text events
    }
    d.refresh_layout();
    *typed |= matches!(key, Key::BACKSPACE | Key::DELETE);
}
