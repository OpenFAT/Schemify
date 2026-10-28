//! The cursor readout, legend labels, digital lane names and the measurement tooltip (spec
//! waves §6): display strings the renderer places, decided here so they are tested headless.
//!
//! Contents:
//! - `pub const READOUT_HEADINGS`, `pub const READOUT_NAME_CHARS`, `pub const LANE_NAME_CHARS`
//! - `pub struct ReadoutHead`, `pub struct ReadoutRow`
//! - `pub fn lane_name(src) -> &str`, `pub fn legend_label(waves, i, out)`
//! - `pub fn readout_head(waves, active) -> ReadoutHead`, `pub fn readout_rows(waves, max, out)`
//! - `pub fn measure_at(geo, waves, x, y) -> Option<(f64, f64)>`,
//!   `pub fn measure_label(geo, waves, x, y) -> Option<String>`

use schemify_wave::{PaneMode, Waves, format_si};

use super::{Geometry, plot_area};

/// Column headings of the readout's trace table, one per [`ReadoutRow::cells`] column.
pub const READOUT_HEADINGS: [&str; 4] = ["trace", "@A", "@B", "B−A"];

/// Longest trace name the readout shows, in chars. Longer names are cut, not elided, so the
/// name column (40% of the panel) stays clear of `@A`.
pub const READOUT_NAME_CHARS: usize = 14;

/// The readout's lines above the trace table (spec waves §6), as display strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadoutHead {
    /// `x = …` for cursors a and b; the value is `-` for a hidden cursor.
    pub x: [String; 2],
    /// Which of a and b is shown (its coloured tag is drawn).
    pub shown: [bool; 2],
    /// `Δx = …   1/Δx = …` when both cursors are shown; `1/Δx = -` when they coincide.
    pub dx: Option<String>,
    /// The horizontal cursors of the followed pane and their Δy, when it has a finite one.
    pub dy: Option<String>,
}

/// One row of the readout's trace table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadoutRow {
    /// Trace colour (`0xRRGGBBAA`) for every cell.
    pub color: u32,
    /// Name (`src [step]` when several steps show), the value at a, at b, and b − a. A cell
    /// is `-` where the value is undefined: cursor hidden, outside the data, the pane has its
    /// own x, or b − a of a digital trace.
    pub cells: [String; 4],
}

/// Longest lane name a digital pane shows in its y-label strip, in chars.
pub const LANE_NAME_CHARS: usize = 9;

/// The prefix of `src` a digital lane shows: its first [`LANE_NAME_CHARS`] chars.
#[must_use]
pub fn lane_name(src: &str) -> &str {
    let end = src
        .char_indices()
        .nth(LANE_NAME_CHARS)
        .map_or(src.len(), |(b, _)| b);
    let name = &src[..end];
    debug_assert!(name.chars().count() <= LANE_NAME_CHARS);
    name
}

/// Writes trace `i`'s legend label into `out` (cleared first): its source, between bars for a
/// magnitude, then ` = ` and its value at cursor a when the pane shows the shared x and the
/// trace has a value there (as xschem's labels do).
///
/// # Panics
///
/// Panics if `i` is not a trace of `waves`.
pub fn legend_label(waves: &Waves, i: usize, out: &mut String) {
    assert!(i < waves.trace_count(), "no trace {i}");
    out.clear();
    if waves.is_magnitude(i) {
        out.push('|');
        out.push_str(waves.src(i));
        out.push('|');
    } else {
        out.push_str(waves.src(i));
    }
    let props = waves.props(usize::from(waves.pane_of(i)));
    let a = waves.cursors()[0].filter(|_| !props.own_x());
    if let Some(v) = a.and_then(|a| waves.value_at(i, a)) {
        out.push_str(" = ");
        out.push_str(&format_si(v, 4));
    }
    debug_assert!(out.contains(waves.src(i)));
}

/// The readout's head lines for the viewer's cursors. `active` picks the pane whose
/// horizontal cursors show; it is clamped to the last pane for the lookup but printed as given.
///
/// # Panics
///
/// If `waves` has no pane.
pub fn readout_head(waves: &Waves, active: u8) -> ReadoutHead {
    assert!(!waves.panes().is_empty(), "a viewer always has a pane");
    let [a, b] = waves.cursors();
    let dx = a.zip(b).map(|(a, b)| {
        let dx = b - a;
        let inv = if dx != 0.0 {
            si(Some(1.0 / dx))
        } else {
            "-".into()
        };
        format!("Δx = {}   1/Δx = {inv}", si(Some(dx)))
    });
    let hc = waves
        .hcursors(usize::from(active).min(waves.panes().len() - 1))
        .map(|c| c.filter(|v| v.is_finite()));
    let dy = hc.iter().any(Option::is_some).then(|| {
        let d = hc[0].zip(hc[1]).map(|(a, b)| b - a);
        format!(
            "pane {}: A y = {}  B y = {}  Δy = {}",
            u16::from(active) + 1,
            si(hc[0]),
            si(hc[1]),
            si(d)
        )
    });
    let head = ReadoutHead {
        x: [a, b].map(|v| format!("x = {}", si(v))),
        shown: [a.is_some(), b.is_some()],
        dx,
        dy,
    };
    debug_assert_eq!(head.dx.is_some(), head.shown == [true, true]);
    head
}

/// Fills `out` (cleared first) with at most `max` readout rows in trace order: one per trace,
/// or one per shown step of a trace showing several. A digital trace reads logic values, a bus
/// in its pane's radix.
///
/// # Cost
///
/// O(min(rows, `max`)) value lookups, each a binary search of one trace's samples.
pub fn readout_rows(waves: &Waves, max: usize, out: &mut Vec<ReadoutRow>) {
    out.clear();
    let [ca, cb] = waves.cursors();
    for i in 0..waves.trace_count() {
        if out.len() >= max {
            break;
        }
        let props = waves.props(usize::from(waves.pane_of(i)));
        let (a, b) = if props.own_x() {
            (None, None)
        } else {
            (ca, cb)
        };
        let color = waves.color(i);
        let name: String = waves.src(i).chars().take(READOUT_NAME_CHARS).collect();
        if props.mode == PaneMode::Digital {
            let fmt = |v: Option<u32>| match v {
                Some(v) if waves.bus_bits(i) > 0 => props.radix.format(v),
                Some(v) => v.to_string(),
                None => "-".to_owned(),
            };
            let va = a.and_then(|x| waves.logic_at(i, x));
            let vb = b.and_then(|x| waves.logic_at(i, x));
            out.push(ReadoutRow {
                color,
                cells: [name, fmt(va), fmt(vb), "-".to_owned()],
            });
            continue;
        }
        let steps = waves.shown_steps(i);
        let several = steps.len() > 1;
        for st in steps {
            if out.len() >= max {
                break;
            }
            let at = |x: f64| {
                if several {
                    waves.value_at_step(i, st, x)
                } else {
                    waves.value_at(i, x)
                }
            };
            let (va, vb) = (a.and_then(at), b.and_then(at));
            let label = if several {
                format!("{name} [{st}]")
            } else {
                name.clone()
            };
            let d = va.zip(vb).map(|(a, b)| b - a);
            out.push(ReadoutRow {
                color,
                cells: [label, si(va), si(vb), si(d)],
            });
        }
    }
    debug_assert!(out.len() <= max);
}

/// The data point under the pointer `(x, y)`, as `(x, y)` values of the pane it is over, or
/// `None` off every pane's plot area (the axis strips and the legend do not measure).
///
/// # Panics
///
/// Panics if `geo` has more pane rectangles than `waves` has panes.
#[must_use]
pub fn measure_at(geo: &Geometry, waves: &Waves, x: f32, y: f32) -> Option<(f64, f64)> {
    assert!(
        geo.panes.len() <= waves.panes().len(),
        "more pane rects than panes"
    );
    let k = usize::from(geo.pane_at(x, y)?);
    let pr = plot_area(geo.panes[k]);
    if !pr.contains(x, y) {
        return None;
    }
    let vx = waves.x_of(k).scale(pr.x, pr.x + pr.w).value(x);
    let vy = waves.panes()[k].scale(pr.y + pr.h, pr.y).value(y);
    Some((vx, vy))
}

/// The measurement tooltip's text (xschem `m`): `y=…  x=…` of [`measure_at`] in engineering
/// units, or `None` where it measures nothing.
#[must_use]
pub fn measure_label(geo: &Geometry, waves: &Waves, x: f32, y: f32) -> Option<String> {
    let (vx, vy) = measure_at(geo, waves, x, y)?;
    Some(format!("y={}  x={}", format_si(vy, 4), format_si(vx, 4)))
}

// ---- private ----

/// `v` in engineering units with 4 significant digits, `-` for `None`.
fn si(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| format_si(v, 4))
}
