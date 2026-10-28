//! The graph properties dialog (spec waves §8): OK on an unchanged dialog changes nothing
//! (seeded round trips), edits apply, bad ones keep it open.
//!
//! Contents: no `pub` items.

use schemify_ui::waves::{DialogFx, PROPS_DIALOG};
use schemify_ui::{HealthKind, Rect};
use schemify_wave::{Cursor, PaneMode, PaneProps, Radix, Request};

use crate::{VIEW, Win, data, inside};

// ---- private ----

/// Deterministic xorshift (seed-replayable cases without a proptest dependency).
fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

/// A finite value in one of the canonical number text forms: plain decimal, tiny or huge
/// (exponent), integral.
fn pick(seed: &mut u64) -> f64 {
    let r = rng(seed);
    let m = (r >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
    match r % 5 {
        0 => m * 1e3,
        1 => m * 1e-14,
        2 => m * 1e17,
        3 => (m * 1e6).round(),
        _ => m,
    }
}

/// `lo .. lo + w`, a valid linear range.
fn range(seed: &mut u64) -> (f64, f64) {
    let lo = pick(seed);
    let w = lo.abs().max(1.0) * (0.01 + (rng(seed) % 10_000) as f64 / 100.0);
    (lo, lo + w)
}

#[test]
fn properties_dialog_round_trips() {
    let mut seed = 0x2545_f491_4f6c_dd1d;
    let waves = data();
    for case in 0..64 {
        let mut w = Win::new(waves.clone());
        let modes = [PaneMode::Analog, PaneMode::Digital, PaneMode::Histogram];
        let props = PaneProps {
            mode: modes[(rng(&mut seed) % 3) as usize],
            threshold: pick(&mut seed),
            radix: if rng(&mut seed).is_multiple_of(2) {
                Radix::Hex
            } else {
                Radix::Dec
            },
            bins: 2 + (rng(&mut seed) % 1023) as u16,
            locked: rng(&mut seed).is_multiple_of(2),
            step: rng(&mut seed)
                .is_multiple_of(3)
                .then(|| (rng(&mut seed) % 5) as u32),
        };
        let (xl, xh) = range(&mut seed);
        let (yl, yh) = range(&mut seed);
        let mut opt = || rng(&mut seed).is_multiple_of(2).then(|| pick(&mut seed));
        let (ca, cb, ha, hb) = (opt(), opt(), opt(), opt());
        let setup = [
            Request::SetPane { pane: 0, props },
            Request::SetX {
                pane: 0,
                lo: xl,
                hi: xh,
            },
            Request::SetY {
                pane: 0,
                lo: yl,
                hi: yh,
            },
            Request::Cursor {
                which: Cursor::A,
                at: ca,
            },
            Request::Cursor {
                which: Cursor::B,
                at: cb,
            },
            Request::HCursor {
                pane: 0,
                which: Cursor::A,
                at: ha,
            },
            Request::HCursor {
                pane: 0,
                which: Cursor::B,
                at: hb,
            },
        ];
        for r in &setup {
            w.waves
                .apply(r)
                .unwrap_or_else(|e| panic!("case {case}: {r:?}: {e}"));
        }
        let state = |w: &Win| {
            (
                w.waves.props(0),
                w.waves.x_of(0),
                w.waves.panes()[0],
                w.waves.cursors(),
                w.waves.hcursors(0),
            )
        };
        let before = state(&w);
        w.command("waves.props", "", "", 0);
        assert_eq!(w.last.dialog, DialogFx::Open, "case {case}");
        let reqs = w.command("waves.props.ok", "", "", 0);
        assert_eq!(
            w.last.dialog,
            DialogFx::Close,
            "case {case}: {}",
            w.ui.status()
        );
        assert_eq!(reqs.len(), 1, "case {case}: only the properties: {reqs:?}");
        w.apply(reqs);
        assert_eq!(state(&w), before, "case {case}");
    }
}

#[test]
fn properties_dialog_applies_edits_and_keeps_bad_ones_open() {
    let mut w = Win::new(data());
    w.command("waves.props", "", "", 0);
    let bad = w
        .tables
        .health
        .kind
        .iter()
        .filter(|k| matches!(k, HealthKind::UnknownTarget | HealthKind::Orphaned));
    assert_eq!(bad.count(), 0, "the dialog's rows target declared nodes");
    let window = Rect {
        x: 0.0,
        y: 0.0,
        w: VIEW.0,
        h: VIEW.1,
    };
    assert!(inside(w.rect(PROPS_DIALOG), window));
    // A field that does not parse keeps the dialog open and names the field.
    let reqs = w.command("waves.props.ok", "2meg", "waves.props.f.1", 0);
    assert!(reqs.is_empty());
    assert_eq!(w.last.dialog, DialogFx::Keep);
    assert!(w.ui.status().starts_with("bins"), "{}", w.ui.status());
    assert_eq!(w.state.dialog(), w.strings.find(PROPS_DIALOG).unwrap());
    // Mode cycles analog -> digital; Enter in a field applies the whole dialog.
    w.command("waves.props.mode", "", "", 0);
    let reqs = w.command("waves.props.ok", "64", "waves.props.f.1", 0);
    assert_eq!(w.last.dialog, DialogFx::Close);
    assert!(w.state.dialog().is_none());
    w.apply(reqs);
    let p = w.waves.props(0);
    assert_eq!((p.mode, p.bins), (PaneMode::Digital, 64));
    // A typed cursor position (SI suffix) moves only that cursor.
    w.command("waves.props", "", "", 0);
    let reqs = w.command("waves.props.ok", "1.5u", "waves.props.f.7", 0);
    assert_eq!(reqs.len(), 2, "{reqs:?}");
    let at = reqs.iter().find_map(|r| match r {
        Request::Cursor {
            which: Cursor::A,
            at,
        } => Some(*at),
        _ => None,
    });
    assert!(
        matches!(at, Some(Some(v)) if (v - 1.5e-6).abs() < 1e-20),
        "{reqs:?}"
    );
    w.apply(reqs);
    // An x range typed out of order is refused; Cancel closes without changes.
    w.command("waves.props", "", "", 0);
    let x = w.waves.x();
    let reqs = w.command("waves.props.ok", "5", "waves.props.f.3", 0);
    assert!(reqs.is_empty() && w.ui.status().starts_with("x range"));
    w.command("waves.props.cancel", "", "", 0);
    assert!(w.state.dialog().is_none());
    assert_eq!(w.waves.x(), x);
}
