//! The window's composition fits it: lists measured against their panels, no overflow, the
//! filter and the scroll windowing the signal list.
//!
//! Contents: no `pub` items.

use schemify_ui::waves::{Geometry, WaveUi};
use schemify_ui::{HealthKind, InputEvent, Rect, layout};
use schemify_wave::Request;

use crate::{VIEW, Win, data, inside};

// ---- private ----

#[test]
fn window_composes_and_lists_fit_their_panels() {
    let mut w = Win::new(data());
    let bad = w.tables.health.kind.iter().filter(|k| {
        matches!(
            k,
            HealthKind::UnknownTarget | HealthKind::Orphaned | HealthKind::NodeReplaced
        )
    });
    assert_eq!(bad.count(), 0, "session rows only add nodes and set props");
    let rows = w.signal_rows();
    assert_eq!(rows.len(), usize::from(WaveUi::rows_fit(w.geo.signals)));
    assert!(
        rows.len() > 10 && rows.len() < 120,
        "a window of the 120 signals"
    );
    for &id in &rows {
        let r = w.rect(w.strings.get(id));
        assert!(
            inside(r, w.geo.signals),
            "row {} overflows the list",
            w.strings.get(id)
        );
    }
    let window = Rect {
        x: 0.0,
        y: 0.0,
        w: VIEW.0,
        h: VIEW.1,
    };
    assert!(inside(w.rect("waves.status"), window));
    // Three panes share the column equally and stack without overlap.
    w.apply(vec![Request::AddPane, Request::AddPane]);
    assert_eq!(w.geo.panes.len(), 3);
    for pair in w.geo.panes.windows(2) {
        assert!(pair[0].y + pair[0].h <= pair[1].y + 0.5);
        assert!((pair[0].h - pair[1].h).abs() <= 1.0);
    }
    // A smaller window re-measures the lists instead of overflowing.
    let before = rows.len();
    w.ui.set_capacity(0, 0);
    w.frame = layout(
        &w.tables,
        &w.strings,
        &mut w.text,
        &w.state,
        (1200.0, 500.0),
    );
    w.geo = Geometry::of(&w.tables, &w.frame, &w.strings);
    assert!(WaveUi::rows_fit(w.geo.signals) < before as u16);
}

#[test]
fn filter_and_scroll_window_the_signal_list() {
    let mut w = Win::new(data());
    w.command("waves.filter", "net11", "waves.filter", 0);
    let names: Vec<String> = w
        .signal_rows()
        .iter()
        .map(|&s| w.strings.get(s).to_owned())
        .collect();
    // net11 and net110..net119 match; their rows are the variables' column numbers.
    assert_eq!(names.len(), 11);
    assert!(
        names.contains(&"waves.sig.11".to_owned()) && names.contains(&"waves.sig.119".to_owned())
    );
    w.command("waves.filter", "", "waves.filter", 0);
    let first = w.signal_rows()[0];
    let r = w.geo.signals;
    let wheel = InputEvent::Wheel {
        x: r.x + 10.0,
        y: r.y + 10.0,
        dx: 0.0,
        dy: -2.0,
    };
    w.route(&[wheel], 0);
    w.rebuild();
    assert_ne!(w.signal_rows()[0], first, "wheel down scrolls the list");
    for _ in 0..100 {
        w.route(&[wheel], 0);
    }
    w.rebuild();
    let last = *w.signal_rows().last().unwrap();
    assert_eq!(
        w.strings.get(last),
        "waves.sig.120",
        "scrolling stops at the end"
    );
}
