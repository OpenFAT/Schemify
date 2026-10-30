//! The viewer's capacities and its small per-row value types: pane properties (spec waves §3),
//! trace strokes and colours (§7), the cursor pair (§6).
//!
//! Contents:
//! - `pub const MAX_PANES`, `MAX_TRACES`, `MAX_DATASETS`, `MAX_BINS`, `MAX_BUS_BITS`
//! - `pub const TRACE_COLORS: [u32; 8]`, `pub const PALETTE: [u32; 16]`
//! - `pub enum LineStyle`, `LineStyle::width(self) -> f64`
//! - `pub enum Cursor`
//! - `pub enum PaneMode`
//! - `pub enum Radix`, `Radix::format(self, v: u32) -> String`
//! - `pub struct PaneProps`, `PaneProps::valid(&self) -> bool`, `PaneProps::own_x(&self) -> bool`

/// Most stacked panes in one viewer.
pub const MAX_PANES: usize = 16;
/// Most traces in one viewer.
pub const MAX_TRACES: usize = 64;
/// Most datasets in one viewer (indices are `u8`).
pub const MAX_DATASETS: usize = 256;
/// Most histogram bins in one pane.
pub const MAX_BINS: u16 = 1024;
/// Most bits in one bus trace, so a bus value is an exact `u32`.
pub const MAX_BUS_BITS: usize = 32;

/// Trace colors, assigned in order to new traces (RGBA8, readable on the black plot background).
pub const TRACE_COLORS: [u32; 8] = [
    0xffd8_3cff, // yellow
    0x3ce6_ffff, // cyan
    0xff50_50ff, // red
    0x6cff_5cff, // green
    0xff9f_1aff, // orange
    0xc08c_ffff, // violet
    0x4d94_ffff, // blue
    0xff70_c8ff, // pink
];

/// The colour picker's swatches (spec waves §7): [`TRACE_COLORS`], then eight more.
pub const PALETTE: [u32; 16] = [
    TRACE_COLORS[0],
    TRACE_COLORS[1],
    TRACE_COLORS[2],
    TRACE_COLORS[3],
    TRACE_COLORS[4],
    TRACE_COLORS[5],
    TRACE_COLORS[6],
    TRACE_COLORS[7],
    0xffff_ffff, // white
    0xa0a0_a0ff, // grey
    0x00b0_90ff, // teal
    0xb8e0_40ff, // lime
    0xd8b0_80ff, // tan
    0x9090_ffff, // periwinkle
    0xff40_80ff, // rose
    0x40c0_ffff, // sky
];

/// How a trace is stroked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LineStyle {
    /// 1 px.
    Thin = 0,
    /// 2 px (xschem's highlighted wave).
    Bold = 1,
}

/// A pair of cursors: the vertical ones (x, shared by locked panes) or one pane's horizontal
/// ones (y). The discriminant indexes the `[Option<f64>; 2]` cursor arrays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Cursor {
    /// Cursor A (xschem key `a`; `Shift+A` for the horizontal one).
    A = 0,
    /// Cursor B (xschem key `b`; `Shift+B` for the horizontal one).
    B = 1,
}

/// How a pane draws its traces (spec waves §4, §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PaneMode {
    /// Polylines over x (the default).
    Analog = 0,
    /// One lane per trace with logic levels; buses show their values.
    Digital = 1,
    /// Each trace's distribution of values over the pane's own x axis.
    Histogram = 2,
}

/// How bus values and digital readouts are printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Radix {
    /// `0x1f`.
    Hex = 0,
    /// `31`.
    Dec = 1,
}

/// One pane's properties (spec waves §3), set together by `Request::SetPane`. Every row the
/// viewer holds is [`PaneProps::valid`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneProps {
    /// Drawing mode.
    pub mode: PaneMode,
    /// Logic threshold in y data units (digital lanes, bus bits): `y >= threshold` is 1. Finite.
    pub threshold: f64,
    /// Bus value radix.
    pub radix: Radix,
    /// Histogram bins, `2..=MAX_BINS`.
    pub bins: u16,
    /// Shows the shared x axis (false: its own; a histogram always uses its own).
    pub locked: bool,
    /// The one sweep step shown (dataset tracking); `None` shows every step.
    pub step: Option<u32>,
}

// Requests and trace rows index traces, panes and datasets with a `u8`.
const _: () = assert!(MAX_TRACES <= u8::MAX as usize + 1, "a trace row fits a u8");
const _: () = assert!(MAX_PANES <= u8::MAX as usize + 1, "a pane index fits a u8");
const _: () = assert!(
    MAX_DATASETS <= u8::MAX as usize + 1,
    "a dataset index fits a u8"
);
const _: () = assert!(
    MAX_BUS_BITS <= u32::BITS as usize,
    "a bus value is an exact u32"
);
const _: () = assert!(MAX_BINS >= 2, "a histogram has at least two bins");

impl LineStyle {
    /// Stroke width in logical pixels.
    #[must_use]
    pub fn width(self) -> f64 {
        match self {
            LineStyle::Thin => 1.0,
            LineStyle::Bold => 2.0,
        }
    }
}

impl Radix {
    /// The text of bus value `v` in this radix.
    #[must_use]
    pub fn format(self, v: u32) -> String {
        match self {
            Radix::Hex => format!("0x{v:x}"),
            Radix::Dec => v.to_string(),
        }
    }
}

impl Default for PaneProps {
    fn default() -> Self {
        let props = PaneProps {
            mode: PaneMode::Analog,
            threshold: 0.5,
            radix: Radix::Hex,
            bins: 32,
            locked: true,
            step: None,
        };
        debug_assert!(props.valid(), "every new pane starts inside spec waves §3");
        debug_assert!(!props.own_x(), "a new pane shows the shared x axis");
        props
    }
}

impl PaneProps {
    /// Inside the domains of spec waves §3.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.threshold.is_finite() && (2..=MAX_BINS).contains(&self.bins)
    }

    /// Uses the pane's own x axis rather than the shared one.
    #[must_use]
    pub fn own_x(&self) -> bool {
        !self.locked || self.mode == PaneMode::Histogram
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_domains_close() {
        let d = PaneProps::default();
        assert!(d.valid() && !d.own_x());
        let with = |f: fn(&mut PaneProps)| {
            let mut p = d;
            f(&mut p);
            p
        };
        assert!(with(|p| p.bins = 2).valid());
        assert!(with(|p| p.bins = MAX_BINS).valid());
        assert!(!with(|p| p.bins = 1).valid());
        assert!(!with(|p| p.threshold = f64::NAN).valid());
        assert!(with(|p| p.mode = PaneMode::Histogram).own_x());
        assert!(with(|p| p.locked = false).own_x());
    }

    #[test]
    fn radix_text_and_palette_order() {
        assert_eq!(
            (Radix::Hex.format(31), Radix::Dec.format(31)),
            ("0x1f".into(), "31".into())
        );
        assert_eq!(PALETTE[..8], TRACE_COLORS);
        assert_eq!(
            (LineStyle::Thin.width(), LineStyle::Bold.width()),
            (1.0, 2.0)
        );
    }
}
