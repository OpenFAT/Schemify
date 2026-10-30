//! Interaction options (`EditOpts`): defaults, the user config's, one-number settings.
//!
//! Contents:
//! - `pub struct EditOpts`
//! - `Editor::setting`
//! - `pub(super) fn user_layer_colors`
//! - `pub(super) fn number`

use crate::commands::Cmd;
use crate::export;
use crate::scene::UNIT;

use super::Editor;
use super::canvas::USER_LAYERS;
use super::dialog::{Dialog, TextFor};
use crate::export::Page;
use schemify_config::Options;

/// Interaction options (xschem Options/View state).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditOpts {
    /// Snap step in milli-units (xschem `cadsnap`, default 10 units).
    pub snap: i32,
    /// Grid step in milli-units (display only, default 20 units).
    pub grid: i32,
    /// Grid shown.
    pub show_grid: bool,
    /// Light canvas colors.
    pub light: bool,
    /// Manhattan mode while drawing/moving: 0 straight, 1 horizontal first, 2 vertical
    /// first (Space cycles).
    pub manhattan: u8,
    /// Axis constraint: 0 none, 1 horizontal (`h`), 2 vertical (`v`).
    pub constrain: u8,
    /// Pick the Manhattan mode from the dominant axis (Shift+L).
    pub orthogonal: bool,
    /// Plain moves stretch attached wires (xschem `enable_stretch`, `y`).
    pub stretch: bool,
    /// Wire and line modes continue from each committed segment.
    pub persistent: bool,
    /// Join/trim wires after every wire edit.
    pub auto_trim: bool,
    /// Crosshair at the snapped pointer.
    pub crosshair: bool,
    /// Crosshair half-size in px; 0 = across the canvas.
    pub crosshair_px: u16,
    /// Diamond on the connection point nearest the pointer.
    pub snap_cursor: bool,
    /// The x = 0 / y = 0 axes.
    pub grid_axes: bool,
    /// Current drawing layer of lines, rects, polygons and arcs, `[0, 21]`.
    pub layer: u8,
    /// Hidden canvas layers (bit `l`).
    pub hidden: u32,
    /// Filled layers drawn filled.
    pub fill: bool,
    /// Line width in tenths of a pixel (10 = 1 px), `[1, 100]`.
    pub width_tenths: u8,
    /// Line width follows the zoom.
    pub variable_width: bool,
    /// Dimming toward the background, percent `[0, 100]`.
    pub dim: u8,
    /// Texts inside symbol instances drawn.
    pub symbol_text: bool,
    /// Instances: 0 symbol graphics, 1 boxes for schematic cells, 2 boxes for all.
    pub show_symbols: u8,
    /// Exports use layer colors (else black on white).
    pub export_color: bool,
    /// SVG exports have no background rectangle.
    pub export_transparent: bool,
    /// Netlists are flat (SPICE dialects).
    pub flat: bool,
    /// Every selection change highlights the selection's nets.
    pub auto_highlight: bool,
    /// Instances with a pin on a highlighted net are outlined.
    pub highlight_insts: bool,
    /// Everything but the highlight is dimmed.
    pub only_probes: bool,
    /// Each highlighted net gets its own color.
    pub incr_color: bool,
    /// The nets plotted in the waves dock are highlighted.
    pub highlight_plotted: bool,
    /// The toolbar is shown.
    pub toolbar: bool,
    /// Canvas-only view (menus, toolbar, tabs and status hidden).
    pub fullscreen: bool,
    /// Box width make-symbol draws, milli-units.
    pub symbol_width: i32,
    /// Paper PDF and EPS exports are fitted to.
    pub export_page: Page,
    /// PNG export scale, percent of screen resolution (100 = one image pixel per logical
    /// pixel, 96 dpi), `[10, 1600]`.
    pub png_scale: u16,
}

impl Default for EditOpts {
    fn default() -> Self {
        EditOpts {
            snap: 10 * UNIT,
            grid: 20 * UNIT,
            show_grid: true,
            light: false,
            manhattan: 0,
            constrain: 0,
            orthogonal: false,
            stretch: false,
            persistent: false,
            auto_trim: false,
            crosshair: false,
            crosshair_px: 0,
            snap_cursor: false,
            grid_axes: false,
            layer: 4,
            hidden: 0,
            fill: true,
            width_tenths: 10,
            variable_width: false,
            dim: 0,
            symbol_text: true,
            show_symbols: 0,
            export_color: true,
            export_transparent: false,
            flat: false,
            auto_highlight: false,
            highlight_insts: false,
            only_probes: false,
            incr_color: false,
            highlight_plotted: false,
            toolbar: true,
            fullscreen: false,
            symbol_width: 150 * UNIT,
            export_page: Page::default(),
            png_scale: 100,
        }
    }
}

impl Editor {
    /// A one-number setting: from `arg`, else a dialog prefilled with the current value.
    pub(super) fn setting(&mut self, cmd: Cmd, arg: &str) {
        let units = crate::geometry::coord;
        if arg.trim().is_empty() {
            let (title, value) = match cmd {
                Cmd::CrosshairSize => (
                    "Crosshair size (px, 0 = full)",
                    self.opts.crosshair_px.to_string(),
                ),
                Cmd::LineWidth => (
                    "Line width (px)",
                    format!("{}", f64::from(self.opts.width_tenths) / 10.0),
                ),
                Cmd::DimColors => (
                    "Dim factor (0..1)",
                    format!("{}", f64::from(self.opts.dim) / 100.0),
                ),
                Cmd::SymbolWidth => ("Symbol width (units)", units(self.opts.symbol_width)),
                Cmd::GridDialog => ("Grid spacing (units)", units(self.opts.grid)),
                Cmd::ExportPage => (
                    "Export page (a4, a3, a5, letter, legal, tabloid; portrait)",
                    self.opts.export_page.name(),
                ),
                Cmd::PngScale => (
                    "PNG export scale (1 = screen resolution, 96 dpi)",
                    format!("{}", f64::from(self.opts.png_scale) / 100.0),
                ),
                _ => ("Snap (units)", units(self.opts.snap)),
            };
            self.open_text(TextFor::Setting(cmd), title);
            if let Dialog::Text { typed, .. } = &mut self.dialog {
                *typed = value;
            }
            return;
        }
        let bad = || format!("{arg:?}: out of range");
        match cmd {
            Cmd::CrosshairSize => match number(arg, 0.0, 10_000.0) {
                Some(v) => self.opts.crosshair_px = v as u16,
                None => self.session.say(bad()),
            },
            Cmd::LineWidth => match number(arg, 0.1, 10.0) {
                Some(v) => self.opts.width_tenths = (v * 10.0).round().clamp(1.0, 100.0) as u8,
                None => self.session.say(bad()),
            },
            Cmd::DimColors => match number(arg, 0.0, 1.0) {
                Some(v) => self.opts.dim = (v * 100.0).round() as u8,
                None => self.session.say(bad()),
            },
            Cmd::SymbolWidth => match number(arg, 10.0, 10_000.0) {
                Some(v) => self.opts.symbol_width = (v * f64::from(UNIT)).round() as i32,
                None => self.session.say(bad()),
            },
            Cmd::GridDialog | Cmd::SnapDialog => match number(arg, 0.001, 1000.0) {
                Some(v) => {
                    let m = ((v * f64::from(UNIT)).round() as i32).max(1);
                    if cmd == Cmd::GridDialog {
                        self.opts.grid = m;
                    } else {
                        self.opts.snap = m;
                    }
                }
                None => self.session.say(bad()),
            },
            Cmd::ExportPage => match export::Page::parse(arg) {
                Some(p) => {
                    self.opts.export_page = p;
                    self.session.say(format!("export page: {}", p.name()));
                }
                None => {
                    self.session.say(format!(
                        "{arg:?}: not a page (a4, a3, a5, letter, legal, tabloid; portrait)"
                    ));
                }
            },
            Cmd::PngScale => match number(arg, 0.1, 16.0) {
                Some(v) => self.opts.png_scale = (v * 100.0).round().clamp(10.0, 1600.0) as u16,
                None => self.session.say(bad()),
            },
            _ => {}
        }
        assert!(
            self.opts.snap > 0 && self.opts.grid > 0,
            "a positive snap and grid"
        );
        assert!(
            self.opts.width_tenths >= 1 && self.opts.dim <= 100,
            "a visible line width, a dim factor in [0, 1]"
        );
    }
}

/// The user config's layer colors (`schemify.opt.layer_colors`, config spec §3) as the
/// editor's `(layer, 0xRRGGBB)` overrides, by layer.
pub(super) fn user_layer_colors(o: &Options) -> Vec<(u8, u32)> {
    let out: Vec<(u8, u32)> = o
        .layer_colors
        .iter()
        .enumerate()
        .filter_map(|(l, c)| c.map(|rgb| (l as u8, rgb)))
        .collect();
    assert!(out.iter().all(|c| c.0 < USER_LAYERS && c.1 <= 0x00ff_ffff));
    out
}

/// A parsed number argument in `[lo, hi]`.
pub(super) fn number(arg: &str, lo: f64, hi: f64) -> Option<f64> {
    arg.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= lo && *v <= hi)
}
