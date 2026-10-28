//! A plain-number parameter value → the value snapped to its param's `grid` or `step`
//! (exact decimal arithmetic), plus the parameter facts edits read for it.
//! Cites: D13 (params: computed, grid/step snapping; expressions are elab's).
//!
//! Contents:
//! - `pub(crate) struct ParamFacts`: `snap`; `pub(crate) type ParamMap`
//! - `fn snap(value: &str, grid: &str, origin: &str) -> Option<String>`

use schemify_format::is_number;
use std::collections::BTreeMap;

/// What edits need to know about one parameter of an instantiated cell: whether it is
/// computed (`E030`) and its plain-number snapping rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParamFacts {
    pub(crate) name: String,
    pub(crate) computed: bool,
    pub(crate) grid: Option<String>,
    pub(crate) step: Option<String>,
    pub(crate) min: Option<String>,
}

/// Per cell reference text (as written in this document): the cell's parameters, or
/// `None` when the reference does not resolve (then nothing is snapped or checked;
/// elab reports `E001`).
pub(crate) type ParamMap = BTreeMap<String, Option<Vec<ParamFacts>>>;

/// A decimal `m / 10^s`.
#[derive(Clone, Copy)]
struct Dec {
    m: i128,
    s: u32,
}

/// Digits a snapped operand may have; beyond it the value is left for elab.
const DIGITS_MAX: usize = 30;

impl ParamFacts {
    /// Returns the snapped value when `value` is a plain number and the param has a
    /// plain-number `grid` (multiples of it) or `step` (from `min` when that is a plain
    /// number, else from 0); `None` when nothing changes (expressions are elab's).
    pub(crate) fn snap(&self, value: &str) -> Option<String> {
        if !is_number(value) {
            return None;
        }
        // A `grid` that is an expression wins over `step`: elab evaluates and gates it.
        let snapped = match (self.grid.as_deref(), self.step.as_deref()) {
            (Some(g), _) => snap(value, g, "0"),
            (None, Some(st)) => snap(value, st, self.min.as_deref().unwrap_or("0")),
            (None, None) => None,
        };
        assert!(
            snapped.as_deref().is_none_or(is_number),
            "snapping yields plain numbers"
        );
        snapped.filter(|s| s != value)
    }
}

// ---- private ----

/// Snaps `value` to `origin + k * grid` for the nearest integer `k` (ties away from
/// zero), in exact decimal arithmetic. Returns the shortest decimal text, or `None` when
/// an operand is not a plain number, the grid is not positive, or digits overflow.
fn snap(value: &str, grid: &str, origin: &str) -> Option<String> {
    let (v, g, o) = (dec(value)?, dec(grid)?, dec(origin)?);
    let s = v.s.max(g.s).max(o.s);
    let scale = |d: Dec| d.m.checked_mul(10i128.checked_pow(s - d.s)?);
    let (v, g, o) = (scale(v)?, scale(g)?, scale(o)?);
    if g <= 0 {
        return None;
    }
    let diff = v.checked_sub(o)?;
    let (q, r) = (diff / g, diff % g);
    let k = if 2 * r.abs() >= g {
        q + diff.signum()
    } else {
        q
    };
    let out = o.checked_add(k.checked_mul(g)?)?;
    assert_eq!((out - o) % g, 0, "on grid");
    assert!(2 * (out - v).abs() <= g, "nearest");
    Some(format_dec(out, s))
}

fn dec(text: &str) -> Option<Dec> {
    if !is_number(text) || text.len() > DIGITS_MAX {
        return None;
    }
    let (neg, body) = text.strip_prefix('-').map_or((false, text), |b| (true, b));
    let (whole, frac) = body.split_once('.').unwrap_or((body, ""));
    let mut m: i128 = 0;
    for b in whole.bytes().chain(frac.bytes()) {
        m = m * 10 + i128::from(b - b'0');
    }
    assert!(m >= 0);
    assert!(m < 10i128.pow(DIGITS_MAX as u32), "at most 30 digits");
    Some(Dec {
        m: if neg { -m } else { m },
        s: frac.len() as u32,
    })
}

fn format_dec(m: i128, s: u32) -> String {
    let neg = m < 0;
    let digits = m.unsigned_abs().to_string();
    let s = s as usize;
    let padded = if digits.len() <= s {
        format!("{}{digits}", "0".repeat(s + 1 - digits.len()))
    } else {
        digits
    };
    let (whole, frac) = padded.split_at(padded.len() - s);
    let frac = frac.trim_end_matches('0');
    let mut out = String::new();
    if neg && (whole != "0" || !frac.is_empty()) {
        out.push('-');
    }
    out.push_str(whole);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    assert!(is_number(&out));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_is_exact_decimal() {
        assert_eq!(snap("0.153", "0.005", "0").as_deref(), Some("0.155"));
        assert_eq!(snap("0.152", "0.005", "0").as_deref(), Some("0.15"));
        assert_eq!(
            snap("0.1525", "0.005", "0").as_deref(),
            Some("0.155"),
            "ties away from zero"
        );
        assert_eq!(snap("-0.1525", "0.005", "0").as_deref(), Some("-0.155"));
        assert_eq!(snap("7", "2", "0").as_deref(), Some("8"));
        assert_eq!(snap("0.4", "1", "0").as_deref(), Some("0"));
        assert_eq!(snap("-0.4", "1", "0").as_deref(), Some("0"));
        assert_eq!(snap("1.07", "0.1", "0.15").as_deref(), Some("1.05"));
        assert_eq!(snap("1u", "0.1", "0"), None, "expressions are elab's");
        assert_eq!(snap("1", "0", "0"), None);
        assert_eq!(snap("1", "-1", "0"), None);
        assert_eq!(snap("1", "w", "0"), None);
    }

    #[test]
    fn param_facts_choose_grid_then_step() {
        let mut p = ParamFacts {
            name: "L".into(),
            computed: false,
            grid: Some("0.005".into()),
            step: Some("1".into()),
            min: None,
        };
        assert_eq!(p.snap("0.151").as_deref(), Some("0.15"));
        assert_eq!(p.snap("0.15"), None, "already on grid");
        p.grid = Some("lambda".into());
        assert_eq!(
            p.snap("2.6"),
            None,
            "an expression grid is elab's, even with a step"
        );
        p.grid = None;
        p.min = Some("0.5".into());
        assert_eq!(p.snap("2.6").as_deref(), Some("2.5"));
        p.min = Some("lmin".into());
        assert_eq!(p.snap("2.6"), None, "step from an expression min is elab's");
    }
}
