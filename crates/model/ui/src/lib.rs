//! Schemify's UI as data (D4 as amended 2026-10-30, D6): composed UI tables, their egui view,
//! the keymap, the `math` widget's layout and the `graph` axis ticks. No GPU and no windowing
//! here: egui runs headless, and the `render` crate puts its output on screen.
//!
//! The frame spine (docs/dev/architecture.md) touches this crate in order:
//!
//! ```text
//! records::parse(text)                       -> Contribution   (per contribution, on change)
//! compose(base, plugins)                     -> UiTables       (ui_nodes, ui_styles, keymap, health)
//! show(ui, tables, state) inside egui's pass -> UiLayout, Routed (rects; commands, canvas events)
//! waves::WaveUi::{route,command}             -> wave requests  (the waveform window, D23)
//! ```
//!
//! Owners: [`compose`] alone writes [`UiTables`]; [`show`] writes [`UiLayout`], [`Routed`] and
//! the view's part of [`UiState`]; egui keeps hover, focus, open menus and the caret.
//!
//! ```
//! use schemify_ui::{Routed, Strings, UiLayout, UiState, compose, context, records, show};
//!
//! let mut strings = Strings::new();
//! let (base, rejects) = records::parse(schemify_ui::DEFAULT_UI, &mut strings);
//! assert!(rejects.is_empty());
//! let tables = compose(&strings, &base, &[]);
//! let (mut state, mut layout, mut out) = (UiState::default(), UiLayout::default(), Routed::default());
//! let raw = schemify_ui::InputEvents::default().raw((1280.0, 800.0));
//! let mut o = context().run_ui(raw, |ui| show(ui, &tables, &strings, &mut state, &mut layout, &mut out));
//! o.textures_delta.clear(); // no painter here
//! assert!(layout.len() > 50);
//! ```
//!
//! Contents:
//! - `pub const DEFAULT_UI: &str`
//! - modules `graph`, `icon`, `math`, `prose`, `records`, `waves`; the rest re-exported at the
//!   root

#![forbid(unsafe_code)]

mod compose;
mod fonts;
pub mod graph;
pub mod icon;
mod input;
mod keys;
mod kinds;
mod layout;
mod markdown;
pub mod math;
pub mod prose;
pub mod records;
mod state;
mod strings;
mod tables;
mod view;
pub mod waves;

pub use compose::{compose, patch_last};
pub use fonts::{
    BOLD, FontGlyph, FontInfo, ITALIC, context, font_definitions, ui_font_glyphs, ui_font_info,
    ui_fonts,
};
pub use input::{Button, CommandRequests, InputEvent, InputEvents, Routed};
pub use keys::{Chord, Key, Mods};
pub use kinds::{Kind, Prop, StyleProp};
pub use layout::{AREA_EM_MAX, FLOAT_INSET, NO_CLIP, Rect, SPLIT_GAP, SPLIT_MIN, Slot, UiLayout};
pub use state::{Image, UiState};
pub use strings::{Strings, Sym};
pub use tables::{
    Contribution, Edit, Health, HealthKind, Keymap, NO_NODE, NO_STYLE, NodeIx, UiNodes, UiStyles,
    UiTables,
};
pub use view::{DISABLED_ALPHA, pointer_events, row_style, show};

/// Re-exported so hosts name the same egui types as this crate.
pub use egui;

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
