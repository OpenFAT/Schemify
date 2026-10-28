//! Row colours: a layout row and the interaction state in, the style it paints with out. The
//! presenter functions the renderers call, so the state rules are tested without pixels.
//!
//! Contents:
//! - `pub const DISABLED_ALPHA: u32`, `pub struct RowPaint`
//! - `pub fn row_style(tables, title_style, node, shown, prop) -> u32`
//! - `pub fn row_paint(tables, layout, strings, state, r) -> RowPaint`

use crate::kinds::{Kind, StyleProp};
use crate::layout::{Shown, UiLayout};
use crate::state::UiState;
use crate::strings::Strings;
use crate::tables::{NO_STYLE, NodeIx, UiTables};

/// Alpha of a disabled row's text and icon, as a fraction (numerator over 256) of its style's.
pub const DISABLED_ALPHA: u32 = 102;

/// The colours (`0xRRGGBBAA`) a layout row paints with, after its interaction state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowPaint {
    /// Background; zero alpha paints none.
    pub bg: u32,
    /// Text, icon and indicator colour.
    pub fg: u32,
}

/// A row's interaction state; an active row ignores the pointer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Highlight {
    /// Neither active nor under the pointer.
    Plain,
    /// Under the pointer.
    Hovered,
    /// An open menu's title or cascade row, a checked tab, an open dropdown.
    Active,
}

/// Whether a row's `disabled` prop is set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Availability {
    Enabled,
    Disabled,
}

/// Resolves a style prop for a row, following the [`Shown`] fallback chains.
///
/// # Panics
///
/// Panics if `node` is not a node of `tables`.
#[must_use]
pub fn row_style(
    tables: &UiTables,
    title_style: u16,
    node: NodeIx,
    shown: Shown,
    prop: StyleProp,
) -> u32 {
    let n = &tables.nodes;
    let i = node as usize;
    assert!(i < n.len(), "node index out of range");
    if shown != Shown::Normal {
        debug_assert!(
            matches!(n.kind[i], Kind::Menu | Kind::Dropdown),
            "only menus and dropdowns show other than as themselves"
        );
    }
    match shown {
        Shown::Normal => tables.styles.get(n.class[i], n.kind[i], prop),
        Shown::Title => tables.styles.get(
            if n.class[i] != NO_STYLE {
                n.class[i]
            } else {
                title_style
            },
            Kind::Item,
            prop,
        ),
        Shown::Cascade => tables.styles.get(n.class[i], Kind::Item, prop),
        Shown::Popup => tables.styles.get(NO_STYLE, Kind::Menu, prop),
    }
}

/// Resolves row `r`'s colours from [`row_style`] and the interaction state. An active row (an
/// open menu's title or cascade row, a checked tab, an open dropdown) takes its `active-*`
/// style, else a hovered row its `hover-*`; a state style with zero alpha falls back to the
/// base. A popup panel neither hovers nor activates. A disabled row never hovers and its fg
/// alpha is scaled by [`DISABLED_ALPHA`], so it reads as unavailable.
///
/// # Panics
///
/// Panics if `r` is not a row of `layout`.
#[must_use]
pub fn row_paint(
    tables: &UiTables,
    layout: &UiLayout,
    strings: &Strings,
    state: &UiState,
    r: usize,
) -> RowPaint {
    assert!(r < layout.len(), "no layout row {r}");
    let n = &tables.nodes;
    let (node, shown) = (layout.node[r], layout.shown[r]);
    let (kind, id) = (n.kind[node as usize], n.id[node as usize]);
    let disabled = n.is_disabled(strings, node);
    let hovered = state.hover() == id && shown != Shown::Popup && !disabled;
    let active = match shown {
        Shown::Title | Shown::Cascade => state.open().contains(&id),
        Shown::Normal => {
            (kind == Kind::Tab && n.is_checked(strings, node))
                || (kind == Kind::Dropdown && state.open().contains(&id))
        }
        Shown::Popup => false,
    };
    let style = |prop| row_style(tables, layout.title_style, node, shown, prop);
    let bg = [StyleProp::Bg, StyleProp::HoverBg, StyleProp::ActiveBg].map(style);
    let fg = [StyleProp::Fg, StyleProp::HoverFg, StyleProp::ActiveFg].map(style);
    let highlight = if active {
        Highlight::Active
    } else if hovered {
        Highlight::Hovered
    } else {
        Highlight::Plain
    };
    let availability = if disabled {
        Availability::Disabled
    } else {
        Availability::Enabled
    };
    let paint = paint_of(highlight, availability, bg, fg);
    if highlight == Highlight::Plain && !disabled {
        debug_assert_eq!(
            paint,
            RowPaint {
                bg: bg[0],
                fg: fg[0]
            },
            "a plain row paints its base"
        );
    }
    paint
}

// ---- private ----

/// The state rule of [`row_paint`] over resolved `[base, hover, active]` colours.
fn paint_of(
    highlight: Highlight,
    availability: Availability,
    bg: [u32; 3],
    fg: [u32; 3],
) -> RowPaint {
    if availability == Availability::Disabled {
        debug_assert_ne!(highlight, Highlight::Hovered, "a disabled row never hovers");
    }
    let pick = |[base, hover, act]: [u32; 3]| {
        let v = match highlight {
            Highlight::Active => act,
            Highlight::Hovered => hover,
            Highlight::Plain => 0,
        };
        if v & 0xff != 0 { v } else { base }
    };
    let fg = pick(fg);
    let fg = match availability {
        Availability::Disabled => {
            let a = (fg & 0xff) * DISABLED_ALPHA / 256;
            debug_assert!(a < 0xff, "a disabled row is fainter");
            (fg & !0xff) | a
        }
        Availability::Enabled => fg,
    };
    RowPaint { bg: pick(bg), fg }
}

#[cfg(test)]
#[test]
fn row_state_picks_colours() {
    const BG: [u32; 3] = [0x1111_11ff, 0x2222_22ff, 0x3333_33ff];
    const FG: [u32; 3] = [0xaaaa_aaff, 0xbbbb_bbff, 0xcccc_ccff];
    const CLEAR: [u32; 3] = [0x1111_11ff, 0x2222_2200, 0x3333_3300];
    use Availability::{Disabled, Enabled};
    use Highlight::{Active, Hovered, Plain};
    // (highlight, availability, bg styles, fg styles) -> (bg, fg); active wins over hover in
    // `row_paint`, which never asks for both.
    let cases = [
        ((Plain, Enabled, BG, FG), (0x1111_11ff, 0xaaaa_aaff)),
        ((Hovered, Enabled, BG, FG), (0x2222_22ff, 0xbbbb_bbff)),
        ((Active, Enabled, BG, FG), (0x3333_33ff, 0xcccc_ccff)),
        ((Active, Enabled, CLEAR, FG), (0x1111_11ff, 0xcccc_ccff)), // zero alpha: base
        ((Hovered, Enabled, CLEAR, FG), (0x1111_11ff, 0xbbbb_bbff)),
        ((Plain, Disabled, BG, FG), (0x1111_11ff, 0xaaaa_aa65)), // 0xff * 102 / 256
        ((Active, Disabled, BG, FG), (0x3333_33ff, 0xcccc_cc65)),
        ((Plain, Disabled, BG, [0; 3]), (0x1111_11ff, 0)),
    ];
    for ((h, d, bg, fg), (wb, wf)) in cases {
        let got = paint_of(h, d, bg, fg);
        assert_eq!(got, RowPaint { bg: wb, fg: wf }, "{h:?} {d:?}");
    }
}
