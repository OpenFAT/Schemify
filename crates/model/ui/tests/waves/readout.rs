//! Readout, legend, lane and measurement strings (the render crate only places them).
//!
//! Contents: no `pub` items.

use schemify_ui::Rect;
use schemify_ui::waves::{
    Geometry, ReadoutHead, ReadoutRow, lane_name, legend_label, measure_at, measure_label,
    plot_area, readout_head, readout_rows,
};
use schemify_wave::{Cursor, PaneMode, PaneProps, RawPlot, Request, VarKind, Variable, Waves};

type Head = (
    [&'static str; 2],
    [bool; 2],
    Option<&'static str>,
    Option<&'static str>,
);

/// Cursor a, cursor b, horizontal cursor A of pane 0, the active pane.
type Cursors = (Option<f64>, Option<f64>, Option<f64>, u8);

// ---- private ----

/// Time 0..=10 s: `v(ramp)` = t mV, `v(d0)` = 1 from t = 5, `v(d1)` = 1 at odd t.
fn small() -> Waves {
    let n = 11u8;
    let col = |name: &str, kind| Variable {
        name: name.into(),
        kind,
    };
    let vars = vec![
        col("time", VarKind::Time),
        col("v(ramp)", VarKind::Voltage),
        col("v(d0)", VarKind::Voltage),
        col("v(d1)", VarKind::Voltage),
    ];
    let mut re: Vec<f64> = (0..n).map(f64::from).collect();
    re.extend((0..n).map(|i| f64::from(i) * 1e-3));
    re.extend((0..n).map(|i| f64::from(u8::from(i >= 5))));
    re.extend((0..n).map(|i| f64::from(i % 2)));
    Waves::new(vec![
        RawPlot::from_columns("tran", vars, re, vec![]).expect("four full columns"),
    ])
}

fn add(w: &mut Waves, src: &str, pane: u8) {
    let r = Request::AddTrace {
        src: src.into(),
        dataset: 0,
        pane,
    };
    w.apply(&r).unwrap_or_else(|e| panic!("{src}: {e}"));
}

fn cursors(w: &mut Waves, a: Option<f64>, b: Option<f64>) {
    for (which, at) in [(Cursor::A, a), (Cursor::B, b)] {
        w.apply(&Request::Cursor { which, at })
            .expect("a cursor accepts any position");
    }
}

#[test]
fn readout_head_formats_cursors_and_deltas() {
    // (a, b, horizontal A on pane 0, active pane) -> (x, shown, dx, dy)
    let cases: [(Cursors, Head); 7] = [
        (
            (None, None, None, 0),
            (["x = -", "x = -"], [false, false], None, None),
        ),
        (
            (Some(2.0), None, None, 0),
            (["x = 2", "x = -"], [true, false], None, None),
        ),
        (
            (None, Some(6.0), None, 0),
            (["x = -", "x = 6"], [false, true], None, None),
        ),
        (
            (Some(2.0), Some(6.0), None, 0),
            (
                ["x = 2", "x = 6"],
                [true, true],
                Some("Δx = 4   1/Δx = 250m"),
                None,
            ),
        ),
        (
            (Some(3.0), Some(3.0), None, 0),
            (
                ["x = 3", "x = 3"],
                [true, true],
                Some("Δx = 0   1/Δx = -"),
                None,
            ),
        ),
        (
            (None, None, Some(1e-3), 0),
            (
                ["x = -", "x = -"],
                [false, false],
                None,
                Some("pane 1: A y = 1m  B y = -  Δy = -"),
            ),
        ),
        // An active pane past the last reads the last pane's cursors but prints as given.
        (
            (None, None, Some(1e-3), 5),
            (
                ["x = -", "x = -"],
                [false, false],
                None,
                Some("pane 6: A y = 1m  B y = -  Δy = -"),
            ),
        ),
    ];
    for ((a, b, h, active), (x, shown, dx, dy)) in cases {
        let mut w = small();
        add(&mut w, "v(ramp)", 0);
        cursors(&mut w, a, b);
        w.apply(&Request::HCursor {
            pane: 0,
            which: Cursor::A,
            at: h,
        })
        .unwrap();
        let want = ReadoutHead {
            x: x.map(str::to_owned),
            shown,
            dx: dx.map(str::to_owned),
            dy: dy.map(str::to_owned),
        };
        assert_eq!(readout_head(&w, active), want, "a={a:?} b={b:?} h={h:?}");
    }
}

#[test]
fn readout_rows_read_traces_at_the_cursors() {
    let mut w = small();
    w.apply(&Request::AddPane).unwrap();
    let digital = PaneProps {
        mode: PaneMode::Digital,
        ..PaneProps::default()
    };
    w.apply(&Request::SetPane {
        pane: 1,
        props: digital,
    })
    .unwrap();
    add(&mut w, "v(ramp)", 0);
    add(&mut w, "v(ramp) + v(ramp) - v(ramp)", 0);
    add(&mut w, "v(d0)", 1);
    add(&mut w, "{v(d0), v(d1)}", 1);
    let row = |w: &Waves, t: usize, cells: [&str; 4]| ReadoutRow {
        color: w.color(t),
        cells: cells.map(str::to_owned),
    };
    let all = [
        row(&w, 0, ["v(ramp)", "2m", "6m", "4m"]),
        row(&w, 1, ["v(ramp) + v(ra", "2m", "6m", "4m"]), // cut at READOUT_NAME_CHARS
        row(&w, 2, ["v(d0)", "0", "1", "-"]),
        row(&w, 3, ["{v(d0), v(d1)}", "0x0", "0x2", "-"]), // a bus in the pane's radix
    ];
    let mut out = Vec::new();
    cursors(&mut w, Some(2.0), Some(6.0));
    for max in [0, 1, 3, 4, 100] {
        readout_rows(&w, max, &mut out);
        assert_eq!(out, all[..max.min(4)], "max {max}");
    }
    // No cursors, or a pane on its own x: every value is `-`.
    cursors(&mut w, None, None);
    readout_rows(&w, 1, &mut out);
    assert_eq!(out, [row(&w, 0, ["v(ramp)", "-", "-", "-"])]);
    cursors(&mut w, Some(2.0), Some(6.0));
    let props = PaneProps {
        locked: false,
        ..PaneProps::default()
    };
    w.apply(&Request::SetPane { pane: 0, props }).unwrap();
    readout_rows(&w, 1, &mut out);
    assert_eq!(out, [row(&w, 0, ["v(ramp)", "-", "-", "-"])]);
    // Outside the data: `-`.
    cursors(&mut w, Some(-50.0), Some(50.0));
    readout_rows(&w, 4, &mut out);
    assert!(
        out.iter().all(|r| r.cells[1..] == ["-", "-", "-"]),
        "{out:?}"
    );
}

#[test]
fn legend_and_lane_labels() {
    let mut w = small();
    add(&mut w, "v(ramp)", 0);
    let mut t = String::from("stale");
    // (cursor a, pane locked) -> label
    for (a, locked, want) in [
        (None, true, "v(ramp)"),
        (Some(2.0), true, "v(ramp) = 2m"),
        (Some(50.0), true, "v(ramp)"), // outside the data
        (Some(2.0), false, "v(ramp)"), // own x: cursor a does not apply
    ] {
        cursors(&mut w, a, None);
        let props = PaneProps {
            locked,
            ..PaneProps::default()
        };
        w.apply(&Request::SetPane { pane: 0, props }).unwrap();
        legend_label(&w, 0, &mut t);
        assert_eq!(t, want, "a={a:?} locked={locked}");
    }
    for (src, want) in [
        ("v(d0)", "v(d0)"),
        ("0123456789ab", "012345678"),
        ("µµµµµµµµµµµ", "µµµµµµµµµ"),
        ("", ""),
    ] {
        assert_eq!(lane_name(src), want);
    }
}

#[test]
fn measure_reads_the_plot_under_the_pointer() {
    let mut w = small();
    add(&mut w, "v(ramp)", 0);
    let pane = Rect {
        x: 0.0,
        y: 0.0,
        w: 200.0,
        h: 100.0,
    };
    let geo = Geometry {
        panes: vec![pane],
        ..Geometry::default()
    };
    let p = plot_area(pane);
    let (xa, ya) = (w.x_of(0), w.panes()[0]);
    let near = |got: Option<(f64, f64)>, want: (f64, f64)| {
        got.is_some_and(|(x, y)| {
            (x - want.0).abs() <= 1e-4 * (xa.hi - xa.lo)
                && (y - want.1).abs() <= 1e-4 * (ya.hi - ya.lo)
        })
    };
    let (cx, cy) = (p.x + p.w / 2.0, p.y + p.h / 2.0);
    let mid = ((xa.lo + xa.hi) / 2.0, (ya.lo + ya.hi) / 2.0);
    assert!(near(measure_at(&geo, &w, cx, cy), mid), "plot centre");
    let corner = measure_at(&geo, &w, p.x, p.y + p.h - 1e-3);
    assert!(near(corner, (xa.lo, ya.lo)), "bottom left {corner:?}");
    // Negative space: the y-label strip, the legend, outside every pane, no panes at all.
    for (x, y) in [(10.0, cy), (cx, 5.0), (cx, 150.0), (-1.0, -1.0)] {
        assert_eq!(measure_at(&geo, &w, x, y), None, "({x}, {y})");
        assert_eq!(measure_label(&geo, &w, x, y), None);
    }
    assert_eq!(measure_at(&Geometry::default(), &w, cx, cy), None);
    let (vx, vy) = measure_at(&geo, &w, cx, cy).unwrap();
    let want = format!(
        "y={}  x={}",
        schemify_wave::format_si(vy, 4),
        schemify_wave::format_si(vx, 4)
    );
    assert_eq!(measure_label(&geo, &w, cx, cy), Some(want));
}
