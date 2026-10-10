//! The AccessKit tree: one transform from the composed tables and the frame's layout.
//!
//! Node ids are the interned public ids ([`Sym::raw`]), so they are stable across frames and
//! recompositions; only visible rows are exported (a closed menu's items are not).

use std::fmt::Write as _;

use accesskit::{Action, Node, NodeId, Role, Toggled, TreeInfo, TreeUpdate};

use crate::input::UiState;
use crate::layout::{Shown, UiLayout};
use crate::strings::{Strings, Sym};
use crate::tables::{Kind, Prop, UiTables};

fn role(kind: Kind, shown: Shown) -> Role {
    match (kind, shown) {
        (_, Shown::Title | Shown::Cascade) => Role::MenuItem,
        (_, Shown::Popup) => Role::Menu,
        (Kind::Window, _) => Role::Window,
        (Kind::Menubar, _) => Role::MenuBar,
        (Kind::Menu, _) => Role::Menu,
        (Kind::Item, _) => Role::MenuItem,
        (Kind::Separator, _) => Role::GenericContainer,
        (Kind::Toolbar, _) => Role::Toolbar,
        (Kind::Button, _) => Role::Button,
        (Kind::Tabs, _) => Role::TabList,
        (Kind::Tab, _) => Role::Tab,
        (Kind::Panel, _) => Role::Pane,
        (Kind::Split, _) => Role::Splitter,
        (Kind::Label, _) => Role::Label,
        (Kind::TextInput, _) => Role::TextInput,
        (Kind::Checkbox, _) => Role::CheckBox,
        (Kind::Dropdown, _) => Role::ComboBox,
        (Kind::List, _) => Role::List,
        (Kind::Tree, _) => Role::Tree,
        (Kind::Scroll, _) => Role::ScrollView,
        (Kind::Canvas, _) => Role::Canvas,
        (Kind::Graph, _) => Role::Figure,
        (Kind::Math, _) => Role::Math,
        (Kind::Dialog, _) => Role::Dialog,
        (Kind::Status, _) => Role::Status,
    }
}

/// AccessKit node id of public node id `id`; a menu's popup gets a distinct id (high bit set).
pub fn access_id(id: Sym, shown: Shown) -> NodeId {
    let popup = if shown == Shown::Popup { 1u64 << 32 } else { 0 };
    NodeId(u64::from(id.raw()) | popup)
}

/// Public node id behind an AccessKit id from [`access_id`] (a popup maps to its menu).
pub fn sym_of(id: NodeId) -> Sym {
    Sym((id.0 & 0xffff_ffff) as u32)
}

/// Builds a full tree update from the visible rows. The window root is the tree root; open popups
/// are children of their menu row and the open dialog is a child of the root.
///
/// # Panics
///
/// Panics if `layout` was computed from different tables (row nodes out of range).
pub fn access_tree(
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    state: &UiState,
) -> TreeUpdate {
    let n = &tables.nodes;
    let rows = layout.len();
    assert!(
        layout.node.iter().all(|&i| (i as usize) < n.len()),
        "layout from other tables"
    );
    let mut built: Vec<(NodeId, Node)> = Vec::with_capacity(rows + 1);
    // Parent row of each row: the nearest earlier row showing the node's parent (pre-order), or
    // for a popup, the row that shows its menu.
    // ponytail: O(rows²) backward scans; fine for a few hundred visible rows, index by node if a
    // view ever shows thousands.
    let parent_row: Vec<Option<usize>> = (0..rows)
        .map(|r| {
            let node = layout.node[r];
            if layout.shown[r] == Shown::Popup {
                (0..r)
                    .rev()
                    .find(|&q| layout.node[q] == node && layout.shown[q] != Shown::Popup)
            } else {
                let p = n.parent[node as usize];
                (0..r).rev().find(|&q| {
                    layout.node[q] == p && matches!(layout.shown[q], Shown::Normal | Shown::Popup)
                })
            }
        })
        .collect();
    let root_row = (0..rows)
        .find(|&r| parent_row[r].is_none() && n.kind[layout.node[r] as usize] == Kind::Window);
    // Orphan rows (the dialog) hang off the root.
    let mut kids: Vec<Vec<NodeId>> = vec![Vec::new(); rows];
    for (q, parent) in parent_row.iter().enumerate() {
        let p = parent.or(root_row.filter(|&w| w != q));
        if let Some(p) = p {
            kids[p].push(access_id(n.id[layout.node[q] as usize], layout.shown[q]));
        }
    }
    for (r, kids) in kids.into_iter().enumerate() {
        let i = layout.node[r] as usize;
        let shown = layout.shown[r];
        let mut node = Node::new(role(n.kind[i], shown));
        let label = n.prop(Prop::Label)[i];
        if !label.is_none() {
            node.set_label(strings.get(label));
        }
        let rect = layout.rect[r];
        node.set_bounds(accesskit::Rect {
            x0: f64::from(rect.x),
            y0: f64::from(rect.y),
            x1: f64::from(rect.x + rect.w),
            y1: f64::from(rect.y + rect.h),
        });
        let accel = n.prop(Prop::Accel)[i];
        if !accel.is_none() {
            node.set_keyboard_shortcut(strings.get(accel));
        }
        if !n.prop(Prop::Checked)[i].is_none() {
            node.set_toggled(Toggled::from(n.is_checked(strings, i as u16)));
        }
        let value = n.prop(Prop::Value)[i];
        if !value.is_none() {
            node.set_value(strings.get(value));
        }
        if n.is_disabled(strings, i as u16) {
            node.set_disabled();
        }
        match shown {
            Shown::Title | Shown::Cascade => {
                node.set_has_popup(accesskit::HasPopup::Menu);
                node.set_expanded(state.open.contains(&n.id[i]));
                node.add_action(Action::Click);
            }
            _ if !n.prop(Prop::Command)[i].is_none() => node.add_action(Action::Click),
            _ => {}
        }
        if n.kind[i] == Kind::TextInput {
            node.add_action(Action::Focus);
        }
        node.set_children(kids);
        built.push((access_id(n.id[i], shown), node));
    }
    let root = root_row.map_or(NodeId(0), |r| {
        access_id(n.id[layout.node[r] as usize], Shown::Normal)
    });
    if root_row.is_none() {
        built.push((root, Node::new(Role::Window)));
    }
    let focus = n
        .find(state.focus)
        .and_then(|f| layout.row_of(f))
        .map_or(root, |r| {
            access_id(n.id[layout.node[r] as usize], layout.shown[r])
        });
    TreeUpdate {
        nodes: built,
        tree: Some(TreeInfo::new(root)),
        tree_id: accesskit::TreeId::ROOT,
        focus,
    }
}

/// Deterministic text rendering of a tree update for snapshot tests: one line per node,
/// indented by depth, `Role "label" [x0 y0 x1 y1]` plus toggled/expanded/shortcut markers.
pub fn dump(update: &TreeUpdate) -> String {
    let mut out = String::new();
    let Some(info) = &update.tree else { return out };
    let find = |id: NodeId| update.nodes.iter().find(|(i, _)| *i == id).map(|(_, n)| n);
    // Explicit stack (depth, id): bounded by the node count since every id appears once.
    let mut stack = vec![(0usize, info.root)];
    let mut seen = 0usize;
    while let Some((depth, id)) = stack.pop() {
        seen += 1;
        assert!(seen <= update.nodes.len(), "cycle in access tree");
        let Some(node) = find(id) else { continue };
        let _ = write!(out, "{:indent$}{:?}", "", node.role(), indent = depth * 2);
        if let Some(l) = node.label() {
            let _ = write!(out, " {l:?}");
        }
        if let Some(b) = node.bounds() {
            let _ = write!(
                out,
                " [{} {} {} {}]",
                b.x0.round(),
                b.y0.round(),
                b.x1.round(),
                b.y1.round()
            );
        }
        if let Some(t) = node.toggled() {
            let _ = write!(out, " toggled={t:?}");
        }
        if let Some(e) = node.is_expanded() {
            let _ = write!(out, " expanded={e}");
        }
        if let Some(k) = node.keyboard_shortcut() {
            let _ = write!(out, " key={k:?}");
        }
        out.push('\n');
        stack.extend(node.children().iter().rev().map(|&c| (depth + 1, c)));
    }
    out
}
