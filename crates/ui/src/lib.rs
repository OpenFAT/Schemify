//! Schemify's UI toolkit as data (D4, D6): composed UI tables, layout, text, input routing,
//! accessibility, and the `math` and `graph` widget models. No GPU and no windowing here; the
//! `render` crate draws what this crate lays out.
//!
//! The frame spine (ARCHITECTURE.md) touches this crate in order:
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

#![forbid(unsafe_code)]

pub mod access;
pub mod graph;
mod input;
mod layout;
pub mod math;
pub mod prose;
pub mod records;
mod strings;
mod tables;
mod text;
pub mod waves;

pub use input::{Button, CommandRequests, InputEvent, InputEvents, Routed, UiState, route_input};
pub use layout::{Layer, NO_CLIP, Rect, Shown, UiLayout, layout, row_style};
pub use strings::{Strings, Sym};
pub use tables::{
    Chord, Contribution, Edit, Health, HealthKind, Key, Keymap, Kind, Mods, NO_NODE, NO_STYLE,
    NodeIx, Prop, StyleProp, UiNodes, UiStyles, UiTables, compose,
};
pub use text::{
    FontGlyph, FontInfo, NO_TEXT, TextId, TextSystem, ui_font_glyphs, ui_font_info, ui_fonts,
};

/// Re-exported so renderers name the same layout and font types as this crate.
pub use parley;

/// The bundled default-UI plugin's two contribution sets (`plugins/default-ui`, D6): its
/// `contribute ui` records (menus, toolbar, tabs, canvas, status bar, dialogs, theme) followed by
/// its `contribute keymap` records (xschem's bindings, D25), as one text [`records::parse`]
/// accepts. The app loads the plugin itself; this copy is a fixture for the ui/render tests and
/// the render demo, so they need no wasm host.
pub const DEFAULT_UI: &str = concat!(
    include_str!("../../../plugins/default-ui/ui.records"),
    include_str!("../../../plugins/default-ui/keymap.records")
);
