//! UI-as-data tables (D4, D6): the edits a contribution holds and the composed tables
//! [`crate::compose`] writes from them. Only `compose` and `patch_last` write; everyone else
//! reads through the accessors.
//!
//! # Data design
//!
//! 1. **In → out.** Contributions (the core base plus one per plugin, each a list of [`Edit`]s
//!    parsed from line records) → composed `ui_nodes` ([`UiNodes`]), `ui_styles` ([`UiStyles`]),
//!    `keymap` ([`Keymap`]) and the [`Health`] report of overridden edits. Last contribution wins
//!    (D8).
//! 2. **How many.** The xschem shell is ~450 nodes, ~30 style selectors, ~150 key bindings.
//!    Capacities: [`UiNodes::CAPACITY`] nodes, 255 contributions (`u8` owner), 2^16 selectors.
//! 3. **How wide.** Node index `u16` ([`NO_NODE`] = `u16::MAX`); owner `u8` (0 = base);
//!    [`Kind`] and [`Prop`] `u8` enums; every string is a `u32` [`Sym`]; [`Chord`] packs a
//!    21-bit key plus a named-key range into 24 bits and 8 modifier bits; style values are `u32`
//!    (RGBA8 for colors, logical pixels for lengths).
//! 4. **Access.** Layout, hit testing and the AccessKit transform walk nodes in pre-order reading
//!    `kind`, `parent` and `end` together, then single prop columns (`label`, `command`). The
//!    keymap is binary-searched once per key press. Styles are looked up per (node, prop) while
//!    laying out and painting; all props of one selector are read together, so a selector row is
//!    an array (AoS) while nodes are SoA.
//! 5. **Lifetime.** Session. Composition is immediate-mode: the tables are rebuilt from scratch
//!    whenever any contribution changes (plugin-abi §1), so there is no diffing and no stale row.
//! 6. **Parallel.** No: a few hundred rows rebuilt rarely, and "last wins" is order-dependent by
//!    definition.
//!
//! Contents:
//! - `pub type NodeIx = u16`, `pub const NO_NODE`, `pub const NO_STYLE`
//! - `pub enum Edit`, `pub struct Contribution` (`CAPACITY`)
//! - `pub enum HealthKind`, `pub struct Health`: `len`, `is_empty`; crate-only `push`
//! - `pub struct UiNodes`: `CAPACITY`, `len`, `is_empty`, `id`, `kind`, `parent`, `end`,
//!   `owner`, `prop`, `find`, `children`, `is_checked`, `is_disabled`
//! - `pub struct UiStyles`: `row(Sym) -> u16`, `get(class, kind, prop) -> u32`
//! - `pub struct Keymap`: `lookup(Chord) -> Option<Sym>`, `chords`, `commands`
//! - `pub struct UiTables`

use std::collections::HashMap;

use crate::keys::Chord;
use crate::kinds::{Kind, Prop, StyleProp};
use crate::strings::{Strings, Sym};

/// Index of a node in [`UiNodes`]; valid until the next [`crate::compose`].
pub type NodeIx = u16;

/// "No node" sentinel for [`NodeIx`] columns (parent of a root, empty focus, …).
pub const NO_NODE: NodeIx = u16::MAX;

/// "No selector" sentinel for style rows.
pub const NO_STYLE: u16 = u16::MAX;

/// One UI edit: the vocabulary of `contribute ui` / `contribute keymap` (plugin-abi §7).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Edit {
    /// Declares (or re-declares, resetting props) node `id`.
    Node {
        /// Stable public id, e.g. `file.save`.
        id: Sym,
        /// Widget kind.
        kind: Kind,
    },
    /// Sets one string prop.
    Set {
        /// Target node id.
        id: Sym,
        /// Prop to set.
        prop: Prop,
        /// New value (`Sym::NONE` clears it).
        value: Sym,
    },
    /// Re-parents a node (`Sym::NONE` makes it a root, e.g. a dialog).
    Parent {
        /// Target node id.
        id: Sym,
        /// New parent id.
        parent: Sym,
    },
    /// Sets the sibling sort key (default 0; ties keep declaration order).
    Order {
        /// Target node id.
        id: Sym,
        /// Sort key.
        order: i32,
    },
    /// Removes a node and, implicitly, its subtree.
    Remove {
        /// Target node id.
        id: Sym,
    },
    /// Sets one style prop on a selector (a kind name, a class, or `*`).
    Style {
        /// Selector.
        selector: Sym,
        /// Prop.
        prop: StyleProp,
        /// Parsed value (colors RGBA8, lengths px, `dir` 1 = row).
        value: u32,
    },
    /// Binds a chord to a command.
    Key {
        /// The chord.
        chord: Chord,
        /// Command name.
        command: Sym,
    },
}

/// One contribution's edits, in record order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contribution {
    /// Edits; applied in order by [`crate::compose`].
    pub edits: Vec<Edit>,
}

/// What a health row reports (D8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HealthKind {
    /// A node declared by `over` was re-declared by `by`.
    NodeReplaced,
    /// A prop (or parent/order) of a node declared by `over` was set by `by`.
    PropOverridden,
    /// A node declared by `over` was removed by `by`.
    NodeRemoved,
    /// A style prop set by `over` was set again by `by`.
    StyleOverridden,
    /// A chord bound by `over` (to `target`) was rebound by `by`.
    KeyOverridden,
    /// `by` edited a node id that does not exist (`over` = `by`).
    UnknownTarget,
    /// A node is unreachable from a root (missing parent or a parent cycle) and was dropped.
    Orphaned,
}

/// Health report: one row per overridden or rejected edit, in application order. The four
/// columns always have equal length.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    /// What happened.
    pub kind: Vec<HealthKind>,
    /// Node id, style selector, or overridden command.
    pub target: Vec<Sym>,
    /// Contribution that won (0 = base).
    pub by: Vec<u8>,
    /// Contribution that lost (0 = base).
    pub over: Vec<u8>,
}

/// Composed `ui_nodes`: SoA in depth-first pre-order, siblings sorted by (order, declaration).
/// Every column has [`UiNodes::len`] rows; a parent always precedes its children.
#[derive(Clone, Debug, Default)]
pub struct UiNodes {
    pub(crate) id: Vec<Sym>,
    pub(crate) kind: Vec<Kind>,
    pub(crate) parent: Vec<NodeIx>,
    pub(crate) end: Vec<NodeIx>,
    pub(crate) owner: Vec<u8>,
    pub(crate) class: Vec<u16>,
    pub(crate) props: [Vec<Sym>; Prop::COUNT],
    pub(crate) index: HashMap<Sym, NodeIx>,
}

/// Composed `ui_styles`: one row per selector; precedence class > kind > `*` > 0.
#[derive(Clone, Debug)]
pub struct UiStyles {
    pub(crate) selector: Vec<Sym>,
    pub(crate) value: Vec<[u32; StyleProp::COUNT]>,
    /// Bit `p` set when the row sets prop `p`.
    pub(crate) set: Vec<u32>,
    pub(crate) kind_row: [u16; Kind::COUNT],
    pub(crate) default_row: u16,
}

/// Composed `keymap`, sorted by chord for binary search.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    pub(crate) chord: Vec<Chord>,
    pub(crate) command: Vec<Sym>,
}

/// Everything [`crate::compose`] writes. Other crates only ever read it (single owner).
#[derive(Clone, Debug, Default)]
pub struct UiTables {
    /// `ui_nodes`.
    pub nodes: UiNodes,
    /// `ui_styles`.
    pub styles: UiStyles,
    /// `keymap`.
    pub keymap: Keymap,
    /// Overridden or rejected edits (D8).
    pub health: Health,
    /// Contribution names: `[Sym::NONE (base), plugin₁, …]`, indexed by owner.
    pub owners: Vec<Sym>,
}

impl Contribution {
    /// Maximum edits per contribution.
    pub const CAPACITY: usize = 1 << 18;
}

impl Health {
    /// Number of rows.
    pub fn len(&self) -> usize {
        self.kind.len()
    }

    /// True when nothing was overridden or rejected.
    pub fn is_empty(&self) -> bool {
        self.kind.is_empty()
    }

    /// Appends one row.
    pub(crate) fn push(&mut self, kind: HealthKind, target: Sym, by: u8, over: u8) {
        self.kind.push(kind);
        self.target.push(target);
        self.by.push(by);
        self.over.push(over);
        debug_assert_eq!(self.target.len(), self.kind.len());
    }
}

impl UiNodes {
    /// Maximum node count (one index is the [`NO_NODE`] sentinel).
    pub const CAPACITY: usize = u16::MAX as usize;

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.id.len()
    }

    /// True when no node exists.
    pub fn is_empty(&self) -> bool {
        self.id.is_empty()
    }

    /// Stable public ids.
    pub fn id(&self) -> &[Sym] {
        &self.id
    }

    /// Widget kinds.
    pub fn kind(&self) -> &[Kind] {
        &self.kind
    }

    /// Parent index per node ([`NO_NODE`] for roots: the window and dialogs).
    pub fn parent(&self) -> &[NodeIx] {
        &self.parent
    }

    /// Pre-order subtree end: the descendants of `i` are exactly `i + 1 .. end[i]`.
    pub fn end(&self) -> &[NodeIx] {
        &self.end
    }

    /// Contribution that declared each node (0 = base).
    pub fn owner(&self) -> &[u8] {
        &self.owner
    }

    /// One prop column (`Sym::NONE` = absent).
    pub fn prop(&self, p: Prop) -> &[Sym] {
        &self.props[p as usize]
    }

    /// Index of the node with public id `id`.
    pub fn find(&self, id: Sym) -> Option<NodeIx> {
        self.index.get(&id).copied()
    }

    /// Direct children of `i`, in sibling order. O(children) using the pre-order `end` column.
    ///
    /// # Panics
    ///
    /// Panics if `i` is not a node index.
    pub fn children(&self, i: NodeIx) -> impl Iterator<Item = NodeIx> + '_ {
        assert!((i as usize) < self.len(), "node index out of range");
        let end = self.end[i as usize];
        let mut c = i + 1;
        std::iter::from_fn(move || {
            if c >= end {
                return None;
            }
            let this = c;
            c = self.end[c as usize];
            Some(this)
        })
    }

    /// True when the `checked` prop is set to anything but `"0"`.
    pub fn is_checked(&self, strings: &Strings, i: NodeIx) -> bool {
        is_true(strings, self.props[Prop::Checked as usize][i as usize])
    }

    /// True when the `disabled` prop is set to anything but `"0"`.
    pub fn is_disabled(&self, strings: &Strings, i: NodeIx) -> bool {
        is_true(strings, self.props[Prop::Disabled as usize][i as usize])
    }
}

impl Default for UiStyles {
    fn default() -> Self {
        UiStyles {
            selector: Vec::new(),
            value: Vec::new(),
            set: Vec::new(),
            kind_row: [NO_STYLE; Kind::COUNT],
            default_row: NO_STYLE,
        }
    }
}

impl UiStyles {
    /// Row of `selector`, or [`NO_STYLE`]. O(selectors), which is ~30.
    pub fn row(&self, selector: Sym) -> u16 {
        self.selector
            .iter()
            .position(|&s| s == selector)
            .map_or(NO_STYLE, |r| r as u16)
    }

    /// Resolves `prop` for a node with class row `class` and kind `kind`; 0 when no selector
    /// sets it.
    pub fn get(&self, class: u16, kind: Kind, prop: StyleProp) -> u32 {
        self.lookup(class, prop)
            .or_else(|| self.lookup(self.kind_row[kind as usize], prop))
            .or_else(|| self.lookup(self.default_row, prop))
            .unwrap_or(0)
    }
}

impl Keymap {
    /// Command bound to `chord`. O(log bindings).
    pub fn lookup(&self, chord: Chord) -> Option<Sym> {
        self.chord
            .binary_search(&chord)
            .ok()
            .map(|i| self.command[i])
    }

    /// Bound chords, ascending.
    pub fn chords(&self) -> &[Chord] {
        &self.chord
    }

    /// Commands, parallel to [`Keymap::chords`].
    pub fn commands(&self) -> &[Sym] {
        &self.command
    }
}

// ---- private ----

/// A boolean prop: set and not `"0"`.
fn is_true(strings: &Strings, v: Sym) -> bool {
    !v.is_none() && strings.get(v) != "0"
}

impl UiStyles {
    /// Prop `prop` of style row `row` when that row sets it.
    fn lookup(&self, row: u16, prop: StyleProp) -> Option<u32> {
        let r = row as usize;
        (row != NO_STYLE && self.set[r] & (1 << prop as u32) != 0)
            .then(|| self.value[r][prop as usize])
    }
}
