//! UI-as-data tables (D4, D6) and their single writer, [`compose`].
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

use std::collections::HashMap;

use crate::strings::{Strings, Sym};

/// Index of a node in [`UiNodes`]; valid until the next [`compose`].
pub type NodeIx = u16;

/// "No node" sentinel for [`NodeIx`] columns (parent of a root, empty focus, …).
pub const NO_NODE: NodeIx = u16::MAX;

/// "No selector" sentinel for style rows.
pub const NO_STYLE: u16 = u16::MAX;

macro_rules! named_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum $name { $($(#[$vm])* $v),+ }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$v),+];
            /// Number of variants; sizes per-variant arrays.
            pub const COUNT: usize = Self::ALL.len();

            /// The record spelling (line-record word).
            pub fn name(self) -> &'static str {
                match self { $($name::$v => $s),+ }
            }

            /// Parses the record spelling; `None` for anything else.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$v),)+ _ => None }
            }
        }
    };
}

named_enum!(
    /// Widget kind: the closed set of node types (docs/dev/architecture.md `ui`). A new widget is a new
    /// variant plus its layout, paint and AccessKit arms.
    Kind {
        /// Root of the main window.
        Window = "window",
        /// Row of menu titles.
        Menubar = "menubar",
        /// Title in a menubar or cascade row in a menu; its children form the popup.
        Menu = "menu",
        /// Menu row that runs a command (checkable when it has `checked`).
        Item = "item",
        /// Horizontal rule in a menu.
        Separator = "separator",
        /// Row of buttons.
        Toolbar = "toolbar",
        /// Clickable command button.
        Button = "button",
        /// Row of tabs.
        Tabs = "tabs",
        /// One tab; `checked` marks the active one.
        Tab = "tab",
        /// Generic flex container.
        Panel = "panel",
        /// Two panes with a draggable divider (the gap between its first two children): the
        /// first child takes the free space, the second keeps the size the divider was dragged
        /// to ([`crate::UiState::split_of`]; its own size until then).
        Split = "split",
        /// Static text.
        Label = "label",
        /// Single-line editable text (`value`), IME-capable.
        TextInput = "text_input",
        /// Toggle with a label.
        Checkbox = "checkbox",
        /// Choice from a list: a click opens its `item` children as a popup below it (keyboard
        /// and pointer as in a menu); its `label` shows the current choice, which the owner
        /// sets. One without children runs its `command` instead.
        Dropdown = "dropdown",
        /// Vertical list of rows.
        List = "list",
        /// Hierarchical list.
        Tree = "tree",
        /// Scrolling viewport: children shift up by the wheel offset kept in
        /// [`crate::UiState`] and are clipped to the node's rectangle.
        Scroll = "scroll",
        /// The schematic canvas; drawn by `render`'s canvas layer.
        Canvas = "canvas",
        /// Waveform plot area; data is bound by the caller at paint time.
        Graph = "graph",
        /// LaTeX math (`value`), D21.
        Math = "math",
        /// Markdown with inline and display LaTeX math (`value`), wrapped to the node's width
        /// ([`crate::prose`]); clickable when it has a `command` (the docs page, D19).
        Prose = "prose",
        /// Multi-line editable text (`value`): Enter inserts a line break, Ctrl+Enter submits.
        TextArea = "text_area",
        /// Modal window root (no parent); shown when opened in [`crate::UiState`].
        Dialog = "dialog",
        /// Status bar row.
        Status = "status",
    }
);

named_enum!(
    /// Node property with a string value (`set <id> <prop> <value>`). `parent` and `order` are
    /// structural and have their own [`Edit`] variants.
    Prop {
        /// Visible text.
        Label = "label",
        /// Command name run on activation.
        Command = "command",
        /// Accelerator text shown right-aligned in menus (display only; bindings live in the keymap).
        Accel = "accel",
        /// Style selector (class) overriding the kind's style.
        Style = "style",
        /// `"1"` checked, `"0"` unchecked, absent = not checkable.
        Checked = "checked",
        /// Editable value (text input) or source (math).
        Value = "value",
        /// Hover help text.
        Tooltip = "tooltip",
        /// Non-empty and not `"0"` disables activation.
        Disabled = "disabled",
        /// Text fields: command run with the field's text whenever an edit changes it (typing,
        /// deletion, an IME commit), at most once per [`crate::route_input`] call and always
        /// before the field loses focus; `command` still runs on Enter.
        Input = "input",
        /// Buttons: a vector icon drawn instead of the label, which stays the accessible name
        /// (SVG path data in a 16 × 16 box, [`crate::icon`]). Unparsable data shows the label.
        Icon = "icon",
    }
);

named_enum!(
    /// Style property. Colors are `rrggbb[aa]` hex (optionally `"#…"`), lengths logical pixels,
    /// `dir` is `row|column`.
    StyleProp {
        /// Background color.
        Bg = "bg",
        /// Text color.
        Fg = "fg",
        /// Background under the pointer.
        HoverBg = "hover-bg",
        /// Text under the pointer.
        HoverFg = "hover-fg",
        /// Background when open, pressed or checked.
        ActiveBg = "active-bg",
        /// Text when open, pressed or checked.
        ActiveFg = "active-fg",
        /// Accelerator text color.
        AccelFg = "accel-fg",
        /// Border color.
        BorderColor = "border-color",
        /// Border width.
        Border = "border",
        /// Corner radius.
        Radius = "radius",
        /// Font size.
        Font = "font",
        /// Horizontal padding.
        Pad = "pad",
        /// Vertical padding.
        PadY = "pad-y",
        /// Gap between children (containers) or between label and accelerator (items).
        Gap = "gap",
        /// Fixed width (0 = from content).
        Width = "width",
        /// Minimum width.
        MinWidth = "min-width",
        /// Fixed height (0 = from content).
        Height = "height",
        /// Flex grow factor.
        Grow = "grow",
        /// Child direction: 1 = row, 0 = column.
        Dir = "dir",
        /// Rows only: 1 stretches children to the row's height (side-by-side panels) instead of
        /// centering them (bars).
        Stretch = "stretch",
    }
);

impl StyleProp {
    /// Parses a style value for this prop; `None` when malformed.
    pub fn parse_value(self, s: &str) -> Option<u32> {
        if (self as u8) <= StyleProp::BorderColor as u8 {
            // A bare value may not start with `#` (format §2), so `rrggbb` and `"#rrggbb"` are
            // both accepted.
            let hex = s.strip_prefix('#').unwrap_or(s);
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let v = u32::from_str_radix(hex, 16).ok()?;
            return match hex.len() {
                6 => Some((v << 8) | 0xff),
                8 => Some(v),
                _ => None,
            };
        }
        if self == StyleProp::Dir {
            return match s {
                "row" => Some(1),
                "column" => Some(0),
                _ => None,
            };
        }
        s.parse::<u32>().ok().filter(|&v| v <= 1 << 14)
    }
}

/// Keyboard modifier bits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Mods(pub u8);

impl Mods {
    /// Control.
    pub const CTRL: u8 = 1;
    /// Shift.
    pub const SHIFT: u8 = 2;
    /// Alt / Option.
    pub const ALT: u8 = 4;
    /// Super / Command / Windows.
    pub const SUPER: u8 = 8;
}

/// A key: a Unicode scalar for printable keys, or a named key at [`Key::NAMED`]` + n`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key(pub u32);

const NAMED_KEYS: [&str; 15] = [
    "Esc",
    "Enter",
    "Tab",
    "BackSpace",
    "Del",
    "Ins",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Left",
    "Right",
    "Up",
    "Down",
    "Print",
];

impl Key {
    /// First named-key code (above the Unicode range).
    pub const NAMED: u32 = 0x11_0000;
    /// Escape.
    pub const ESCAPE: Key = Key(Self::NAMED);
    /// Enter / Return.
    pub const ENTER: Key = Key(Self::NAMED + 1);
    /// Tab.
    pub const TAB: Key = Key(Self::NAMED + 2);
    /// Backspace.
    pub const BACKSPACE: Key = Key(Self::NAMED + 3);
    /// Delete.
    pub const DELETE: Key = Key(Self::NAMED + 4);
    /// Insert.
    pub const INSERT: Key = Key(Self::NAMED + 5);
    /// Home.
    pub const HOME: Key = Key(Self::NAMED + 6);
    /// End.
    pub const END: Key = Key(Self::NAMED + 7);
    /// Page up.
    pub const PAGE_UP: Key = Key(Self::NAMED + 8);
    /// Page down.
    pub const PAGE_DOWN: Key = Key(Self::NAMED + 9);
    /// Arrow left.
    pub const LEFT: Key = Key(Self::NAMED + 10);
    /// Arrow right.
    pub const RIGHT: Key = Key(Self::NAMED + 11);
    /// Arrow up.
    pub const UP: Key = Key(Self::NAMED + 12);
    /// Arrow down.
    pub const DOWN: Key = Key(Self::NAMED + 13);
    /// Print screen.
    pub const PRINT: Key = Key(Self::NAMED + 14);

    /// Function key `F<n>` for `n` in `1..=24`.
    pub fn f(n: u8) -> Key {
        assert!((1..=24).contains(&n), "F-key out of range");
        Key(Self::NAMED + 0x100 + u32::from(n))
    }

    /// Parses a key name: one character, a named key (case-insensitive, with the aliases
    /// `Escape`, `Return`, `Delete`, `Insert`, `Space`), or `F1`..`F24`.
    pub fn parse(s: &str) -> Option<Key> {
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Some(Key(c as u32));
        }
        let alias = match s.to_ascii_lowercase().as_str() {
            "escape" => "Esc",
            "return" => "Enter",
            "delete" => "Del",
            "insert" => "Ins",
            "space" => return Some(Key(' ' as u32)),
            _ => s,
        };
        if let Some(i) = NAMED_KEYS
            .iter()
            .position(|n| n.eq_ignore_ascii_case(alias))
        {
            return Some(Key(Self::NAMED + i as u32));
        }
        let n: u8 = s.strip_prefix(['F', 'f'])?.parse().ok()?;
        (1..=24).contains(&n).then(|| Key::f(n))
    }
}

/// A key plus modifiers, normalized so that the same physical chord always packs the same way.
///
/// Normalization (shared by [`Chord::parse`] and event translation): letters are lowercased and
/// keep `Shift`; other printable characters drop `Shift`, because the character already encodes
/// it (`%` is Shift+5 on a US layout, and xschem writes it as plain `%`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Chord(u32);

impl Chord {
    /// Builds a normalized chord.
    pub fn new(mods: Mods, key: Key) -> Chord {
        assert!(key.0 < 1 << 24, "key code wider than 24 bits");
        let mut m = mods.0;
        let mut k = key.0;
        if k < Key::NAMED
            && let Some(c) = char::from_u32(k)
        {
            if c.is_alphabetic() {
                let mut lower = c.to_lowercase();
                if let (Some(l), None) = (lower.next(), lower.next()) {
                    k = l as u32;
                }
            } else if c != ' ' {
                m &= !Mods::SHIFT;
            }
        }
        Chord((u32::from(m) << 24) | k)
    }

    /// Parses `Ctrl+Shift+S`, `Shift-Ins`, `Alt+-`, `%`: modifiers (`Ctrl`/`Control`, `Shift`,
    /// `Alt`, `Super`/`Cmd`, case-insensitive) each followed by `+` or `-`, then a [`Key`] name.
    pub fn parse(s: &str) -> Option<Chord> {
        const MODS: [(&str, u8); 6] = [
            ("ctrl", Mods::CTRL),
            ("control", Mods::CTRL),
            ("shift", Mods::SHIFT),
            ("alt", Mods::ALT),
            ("super", Mods::SUPER),
            ("cmd", Mods::SUPER),
        ];
        let mut mods = 0u8;
        let mut rest = s;
        // Bounded: each pass consumes one modifier and there are at most 4 distinct bits.
        for _ in 0..8 {
            let hit = MODS.iter().find(|(name, _)| {
                rest.len() > name.len()
                    && rest.is_char_boundary(name.len())
                    && rest[..name.len()].eq_ignore_ascii_case(name)
                    && matches!(rest.as_bytes()[name.len()], b'+' | b'-')
            });
            let Some(&(name, bit)) = hit else { break };
            mods |= bit;
            rest = &rest[name.len() + 1..];
        }
        Some(Chord::new(Mods(mods), Key::parse(rest)?))
    }

    /// Modifier bits.
    pub fn mods(self) -> Mods {
        Mods((self.0 >> 24) as u8)
    }

    /// Key code.
    pub fn key(self) -> Key {
        Key(self.0 & 0xff_ffff)
    }
}

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
        /// Parsed value (see [`StyleProp::parse_value`]).
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
    /// Edits; applied in order by [`compose`].
    pub edits: Vec<Edit>,
}

impl Contribution {
    /// Maximum edits per contribution.
    pub const CAPACITY: usize = 1 << 18;
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

/// Health report: one row per overridden or rejected edit, in application order.
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

impl Health {
    fn push(&mut self, kind: HealthKind, target: Sym, by: u8, over: u8) {
        self.kind.push(kind);
        self.target.push(target);
        self.by.push(by);
        self.over.push(over);
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.kind.len()
    }

    /// True when nothing was overridden or rejected.
    pub fn is_empty(&self) -> bool {
        self.kind.is_empty()
    }
}

/// Composed `ui_nodes`: SoA in depth-first pre-order, siblings sorted by (order, declaration).
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

fn is_true(strings: &Strings, v: Sym) -> bool {
    !v.is_none() && strings.get(v) != "0"
}

/// Composed `ui_styles`: one row per selector; precedence class > kind > `*` > 0.
#[derive(Clone, Debug)]
pub struct UiStyles {
    pub(crate) selector: Vec<Sym>,
    pub(crate) value: Vec<[u32; StyleProp::COUNT]>,
    pub(crate) set: Vec<u32>,
    pub(crate) kind_row: [u16; Kind::COUNT],
    pub(crate) default_row: u16,
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

    fn lookup(&self, row: u16, prop: StyleProp) -> Option<u32> {
        let r = row as usize;
        (row != NO_STYLE && self.set[r] & (1 << prop as u32) != 0)
            .then(|| self.value[r][prop as usize])
    }

    /// Resolves `prop` for a node with class row `class` and kind `kind`.
    pub fn get(&self, class: u16, kind: Kind, prop: StyleProp) -> u32 {
        self.lookup(class, prop)
            .or_else(|| self.lookup(self.kind_row[kind as usize], prop))
            .or_else(|| self.lookup(self.default_row, prop))
            .unwrap_or(0)
    }
}

/// Composed `keymap`, sorted by chord for binary search.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    pub(crate) chord: Vec<Chord>,
    pub(crate) command: Vec<Sym>,
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

/// Everything [`compose`] writes. Other crates only ever read it (single owner).
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

struct Row {
    id: Sym,
    kind: Kind,
    parent: Sym,
    order: i32,
    seq: u32,
    owner: u8,
    live: bool,
    props: [Sym; Prop::COUNT],
}

/// Composes `base ⊕ plugin₁ ⊕ … ⊕ pluginₙ` (last wins, D8) into fresh tables; this is the
/// spine's `ui::apply_ui_edits`.
///
/// Every cross-contribution override, every edit of an unknown id, and every unreachable node is
/// recorded in [`UiTables::health`]; nothing is rejected silently and nothing panics on bad
/// contributions.
///
/// # Panics
///
/// Panics on more than 254 plugins or more than [`UiNodes::CAPACITY`] composed nodes.
///
/// # Time complexity
///
/// O(E + N log N) for E edits and N nodes.
pub fn compose(
    strings: &Strings,
    base: &Contribution,
    plugins: &[(Sym, &Contribution)],
) -> UiTables {
    assert!(plugins.len() < u8::MAX as usize, "too many plugins");
    let mut health = Health::default();
    let mut rows: Vec<Row> = Vec::with_capacity(base.edits.len() / 2);
    let mut by_id: HashMap<Sym, u32> = HashMap::new();
    let mut sel: Vec<(Sym, [u32; StyleProp::COUNT], u32, [u8; StyleProp::COUNT])> = Vec::new();
    let mut keys: HashMap<Chord, (Sym, u8)> = HashMap::new();

    let all = std::iter::once((Sym::NONE, base)).chain(plugins.iter().copied());
    for (owner, (_, contribution)) in all.enumerate() {
        let owner = owner as u8;
        assert!(
            contribution.edits.len() <= Contribution::CAPACITY,
            "contribution too large"
        );
        for &edit in &contribution.edits {
            let target = |id: Sym, by_id: &HashMap<Sym, u32>, rows: &[Row]| {
                by_id.get(&id).copied().filter(|&r| rows[r as usize].live)
            };
            match edit {
                Edit::Node { id, kind } => {
                    let fresh = Row {
                        id,
                        kind,
                        parent: Sym::NONE,
                        order: 0,
                        seq: rows.len() as u32,
                        owner,
                        live: true,
                        props: [Sym::NONE; Prop::COUNT],
                    };
                    match by_id.get(&id) {
                        Some(&r) => {
                            let old = &mut rows[r as usize];
                            if old.live && old.owner != owner {
                                health.push(HealthKind::NodeReplaced, id, owner, old.owner);
                            }
                            *old = Row {
                                seq: old.seq,
                                ..fresh
                            };
                        }
                        None => {
                            by_id.insert(id, rows.len() as u32);
                            rows.push(fresh);
                        }
                    }
                }
                Edit::Set { id, .. } | Edit::Parent { id, .. } | Edit::Order { id, .. } => {
                    let Some(r) = target(id, &by_id, &rows) else {
                        health.push(HealthKind::UnknownTarget, id, owner, owner);
                        continue;
                    };
                    let row = &mut rows[r as usize];
                    if row.owner != owner {
                        health.push(HealthKind::PropOverridden, id, owner, row.owner);
                    }
                    match edit {
                        Edit::Set { prop, value, .. } => row.props[prop as usize] = value,
                        Edit::Parent { parent, .. } => row.parent = parent,
                        Edit::Order { order, .. } => row.order = order,
                        _ => unreachable!(),
                    }
                }
                Edit::Remove { id } => {
                    let Some(r) = target(id, &by_id, &rows) else {
                        health.push(HealthKind::UnknownTarget, id, owner, owner);
                        continue;
                    };
                    let row = &mut rows[r as usize];
                    if row.owner != owner {
                        health.push(HealthKind::NodeRemoved, id, owner, row.owner);
                    }
                    row.live = false;
                }
                Edit::Style {
                    selector,
                    prop,
                    value,
                } => {
                    let p = prop as usize;
                    let i = match sel.iter().position(|s| s.0 == selector) {
                        Some(i) => i,
                        None => {
                            sel.push((selector, [0; StyleProp::COUNT], 0, [0; StyleProp::COUNT]));
                            sel.len() - 1
                        }
                    };
                    let s = &mut sel[i];
                    if s.2 & (1 << p) != 0 && s.3[p] != owner {
                        health.push(HealthKind::StyleOverridden, selector, owner, s.3[p]);
                    }
                    s.1[p] = value;
                    s.2 |= 1 << p;
                    s.3[p] = owner;
                }
                Edit::Key { chord, command } => {
                    if let Some(&(old, old_owner)) = keys.get(&chord)
                        && old_owner != owner
                    {
                        health.push(HealthKind::KeyOverridden, old, owner, old_owner);
                    }
                    keys.insert(chord, (command, owner));
                }
            }
        }
    }

    let styles = build_styles(strings, sel);
    let nodes = build_tree(&rows, &by_id, &styles, &mut health);
    let mut bindings: Vec<(Chord, Sym)> = keys.into_iter().map(|(c, (s, _))| (c, s)).collect();
    bindings.sort_unstable_by_key(|b| b.0);
    let keymap = Keymap {
        chord: bindings.iter().map(|b| b.0).collect(),
        command: bindings.iter().map(|b| b.1).collect(),
    };
    debug_assert!(keymap.chord.windows(2).all(|w| w[0] < w[1]));
    let mut owners = vec![Sym::NONE];
    owners.extend(plugins.iter().map(|p| p.0));
    UiTables {
        nodes,
        styles,
        keymap,
        health,
        owners,
    }
}

fn build_styles(
    strings: &Strings,
    sel: Vec<(Sym, [u32; StyleProp::COUNT], u32, [u8; StyleProp::COUNT])>,
) -> UiStyles {
    assert!(sel.len() < NO_STYLE as usize, "too many style selectors");
    let mut styles = UiStyles {
        selector: sel.iter().map(|s| s.0).collect(),
        value: sel.iter().map(|s| s.1).collect(),
        set: sel.iter().map(|s| s.2).collect(),
        ..UiStyles::default()
    };
    for &k in Kind::ALL {
        styles.kind_row[k as usize] = strings.find(k.name()).map_or(NO_STYLE, |s| styles.row(s));
    }
    styles.default_row = strings.find("*").map_or(NO_STYLE, |s| styles.row(s));
    styles
}

fn build_tree(
    rows: &[Row],
    by_id: &HashMap<Sym, u32>,
    styles: &UiStyles,
    health: &mut Health,
) -> UiNodes {
    const ROOT: u32 = u32::MAX;
    // (parent row or ROOT, order, seq, row) sorted gives every sibling group contiguous and ordered.
    let mut edges: Vec<(u32, i32, u32, u32)> = Vec::with_capacity(rows.len());
    for (r, row) in rows.iter().enumerate().filter(|(_, row)| row.live) {
        let parent = if row.parent.is_none() {
            ROOT
        } else {
            match by_id.get(&row.parent) {
                Some(&p) if rows[p as usize].live => p,
                _ => continue, // reported as orphan below
            }
        };
        edges.push((parent, row.order, row.seq, r as u32));
    }
    edges.sort_unstable();
    let group = |parent: u32| {
        let lo = edges.partition_point(|e| e.0 < parent);
        let hi = edges.partition_point(|e| e.0 <= parent);
        &edges[lo..hi]
    };

    let mut nodes = UiNodes::default();
    let mut visited = vec![false; rows.len()];
    // Explicit stack: (row, parent index); children pushed in reverse to pop in order.
    let mut stack: Vec<(u32, NodeIx)> = group(ROOT).iter().rev().map(|e| (e.3, NO_NODE)).collect();
    while let Some((r, parent)) = stack.pop() {
        assert!(
            nodes.id.len() < UiNodes::CAPACITY,
            "ui node capacity exceeded"
        );
        let row = &rows[r as usize];
        let ix = nodes.id.len() as NodeIx;
        visited[r as usize] = true;
        nodes.id.push(row.id);
        nodes.kind.push(row.kind);
        nodes.parent.push(parent);
        nodes.end.push(ix + 1);
        nodes.owner.push(row.owner);
        let class = row.props[Prop::Style as usize];
        nodes.class.push(if class.is_none() {
            NO_STYLE
        } else {
            styles.row(class)
        });
        for (col, &v) in nodes.props.iter_mut().zip(&row.props) {
            col.push(v);
        }
        nodes.index.insert(row.id, ix);
        stack.extend(group(r).iter().rev().map(|e| (e.3, ix)));
    }
    // Pre-order: every descendant follows its parent, so one reverse pass propagates subtree ends.
    for i in (0..nodes.len()).rev() {
        let p = nodes.parent[i];
        if p != NO_NODE {
            nodes.end[p as usize] = nodes.end[p as usize].max(nodes.end[i]);
        }
    }
    // Unreached rows are orphans, except descendants of a removed node: those went with it.
    for (r, row) in rows.iter().enumerate() {
        if !row.live || visited[r] {
            continue;
        }
        let mut p = row.parent;
        let mut removed_ancestor = false;
        for _ in 0..rows.len() {
            match by_id.get(&p) {
                Some(&q) if !rows[q as usize].live => {
                    removed_ancestor = true;
                    break;
                }
                Some(&q) => p = rows[q as usize].parent,
                None => break,
            }
        }
        if !removed_ancestor {
            health.push(HealthKind::Orphaned, row.id, row.owner, row.owner);
        }
    }
    debug_assert!(
        nodes
            .parent
            .iter()
            .enumerate()
            .all(|(i, &p)| p == NO_NODE || (p as usize) < i)
    );
    nodes
}
