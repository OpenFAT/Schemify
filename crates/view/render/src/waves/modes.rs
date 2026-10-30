//! The plot of one pane in its mode: decimated polylines (analog), logic lanes (digital) or
//! bin outlines (histogram), each trace in its colour. [`super::draw_pane`] calls one per pane,
//! clipped to the plot, after the grid and before the cursors.
//!
//! Contents:
//! - `pub(super) fn draw_lines<P: Painter>(p, waves, k, sx, sy, s)`
//! - `pub(super) fn draw_lanes<P: Painter>(p, waves, k, sx, plot, look, text, s)`
//! - `pub(super) fn draw_histogram<P: Painter>(p, waves, k, xa, sx, sy, plot, s)`

use std::mem;

use kurbo::Rect;

use schemify_ui::TextSystem;
use schemify_wave::{Axis, PaneMode, Scale, TraceColumns, Waves};

use super::{TICK_PX, WaveLook, WaveScratch};
use crate::painter::{self, Painter};
use crate::widgets;

/// Analog mode: each trace's shown steps as decimated polylines.
pub(super) fn draw_lines<P: Painter>(
    p: &mut P,
    waves: &Waves,
    k: u8,
    sx: &Scale,
    sy: &Scale,
    s: &mut WaveScratch,
) {
    for i in super::traces_of(waves, k) {
        let TraceColumns { x, y } = waves.xy(i);
        assert_eq!(x.len(), y.len(), "trace {i}: x and y columns in step");
        s.chains.clear();
        for st in &waves.steps(i)[waves.shown_steps(i)] {
            let r = st.start as usize..st.end as usize;
            debug_assert!(r.end <= x.len(), "trace {i}: a step inside its columns");
            s.pts.clear();
            schemify_wave::decimate(&x[r.clone()], &y[r], sx, sy, &mut s.pts);
            if s.pts.len() >= 2 {
                s.chains.push(&s.pts);
            }
        }
        if !s.chains.is_empty() {
            p.stroke_chains(
                &s.chains,
                false,
                waves.style(i).width(),
                painter::rgba(waves.color(i)),
            );
        }
    }
}

/// Digital mode (spec waves §4): one lane per trace; a signal as a 0/1 level line, a bus as
/// value boxes labelled in the pane's radix, busy columns filled.
#[allow(
    clippy::too_many_arguments,
    reason = "a step of `draw_pane`: the pane's inputs and the plot rectangle"
)]
pub(super) fn draw_lanes<P: Painter>(
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
    assert_eq!(props.mode, PaneMode::Digital, "lanes draw a digital pane");
    let lh = plot.height() / super::traces_of(waves, k).count().max(1) as f64;
    debug_assert!(lh > 0.0, "a lane has height");
    for (j, i) in super::traces_of(waves, k).enumerate() {
        let top = plot.y0 + j as f64 * lh;
        let (hi, lo) = (top + 0.2 * lh, top + 0.8 * lh);
        let bus = waves.bus_bits(i) > 0;
        let thr = (!bus).then_some(props.threshold);
        let color = painter::rgba(waves.color(i));
        let busy = painter::rgba((waves.color(i) & 0xffff_ff00) | 0x60);
        let TraceColumns { x, y } = waves.xy(i);
        assert_eq!(x.len(), y.len(), "trace {i}: x and y columns in step");
        s.runs.clear();
        for st in &waves.steps(i)[waves.shown_steps(i)] {
            let r = st.start as usize..st.end as usize;
            debug_assert!(r.end <= x.len(), "trace {i}: a step inside its columns");
            schemify_wave::logic_runs(&x[r.clone()], &y[r], thr, sx, &mut s.runs);
        }
        let px = |c: u32| f64::from(sx.p0) + f64::from(c);
        s.chains.clear();
        s.pts.clear();
        s.path.truncate(0);
        let mut last: Option<u32> = None;
        for &run in &s.runs {
            debug_assert!(run.c0 <= run.c1, "a run spans its columns left to right");
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
                painter::push_rect(&mut s.path, Rect::new(x0, hi, x1, lo));
                if x1 - x0 >= 16.0 {
                    text.shape_into(&props.radix.format(v), TICK_PX, &mut s.label);
                    let w = f64::from(s.label.width());
                    if w + 4.0 <= x1 - x0 {
                        let ty = (hi + lo) / 2.0 - 7.0;
                        widgets::draw_text(p, &s.label, (x0 + x1 - w) / 2.0, ty, color, &mut s.ui);
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
                painter::rgba(look.grid),
            );
        }
    }
}

/// Histogram mode (spec waves §5): each trace's bin counts over the pane's own x, as a filled
/// step outline in the trace's colour.
#[allow(
    clippy::too_many_arguments,
    reason = "a step of `draw_pane`: the pane's inputs and the plot rectangle"
)]
pub(super) fn draw_histogram<P: Painter>(
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
    for i in super::traces_of(waves, k) {
        let mut bins = mem::take(&mut s.bins);
        waves.histogram(i, &mut bins);
        let n = bins.len();
        let props = waves.props(usize::from(k));
        assert_eq!(n, usize::from(props.bins), "trace {i}: a count per bin");
        let edge = |b: usize| sx.px(xa.at(b as f64 / n as f64));
        s.pts.clear();
        s.pts.push([edge(0), base]);
        for (b, &c) in bins.iter().enumerate() {
            let h = sy.px(f64::from(c));
            s.pts.push([edge(b), h]);
            s.pts.push([edge(b + 1), h]);
        }
        s.pts.push([edge(n), base]);
        assert_eq!(
            s.pts.len(),
            2 * n + 2,
            "two points per bin, two on the base"
        );
        s.chains.clear();
        s.chains.push(&s.pts);
        p.fill_chains(
            &s.chains,
            painter::rgba((waves.color(i) & 0xffff_ff00) | 0x40),
        );
        p.stroke_chains(
            &s.chains,
            false,
            waves.style(i).width(),
            painter::rgba(waves.color(i)),
        );
        s.bins = bins;
    }
}
