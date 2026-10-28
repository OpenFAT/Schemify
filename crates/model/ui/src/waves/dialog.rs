//! The graph properties dialog (spec waves §8, [`super::PROPS_DIALOG`]): its working copy
//! opened from the viewer's values, and the requests its OK applies.
//!
//! Contents:
//! - `pub(super) WaveUi::open_dialog(waves, pane) -> Effect`
//! - `pub(super) WaveUi::dialog_requests(waves, d) -> Result<Vec<Request>, String>`
//! - `pub(super) fn field_index(node) -> Option<usize>`

use schemify_wave::{Axis, Cursor, PaneProps, Request, Waves, parse_num, write_num};

use super::{Dialog, DialogFx, Drag, Effect, FIELDS, WaveUi};

impl WaveUi {
    /// Opens the properties dialog on pane `pane` with the values the viewer shows now.
    pub(super) fn open_dialog(&mut self, waves: &Waves, pane: u8) -> Effect {
        let p = usize::from(pane);
        let props = waves.props(p);
        let (xa, ya) = (waves.x_of(p), waves.panes()[p]);
        let [a, b] = waves.cursors();
        let [ha, hb] = waves.hcursors(p);
        let shown = [
            Some(props.threshold),
            Some(f64::from(props.bins)),
            props.step.map(f64::from),
            Some(xa.lo),
            Some(xa.hi),
            Some(ya.lo),
            Some(ya.hi),
            a,
            b,
            ha,
            hb,
        ];
        let mut text: [String; FIELDS.len()] = Default::default();
        for (t, v) in text.iter_mut().zip(shown) {
            if let Some(v) = v {
                write_num(t, v);
            }
        }
        self.dialog = Some(Dialog {
            pane,
            props,
            text,
            shown,
        });
        // The dialog is modal: the gesture that opened it ends here.
        self.drag = Drag::None;
        Effect {
            redraw: true,
            recompose: true,
            dialog: DialogFx::Open,
        }
    }

    /// The requests the open dialog's OK applies (spec waves §8): the pane's properties, then
    /// each range or cursor whose field now reads a different value than it showed. `Err`
    /// names the first field that does not parse or a range that is not valid.
    pub(super) fn dialog_requests(
        &self,
        waves: &Waves,
        d: &Dialog,
    ) -> Result<Vec<Request>, String> {
        let mut v = [None; FIELDS.len()];
        for (k, t) in d.text.iter().enumerate() {
            let t = t.trim();
            if !t.is_empty() {
                let n = parse_num(t).map_err(|_| format!("{}: not a number: {t}", FIELDS[k]))?;
                v[k] = Some(n);
            }
        }
        let whole = |k: usize, max: f64| match v[k] {
            Some(n) if n.fract() == 0.0 && (0.0..=max).contains(&n) => Ok(Some(n)),
            None => Ok(None),
            Some(_) => Err(format!("{}: a whole number up to {max}", FIELDS[k])),
        };
        let threshold = v[0].ok_or("threshold: a number")?;
        let bins = whole(1, f64::from(schemify_wave::MAX_BINS))?.ok_or("bins: a number")? as u16;
        let step = whole(2, f64::from(u32::MAX))?.map(|n| n as u32);
        let props = PaneProps {
            threshold,
            bins,
            step,
            ..d.props
        };
        if !props.valid() {
            return Err(format!(
                "bins: 2 to {} (threshold: finite)",
                schemify_wave::MAX_BINS
            ));
        }
        let pane = d.pane;
        let p = usize::from(pane);
        let mut out = vec![Request::SetPane { pane, props }];
        for (k, log, what) in [(3, waves.x_of(p).log, "x"), (5, waves.panes()[p].log, "y")] {
            if (v[k], v[k + 1]) == (d.shown[k], d.shown[k + 1]) {
                continue;
            }
            let (Some(lo), Some(hi)) = (v[k], v[k + 1]) else {
                return Err(format!("{what} min/max: two numbers"));
            };
            if !(Axis { lo, hi, log }).valid() {
                return Err(format!("{what} range: invalid ({lo} .. {hi})"));
            }
            out.push(if k == 3 {
                Request::SetX { pane, lo, hi }
            } else {
                Request::SetY { pane, lo, hi }
            });
        }
        for (k, which) in [(7, Cursor::A), (8, Cursor::B)] {
            if v[k] != d.shown[k] {
                out.push(Request::Cursor { which, at: v[k] });
            }
        }
        for (k, which) in [(9, Cursor::A), (10, Cursor::B)] {
            if v[k] != d.shown[k] {
                out.push(Request::HCursor {
                    pane,
                    which,
                    at: v[k],
                });
            }
        }
        debug_assert!(matches!(out[0], Request::SetPane { .. }));
        Ok(out)
    }
}

/// The dialog field `node` names (`waves.props.f.<k>`), if any.
pub(super) fn field_index(node: &str) -> Option<usize> {
    node.strip_prefix("waves.props.f.")
        .and_then(|k| k.parse::<usize>().ok())
        .filter(|&k| k < FIELDS.len())
}
