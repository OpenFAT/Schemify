//! Waveform panes (D23, spec `docs/spec/waves.md`): grid with engineering-unit ticks, decimated
//! traces (analog), logic lanes (digital) or value histograms, vertical and horizontal cursors,
//! the legend, the box-zoom band, the cursor readout table and the measurement tooltip. Generic
//! over [`Painter`], so the window, an embedded graph and the CPU golden images all draw through
//! the same code.
//!
//! Per frame and pane the cost is O(pixel columns) per trace step after
//! [`schemify_wave::decimate`] or [`schemify_wave::logic_runs`] (which scan the visible samples
//! once), O(samples) per histogram trace, plus shaping the tick and legend labels.

use vello::kurbo::{BezPath, Rect};
use vello::peniko::Color;

use schemify_ui::graph::{log_ticks, ticks};
use schemify_ui::parley::Layout;
use schemify_ui::waves::{Drag, Geometry, WaveUi, plot_area};
use schemify_ui::{Rect as UiRect, TextSystem};
use schemify_wave::{Axis, Cursor, PaneMode, Run, Scale, Waves, decimate, format_si, logic_runs};

use crate::painter::{Chains, Painter, rgba};
use crate::widgets::{UiScratch, draw_text};

/// Colors of the pane chrome (RGBA8). The default follows xschem's graphs: black plot, grey
/// grid, light labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaveLook {
    /// Pane background.
    pub bg: u32,
    /// Grid lines and plot frame.
    pub grid: u32,
    /// Tick labels.
    pub label: u32,
    /// Frame of the active pane (when there are several).
    pub active: u32,
    /// Cursor A and B.
    pub cursor: [u32; 2],
}

impl Default for WaveLook {
    fn default() -> Self {
        WaveLook {
            bg: 0x0000_00ff,
            grid: 0x3a3a_3aff,
            label: 0xb0b0_b0ff,
            active: 0x5a8c_d2ff,
            cursor: [0xffff_ffff, 0xff8c_ffff],
        }
    }
}

/// Buffers reused across frames (no allocation per frame after warm-up).
#[derive(Debug, Default)]
pub struct WaveScratch {
    pts: Vec<[f32; 2]>,
    chains: Chains,
    ticks: Vec<f64>,
    label: Layout<()>,
    text: String,
    path: BezPath,
    runs: Vec<Run>,
    bins: Vec<u32>,
    ui: UiScratch,
}

fn kr(r: UiRect) -> Rect {
    Rect::new(
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.x + r.w),
        f64::from(r.y + r.h),
    )
}

/// Tick label size in logical pixels.
const TICK_PX: f32 = 10.0;

/// Draws every pane of the window into its layout rectangle, then the readout panel and, while
/// it is on, the measurement tooltip at the pointer.
pub fn draw_window<P: Painter>(
    p: &mut P,
    geo: &Geometry,
    waves: &Waves,
    ui: &WaveUi,
    look: &WaveLook,
    text: &mut TextSystem,
    s: &mut WaveScratch,
) {
    assert!(
        geo.panes.len() <= waves.panes().len(),
        "more pane rects than panes"
    );
    let several = geo.panes.len() > 1;
    for (k, &r) in geo.panes.iter().enumerate() {
        let active = several && ui.active_pane() as usize == k;
        draw_pane(p, r, waves, k as u8, active, ui.drag(), look, text, s);
    }
    if geo.readout.w > 0.0 && geo.readout.h > 0.0 {
        draw_readout(p, geo.readout, waves, ui.active_pane(), look, text, s);
    }
    if let (true, Some((x, y))) = (ui.measure(), ui.pointer()) {
        draw_measure(p, geo, waves, x, y, text, s);
    }
}

/// Draws pane `k` (its traces, grid, cursors, legend) filling `area`, in the pane's mode.
/// `drag` adds the box-zoom band when it belongs to this pane. An embedded graph calls this
/// with its own rectangle.
///
/// # Panics
///
/// Panics if `k` is not a pane of `waves`.
#[allow(clippy::too_many_arguments)]
pub fn draw_pane<P: Painter>(
    p: &mut P,
    area: UiRect,
    waves: &Waves,
    k: u8,
    active: bool,
    drag: Drag,
    look: &WaveLook,
    text: &mut TextSystem,
    s: &mut WaveScratch,
) {
    assert!(usize::from(k) < waves.panes().len(), "no pane {k}");
    let pane = kr(area);
    p.fill_rect(pane, rgba(look.bg));
    let pr = plot_area(area);
    if pr.w < 8.0 || pr.h < 8.0 {
        return;
    }
    let plot = kr(pr);
    let props = waves.props(usize::from(k));
    let (xa, ya) = (waves.x_of(usize::from(k)), waves.panes()[usize::from(k)]);
    let sx = xa.scale(pr.x, pr.x + pr.w);
    let sy = ya.scale(pr.y + pr.h, pr.y);
    let (grid, label) = (rgba(look.grid), rgba(look.label));
    let digital = props.mode == PaneMode::Digital;

    // Grid and tick labels (a digital pane has lanes, not a y scale).
    let nx = (pr.w / 90.0).clamp(2.0, 12.0) as u8;
    axis_ticks(xa, nx, &mut s.ticks);
    let ts = std::mem::take(&mut s.ticks);
    let digits = label_digits(&ts, xa.log);
    for &t in &ts {
        let x = f64::from(sx.px(t)).floor() + 0.5;
        if x < plot.x0 - 0.5 || x > plot.x1 + 0.5 {
            continue;
        }
        p.fill_rect(Rect::new(x - 0.5, plot.y0, x + 0.5, plot.y1), grid);
        label_at(
            p,
            text,
            s,
            &format_si(t, digits),
            x,
            plot.y1 + 2.0,
            Align::Center,
            label,
        );
    }
    s.ticks = ts;
    if !digital {
        let ny = (pr.h / 26.0).clamp(2.0, 10.0) as u8;
        axis_ticks(ya, ny, &mut s.ticks);
        let ts = std::mem::take(&mut s.ticks);
        let digits = label_digits(&ts, ya.log);
        for &t in &ts {
            let y = f64::from(sy.px(t)).floor() + 0.5;
            if y < plot.y0 - 0.5 || y > plot.y1 + 0.5 {
                continue;
            }
            p.fill_rect(Rect::new(plot.x0, y - 0.5, plot.x1, y + 0.5), grid);
            let t = format_si(t, digits);
            label_at(p, text, s, &t, plot.x0 - 4.0, y - 7.0, Align::Right, label);
        }
        s.ticks = ts;
    }
    outline(p, &mut s.path, plot, 1.0, grid);

    // Traces, clipped to the plot.
    p.push_clip(plot);
    match props.mode {
        PaneMode::Analog => draw_lines(p, waves, k, &sx, &sy, s),
        PaneMode::Digital => draw_lanes(p, waves, k, &sx, plot, look, text, s),
        PaneMode::Histogram => draw_histogram(p, waves, k, xa, &sx, &sy, plot, s),
    }
    // Vertical cursors span the plots that show the shared x; their tags sit on the x axis.
    for c in [Cursor::A, Cursor::B] {
        let Some(at) = waves.cursors()[c as usize].filter(|_| !props.own_x()) else {
            continue;
        };
        let x = f64::from(sx.px(at)).floor() + 0.5;
        if x < plot.x0 || x > plot.x1 {
            continue;
        }
        let color = rgba(look.cursor[c as usize]);
        p.fill_rect(Rect::new(x - 0.5, plot.y0, x + 0.5, plot.y1), color);
        let tag = if c == Cursor::A { "A" } else { "B" };
        p.fill_rect(Rect::new(x - 6.0, plot.y0, x + 6.0, plot.y0 + 13.0), color);
        label_at(p, text, s, tag, x, plot.y0, Align::Center, rgba(look.bg));
    }
    // Horizontal cursors span the plot; their tags sit on the y axis.
    for c in [Cursor::A, Cursor::B] {
        let Some(at) = waves.hcursors(usize::from(k))[c as usize].filter(|_| !digital) else {
            continue;
        };
        let y = f64::from(sy.px(at)).floor() + 0.5;
        if y < plot.y0 || y > plot.y1 {
            continue;
        }
        let color = rgba(look.cursor[c as usize]);
        p.fill_rect(Rect::new(plot.x0, y - 0.5, plot.x1, y + 0.5), color);
        let tag = if c == Cursor::A { "A" } else { "B" };
        p.fill_rect(Rect::new(plot.x0, y - 6.5, plot.x0 + 12.0, y + 6.5), color);
        label_at(
            p,
            text,
            s,
            tag,
            plot.x0 + 6.0,
            y - 6.5,
            Align::Center,
            rgba(look.bg),
        );
    }
    if let Drag::Box {
        pane,
        x0,
        y0,
        x1,
        y1,
    } = drag
        && pane == k
    {
        let band = Rect::new(
            f64::from(x0.min(x1)),
            f64::from(y0.min(y1)),
            f64::from(x0.max(x1)),
            f64::from(y0.max(y1)),
        );
        p.fill_rect(band, Color::from_rgba8(120, 160, 255, 48));
        outline(
            p,
            &mut s.path,
            band,
            1.0,
            Color::from_rgba8(150, 190, 255, 220),
        );
    }
    p.pop_clip();

    if digital {
        // Lane names in the y-label strip, in their trace's colour.
        let lanes = (0..waves.trace_count())
            .filter(|&i| waves.pane_of(i) == k)
            .count();
        let lh = plot.height() / lanes.max(1) as f64;
        let mine = (0..waves.trace_count()).filter(|&i| waves.pane_of(i) == k);
        for (j, i) in mine.enumerate() {
            let name: String = waves.src(i).chars().take(9).collect();
            let y = plot.y0 + (j as f64 + 0.5) * lh - 7.0;
            let color = rgba(waves.color(i));
            label_at(p, text, s, &name, plot.x0 - 4.0, y, Align::Right, color);
        }
    } else {
        // Legend: trace names in their colors, with the value at cursor A (as xschem's labels
        // do) where the pane shows the shared x.
        let mut lx = plot.x0 + 2.0;
        for i in (0..waves.trace_count()).filter(|&i| waves.pane_of(i) == k) {
            s.text.clear();
            if waves.is_magnitude(i) {
                s.text.push('|');
                s.text.push_str(waves.src(i));
                s.text.push('|');
            } else {
                s.text.push_str(waves.src(i));
            }
            let a = waves.cursors()[0].filter(|_| !props.own_x());
            if let Some(v) = a.and_then(|a| waves.value_at(i, a)) {
                s.text.push_str(" = ");
                s.text.push_str(&format_si(v, 4));
            }
            let t = std::mem::take(&mut s.text);
            let w = label_at(
                p,
                text,
                s,
                &t,
                lx,
                pane.y0 + 2.0,
                Align::Left,
                rgba(waves.color(i)),
            );
            s.text = t;
            lx += w + 14.0;
            if lx > plot.x1 {
                break;
            }
        }
    }
    if active {
        outline(p, &mut s.path, pane.inset(-0.5), 1.0, rgba(look.active));
    }
}

/// Analog mode: each trace's shown steps as decimated polylines.
fn draw_lines<P: Painter>(
    p: &mut P,
    waves: &Waves,
    k: u8,
    sx: &Scale,
    sy: &Scale,
    s: &mut WaveScratch,
) {
    for i in (0..waves.trace_count()).filter(|&i| waves.pane_of(i) == k) {
        let (x, y) = waves.xy(i);
        s.chains.clear();
        for st in &waves.steps(i)[waves.shown_steps(i)] {
            let r = st.start as usize..st.end as usize;
            s.pts.clear();
            decimate(&x[r.clone()], &y[r], sx, sy, &mut s.pts);
            if s.pts.len() >= 2 {
                s.chains.push(&s.pts);
            }
        }
        if !s.chains.is_empty() {
            p.stroke_chains(
                &s.chains,
                false,
                waves.style(i).width(),
                rgba(waves.color(i)),
            );
        }
    }
}

/// Digital mode (spec waves §4): one lane per trace; a signal as a 0/1 level line, a bus as
/// value boxes labelled in the pane's radix, busy columns filled.
#[allow(clippy::too_many_arguments)]
fn draw_lanes<P: Painter>(
    p: &mut P,
    waves: &Waves,
    k: u8,
    sx: &Scale,
    plot: Rect,
    look: &WaveLook,
    text: &mut TextSystem,
    s: &mut WaveScratch,
) {
    let props = waves.props(usize::from(k));
    let mine: Vec<usize> = (0..waves.trace_count())
        .filter(|&i| waves.pane_of(i) == k)
        .collect();
    let lh = plot.height() / mine.len().max(1) as f64;
    for (j, &i) in mine.iter().enumerate() {
        let top = plot.y0 + j as f64 * lh;
        let (hi, lo) = (top + 0.2 * lh, top + 0.8 * lh);
        let bus = waves.bus_bits(i) > 0;
        let thr = (!bus).then_some(props.threshold);
        let color = rgba(waves.color(i));
        let busy = rgba((waves.color(i) & 0xffff_ff00) | 0x60);
        let (x, y) = waves.xy(i);
        s.runs.clear();
        for st in &waves.steps(i)[waves.shown_steps(i)] {
            let r = st.start as usize..st.end as usize;
            logic_runs(&x[r.clone()], &y[r], thr, sx, &mut s.runs);
        }
        let px = |c: u32| f64::from(sx.p0) + f64::from(c);
        s.chains.clear();
        s.pts.clear();
        s.path.truncate(0);
        let mut last: Option<u32> = None;
        for &run in &s.runs {
            let (x0, x1) = (px(run.c0), px(run.c1));
            let Some(v) = run.value else {
                p.fill_rect(Rect::new(x0, hi, x1, lo), busy);
                if s.pts.len() >= 2 {
                    s.chains.push(&s.pts);
                }
                s.pts.clear();
                last = None;
                continue;
            };
            if bus {
                s.path.move_to((x0, hi));
                s.path.line_to((x1, hi));
                s.path.line_to((x1, lo));
                s.path.line_to((x0, lo));
                s.path.close_path();
                if x1 - x0 >= 16.0 {
                    text.shape_into(&props.radix.format(v), TICK_PX, &mut s.label);
                    let w = f64::from(s.label.width());
                    if w + 4.0 <= x1 - x0 {
                        let ty = (hi + lo) / 2.0 - 7.0;
                        draw_text(p, &s.label, (x0 + x1 - w) / 2.0, ty, color, &mut s.ui);
                    }
                }
                continue;
            }
            let level = if v != 0 { hi } else { lo };
            if last != Some(run.c0) {
                if s.pts.len() >= 2 {
                    s.chains.push(&s.pts);
                }
                s.pts.clear();
            }
            s.pts.push([x0 as f32, level as f32]);
            s.pts.push([x1 as f32, level as f32]);
            last = Some(run.c1);
        }
        if s.pts.len() >= 2 {
            s.chains.push(&s.pts);
        }
        let width = waves.style(i).width();
        if !s.chains.is_empty() {
            p.stroke_chains(&s.chains, false, width, color);
        }
        if bus && !s.path.elements().is_empty() {
            p.stroke_path(&s.path, width, color);
        }
        // Lane separator.
        if j > 0 {
            p.fill_rect(
                Rect::new(plot.x0, top - 0.5, plot.x1, top + 0.5),
                rgba(look.grid),
            );
        }
    }
}

/// Histogram mode (spec waves §5): each trace's bin counts over the pane's own x, as a filled
/// step outline in the trace's colour.
#[allow(clippy::too_many_arguments)]
fn draw_histogram<P: Painter>(
    p: &mut P,
    waves: &Waves,
    k: u8,
    xa: Axis,
    sx: &Scale,
    sy: &Scale,
    plot: Rect,
    s: &mut WaveScratch,
) {
    let base = plot.y1 as f32;
    for i in (0..waves.trace_count()).filter(|&i| waves.pane_of(i) == k) {
        let mut bins = std::mem::take(&mut s.bins);
        waves.histogram(i, &mut bins);
        let n = bins.len();
        let edge = |b: usize| sx.px(xa.at(b as f64 / n as f64));
        s.pts.clear();
        s.pts.push([edge(0), base]);
        for (b, &c) in bins.iter().enumerate() {
            let h = sy.px(f64::from(c));
            s.pts.push([edge(b), h]);
            s.pts.push([edge(b + 1), h]);
        }
        s.pts.push([edge(n), base]);
        s.chains.clear();
        s.chains.push(&s.pts);
        p.fill_chains(&s.chains, rgba((waves.color(i) & 0xffff_ff00) | 0x40));
        p.stroke_chains(
            &s.chains,
            false,
            waves.style(i).width(),
            rgba(waves.color(i)),
        );
        s.bins = bins;
    }
}

/// Draws the cursor readout: a, b, Δx and 1/Δx, the active pane's horizontal cursors and Δy
/// when it has any, then each trace's value at a and b and their difference in the trace's
/// color (one row per shown sweep step when there are several; `-` for a trace whose pane has
/// its own x axis). Rows that do not fit are left out.
pub fn draw_readout<P: Painter>(
    p: &mut P,
    area: UiRect,
    waves: &Waves,
    active: u8,
    look: &WaveLook,
    text: &mut TextSystem,
    s: &mut WaveScratch,
) {
    let r = kr(area);
    p.push_clip(r);
    let fg = rgba(0xd0d0_d0ff);
    let row = 15.0;
    let mut y = r.y0 + 4.0;
    let [a, b] = waves.cursors();
    // Columns: name 40%, then A, B and B−A at 20% each.
    let col = |k: usize| r.x0 + 6.0 + [0.0, 0.4, 0.6, 0.8][k] * (r.width() - 12.0);
    let si = |v: Option<f64>| v.map_or_else(|| "-".to_owned(), |v| format_si(v, 4));
    for (k, name, v) in [(0, "A", a), (1, "B", b)] {
        if v.is_some() {
            label_at(
                p,
                text,
                s,
                name,
                col(0),
                y,
                Align::Left,
                rgba(look.cursor[k]),
            );
        }
        label_at(
            p,
            text,
            s,
            &format!("x = {}", si(v)),
            col(0) + 16.0,
            y,
            Align::Left,
            fg,
        );
        y += row;
    }
    if let (Some(a), Some(b)) = (a, b) {
        let dx = b - a;
        let inv = if dx != 0.0 {
            si(Some(1.0 / dx))
        } else {
            "-".into()
        };
        let t = format!("Δx = {}   1/Δx = {inv}", si(Some(dx)));
        label_at(p, text, s, &t, col(0), y, Align::Left, fg);
    }
    let hc = waves
        .hcursors(usize::from(active).min(waves.panes().len() - 1))
        .map(|c| c.filter(|v| v.is_finite()));
    if hc.iter().any(Option::is_some) {
        y += row;
        let dy = hc[0].zip(hc[1]).map(|(a, b)| b - a);
        let t = format!(
            "pane {}: A y = {}  B y = {}  Δy = {}",
            active + 1,
            si(hc[0]),
            si(hc[1]),
            si(dy)
        );
        label_at(p, text, s, &t, col(0), y, Align::Left, fg);
    }
    y += row + 4.0;
    for (k, h) in ["trace", "@A", "@B", "B−A"].iter().enumerate() {
        label_at(p, text, s, h, col(k), y, Align::Left, rgba(look.label));
    }
    y += row;
    'traces: for i in 0..waves.trace_count() {
        let props = waves.props(usize::from(waves.pane_of(i)));
        let (a, b) = if props.own_x() { (None, None) } else { (a, b) };
        let color = rgba(waves.color(i));
        let name: String = waves.src(i).chars().take(14).collect();
        if props.mode == PaneMode::Digital {
            if y + row > r.y1 {
                break;
            }
            let fmt = |v: Option<u32>| match v {
                Some(v) if waves.bus_bits(i) > 0 => props.radix.format(v),
                Some(v) => v.to_string(),
                None => "-".to_owned(),
            };
            label_at(p, text, s, &name, col(0), y, Align::Left, color);
            let va = a.and_then(|x| waves.logic_at(i, x));
            let vb = b.and_then(|x| waves.logic_at(i, x));
            for (k, t) in [fmt(va), fmt(vb), "-".to_owned()].iter().enumerate() {
                label_at(p, text, s, t, col(k + 1), y, Align::Left, color);
            }
            y += row;
            continue;
        }
        let steps = waves.shown_steps(i);
        let several = steps.len() > 1;
        for st in steps {
            if y + row > r.y1 {
                break 'traces;
            }
            let (va, vb) = if several {
                (
                    a.and_then(|x| waves.value_at_step(i, st, x)),
                    b.and_then(|x| waves.value_at_step(i, st, x)),
                )
            } else {
                (
                    a.and_then(|x| waves.value_at(i, x)),
                    b.and_then(|x| waves.value_at(i, x)),
                )
            };
            let label = if several {
                format!("{name} [{st}]")
            } else {
                name.clone()
            };
            label_at(p, text, s, &label, col(0), y, Align::Left, color);
            let d = va.zip(vb).map(|(a, b)| b - a);
            for (k, v) in [va, vb, d].into_iter().enumerate() {
                label_at(p, text, s, &si(v), col(k + 1), y, Align::Left, color);
            }
            y += row;
        }
    }
    p.pop_clip();
}

/// The measurement tooltip (xschem `m`): `y=` and `x=` of the pointer over a plot, in
/// engineering units, on a black box beside the pointer.
fn draw_measure<P: Painter>(
    p: &mut P,
    geo: &Geometry,
    waves: &Waves,
    x: f32,
    y: f32,
    text: &mut TextSystem,
    s: &mut WaveScratch,
) {
    let Some(k) = geo.panes.iter().position(|r| r.contains(x, y)) else {
        return;
    };
    let pr = plot_area(geo.panes[k]);
    if !pr.contains(x, y) {
        return;
    }
    let vx = waves.x_of(k).scale(pr.x, pr.x + pr.w).value(x);
    let vy = waves.panes()[k].scale(pr.y + pr.h, pr.y).value(y);
    let t = format!("y={}  x={}", format_si(vy, 4), format_si(vx, 4));
    text.shape_into(&t, TICK_PX, &mut s.label);
    let w = f64::from(s.label.width());
    let (bx, by) = (f64::from(x) + 12.0, f64::from(y) - 20.0);
    p.fill_rect(
        Rect::new(bx, by, bx + w + 8.0, by + 15.0),
        rgba(0x0000_00ff),
    );
    draw_text(
        p,
        &s.label,
        bx + 4.0,
        by + 1.0,
        rgba(0xffff_40ff),
        &mut s.ui,
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
    Right,
}

/// Shapes and draws `t` with its top at `y`, anchored at `x`; returns the width.
#[allow(clippy::too_many_arguments)]
fn label_at<P: Painter>(
    p: &mut P,
    text: &mut TextSystem,
    s: &mut WaveScratch,
    t: &str,
    x: f64,
    y: f64,
    align: Align,
    color: Color,
) -> f64 {
    // ponytail: every tick, legend and readout label is shaped each frame (~0.3 ms for a
    // three-pane window). Cache by (text, size) in `TextSystem` if a profile ever shows it.
    text.shape_into(t, TICK_PX, &mut s.label);
    let w = f64::from(s.label.width());
    let x = match align {
        Align::Left => x,
        Align::Center => x - w / 2.0,
        Align::Right => x - w,
    };
    draw_text(p, &s.label, x, y, color, &mut s.ui);
    w
}

fn outline<P: Painter>(p: &mut P, path: &mut BezPath, r: Rect, w: f64, color: Color) {
    path.truncate(0);
    path.move_to((r.x0, r.y0));
    path.line_to((r.x1, r.y0));
    path.line_to((r.x1, r.y1));
    path.line_to((r.x0, r.y1));
    path.close_path();
    p.stroke_path(path, w, color);
}

/// Significant digits that tell neighbouring ticks apart: at least 3, more when the view is
/// narrow next to its distance from zero (a 50 ps window at 4.2 ms needs 9).
fn label_digits(ts: &[f64], log: bool) -> usize {
    let (Some(&a), Some(&b)) = (ts.first(), ts.get(1)) else {
        return 3;
    };
    let big = ts.iter().fold(0.0f64, |m, t| m.max(t.abs()));
    if log || b <= a || big == 0.0 {
        return 3;
    }
    ((big / (b - a)).log10().ceil() as i64 + 1).clamp(3, 15) as usize
}

#[cfg(test)]
#[test]
fn tick_labels_get_enough_digits_to_differ() {
    assert_eq!(label_digits(&[0.0, 5e-6, 10e-6], false), 3);
    assert_eq!(label_digits(&[-100.0, -80.0, -60.0], false), 3);
    let narrow = [4.2e-3, 4.2e-3 + 1e-11, 4.2e-3 + 2e-11];
    let d = label_digits(&narrow, false);
    let s: Vec<String> = narrow.iter().map(|&t| format_si(t, d)).collect();
    assert!(s[0] != s[1] && s[1] != s[2], "{s:?}");
    assert_eq!(label_digits(&[1e3, 1e4], true), 3);
    assert_eq!(label_digits(&[1.0], false), 3);
}

/// Tick positions for one axis: decades on a log axis, nice 1-2-5 steps otherwise.
fn axis_ticks(a: Axis, n: u8, out: &mut Vec<f64>) {
    if a.log {
        log_ticks(a.lo, a.hi, n, out);
    } else {
        ticks(a.lo, a.hi, n, out);
    }
}
