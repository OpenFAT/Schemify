//! Waveform panes painted with egui (D23, spec `docs/spec/waves.md`): grid with
//! engineering-unit ticks, decimated traces (analog), logic lanes (digital) or value histograms,
//! vertical and horizontal cursors, the legend, the box-zoom band, the cursor readout table and
//! the measurement tooltip. The window and an embedded graph draw through the same code.
//!
//! Per frame and pane the cost is O(pixel columns) per trace step after
//! [`schemify_wave::decimate`] or [`schemify_wave::logic_runs`], O(samples) per histogram trace.
//!
//! Contents: `pub fn paint_window(p, geo, waves, ui)`, `pub fn paint_pane(p, area, waves, k,
//! active, drag)`

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect as ERect, Shape, Stroke, pos2};

use schemify_wave::{Axis, AxisScale, Cursor, PaneMode, Scale, TraceColumns, Waves};

use super::{
    Drag, Geometry, READOUT_HEADINGS, WaveUi, lane_name, legend_label, measure_label, plot_area,
    readout_head, readout_rows,
};
use crate::graph;
use crate::layout::Rect;
use crate::view::color;

/// Tick and legend label size in px.
const TICK_PX: f32 = 10.0;
const BG: u32 = 0x0000_00ff;
const GRID: u32 = 0x3a3a_3aff;
const LABEL: u32 = 0xb0b0_b0ff;
const ACTIVE: u32 = 0x5a8c_d2ff;
const CURSOR: [u32; 2] = [0xffff_ffff, 0xff8c_ffff];

/// Every pane of the window, the readout panel and the measurement tooltip.
///
/// # Panics
///
/// Panics on more pane rectangles than `waves` has panes.
pub fn paint_window(p: &Painter, geo: &Geometry, waves: &Waves, ui: &WaveUi) {
    assert!(
        geo.panes.len() <= waves.panes().len(),
        "more pane rects than panes"
    );
    let several = geo.panes.len() > 1;
    for (k, &r) in geo.panes.iter().enumerate() {
        let active = several && usize::from(ui.active_pane()) == k;
        paint_pane(p, r, waves, k as u8, active, ui.drag());
    }
    if geo.readout.w > 0.0 && geo.readout.h > 0.0 {
        readout(p, geo.readout, waves, ui.active_pane());
    }
    if let (true, Some((x, y))) = (ui.measure(), ui.pointer()) {
        measure(p, geo, waves, x, y);
    }
}

/// Pane `k` of `waves` in `area`: grid and ticks, the plot in its mode, cursors, the box-zoom
/// band of `drag`, the legend, and an outline when `active`.
///
/// # Panics
///
/// Panics if `waves` has no pane `k`.
pub fn paint_pane(p: &Painter, area: Rect, waves: &Waves, k: u8, active: bool, drag: Drag) {
    assert!(usize::from(k) < waves.panes().len(), "no pane {k}");
    let pane = rect(area);
    p.rect_filled(pane, 0.0, color(BG));
    let pr = plot_area(area);
    if pr.w < 8.0 || pr.h < 8.0 {
        return;
    }
    let plot = rect(pr);
    let props = waves.props(usize::from(k));
    let (xa, ya) = (waves.x_of(usize::from(k)), waves.panes()[usize::from(k)]);
    let sx = xa.scale(pr.x, pr.x + pr.w);
    let sy = ya.scale(pr.y + pr.h, pr.y);
    let (grid, label) = (color(GRID), color(LABEL));
    let digital = props.mode == PaneMode::Digital;
    let mut ticks = Vec::new();
    axis_ticks(xa, (pr.w / 90.0).clamp(2.0, 12.0) as u8, &mut ticks);
    let digits = graph::tick_digits(&ticks, xa.scale);
    for &t in &ticks {
        let x = sx.px(t).floor() + 0.5;
        if x < plot.left() - 0.5 || x > plot.right() + 0.5 {
            continue;
        }
        p.vline(x, plot.y_range(), Stroke::new(1.0, grid));
        text(
            p,
            &schemify_wave::format_si(t, digits),
            pos2(x, plot.bottom() + 2.0),
            Align2::CENTER_TOP,
            label,
        );
    }
    if !digital {
        axis_ticks(ya, (pr.h / 26.0).clamp(2.0, 10.0) as u8, &mut ticks);
        let digits = graph::tick_digits(&ticks, ya.scale);
        for &t in &ticks {
            let y = sy.px(t).floor() + 0.5;
            if y < plot.top() - 0.5 || y > plot.bottom() + 0.5 {
                continue;
            }
            p.hline(plot.x_range(), y, Stroke::new(1.0, grid));
            text(
                p,
                &schemify_wave::format_si(t, digits),
                pos2(plot.left() - 4.0, y),
                Align2::RIGHT_CENTER,
                label,
            );
        }
    }
    p.rect_stroke(plot, 0.0, Stroke::new(1.0, grid), egui::StrokeKind::Inside);
    let clipped = p.with_clip_rect(plot);
    match props.mode {
        PaneMode::Analog => lines(&clipped, waves, k, &sx, &sy),
        PaneMode::Digital => lanes(&clipped, waves, k, &sx, plot),
        PaneMode::Histogram => histogram(&clipped, waves, k, xa, &sx, &sy, plot),
    }
    for c in [Cursor::A, Cursor::B] {
        let Some(at) = waves.cursors()[c as usize].filter(|_| !props.own_x()) else {
            continue;
        };
        let x = sx.px(at).floor() + 0.5;
        if x < plot.left() || x > plot.right() {
            continue;
        }
        let col = color(CURSOR[c as usize]);
        clipped.vline(x, plot.y_range(), Stroke::new(1.0, col));
        let tag = ERect::from_min_max(pos2(x - 6.0, plot.top()), pos2(x + 6.0, plot.top() + 13.0));
        clipped.rect_filled(tag, 0.0, col);
        text(
            &clipped,
            cursor_name(c),
            tag.center(),
            Align2::CENTER_CENTER,
            color(BG),
        );
    }
    for c in [Cursor::A, Cursor::B] {
        let Some(at) = waves.hcursors(usize::from(k))[c as usize].filter(|_| !digital) else {
            continue;
        };
        let y = sy.px(at).floor() + 0.5;
        if y < plot.top() || y > plot.bottom() {
            continue;
        }
        let col = color(CURSOR[c as usize]);
        clipped.hline(plot.x_range(), y, Stroke::new(1.0, col));
        let tag = ERect::from_min_max(
            pos2(plot.left(), y - 6.5),
            pos2(plot.left() + 12.0, y + 6.5),
        );
        clipped.rect_filled(tag, 0.0, col);
        text(
            &clipped,
            cursor_name(c),
            tag.center(),
            Align2::CENTER_CENTER,
            color(BG),
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
        let band = ERect::from_two_pos(pos2(x0, y0), pos2(x1, y1));
        clipped.rect_filled(
            band,
            0.0,
            Color32::from_rgba_unmultiplied(120, 160, 255, 48),
        );
        let edge = Stroke::new(1.0, Color32::from_rgba_unmultiplied(150, 190, 255, 220));
        clipped.rect_stroke(band, 0.0, edge, egui::StrokeKind::Inside);
    }
    if digital {
        let lh = plot.height() / traces_of(waves, k).count().max(1) as f32;
        for (j, i) in traces_of(waves, k).enumerate() {
            let y = plot.top() + (j as f32 + 0.5) * lh;
            text(
                p,
                lane_name(waves.src(i)),
                pos2(plot.left() - 4.0, y),
                Align2::RIGHT_CENTER,
                color(waves.color(i)),
            );
        }
    } else {
        let mut lx = plot.left() + 2.0;
        let mut t = String::new();
        for i in traces_of(waves, k) {
            legend_label(waves, i, &mut t);
            let r = text(
                p,
                &t,
                pos2(lx, pane.top() + 2.0),
                Align2::LEFT_TOP,
                color(waves.color(i)),
            );
            lx += r.width() + 14.0;
            if lx > plot.right() {
                break;
            }
        }
    }
    if active {
        p.rect_stroke(
            pane.expand(0.5),
            0.0,
            Stroke::new(1.0, color(ACTIVE)),
            egui::StrokeKind::Inside,
        );
    }
}

// ---- private ----

fn rect(r: Rect) -> ERect {
    ERect::from_min_size(pos2(r.x, r.y), egui::vec2(r.w, r.h))
}

fn cursor_name(c: Cursor) -> &'static str {
    if c == Cursor::A { "A" } else { "B" }
}

fn text(p: &Painter, t: &str, at: Pos2, align: Align2, c: Color32) -> ERect {
    p.text(at, align, t, FontId::proportional(TICK_PX), c)
}

fn axis_ticks(a: Axis, n: u8, out: &mut Vec<f64>) {
    assert!(n >= 2, "an axis shows at least two ticks");
    match a.scale {
        AxisScale::Linear => graph::ticks(a.lo, a.hi, n, out),
        AxisScale::Log => graph::log_ticks(a.lo, a.hi, n, out),
    }
}

fn traces_of(waves: &Waves, k: u8) -> impl Iterator<Item = usize> + '_ {
    (0..waves.trace_count()).filter(move |&i| waves.pane_of(i) == k)
}

/// Decimated polylines, one per shown step of each trace.
fn lines(p: &Painter, waves: &Waves, k: u8, sx: &Scale, sy: &Scale) {
    let mut pts = Vec::new();
    for i in traces_of(waves, k) {
        let TraceColumns { x, y } = waves.xy(i);
        assert_eq!(x.len(), y.len(), "trace {i}: x and y columns in step");
        let stroke = Stroke::new(waves.style(i).width() as f32, color(waves.color(i)));
        for st in &waves.steps(i)[waves.shown_steps(i)] {
            let r = st.start as usize..st.end as usize;
            pts.clear();
            schemify_wave::decimate(&x[r.clone()], &y[r], sx, sy, &mut pts);
            if pts.len() >= 2 {
                p.add(Shape::line(
                    pts.iter().map(|q| pos2(q[0], q[1])).collect(),
                    stroke,
                ));
            }
        }
    }
}

/// Logic lanes: levels as steps, unknown runs shaded, buses as boxes with their value.
fn lanes(p: &Painter, waves: &Waves, k: u8, sx: &Scale, plot: ERect) {
    let props = waves.props(usize::from(k));
    let lh = plot.height() / traces_of(waves, k).count().max(1) as f32;
    let mut runs = Vec::new();
    for (j, i) in traces_of(waves, k).enumerate() {
        let top = plot.top() + j as f32 * lh;
        let (hi, lo) = (top + 0.2 * lh, top + 0.8 * lh);
        let bus = waves.bus_bits(i) > 0;
        let thr = (!bus).then_some(props.threshold);
        let col = color(waves.color(i));
        let busy = color((waves.color(i) & 0xffff_ff00) | 0x60);
        let stroke = Stroke::new(waves.style(i).width() as f32, col);
        let TraceColumns { x, y } = waves.xy(i);
        runs.clear();
        for st in &waves.steps(i)[waves.shown_steps(i)] {
            let r = st.start as usize..st.end as usize;
            schemify_wave::logic_runs(&x[r.clone()], &y[r], thr, sx, &mut runs);
        }
        let px = |c: u32| sx.p0 + c as f32;
        let mut pts: Vec<Pos2> = Vec::new();
        let mut last: Option<u32> = None;
        for &run in &runs {
            let (x0, x1) = (px(run.c0), px(run.c1));
            let Some(v) = run.value else {
                p.rect_filled(ERect::from_min_max(pos2(x0, hi), pos2(x1, lo)), 0.0, busy);
                flush(p, &mut pts, stroke);
                last = None;
                continue;
            };
            if bus {
                let b = ERect::from_min_max(pos2(x0, hi), pos2(x1, lo));
                p.rect_stroke(b, 0.0, stroke, egui::StrokeKind::Middle);
                if x1 - x0 >= 16.0 {
                    let g =
                        p.layout_no_wrap(props.radix.format(v), FontId::proportional(TICK_PX), col);
                    if g.size().x + 4.0 <= x1 - x0 {
                        p.galley(b.center() - g.size() / 2.0, g, col);
                    }
                }
                continue;
            }
            let level = if v != 0 { hi } else { lo };
            if last != Some(run.c0) {
                flush(p, &mut pts, stroke);
            }
            pts.push(pos2(x0, level));
            pts.push(pos2(x1, level));
            last = Some(run.c1);
        }
        flush(p, &mut pts, stroke);
        if j > 0 {
            p.hline(plot.x_range(), top, Stroke::new(1.0, color(GRID)));
        }
    }
}

fn flush(p: &Painter, pts: &mut Vec<Pos2>, stroke: Stroke) {
    if pts.len() >= 2 {
        p.add(Shape::line(std::mem::take(pts), stroke));
    }
    pts.clear();
}

/// Value histograms: each bin a shaded bar under the trace's outline.
fn histogram(p: &Painter, waves: &Waves, k: u8, xa: Axis, sx: &Scale, sy: &Scale, plot: ERect) {
    let base = plot.bottom();
    let mut bins = Vec::new();
    for i in traces_of(waves, k) {
        waves.histogram(i, &mut bins);
        let n = bins.len();
        let edge = |b: usize| sx.px(xa.at(b as f64 / n as f64));
        let fill = color((waves.color(i) & 0xffff_ff00) | 0x40);
        let mut outline = vec![pos2(edge(0), base)];
        for (b, &c) in bins.iter().enumerate() {
            let h = sy.px(f64::from(c));
            p.rect_filled(
                ERect::from_two_pos(pos2(edge(b), h), pos2(edge(b + 1), base)),
                0.0,
                fill,
            );
            outline.push(pos2(edge(b), h));
            outline.push(pos2(edge(b + 1), h));
        }
        outline.push(pos2(edge(n), base));
        p.add(Shape::line(
            outline,
            Stroke::new(waves.style(i).width() as f32, color(waves.color(i))),
        ));
    }
}

/// The cursor readout: both cursors' x, their differences, then a row per trace.
fn readout(p: &Painter, area: Rect, waves: &Waves, active: u8) {
    let r = rect(area);
    let p = p.with_clip_rect(r);
    let fg = color(0xd0d0_d0ff);
    let row = 15.0;
    let mut y = r.top() + 4.0;
    let head = readout_head(waves, active);
    let col = |k: usize| r.left() + 6.0 + [0.0, 0.4, 0.6, 0.8][k] * (r.width() - 12.0);
    let at = |x: f32, y: f32| pos2(x, y);
    for (k, name) in ["A", "B"].into_iter().enumerate() {
        if head.shown[k] {
            text(&p, name, at(col(0), y), Align2::LEFT_TOP, color(CURSOR[k]));
        }
        text(&p, &head.x[k], at(col(0) + 16.0, y), Align2::LEFT_TOP, fg);
        y += row;
    }
    if let Some(t) = &head.dx {
        text(&p, t, at(col(0), y), Align2::LEFT_TOP, fg);
    }
    if let Some(t) = &head.dy {
        y += row;
        text(&p, t, at(col(0), y), Align2::LEFT_TOP, fg);
    }
    y += row + 4.0;
    for (k, h) in READOUT_HEADINGS.iter().enumerate() {
        text(&p, h, at(col(k), y), Align2::LEFT_TOP, color(LABEL));
    }
    y += row;
    let fit = ((r.bottom() - y) / row).floor().max(0.0) as usize;
    let mut rows = Vec::new();
    readout_rows(waves, fit, &mut rows);
    assert!(rows.len() <= fit, "the readout keeps to the rows that fit");
    for rr in &rows {
        for (k, t) in rr.cells.iter().enumerate() {
            text(&p, t, at(col(k), y), Align2::LEFT_TOP, color(rr.color));
        }
        y += row;
    }
}

/// The measurement tooltip beside the pointer.
fn measure(p: &Painter, geo: &Geometry, waves: &Waves, x: f32, y: f32) {
    let Some(t) = measure_label(geo, waves, x, y) else {
        return;
    };
    let g = p.layout_no_wrap(t, FontId::proportional(TICK_PX), color(0xffff_40ff));
    let at = pos2(x + 12.0, y - 20.0);
    let bg = ERect::from_min_size(at, g.size() + egui::vec2(8.0, 2.0));
    p.rect_filled(bg, 0.0, Color32::BLACK);
    p.galley(at + egui::vec2(4.0, 1.0), g, Color32::WHITE);
}
