//! `shell`: UI composition for the editor window — the spine's `ui::apply_ui_edits` step.
//! Contributions, in composition order (D8, last wins):
//!
//! 1. the core base: a window with the canvas, applied only when no plugin declares a
//!    `window` node (a plugin shell re-declaring them would otherwise inherit the base's
//!    sibling positions, since a re-declared node keeps the position it first appeared at);
//! 2. each plugin's `ui` + `keymap` sets, in plugin load order;
//! 3. the app's *state* contribution: `set`s and dynamic child nodes that project session
//!    state into plugin-declared nodes (tab titles, status fields, dialog rows). It goes
//!    last because it is live data; it only targets nodes the plugins declared, and its
//!    overrides are excluded from the health count shown to the user.
//!
//! Also derives the canvas [`Palette`] from the `canvas.<theme>.<layer>` style selectors, so
//! a plugin themes the schematic the same way it themes widgets.

use schemify_render::canvas::{LAYERS, Palette};
use schemify_ui::{
    Contribution, Edit, Kind, Strings, StyleProp, Sym, TextSystem, UiLayout, UiState, UiTables,
    compose, layout, records,
};

use crate::checks::{LAYER_ANNOT, LAYER_DIFF, LAYER_ERROR, LAYER_HILITE, LAYER_INFO, LAYER_WARN};
use crate::scene::{LAYER_BUS, LAYER_JUNCTION, LAYER_MISSING, LAYER_SEL};

/// The core base: the minimum the editor needs to be usable without the default UI — the
/// canvas, the docs page beside it (D21: docs render without plugins), and the `bottom`
/// container the checks and results panels go into.
pub const CORE_UI: &str = "\
style window
  dir column
  bg 000000
style work
  dir row
  grow 1
  stretch 1
style canvas
  grow 1
style docs-pane
  bg ffffff
  dir column
  width 520
style docs-page
  grow 1
  dir column
  pad 12
  pad-y 10
  gap 4
node window window
node work split
  parent window
  style work
node canvas canvas
  parent work
node docs.pane panel
  parent work
  style docs-pane
node docs.page scroll
  parent docs.pane
  style docs-page
style bottom
  dir column
node bottom panel
  parent window
  style bottom
";

/// Composed UI tables, interaction state and the current layout.
pub struct Shell {
    /// String arena of every UI table.
    pub strings: Strings,
    base: Contribution,
    plugins: Vec<(Sym, Contribution)>,
    plugin_window: bool,
    state: Contribution,
    state_text: String,
    state_name: Sym,
    /// `ui_nodes`, `ui_styles`, `keymap` (owner: `ui::compose`).
    pub tables: UiTables,
    /// Menus, dialog, hover and focus (owner: `ui`).
    pub ui: UiState,
    /// This frame's layout.
    pub layout: UiLayout,
    /// Shaped text cache.
    pub text: TextSystem,
    /// Lines of plugin contributions the UI parser rejected (shown in the status line).
    pub rejects: usize,
    composed: bool,
    laid_out: Option<(f32, f32)>,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell")
            .field("nodes", &self.tables.nodes.len())
            .field("plugins", &self.plugins.len())
            .finish()
    }
}

impl Default for Shell {
    fn default() -> Self {
        Shell::new()
    }
}

impl Shell {
    /// A shell with the core base only.
    pub fn new() -> Shell {
        let mut strings = Strings::new();
        let (base, rejects) = records::parse(CORE_UI, &mut strings);
        assert!(rejects.is_empty(), "core UI parses");
        let state_name = strings.intern("app.state");
        Shell {
            strings,
            base,
            plugins: Vec::new(),
            plugin_window: false,
            state: Contribution::default(),
            state_text: String::new(),
            state_name,
            tables: UiTables::default(),
            ui: UiState::default(),
            layout: UiLayout::default(),
            text: TextSystem::new(),
            rejects: 0,
            composed: false,
            laid_out: None,
        }
    }

    /// Replaces the plugin contributions: `(plugin name, ui set + keymap set text)` in load
    /// order. Rejected lines are counted, never fatal (plugin-abi §8).
    pub fn set_plugins(&mut self, sets: &[(String, String)]) {
        self.rejects = 0;
        self.plugins.clear();
        for (name, text) in sets {
            let (c, rejects) = records::parse(text, &mut self.strings);
            self.rejects += rejects.len();
            let name = self.strings.intern(name);
            self.plugins.push((name, c));
        }
        let is_window = |e: &Edit| {
            matches!(
                e,
                Edit::Node {
                    kind: Kind::Window,
                    ..
                }
            )
        };
        self.plugin_window = self
            .plugins
            .iter()
            .any(|(_, c)| c.edits.iter().any(is_window));
        self.composed = false;
    }

    /// Replaces the state contribution when its text changed. Its free text (labels, values)
    /// is one live generation of the string table (`records::parse_live`), so a status line
    /// that changes every frame does not grow it; the next [`Shell::refresh`] recomposes.
    pub fn set_state(&mut self, text: &str) {
        if text == self.state_text {
            return;
        }
        let (c, rejects) = records::parse_live(text, &mut self.strings);
        debug_assert!(
            rejects.is_empty(),
            "state contribution rejects: {rejects:?}\n{text}"
        );
        self.state = c;
        self.state_text.clear();
        self.state_text.push_str(text);
        self.composed = false;
    }

    /// Composes (if any contribution changed) and lays out (if anything changed) for
    /// `viewport`. Returns whether the layout was rebuilt.
    pub fn refresh(&mut self, viewport: (f32, f32), relayout: bool) -> bool {
        let recompose = !self.composed;
        if recompose {
            let mut all: Vec<(Sym, &Contribution)> =
                self.plugins.iter().map(|(n, c)| (*n, c)).collect();
            all.push((self.state_name, &self.state));
            let none = Contribution::default();
            let base = if self.plugin_window {
                &none
            } else {
                &self.base
            };
            self.tables = compose(&self.strings, base, &all);
            self.composed = true;
        }
        let stale = recompose || relayout || self.laid_out != Some(viewport);
        if stale && viewport.0 > 0.0 && viewport.1 > 0.0 {
            self.layout = layout(
                &self.tables,
                &self.strings,
                &mut self.text,
                &self.ui,
                viewport,
            );
            self.laid_out = Some(viewport);
        }
        stale
    }

    /// Forces a relayout on the next [`Shell::refresh`] (UI state changed: dialog, focus).
    pub fn invalidate(&mut self) {
        self.laid_out = None;
    }

    /// Interns `s` in the UI arena.
    pub fn sym(&mut self, s: &str) -> Sym {
        self.strings.intern(s)
    }

    /// Screen rectangle of the canvas node (logical px), or the whole viewport when no
    /// canvas is laid out.
    pub fn canvas_rect(&self, viewport: (f32, f32)) -> schemify_ui::Rect {
        let n = &self.tables.nodes;
        let canvas = (0..n.len()).find(|&i| n.kind()[i] == Kind::Canvas);
        canvas
            .and_then(|i| self.layout.row_of(i as u16))
            .map(|r| self.layout.rect()[r])
            .filter(|r| r.w > 0.0 && r.h > 0.0)
            .unwrap_or(schemify_ui::Rect {
                x: 0.0,
                y: 0.0,
                w: viewport.0,
                h: viewport.1,
            })
    }

    /// The canvas palette for `light` or dark from the `canvas.<theme>.<n>` / `.grid` /
    /// `.sel` selectors (`fg`); layers without a selector keep the built-in xschem dark
    /// colors. Returns the palette and the selection color.
    pub fn palette(&self, light: bool) -> (Palette, u32) {
        let theme = if light { "light" } else { "dark" };
        let mut p = Palette::xschem_dark();
        let mut sel = 0xe0e0_e0ff;
        let color = |name: &str| -> Option<u32> {
            let s = self.strings.find(&format!("canvas.{theme}.{name}"))?;
            let row = self.tables.styles.row(s);
            (row != schemify_ui::NO_STYLE)
                .then(|| self.tables.styles.get(row, Kind::Canvas, StyleProp::Fg))
        };
        for (layer, slot) in p.color.iter_mut().enumerate().take(22) {
            if let Some(c) = color(&layer.to_string()) {
                *slot = c;
            }
        }
        if let Some(c) = color("grid") {
            p.grid = c;
        }
        if let Some(c) = color("sel") {
            sel = c;
        }
        p.background = p.color[0];
        p.color[usize::from(LAYER_JUNCTION)] = p.color[1];
        p.color[usize::from(LAYER_BUS)] = p.color[1];
        p.color[usize::from(LAYER_MISSING)] = 0xff40_40ff;
        p.color[usize::from(LAYER_SEL)] = sel;
        // Decor layers (checks overlay, highlight, annotations, staged diff): from the
        // theme like the rest, else xschem-like defaults.
        let decor = [
            (LAYER_HILITE, "hilite", 0xff40_ffff, 4.0),
            (LAYER_ERROR, "error", 0xff30_30ff, 2.0),
            (LAYER_WARN, "warning", 0xffb0_20ff, 1.5),
            (LAYER_INFO, "info", 0x60a0_ffff, 1.0),
            (LAYER_ANNOT, "annot", 0x40e0_e0ff, 1.0),
            (LAYER_DIFF, "diff", 0x30ff_60ff, 2.0),
        ];
        for (layer, name, default, width) in decor {
            p.color[usize::from(layer)] = color(name).unwrap_or(default);
            p.width[usize::from(layer)] = width;
        }
        p.width[usize::from(LAYER_BUS)] = 3.0;
        p.fill = (1 << 5) | (1 << LAYER_JUNCTION);
        assert!(LAYERS > usize::from(LAYER_SEL));
        (p, sel)
    }
}
