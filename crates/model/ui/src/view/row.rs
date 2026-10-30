//! Clickable and static rows (labels, buttons, menu items, tabs, checkboxes, dropdowns, menu
//! titles and cascades), each painted from its style props, plus the menubar and the popups
//! menus and dropdowns open.
//!
//! Contents: `pub const DISABLED_ALPHA`, `pub fn row_style`; crate-only `widget`, `menubar`,
//! `separator`

use egui::{Id, Pos2, Response, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};

use super::{View, color, font, paint_paths};
use crate::icon;
use crate::kinds::{Kind, Prop, StyleProp};
use crate::tables::{NO_STYLE, NodeIx, UiTables};

/// Alpha a disabled row's text keeps, out of 256.
pub const DISABLED_ALPHA: u32 = 102;

/// How a node shows in a row. A menu appears as a title or cascade row *and*, when open, as a
/// popup panel; the other kinds always show as themselves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Shown {
    /// The node's own kind.
    Normal,
    /// A menu's title in a menubar (selector `menu-title`, then `item`).
    Title,
    /// A menu as a row inside another menu's popup (styled like `item`, with an arrow).
    Cascade,
}

/// Style prop `prop` of `node` as shown: a menu title by `menu-title` (`title_style`), a cascade
/// like an `item`, otherwise its own kind and class.
///
/// # Panics
///
/// Panics if `node` is not a node index.
#[must_use]
pub fn row_style(tables: &UiTables, title_style: u16, node: NodeIx, cascade: bool, title: bool, prop: StyleProp) -> u32 {
    let n = &tables.nodes;
    let i = usize::from(node);
    assert!(i < n.len(), "node index out of range");
    let class = n.class[i];
    if title {
        let c = if class != NO_STYLE { class } else { title_style };
        return tables.styles.get(c, Kind::Item, prop);
    }
    if cascade {
        return tables.styles.get(class, Kind::Item, prop);
    }
    tables.styles.get(class, n.kind[i], prop)
}

/// A label, button, item, tab, checkbox, dropdown or stray menu, by kind.
pub(crate) fn widget(v: &mut View, ui: &mut Ui, n: NodeIx) {
    match v.kind(n) {
        Kind::Dropdown if v.t.nodes.children(n).next().is_some() => {
            let open = egui::Popup::is_id_open(ui.ctx(), popup_id(v, n));
            let r = row(v, ui, n, Shown::Normal, open);
            popup(v, ui, n, &r);
        }
        Kind::Menu => {
            let open = egui::Popup::is_id_open(ui.ctx(), popup_id(v, n));
            let r = row(v, ui, n, Shown::Cascade, open);
            popup(v, ui, n, &r);
        }
        Kind::Label => {
            row(v, ui, n, Shown::Normal, false);
        }
        kind => {
            let active = kind == Kind::Tab && v.t.nodes.is_checked(v.s, n);
            let r = row(v, ui, n, Shown::Normal, active);
            if r.clicked() {
                v.run(n, Prop::Command, "");
                if kind == Kind::Item {
                    ui.close();
                }
            }
        }
    }
}

/// The menubar: its frame around a row of menu titles (each opening its popup; hovering
/// another title while one is open switches to it) and whatever else it holds.
pub(crate) fn menubar(v: &mut View, ui: &mut Ui, n: NodeIx) {
    let bar = v.egui_id(n).with("open");
    let r = v.framed(ui, n, |v, ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(v.px(n, StyleProp::Gap), 0.0);
            let kids: Vec<NodeIx> = v.t.nodes.children(n).collect();
            for c in kids {
                if v.kind(c) != Kind::Menu {
                    v.node(ui, c);
                    continue;
                }
                let id = popup_id(v, c);
                let open = egui::Popup::is_id_open(ui.ctx(), id);
                let r = row(v, ui, c, Shown::Title, open);
                let other = ui.data(|d| d.get_temp::<Id>(bar));
                if r.hovered()
                    && other.is_some_and(|o| o != id && egui::Popup::is_id_open(ui.ctx(), o))
                {
                    egui::Popup::open_id(ui.ctx(), id);
                }
                if popup(v, ui, c, &r) {
                    ui.data_mut(|d| d.insert_temp(bar, id));
                }
            }
        });
    });
    v.record(n, r, None);
}

/// A menu separator: a rule across the row, `height` tall.
pub(crate) fn separator(v: &mut View, ui: &mut Ui, n: NodeIx) {
    let h = v.px(n, StyleProp::Height).max(1.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let y = rect.center().y;
    let fg = color(v.style(n, StyleProp::Fg));
    ui.painter().hline(rect.x_range(), y, Stroke::new(1.0, fg));
    v.record(n, rect, None);
}

// ---- private ----

fn popup_id(v: &View, n: NodeIx) -> Id {
    v.egui_id(n).with("popup")
}

/// The popup of menu or dropdown `n` under `r`, styled by the `menu` selector. Returns whether
/// it is open.
fn popup(v: &mut View, ui: &Ui, n: NodeIx, r: &Response) -> bool {
    let menu = |p| v.t.styles.get(NO_STYLE, Kind::Menu, p);
    let (bg, border, border_c, pad_y, min_w) = (
        color(menu(StyleProp::Bg)),
        menu(StyleProp::Border) as f32,
        color(menu(StyleProp::BorderColor)),
        menu(StyleProp::PadY) as f32,
        menu(StyleProp::MinWidth) as f32,
    );
    let frame = egui::Frame::new()
        .fill(bg)
        .stroke(Stroke::new(border, border_c))
        .inner_margin(egui::Margin::symmetric(0, pad_y.min(127.0) as i8));
    let cascade = v.kind(n) == Kind::Menu && v.t.nodes.parent[usize::from(n)] != crate::tables::NO_NODE
        && v.kind(v.t.nodes.parent[usize::from(n)]) == Kind::Menu;
    let shown = if cascade {
        let id = popup_id(v, n);
        if r.hovered() {
            egui::Popup::open_id(ui.ctx(), id);
        }
        egui::Popup::from_response(r)
            .id(id)
            .kind(egui::PopupKind::Menu)
            .align(egui::RectAlign::RIGHT_START)
            .frame(frame)
            .gap(0.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| body(v, ui, n, min_w))
    } else {
        egui::Popup::menu(r)
            .id(popup_id(v, n))
            .frame(frame)
            .show(|ui| body(v, ui, n, min_w))
    };
    shown.is_some()
}

/// The rows of an open popup: items, separators and cascades.
fn body(v: &mut View, ui: &mut Ui, n: NodeIx, min_w: f32) {
    ui.set_min_width(min_w);
    ui.spacing_mut().item_spacing = Vec2::ZERO;
    let kids: Vec<NodeIx> = v.t.nodes.children(n).collect();
    for c in kids {
        v.node(ui, c);
    }
}

/// Paints one row of node `n` from its styles and returns its response (clicks only when it
/// is enabled and clickable).
fn row(v: &mut View, ui: &mut Ui, n: NodeIx, shown: Shown, active: bool) -> Response {
    let kind = v.kind(n);
    let (title, cascade) = (shown == Shown::Title, shown == Shown::Cascade);
    let style = |p| row_style(v.t, v.title_style, n, cascade, title, p);
    let px = match style(StyleProp::Font) {
        0 => v.font(n),
        f => f as f32,
    };
    let (pad, pad_y, gap) = (
        style(StyleProp::Pad) as f32,
        style(StyleProp::PadY) as f32,
        style(StyleProp::Gap) as f32,
    );
    let label = v.prop(n, Prop::Label).to_owned();
    let accel = if matches!(kind, Kind::Item) { v.prop(n, Prop::Accel).to_owned() } else { String::new() };
    let checkable = !v.t.nodes.props[Prop::Checked as usize][usize::from(n)].is_none()
        && matches!(kind, Kind::Item | Kind::Checkbox);
    let checked = v.t.nodes.is_checked(v.s, n);
    let disabled = v.t.nodes.is_disabled(v.s, n);
    let arrow = cascade || kind == Kind::Dropdown;
    let icon = (kind == Kind::Button).then(|| icon::parse(v.prop(n, Prop::Icon))).flatten();
    let painter = ui.painter().clone();
    let text = painter.layout_no_wrap(label.clone(), font(px), egui::Color32::PLACEHOLDER);
    let acc = painter.layout_no_wrap(accel.clone(), font(px), egui::Color32::PLACEHOLDER);
    let check_w = if checkable { px * 1.2 } else { 0.0 };
    let arrow_w = if arrow { px } else { 0.0 };
    let label_w = if icon.is_some() { 16.0 } else { text.size().x };
    let acc_w = if accel.is_empty() { 0.0 } else { gap + acc.size().x };
    let content = vec2(pad * 2.0 + check_w + label_w + acc_w + arrow_w, text.size().y.max(px) + pad_y * 2.0);
    let fixed = vec2(style(StyleProp::Width) as f32, style(StyleProp::Height) as f32);
    let mut size = vec2(
        if fixed.x > 0.0 { fixed.x } else { content.x.max(style(StyleProp::MinWidth) as f32) },
        if fixed.y > 0.0 { fixed.y } else { content.y },
    );
    // Across a column cell (its minimum: nothing is placed in it yet) or a justified popup,
    // like a flex column's stretch; in a row the minimum is what the row already holds.
    if ui.layout().is_vertical() {
        size.x = size.x.max(ui.min_size().x);
    }
    if ui.layout().horizontal_justify() {
        size.x = size.x.max(ui.available_width());
    }
    let clickable = !disabled && kind != Kind::Label;
    let (rect, r) = ui.allocate_exact_size(size, if clickable { Sense::click() } else { Sense::hover() });
    let hovered = clickable && r.hovered();
    let pick = |base: StyleProp, hover: StyleProp, act: StyleProp| {
        let c = if active { style(act) } else if hovered { style(hover) } else { 0 };
        if c & 0xff != 0 { c } else { style(base) }
    };
    let bg = pick(StyleProp::Bg, StyleProp::HoverBg, StyleProp::ActiveBg);
    let mut fg = pick(StyleProp::Fg, StyleProp::HoverFg, StyleProp::ActiveFg);
    if disabled {
        fg = (fg & !0xff) | ((fg & 0xff) * DISABLED_ALPHA / 256);
    }
    let radius = style(StyleProp::Radius).min(255) as u8;
    if bg & 0xff != 0 {
        painter.rect_filled(rect, radius, color(bg));
    }
    let border = style(StyleProp::Border) as f32;
    if border > 0.0 {
        let s = Stroke::new(border, color(style(StyleProp::BorderColor)));
        painter.rect_stroke(rect, radius, s, egui::StrokeKind::Inside);
    }
    let fgc = color(fg);
    let mut x = rect.left() + pad;
    let mid = rect.center().y;
    if checkable && checked {
        let s = px * 0.35;
        let c = pos2(x + s, mid);
        let pts = vec![pos2(c.x - s, c.y), pos2(c.x - s * 0.3, c.y + s * 0.7), pos2(c.x + s, c.y - s * 0.8)];
        painter.add(Shape::line(pts, Stroke::new(1.5, fgc)));
    }
    x += check_w;
    match &icon {
        Some(icon) => {
            let at = Pos2::new(x, mid - 8.0);
            paint_paths(&painter, &icon.fill, &icon.stroke, at, 1.0, icon::ICON_STROKE, fgc);
        }
        None => {
            painter.galley(pos2(x, mid - text.size().y / 2.0), text.clone(), fgc);
        }
    }
    if !accel.is_empty() {
        let ac = color(style(StyleProp::AccelFg));
        let ac = if ac.a() == 0 { fgc } else { ac };
        let ax = rect.right() - pad - arrow_w - acc.size().x;
        painter.galley(pos2(ax, mid - acc.size().y / 2.0), acc, ac);
    }
    if arrow {
        let (ax, s) = (rect.right() - pad - px * 0.5, px * 0.25);
        let pts = if cascade {
            vec![pos2(ax - s * 0.5, mid - s), pos2(ax + s * 0.5, mid), pos2(ax - s * 0.5, mid + s)]
        } else {
            vec![pos2(ax - s, mid - s * 0.5), pos2(ax + s, mid - s * 0.5), pos2(ax, mid + s * 0.5)]
        };
        painter.add(Shape::convex_polygon(pts, fgc, Stroke::NONE));
    }
    let wt = match kind {
        Kind::Checkbox => egui::Role::CheckBox,
        Kind::Label => egui::Role::Label,
        Kind::Dropdown => egui::Role::ComboBox,
        _ => egui::Role::Button,
    };
    r.widget_info(|| egui::WidgetInfo::selected(wt, !disabled, checked, &label));
    v.record(n, rect, None);
    let tip = v.prop(n, Prop::Tooltip).to_owned();
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}
