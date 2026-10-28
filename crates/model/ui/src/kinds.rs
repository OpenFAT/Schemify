//! The closed vocabularies of the UI tables: widget [`Kind`]s, node [`Prop`]s and
//! [`StyleProp`]s, each a `u8` enum with its line-record spelling (plugin-abi §7).
//!
//! A new widget is a new [`Kind`] variant plus its layout, paint and AccessKit arms; the
//! exhaustive matches over it list every site.
//!
//! Contents:
//! - `pub enum Kind`, `pub enum Prop`, `pub enum StyleProp`: each with `ALL`, `COUNT`,
//!   `name(self) -> &'static str`, `parse(&str) -> Option<Self>`
//! - `StyleProp::parse_value(self, &str) -> Option<u32>` (crate-only)

use crate::records::LENGTH_MAX;

/// Declares a `u8` enum whose variants each carry their record spelling.
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
            #[must_use]
            pub fn name(self) -> &'static str {
                // `ALL` is in discriminant order: per-variant arrays index by `self as usize`.
                debug_assert_eq!(Self::ALL[self as usize], self);
                match self { $($name::$v => $s),+ }
            }

            /// Parses the record spelling; `None` for anything else.
            #[must_use]
            pub fn parse(s: &str) -> Option<Self> {
                let parsed = match s { $($s => Some($name::$v),)+ _ => None };
                if let Some(v) = parsed {
                    debug_assert_eq!(v.name(), s, "spellings round-trip");
                }
                parsed
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
    /// structural and have their own [`Edit`](crate::Edit) variants.
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

// `UiStyles::set` holds one bit per style prop.
const _: () = assert!(StyleProp::COUNT <= 32, "style props fit a u32 bit mask");
// `parse_value` reads the props up to `border-color` as colours.
const _: () = assert!((StyleProp::Dir as u8) > StyleProp::BorderColor as u8);
const _: () = assert!((StyleProp::Border as u8) > StyleProp::BorderColor as u8);

impl StyleProp {
    /// Parses a style value for this prop; `None` when malformed.
    pub(crate) fn parse_value(self, s: &str) -> Option<u32> {
        if (self as u8) <= StyleProp::BorderColor as u8 {
            // A bare value may not start with `#` (format §2), so `rrggbb` and `"#rrggbb"` are
            // both accepted.
            let hex = s.strip_prefix('#').unwrap_or(s);
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let v = u32::from_str_radix(hex, 16).ok()?;
            return match hex.len() {
                6 => {
                    debug_assert!(v <= 0xff_ffff, "six hex digits are 24 bits");
                    Some((v << 8) | 0xff)
                }
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
        let length: u32 = s.parse().ok()?;
        (length <= LENGTH_MAX).then_some(length)
    }
}
