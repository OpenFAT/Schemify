//! The humble view (D4 as amended 2026-10-30, D6): the composed UI tables drawn as egui
//! widgets, once per frame. It reads the tables, writes only [`UiState`] (dialog, edit buffer,
//! scroll and split), the frame's [`UiLayout`] and [`Routed`] (commands, canvas events), and
//! paints through egui. Styles stay plugin data: every row is painted from its style props by
//! [`row`], so a plugin restyles any element; egui supplies layout, hit testing, focus, text,
//! popups, scrolling and accessibility.
//!
//! Containers follow the records' flex props: `dir` picks the axis, `gap` the spacing, a child
//! with `grow` takes the free space left by its siblings' sizes in the previous frame (egui is
//! single-pass; a size that changed asks for a second pass), `width`/`height` fix a size, a
//! column stretches its children across, a row centres them unless `stretch` is set.
//!
//! Contents:
//! - `pub fn show(ui, tables, strings, state, layout, out)`
//! - crate-only `View`: the per-frame walk shared by [`row`], [`field`], [`prose`]

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Margin, Order, Rect as ERect,
    Sense, Stroke, Ui, UiBuilder, Vec2, pos2, vec2,
};

use crate::input::Routed;
use crate::kinds::{Kind, Prop, StyleProp};
use crate::layout::{FLOAT_INSET, NO_CLIP, Rect, SPLIT_GAP, SPLIT_MIN, Slot, UiLayout};
use crate::state::UiState;
use crate::strings::{Strings, Sym};
use crate::tables::{NO_NODE, NO_STYLE, NodeIx, UiTables};

mod events;
mod field;
mod paths;
mod prose;
mod row;

pub(crate) use paths::paint_paths;
pub use events::pointer_events;
pub use row::{DISABLED_ALPHA, row_style};

/// Font size when no style sets one.
const FONT_PX: f32 = 13.0;

/// Draws the window tree of `tables` into `ui`'s rectangle and the open dialog above it, then
/// routes the frame's keys through the keymap. `layout` is rebuilt; this frame's commands and
/// canvas pointer events are appended to `out`, which the caller clears per frame (egui may run
/// a frame's view twice, and only the first pass sees the input).
///
/// The canvas and graph nodes leave a reserved shape ([`Slot`] in `layout`) for the host to
/// paint after the view, so the schematic sits under the floats and popups.
pub fn show(
    ui: &mut Ui,
    tables: &UiTables,
    strings: &Strings,
    state: &mut UiState,
    layout: &mut UiLayout,
    out: &mut Routed,
) {
    layout.clear();
    let title = strings
        .find("menu-title")
        .map_or(NO_STYLE, |s| tables.styles.row(s));
    let mut v = View {
        t: tables,
        s: strings,
        st: state,
        lay: layout,
        out,
        clip: NO_CLIP,
        title_style: title,
    };
    // Keys typed this frame belong to the field focused when it began (Enter may blur it).
    let typing = ui.ctx().egui_wants_keyboard_input();
    let n = &tables.nodes;
    let root = (0..n.len()).find(|&i| n.kind[i] == Kind::Window && n.parent[i] == NO_NODE);
    if let Some(root) = root {
        let rect = ui.max_rect();
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)));
        v.sized(&mut child, root as NodeIx, rect.size(), true, true, false, None);
    }
    v.dialog(ui.ctx());
    events::keys(&mut v, ui.ctx(), typing);
    debug_assert_eq!(v.lay.len(), v.lay.rect().len(), "layout columns in step");
}

/// One frame's walk over the tables.
pub(crate) struct View<'a> {
    pub(crate) t: &'a UiTables,
    pub(crate) s: &'a Strings,
    pub(crate) st: &'a mut UiState,
    pub(crate) lay: &'a mut UiLayout,
    pub(crate) out: &'a mut Routed,
    /// The innermost scroll viewport the current node is drawn in.
    pub(crate) clip: Rect,
    /// The `menu-title` selector's row.
    pub(crate) title_style: u16,
}

impl View<'_> {
    /// Style prop `p` of node `n` as its kind and class resolve it.
    pub(crate) fn style(&self, n: NodeIx, p: StyleProp) -> u32 {
        let i = usize::from(n);
        debug_assert!(i < self.t.nodes.len(), "node index out of range");
        self.t.styles.get(self.t.nodes.class[i], self.t.nodes.kind[i], p)
    }

    /// Length style `p` of `n` in px.
    pub(crate) fn px(&self, n: NodeIx, p: StyleProp) -> f32 {
        self.style(n, p) as f32
    }

    /// Font size of `n` in px.
    pub(crate) fn font(&self, n: NodeIx) -> f32 {
        match self.style(n, StyleProp::Font) {
            0 => FONT_PX,
            v => v as f32,
        }
    }

    /// String prop `p` of `n` (`""` when unset).
    pub(crate) fn prop(&self, n: NodeIx, p: Prop) -> &str {
        self.s.get(self.t.nodes.props[p as usize][usize::from(n)])
    }

    pub(crate) fn id(&self, n: NodeIx) -> Sym {
        self.t.nodes.id[usize::from(n)]
    }

    pub(crate) fn kind(&self, n: NodeIx) -> Kind {
        self.t.nodes.kind[usize::from(n)]
    }

    /// egui id of node `n`, stable across recomposition (its record id).
    pub(crate) fn egui_id(&self, n: NodeIx) -> Id {
        Id::unique(("ui", self.id(n).raw()))
    }

    /// Records that `n` was shown at `r`.
    pub(crate) fn record(&mut self, n: NodeIx, r: ERect, slot: Option<Slot>) {
        self.lay.push(n, Rect::of(r), self.clip, slot);
    }

    /// Queues command prop `p` of `n` with `arg`, if `n` has one.
    pub(crate) fn run(&mut self, n: NodeIx, p: Prop, arg: &str) {
        let cmd = self.t.nodes.props[p as usize][usize::from(n)];
        if !cmd.is_none() {
            self.out.commands.push(cmd, self.id(n), arg);
        }
    }

    /// The frame of a container: background, border, corner radius, padding.
    pub(crate) fn frame(&self, n: NodeIx) -> egui::Frame {
        let border = self.px(n, StyleProp::Border);
        egui::Frame::new()
            .fill(color(self.style(n, StyleProp::Bg)))
            .stroke(Stroke::new(border, color(self.style(n, StyleProp::BorderColor))))
            .corner_radius(CornerRadius::same(self.style(n, StyleProp::Radius).min(255) as u8))
            .inner_margin(Margin::symmetric(
                self.style(n, StyleProp::Pad).min(127) as i8,
                self.style(n, StyleProp::PadY).min(127) as i8,
            ))
    }

    /// Draws one node by kind.
    pub(crate) fn node(&mut self, ui: &mut Ui, n: NodeIx) {
        match self.kind(n) {
            Kind::Window
            | Kind::Panel
            | Kind::Toolbar
            | Kind::Tabs
            | Kind::List
            | Kind::Tree
            | Kind::Status
            | Kind::Dialog => self.container(ui, n),
            Kind::Menubar => row::menubar(self, ui, n),
            Kind::Scroll => self.scroll(ui, n),
            Kind::Split => self.split(ui, n),
            Kind::Canvas => self.canvas(ui, n),
            Kind::Graph => self.graph(ui, n),
            Kind::Label
            | Kind::Button
            | Kind::Item
            | Kind::Tab
            | Kind::Checkbox
            | Kind::Dropdown
            | Kind::Menu => row::widget(self, ui, n),
            Kind::Separator => row::separator(self, ui, n),
            Kind::TextInput | Kind::TextArea => field::field(self, ui, n),
            Kind::Math => prose::math_node(self, ui, n),
            Kind::Prose => prose::prose_node(self, ui, n),
        }
    }

    /// A flex container: its frame around its children laid along `dir`.
    pub(crate) fn container(&mut self, ui: &mut Ui, n: NodeIx) {
        let r = self.framed(ui, n, |v, ui| v.children(ui, n));
        self.record(n, r, None);
    }

    /// Runs `f` inside `n`'s frame, the frame filling the size the cell asked the node to fill
    /// (`ui`'s minimum). Returns the frame's rectangle.
    pub(crate) fn framed(&mut self, ui: &mut Ui, n: NodeIx, f: impl FnOnce(&mut Self, &mut Ui)) -> ERect {
        let frame = self.frame(n);
        let fill = (ui.min_size() - frame.total_margin().sum()).max(Vec2::ZERO);
        let cell = ui.max_rect();
        let r = frame
            .show(ui, |ui| {
                ui.set_min_size(fill);
                f(self, ui);
            })
            .response
            .rect;
        // What shows of an overflowing container is its cell.
        r.intersect(cell)
    }

    /// The children of `n` along its axis: each in its own clipped cell, a grown child's cell
    /// sized from the free space its later siblings left in the previous frame.
    pub(crate) fn children(&mut self, ui: &mut Ui, n: NodeIx) {
        let row = self.style(n, StyleProp::Dir) == 1;
        let stretch = self.style(n, StyleProp::Stretch) == 1;
        let gap = self.px(n, StyleProp::Gap);
        let kids: Vec<NodeIx> = self
            .t
            .nodes
            .children(n)
            .filter(|&c| self.kind(c) != Kind::Dialog)
            .collect();
        let layout = if row { Layout::left_to_right(Align::Min) } else { Layout::top_down(Align::Min) };
        // A row centres its children across when its height is known (fixed or filled).
        let cross = (row && !stretch && ui.min_size().y > 0.0).then(|| ui.min_size().y);
        ui.with_layout(layout, |ui| {
            ui.spacing_mut().item_spacing = if row { vec2(gap, 0.0) } else { vec2(0.0, gap) };
            let grow: Vec<f32> = kids.iter().map(|&c| self.px(c, StyleProp::Grow)).collect();
            for (k, &c) in kids.iter().enumerate() {
                let avail = ui.available_rect_before_wrap().size();
                let share = (grow[k] > 0.0).then(|| {
                    let later: f32 = kids[k + 1..]
                        .iter()
                        .zip(&grow[k + 1..])
                        .filter(|(_, g)| **g == 0.0)
                        .map(|(&s, _)| self.last_size(ui, s, row) + gap)
                        .sum();
                    let main = if row { avail.x } else { avail.y };
                    ((main - later) * grow[k] / grow[k..].iter().sum::<f32>()).max(0.0)
                });
                let size = match (row, share) {
                    (true, Some(w)) => vec2(w, avail.y),
                    (false, Some(h)) => vec2(avail.x, h),
                    _ => avail,
                };
                let rect = self.sized(ui, c, size, share.is_some(), !row || stretch, row, cross);
                self.keep_size(ui, c, row, if row { rect.width() } else { rect.height() });
            }
        });
    }

    /// Draws `c` in a cell of at most `size` at the cursor, clipped to it: filled along the
    /// main axis when `grown`, across when `fill_cross`, centred in a `center`-tall row.
    /// Returns the rectangle it took (never larger than the cell, so an overflowing row stays
    /// inside its parent).
    #[expect(clippy::too_many_arguments, reason = "one placement, read once per child")]
    fn sized(
        &mut self,
        ui: &mut Ui,
        c: NodeIx,
        size: Vec2,
        grown: bool,
        fill_cross: bool,
        row: bool,
        center: Option<f32>,
    ) -> ERect {
        // A grown cell's main size (free space, a dragged split) wins over a fixed style.
        let mut fixed = vec2(self.px(c, StyleProp::Width), self.px(c, StyleProp::Height));
        if grown {
            *if row { &mut fixed.x } else { &mut fixed.y } = 0.0;
        }
        let cell = vec2(
            if fixed.x > 0.0 { fixed.x } else { size.x.max(0.0) },
            if fixed.y > 0.0 { fixed.y } else { size.y.max(0.0) },
        );
        let (fill_main, fill_x) = (grown, if row { grown } else { fill_cross });
        let fill_y = if row { fill_cross } else { fill_main };
        let mut at = ui.available_rect_before_wrap().min;
        if let Some(h) = center {
            let last = ui.data(|d| d.get_temp::<f32>(self.egui_id(c).with("cross")).unwrap_or(h));
            at.y += ((h - last) / 2.0).max(0.0);
        }
        let rect = ERect::from_min_size(at, cell);
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)));
        child.shrink_clip_rect(rect);
        if fill_x || fixed.x > 0.0 {
            child.set_min_width(cell.x);
        }
        if fill_y || fixed.y > 0.0 {
            child.set_min_height(cell.y);
        }
        child.set_min_width(self.px(c, StyleProp::MinWidth).min(cell.x));
        self.node(&mut child, c);
        let used = child.min_rect().intersect(rect);
        if center.is_some() {
            ui.data_mut(|d| d.insert_temp(self.egui_id(c).with("cross"), used.height()));
        }
        ui.advance_cursor_after_rect(used);
        used
    }

    /// `c`'s main-axis size in the previous frame (0 the first time).
    fn last_size(&self, ui: &Ui, c: NodeIx, row: bool) -> f32 {
        let fixed = self.px(c, if row { StyleProp::Width } else { StyleProp::Height });
        if fixed > 0.0 {
            return fixed;
        }
        ui.data(|d| d.get_temp::<f32>(self.egui_id(c).with(row)).unwrap_or(0.0))
    }

    /// Remembers `c`'s size for the next frame's free space; a change asks for another pass.
    fn keep_size(&self, ui: &Ui, c: NodeIx, row: bool, size: f32) {
        let key = self.egui_id(c).with(row);
        let old = ui.data_mut(|d| d.insert_temp(key, size));
        let _ = old;
        let before = ui.data(|d| d.get_temp::<f32>(key.with("prev")));
        if before.is_none_or(|b| (b - size).abs() > 0.5) {
            ui.data_mut(|d| d.insert_temp(key.with("prev"), size));
            if before.is_some() {
                ui.ctx().request_discard("a flex child changed size");
            }
        }
    }

    /// A `scroll` node: its children in an egui scroll area, the offset kept in [`UiState`].
    fn scroll(&mut self, ui: &mut Ui, n: NodeIx) {
        let id = self.id(n);
        let mut area = egui::ScrollArea::vertical()
            .id_salt(self.egui_id(n))
            .auto_shrink(false);
        if let Some(o) = self.st.take_scroll_request(id) {
            area = area.vertical_scroll_offset(o);
        }
        let outer_clip = self.clip;
        let mut scrolled = (0.0, 0.0);
        let r = self.framed(ui, n, |v, ui| {
            let o = area.show(ui, |ui| {
                v.clip = Rect::of(ui.clip_rect()).intersect(&outer_clip);
                v.children(ui, n);
            });
            scrolled = (o.state.clamped_offset().y, (o.content_size.y - o.inner_rect.height()).max(0.0));
        });
        self.clip = outer_clip;
        let (offset, max) = scrolled;
        self.st.keep_scroll(id, offset, max);
        self.record(n, r, None);
        self.lay.set_scroll_max(n, max);
    }

    /// A `split` node: the first child takes the rest, the second the dragged size.
    fn split(&mut self, ui: &mut Ui, n: NodeIx) {
        let row = self.style(n, StyleProp::Dir) == 1;
        let gap = self.px(n, StyleProp::Gap).max(SPLIT_GAP);
        let mut kids = self.t.nodes.children(n);
        let (Some(a), b) = (kids.next(), kids.next()) else {
            self.container(ui, n);
            return;
        };
        let rect = ui.available_rect_before_wrap();
        let total = if row { rect.width() } else { rect.height() };
        let second = b.map_or(0.0, |b| {
            let natural = self.last_size(ui, b, row);
            let want = self.st.split_of(self.id(n)).unwrap_or(natural);
            want.clamp(SPLIT_MIN.min(total), (total - gap - SPLIT_MIN).max(SPLIT_MIN.min(total)))
        });
        let first = (total - second - if b.is_some() { gap } else { 0.0 }).max(0.0);
        let frame_r = self.frame(n).show(ui, |ui| {
            let layout = if row { Layout::left_to_right(Align::Min) } else { Layout::top_down(Align::Min) };
            ui.with_layout(layout, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let cross = if row { rect.height() } else { rect.width() };
                let dims = |m: f32| if row { vec2(m, cross) } else { vec2(cross, m) };
                self.sized(ui, a, dims(first), true, true, row, None);
                let Some(b) = b else { return };
                let (div, resp) = ui.allocate_exact_size(dims(gap), Sense::drag());
                if resp.hovered() || resp.dragged() {
                    ui.ctx().set_cursor_icon(if row {
                        egui::CursorIcon::ResizeHorizontal
                    } else {
                        egui::CursorIcon::ResizeVertical
                    });
                }
                if resp.dragged() {
                    let d = if row { resp.drag_delta().x } else { resp.drag_delta().y };
                    let px = (second - d).clamp(SPLIT_MIN, (total - gap - SPLIT_MIN).max(SPLIT_MIN));
                    self.st.set_split(self.id(n), px);
                }
                debug_assert!(div.width() >= 0.0);
                let r = self.sized(ui, b, dims(second), true, true, row, None);
                self.keep_size(ui, b, row, if row { r.width() } else { r.height() });
            });
        });
        self.record(n, frame_r.response.rect, None);
    }

    /// The `canvas` node: a reserved shape for the host's schematic, its pointer events, and
    /// its children as floats centred along its top edge.
    fn canvas(&mut self, ui: &mut Ui, n: NodeIx) {
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let bg = self.style(n, StyleProp::Bg);
        ui.painter().rect_filled(rect, 0.0, color(bg));
        let slot = Slot {
            layer: ui.layer_id(),
            shape: ui.painter().add(egui::Shape::Noop),
        };
        self.record(n, rect, Some(slot));
        events::canvas(self, ui.ctx(), rect, ui.layer_id());
        let kids: Vec<NodeIx> = self.t.nodes.children(n).collect();
        for c in kids {
            egui::Area::new(self.egui_id(c))
                .order(Order::Middle)
                .pivot(Align2::CENTER_TOP)
                .fixed_pos(pos2(rect.center().x, rect.top() + FLOAT_INSET))
                .constrain_to(rect)
                .show(ui.ctx(), |ui| self.node(ui, c));
        }
    }

    /// A `graph` node: a reserved shape the host fills with the plot.
    fn graph(&mut self, ui: &mut Ui, n: NodeIx) {
        let size = vec2(ui.available_width(), self.px(n, StyleProp::Height).max(120.0));
        let (rect, _) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let slot = Slot {
            layer: ui.layer_id(),
            shape: ui.painter().add(egui::Shape::Noop),
        };
        self.record(n, rect, Some(slot));
    }

    /// The open dialog as a modal: centred, or at the point it was opened at (a context menu,
    /// which closes on a click outside it). Escape closes either.
    fn dialog(&mut self, ctx: &egui::Context) {
        let d = self.st.dialog;
        if d.is_none() {
            return;
        }
        let Some(n) = self.t.nodes.find(d) else {
            self.st.set_dialog(Sym::NONE);
            return;
        };
        let id = self.egui_id(n).with(self.st.dialog_serial);
        let at = self.st.dialog_at;
        let area = egui::Modal::default_area(id);
        let area = match at {
            Some((x, y)) => area.anchor(Align2::LEFT_TOP, vec2(x, y)).constrain(true),
            None => area,
        };
        let r = egui::Modal::new(id)
            .area(area)
            .frame(self.frame(n))
            .backdrop_color(Color32::TRANSPARENT)
            .show(ctx, |ui| {
                ui.set_min_width(self.px(n, StyleProp::MinWidth));
                self.children(ui, n);
                ui.min_rect()
            });
        self.record(n, r.response.rect, None);
        let escape = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let outside = at.is_some() && r.backdrop_response.clicked();
        if escape || outside {
            self.st.set_dialog(Sym::NONE);
        }
        debug_assert!(r.inner.width() >= 0.0);
    }
}

/// A style colour (RGBA8, red in the top byte) as egui's.
pub(crate) fn color(c: u32) -> Color32 {
    let [r, g, b, a] = c.to_be_bytes();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// The proportional UI font at `px`.
pub(crate) fn font(px: f32) -> FontId {
    FontId::proportional(px)
}

