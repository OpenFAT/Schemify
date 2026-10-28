//! Schemify's UI toolkit as data (D4, D6): composed UI tables, layout, text, input routing,
//! accessibility, the `math` widget model and the `graph` axis ticks. No GPU and no
//! windowing here; the `render` crate draws what this crate lays out.
//!
//! The frame spine (docs/dev/architecture.md) touches this crate in order:
//!
//! ```text
//! records::parse(text)                 -> Contribution      (per contribution, on change)
//! compose(base, plugins)               -> UiTables          (ui_nodes, ui_styles, keymap, health)
//! layout(tables, state, viewport)      -> UiLayout          (per frame when dirty)
//! route_input(tables, layout, events)  -> Routed            (command_requests, canvas events)
//! access_tree(tables, layout)          -> accesskit::TreeUpdate
//! waves::WaveUi::{route,command}       -> wave requests     (the waveform window, D23)
//! ```
//!
//! Owners: [`compose`] alone writes [`UiTables`]; [`layout`] writes [`UiLayout`];
//! [`route_input`] writes [`UiState`] and [`Routed`]; [`TextSystem`] owns every shaped layout.
//!
//! ```
//! use schemify_ui::{compose, layout, records, Strings, TextSystem, UiState};
//!
//! let mut strings = Strings::new();
//! let (base, rejects) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
//! assert!(rejects.is_empty());
//! let tables = compose(&strings, &base, &[]);
//! let mut text = TextSystem::new();
//! let frame = layout(&tables, &strings, &mut text, &UiState::default(), (1280.0, 800.0));
//! assert!(frame.len() > 50);
//! ```
//!
//! Contents:
//! - `pub const DEFAULT_UI: &str`
//! - modules `access`, `graph`, `icon`, `math`, `prose`, `records`, `waves`; the rest
//!   re-exported at the root

#![forbid(unsafe_code)]

pub mod access;
mod compose;
mod field;
mod flex;
mod fonts;
pub mod graph;
pub mod icon;
mod input;
mod keys;
mod kinds;
mod layout;
pub mod math;
mod menu;
mod paint;
pub mod prose;
pub mod records;
mod route;
mod state;
mod strings;
mod tables;
mod text;
pub mod waves;

pub use compose::{compose, patch_last};
pub use fonts::{FontGlyph, FontInfo, ui_font_glyphs, ui_font_info, ui_fonts};
pub use input::{Button, CommandRequests, InputEvent, InputEvents, Routed};
pub use keys::{Chord, Key, Mods};
pub use kinds::{Kind, Prop, StyleProp};
pub use layout::{
    AREA_EM_MAX, FLOAT_INSET, Layer, NO_CLIP, Rect, SPLIT_GAP, SPLIT_MIN, Shown, UiLayout,
    area_scroll_max, layout,
};
pub use paint::{DISABLED_ALPHA, RowPaint, row_paint, row_style};
pub use route::route_input;
pub use state::UiState;
pub use strings::{Strings, Sym};
pub use tables::{
    Contribution, Edit, Health, HealthKind, Keymap, NO_NODE, NO_STYLE, NodeIx, UiNodes, UiStyles,
    UiTables,
};
pub use text::{Image, NO_TEXT, TextId, TextSystem};

/// Re-exported for [`access::access_tree`]'s return type.
pub use accesskit;
/// Re-exported so renderers name the same layout and font types as this crate.
pub use parley;
/// Re-exported so a renderer reads glyph outlines' boxes from the faces this crate shapes with
/// (the GPU painter sizes an atlas cell by them).
pub use skrifa;

/// The bundled default-UI plugin's two contribution sets (`ui/default-plugin`, D6): its
/// `contribute ui` records (menus, toolbar, tabs, canvas, status bar, dialogs, theme) followed by
/// its `contribute keymap` records (xschem's bindings, D25), as one text [`records::parse`]
/// accepts. The app loads the plugin itself; this copy is a fixture for the ui/render tests and
/// the render demo, so they need no wasm host.
pub const DEFAULT_UI: &str = concat!(
    include_str!(concat!(
        env!("SCHEMIFY_ROOT"),
        "/ui/default-plugin/ui.records"
    )),
    include_str!(concat!(
        env!("SCHEMIFY_ROOT"),
        "/ui/default-plugin/keymap.records"
    ))
);
